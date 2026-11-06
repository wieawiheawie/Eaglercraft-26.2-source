use std::io::{Read, Write};

use aes::{
    cipher::{BlockEncrypt, KeyInit},
    Aes128,
};
use flate2::{
    read::ZlibDecoder,
    write::ZlibEncoder,
    Compression,
};
use rand::{rngs::OsRng, RngCore};
use rsa::{pkcs8::DecodePublicKey, Pkcs1v15Encrypt, RsaPublicKey};
use sha1::{Digest, Sha1};

const PROTOCOL_VERSION: i32 = 776;
const MAX_PACKET_BYTES: usize = 2 * 1024 * 1024;
const MAX_BUFFERED_BYTES: usize = 4 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LoginState {
    AwaitingLogin,
    Authenticating,
    AuthenticationRequired,
    AwaitingLoginSuccess,
    Configuring,
    Ready,
    Failed,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LoginError {
    InvalidTarget,
    InvalidUsername,
    InvalidPacket(&'static str),
    VarIntTooLong,
    NegativeLength,
    PacketTooLarge(usize),
    InvalidUtf8,
    InvalidPublicKey(String),
    RsaFailure(String),
    RandomFailure,
    AuthenticationRequired { server_hash: String },
    AuthenticationNotExpected,
    Compression(String),
    TransportClosed,
}

/// AES/CFB8 stream state. The Minecraft protocol uses the negotiated AES-128
/// secret as both the key and the initial IV, and keeps this state across
/// WebSocket/Wisp message boundaries.
struct AesCfb8 {
    cipher: Aes128,
    iv: [u8; 16],
}

impl AesCfb8 {
    fn new(key: [u8; 16]) -> Self {
        Self {
            cipher: Aes128::new_from_slice(&key).expect("AES-128 key length is fixed"),
            iv: key,
        }
    }

    fn encrypt(&mut self, bytes: &mut [u8]) {
        for byte in bytes {
            let mut block = aes::cipher::Block::<Aes128>::clone_from_slice(&self.iv);
            self.cipher.encrypt_block(&mut block);
            let output = *byte ^ block[0];
            self.iv.copy_within(1.., 0);
            self.iv[15] = output;
            *byte = output;
        }
    }

    fn decrypt(&mut self, bytes: &mut [u8]) {
        for byte in bytes {
            let input = *byte;
            let mut block = aes::Block::clone_from_slice(&self.iv);
            self.cipher.encrypt_block(&mut block);
            let output = input ^ block[0];
            self.iv.copy_within(1.., 0);
            self.iv[15] = input;
            *byte = output;
        }
    }
}

/// Minecraft 26.2 vanilla TCP login and configuration state machine.
///
/// It can finish offline-mode login, including the vanilla RSA challenge and
/// AES-CFB8 transport switch. Online-mode servers expose their SHA-1 session
/// hash and wait for a genuine authenticated Minecraft session join before
/// the encryption response is sent.
pub struct MinecraftLoginSession {
    host: String,
    port: u16,
    username: String,
    offline_uuid: [u8; 16],
    state: LoginState,
    inbound: Vec<u8>,
    inbound_cursor: usize,
    packet_scratch: Vec<u8>,
    outbound: Vec<u8>,
    compression_threshold: Option<usize>,
    encryptor: Option<AesCfb8>,
    decryptor: Option<AesCfb8>,
    pending_public_key: Option<Vec<u8>>,
    pending_challenge: Option<Vec<u8>>,
    pending_server_id: Option<String>,
    pending_secret: Option<[u8; 16]>,
    server_hash: Option<String>,
    failure: Option<String>,
}

impl MinecraftLoginSession {
    pub fn new(host: &str, port: u16, username: &str) -> Result<Self, LoginError> {
        if host.is_empty()
            || host.len() > 255
            || host.chars().any(char::is_whitespace)
            || port == 0
        {
            return Err(LoginError::InvalidTarget);
        }
        if username.is_empty()
            || username.len() > 16
            || !username.bytes().all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
        {
            return Err(LoginError::InvalidUsername);
        }

        let mut session = Self {
            host: host.to_owned(),
            port,
            username: username.to_owned(),
            offline_uuid: offline_uuid(username),
            state: LoginState::AwaitingLogin,
            inbound: Vec::with_capacity(64 * 1024),
            inbound_cursor: 0,
            packet_scratch: Vec::with_capacity(16 * 1024),
            outbound: Vec::with_capacity(4096),
            compression_threshold: None,
            encryptor: None,
            decryptor: None,
            pending_public_key: None,
            pending_challenge: None,
            pending_server_id: None,
            pending_secret: None,
            server_hash: None,
            failure: None,
        };
        session.build_initial_handshake()?;
        Ok(session)
    }

    pub fn state(&self) -> LoginState {
        self.state
    }

    pub fn failure(&self) -> Option<&str> {
        self.failure.as_deref()
    }

    pub fn server_hash(&self) -> Option<&str> {
        self.server_hash.as_deref()
    }

    pub fn profile_uuid(&self) -> [u8; 16] {
        self.offline_uuid
    }

    /// Moves the online-mode handshake forward only after an external official
    /// Minecraft session service has confirmed the server hash for this profile.
    /// This method does not fake or bypass Mojang/Microsoft authentication.
    pub fn complete_online_session_join(&mut self) -> Result<(), LoginError> {
        if self.state != LoginState::AuthenticationRequired {
            return Err(LoginError::AuthenticationNotExpected);
        }
        self.send_encryption_response()
    }

    pub fn take_outbound(&mut self, destination: &mut Vec<u8>) {
        destination.clear();
        destination.append(&mut self.outbound);
    }

    /// Feed raw TCP stream bytes after the Wisp layer. Decryption, if enabled,
    /// is applied before packet framing and survives arbitrary WebSocket splits.
    pub fn receive_tcp(&mut self, bytes: &[u8]) -> Result<LoginState, LoginError> {
        if self.state == LoginState::Failed {
            return Err(LoginError::TransportClosed);
        }
        if self.inbound.len().saturating_sub(self.inbound_cursor) + bytes.len()
            > MAX_BUFFERED_BYTES
        {
            return self.fail(LoginError::PacketTooLarge(bytes.len()));
        }

        let mut decrypted = bytes.to_vec();
        if let Some(cipher) = &mut self.decryptor {
            cipher.decrypt(&mut decrypted);
        }
        self.compact_inbound();
        self.inbound.extend_from_slice(&decrypted);

        loop {
            let remaining = &self.inbound[self.inbound_cursor..];
            let Some((frame_length, prefix_length)) = read_varint(remaining)? else {
                break;
            };
            if frame_length < 0 {
                return self.fail(LoginError::NegativeLength);
            }
            let frame_length = frame_length as usize;
            if frame_length > MAX_PACKET_BYTES {
                return self.fail(LoginError::PacketTooLarge(frame_length));
            }
            if remaining.len() < prefix_length + frame_length {
                break;
            }

            let start = self.inbound_cursor + prefix_length;
            let end = start + frame_length;
            self.packet_scratch.clear();
            self.packet_scratch
                .extend_from_slice(&self.inbound[start..end]);
            self.inbound_cursor = end;

            let mut frame = std::mem::take(&mut self.packet_scratch);
            let decoded = self.decode_compressed_frame(&frame);
            frame.clear();
            self.packet_scratch = frame;
            let body = match decoded {
                Ok(body) => body,
                Err(error) => return self.fail(error),
            };
            self.handle_packet(&body)?;
        }

        if self.inbound_cursor == self.inbound.len() {
            self.inbound.clear();
            self.inbound_cursor = 0;
        } else if self.inbound_cursor >= 64 * 1024
            && self.inbound_cursor >= self.inbound.len() / 2
        {
            self.compact_inbound();
        }
        Ok(self.state)
    }

    fn build_initial_handshake(&mut self) -> Result<(), LoginError> {
        let mut handshake = Vec::with_capacity(self.host.len() + 24);
        write_varint(0, &mut handshake); // Handshake packet id.
        write_varint(PROTOCOL_VERSION, &mut handshake);
        write_string(&self.host, &mut handshake)?;
        handshake.extend_from_slice(&self.port.to_be_bytes());
        write_varint(2, &mut handshake); // Next state: login.
        self.write_frame(&handshake, false)?;

        let mut login_start = Vec::with_capacity(self.username.len() + 24);
        write_varint(0, &mut login_start); // Login start packet id.
        write_string(&self.username, &mut login_start)?;
        login_start.extend_from_slice(&self.offline_uuid);
        self.write_frame(&login_start, false)?;
        self.state = LoginState::AwaitingLoginSuccess;
        Ok(())
    }

    fn handle_packet(&mut self, body: &[u8]) -> Result<(), LoginError> {
        let Some((packet_id, packet_id_len)) = read_varint(body)? else {
            return self.fail(LoginError::InvalidPacket("missing packet id"));
        };
        let payload = &body[packet_id_len..];

        match self.state {
            LoginState::AwaitingLogin => self.fail(LoginError::InvalidPacket("unexpected initial state")),
            LoginState::Authenticating | LoginState::AwaitingLoginSuccess => {
                match packet_id {
                    0 => self.fail(LoginError::InvalidPacket("server disconnected during login")),
                    1 => self.handle_encryption_request(payload),
                    2 => self.handle_login_finished(),
                    3 => self.handle_compression(payload),
                    4 => self.handle_custom_query(payload),
                    5 => self.handle_cookie_request(payload, true),
                    _ => self.fail(LoginError::InvalidPacket("unknown login packet id")),
                }
            }
            LoginState::AuthenticationRequired => Err(LoginError::AuthenticationRequired {
                server_hash: self.server_hash.clone().unwrap_or_default(),
            }),
            LoginState::Configuring => self.handle_configuration_packet(packet_id, payload),
            LoginState::Ready => self.handle_play_common_packet(packet_id, payload),
            LoginState::Failed => Err(LoginError::TransportClosed),
        }
    }

    fn handle_encryption_request(&mut self, payload: &[u8]) -> Result<(), LoginError> {
        let mut cursor = 0;
        let server_id = read_string(payload, &mut cursor, 20)?;
        let public_key = read_byte_array(payload, &mut cursor, 8192)?;
        let challenge = read_byte_array(payload, &mut cursor, 1024)?;
        let should_authenticate = *payload
            .get(cursor)
            .ok_or(LoginError::InvalidPacket("missing online-authentication flag"))?
            != 0;
        cursor += 1;
        if cursor != payload.len() {
            return self.fail(LoginError::InvalidPacket("trailing encryption request bytes"));
        }

        let key = RsaPublicKey::from_public_key_der(&public_key)
            .map_err(|error| LoginError::InvalidPublicKey(error.to_string()))?;
        let mut secret = [0_u8; 16];
        OsRng
            .try_fill_bytes(&mut secret)
            .map_err(|_| LoginError::RandomFailure)?;

        let mut digest = Sha1::new();
        digest.update(server_id.as_bytes());
        digest.update(secret);
        digest.update(&public_key);
        self.server_hash = Some(signed_sha1_hex(&digest.finalize()));
        self.pending_public_key = Some(public_key);
        self.pending_challenge = Some(challenge);
        self.pending_server_id = Some(server_id);

        if should_authenticate {
            // Store the session key until an official session-server join has
            // been confirmed. Do not emit an encryption response prematurely.
            self.pending_secret = Some(secret);
            self.state = LoginState::AuthenticationRequired;
            return Ok(());
        }

        self.send_encryption_response_with_secret(secret, key)
    }

    fn send_encryption_response(&mut self) -> Result<(), LoginError> {
        let secret = self
            .pending_secret
            .take()
            .ok_or(LoginError::InvalidPacket("missing pending shared secret"))?;
        let public_key_der = self
            .pending_public_key
            .as_ref()
            .ok_or(LoginError::InvalidPacket("missing server public key"))?;
        let challenge = self
            .pending_challenge
            .as_ref()
            .ok_or(LoginError::InvalidPacket("missing server challenge"))?;
        let key = RsaPublicKey::from_public_key_der(public_key_der)
            .map_err(|error| LoginError::InvalidPublicKey(error.to_string()))?;
        self.send_encryption_response_with_secret(secret, key)
    }

    fn send_encryption_response_with_secret(
        &mut self,
        secret: [u8; 16],
        public_key: RsaPublicKey,
    ) -> Result<(), LoginError> {
        let mut rng = OsRng;
        let challenge = self
            .pending_challenge
            .as_ref()
            .ok_or(LoginError::InvalidPacket("missing server challenge"))?;
        let encrypted_secret = public_key
            .encrypt(&mut rng, Pkcs1v15Encrypt, &secret)
            .map_err(|error| LoginError::RsaFailure(error.to_string()))?;
        let encrypted_challenge = public_key
            .encrypt(&mut rng, Pkcs1v15Encrypt, challenge)
            .map_err(|error| LoginError::RsaFailure(error.to_string()))?;

        let mut packet = Vec::with_capacity(encrypted_secret.len() + encrypted_challenge.len() + 16);
        write_varint(1, &mut packet); // Serverbound key packet.
        write_byte_array(&encrypted_secret, &mut packet)?;
        write_byte_array(&encrypted_challenge, &mut packet)?;
        self.write_frame(&packet, false)?;

        self.encryptor = Some(AesCfb8::new(secret));
        self.decryptor = Some(AesCfb8::new(secret));
        self.pending_public_key = None;
        self.pending_challenge = None;
        self.pending_server_id = None;
        self.state = LoginState::AwaitingLoginSuccess;
        Ok(())
    }

    fn handle_login_finished(&mut self) -> Result<(), LoginError> {
        // GameProfile details are not needed for the TCP phase transition; the
        // framed packet has already been bounded and consumed in its entirety.
        let login_ack = [3_u8]; // Login serverbound packet id 3, empty body.
        self.write_frame(&login_ack, true)?;

        let mut brand = Vec::with_capacity(64);
        write_varint(2, &mut brand); // Configuration custom payload.
        write_string("minecraft:brand", &mut brand)?;
        write_string("Eaglercraft Rust", &mut brand)?;
        self.write_frame(&brand, true)?;

        let client_information = [
            0, // Client information packet id.
            5, b'e', b'n', b'_', b'u', b's', // String "en_us".
            2, // View distance.
            0, // Chat visibility FULL.
            1, // Chat colors.
            0, // Skin customization bit mask.
            0, // Main hand RIGHT.
            0, // Text filtering disabled.
            0, // Server listing disabled.
            0, // Particles ALL.
        ];
        self.write_frame(&client_information, true)?;
        self.state = LoginState::Configuring;
        Ok(())
    }

    fn handle_compression(&mut self, payload: &[u8]) -> Result<(), LoginError> {
        let Some((threshold, length)) = read_varint(payload)? else {
            return self.fail(LoginError::InvalidPacket("truncated compression threshold"));
        };
        if length != payload.len() || threshold < 0 {
            return self.fail(LoginError::InvalidPacket("invalid compression threshold"));
        }
        self.compression_threshold = Some(threshold as usize);
        Ok(())
    }

    fn handle_custom_query(&mut self, payload: &[u8]) -> Result<(), LoginError> {
        let Some((transaction_id, length)) = read_varint(payload)? else {
            return self.fail(LoginError::InvalidPacket("truncated custom query transaction"));
        };
        if length > payload.len() {
            return self.fail(LoginError::InvalidPacket("invalid custom query transaction"));
        }
        let mut response = Vec::with_capacity(8);
        write_varint(2, &mut response); // Serverbound custom query answer.
        write_varint(transaction_id, &mut response);
        response.push(0); // Null payload.
        self.write_frame(&response, true)
    }

    fn handle_cookie_request(&mut self, payload: &[u8], login_phase: bool) -> Result<(), LoginError> {
        let mut cursor = 0;
        let key = read_string(payload, &mut cursor, 256)?;
        if cursor != payload.len() {
            return self.fail(LoginError::InvalidPacket("trailing cookie request bytes"));
        }
        let mut response = Vec::with_capacity(key.len() + 8);
        write_varint(if login_phase { 4 } else { 1 }, &mut response);
        write_string(&key, &mut response)?;
        response.push(0); // Null cookie payload.
        self.write_frame(&response, true)
    }

    fn handle_configuration_packet(&mut self, packet_id: i32, payload: &[u8]) -> Result<(), LoginError> {
        match packet_id {
            0 => self.handle_cookie_request(payload, false),
            2 => self.fail(LoginError::InvalidPacket("server disconnected during configuration")),
            3 => {
                let finish = [3_u8]; // Finish configuration, empty packet.
                self.write_frame(&finish, true)?;
                self.state = LoginState::Ready;
                Ok(())
            }
            4 if payload.len() == 8 => {
                let mut response = Vec::with_capacity(9);
                write_varint(4, &mut response);
                response.extend_from_slice(payload);
                self.write_frame(&response, true)
            }
            5 if payload.len() == 4 => {
                let mut response = Vec::with_capacity(5);
                write_varint(5, &mut response);
                response.extend_from_slice(payload);
                self.write_frame(&response, true)
            }
            14 => {
                // Select no known packs. Resource/model/registry compatibility
                // is handled by the Rust resource pipeline during the game phase.
                let mut response = Vec::with_capacity(2);
                write_varint(7, &mut response);
                write_varint(0, &mut response);
                self.write_frame(&response, true)
            }
            // Registry data, enabled features, tags and newer metadata packets
            // are fully consumed at the frame boundary. Registry decoding and
            // play-state packet codecs remain a separate parity milestone.
            1 | 6..=13 | 15..=19 => Ok(()),
            _ => self.fail(LoginError::InvalidPacket("unknown configuration packet id")),
        }
    }

    fn handle_play_common_packet(&mut self, packet_id: i32, payload: &[u8]) -> Result<(), LoginError> {
        // Play protocol packet identifiers are revision-specific and will be
        // decoded from the bound Rust game protocol. Never reply to guessed ids.
        let _ = (packet_id, payload);
        Ok(())
    }

    fn decode_compressed_frame(&self, frame: &[u8]) -> Result<Vec<u8>, LoginError> {
        let Some(threshold) = self.compression_threshold else {
            return Ok(frame.to_vec());
        };
        let Some((declared_size, prefix_length)) = read_varint(frame)? else {
            return Err(LoginError::InvalidPacket("missing compression data length"));
        };
        if declared_size < 0 || declared_size as usize > MAX_PACKET_BYTES {
            return Err(LoginError::PacketTooLarge(declared_size.max(0) as usize));
        }
        if declared_size == 0 {
            if frame.len() == prefix_length {
                return Err(LoginError::InvalidPacket("empty uncompressed packet"));
            }
            return Ok(frame[prefix_length..].to_vec());
        }
        if (declared_size as usize) < threshold {
            return Err(LoginError::InvalidPacket("packet compressed below server threshold"));
        }
        let mut decoder = ZlibDecoder::new(&frame[prefix_length..]);
        let mut output = Vec::with_capacity(declared_size as usize);
        decoder
            .take((MAX_PACKET_BYTES + 1) as u64)
            .read_to_end(&mut output)
            .map_err(|error| LoginError::Compression(error.to_string()))?;
        if output.len() != declared_size as usize {
            return Err(LoginError::InvalidPacket("decompressed packet length mismatch"));
        }
        Ok(output)
    }

    fn write_frame(&mut self, body: &[u8], allow_compression: bool) -> Result<(), LoginError> {
        if body.is_empty() || body.len() > MAX_PACKET_BYTES {
            return Err(LoginError::PacketTooLarge(body.len()));
        }
        let mut frame_body = Vec::with_capacity(body.len() + 16);
        if allow_compression {
            if let Some(threshold) = self.compression_threshold {
                if body.len() >= threshold {
                    write_varint(body.len() as i32, &mut frame_body);
                    let mut encoder = ZlibEncoder::new(Vec::new(), Compression::fast());
                    encoder
                        .write_all(body)
                        .map_err(|error| LoginError::Compression(error.to_string()))?;
                    let compressed = encoder
                        .finish()
                        .map_err(|error| LoginError::Compression(error.to_string()))?;
                    frame_body.extend_from_slice(&compressed);
                } else {
                    write_varint(0, &mut frame_body);
                    frame_body.extend_from_slice(body);
                }
            } else {
                frame_body.extend_from_slice(body);
            }
        } else {
            frame_body.extend_from_slice(body);
        }

        let mut packet = Vec::with_capacity(frame_body.len() + 5);
        write_varint(frame_body.len() as i32, &mut packet);
        packet.extend_from_slice(&frame_body);
        if let Some(cipher) = &mut self.encryptor {
            cipher.encrypt(&mut packet);
        }
        self.outbound.extend_from_slice(&packet);
        Ok(())
    }

    fn compact_inbound(&mut self) {
        if self.inbound_cursor == 0 {
            return;
        }
        let remaining = self.inbound.len() - self.inbound_cursor;
        self.inbound.copy_within(self.inbound_cursor.., 0);
        self.inbound.truncate(remaining);
        self.inbound_cursor = 0;
    }

    fn fail<T>(&mut self, error: LoginError) -> Result<T, LoginError> {
        self.state = LoginState::Failed;
        self.failure = Some(format!("{error:?}"));
        Err(error)
    }
}

fn offline_uuid(username: &str) -> [u8; 16] {
    let digest = md5::compute(format!("OfflinePlayer:{username}"));
    let mut uuid = digest.0;
    uuid[6] = (uuid[6] & 0x0f) | 0x30;
    uuid[8] = (uuid[8] & 0x3f) | 0x80;
    uuid
}

fn read_varint(bytes: &[u8]) -> Result<Option<(i32, usize)>, LoginError> {
    let mut value = 0_u32;
    for index in 0..5 {
        let Some(&byte) = bytes.get(index) else {
            return Ok(None);
        };
        value |= u32::from(byte & 0x7f) << (index * 7);
        if byte & 0x80 == 0 {
            return Ok(Some((value as i32, index + 1)));
        }
    }
    Err(LoginError::VarIntTooLong)
}

fn write_varint(mut value: i32, output: &mut Vec<u8>) {
    loop {
        if value & !0x7f == 0 {
            output.push(value as u8);
            return;
        }
        output.push(((value & 0x7f) | 0x80) as u8);
        value = ((value as u32) >> 7) as i32;
    }
}

fn read_string(bytes: &[u8], cursor: &mut usize, maximum: usize) -> Result<String, LoginError> {
    let (length, varint_len) = read_varint(&bytes[*cursor..])?
        .ok_or(LoginError::InvalidPacket("truncated string length"))?;
    if length < 0 || length as usize > maximum {
        return Err(LoginError::PacketTooLarge(length.max(0) as usize));
    }
    *cursor += varint_len;
    let end = cursor
        .checked_add(length as usize)
        .ok_or(LoginError::InvalidPacket("string length overflow"))?;
    let value = bytes
        .get(*cursor..end)
        .ok_or(LoginError::InvalidPacket("truncated string"))?;
    *cursor = end;
    String::from_utf8(value.to_vec()).map_err(|_| LoginError::InvalidUtf8)
}

fn read_byte_array(bytes: &[u8], cursor: &mut usize, maximum: usize) -> Result<Vec<u8>, LoginError> {
    let (length, varint_len) = read_varint(&bytes[*cursor..])?
        .ok_or(LoginError::InvalidPacket("truncated byte-array length"))?;
    if length < 0 || length as usize > maximum {
        return Err(LoginError::PacketTooLarge(length.max(0) as usize));
    }
    *cursor += varint_len;
    let end = cursor
        .checked_add(length as usize)
        .ok_or(LoginError::InvalidPacket("byte-array length overflow"))?;
    let value = bytes
        .get(*cursor..end)
        .ok_or(LoginError::InvalidPacket("truncated byte array"))?;
    *cursor = end;
    Ok(value.to_vec())
}

fn write_string(value: &str, output: &mut Vec<u8>) -> Result<(), LoginError> {
    if value.len() > 32767 {
        return Err(LoginError::PacketTooLarge(value.len()));
    }
    write_varint(value.len() as i32, output);
    output.extend_from_slice(value.as_bytes());
    Ok(())
}

fn write_byte_array(value: &[u8], output: &mut Vec<u8>) -> Result<(), LoginError> {
    if value.len() > MAX_PACKET_BYTES {
        return Err(LoginError::PacketTooLarge(value.len()));
    }
    write_varint(value.len() as i32, output);
    output.extend_from_slice(value);
    Ok(())
}

/// Java BigInteger(byte[]).toString(16) formatting for Minecraft's SHA-1 server ID.
fn signed_sha1_hex(digest: &[u8]) -> String {
    let negative = digest.first().is_some_and(|byte| byte & 0x80 != 0);
    let mut magnitude = digest.to_vec();
    if negative {
        for byte in &mut magnitude {
            *byte = !*byte;
        }
        for byte in magnitude.iter_mut().rev() {
            let (next, carry) = byte.overflowing_add(1);
            *byte = next;
            if !carry {
                break;
            }
        }
    }
    let first = magnitude.iter().position(|byte| *byte != 0).unwrap_or(magnitude.len());
    let magnitude = &magnitude[first..];
    if magnitude.is_empty() {
        return "0".to_owned();
    }
    let mut hex = String::with_capacity(magnitude.len() * 2 + 1);
    if negative {
        hex.push('-');
    }
    for (index, byte) in magnitude.iter().enumerate() {
        if index == 0 {
            hex.push_str(&format!("{byte:x}"));
        } else {
            hex.push_str(&format!("{byte:02x}"));
        }
    }
    hex
}

#[cfg(test)]
mod tests {
    use super::{offline_uuid, signed_sha1_hex, LoginState, MinecraftLoginSession};

    #[test]
    fn creates_deterministic_offline_profile_uuid() {
        let first = offline_uuid("Steve");
        let second = offline_uuid("Steve");
        assert_eq!(first, second);
        assert_ne!(first, offline_uuid("Alex"));
        assert_eq!(first[6] >> 4, 3);
        assert_eq!(first[8] >> 6, 2);
    }

    #[test]
    fn builds_protocol_776_handshake_and_login_start() {
        let mut session = MinecraftLoginSession::new("localhost", 25565, "RustPlayer").unwrap();
        let mut output = Vec::new();
        session.take_outbound(&mut output);
        assert!(!output.is_empty());

        let (frame_len, prefix_len) = super::read_varint(&output).unwrap().unwrap();
        assert_eq!(frame_len as usize + prefix_len, output.len());
        assert_eq!(output[prefix_len], 0); // Handshake packet id.
        assert_eq!(super::read_varint(&output[prefix_len + 1..]).unwrap().unwrap().0, 776);
        assert_eq!(session.state(), LoginState::AwaitingLoginSuccess);
    }

    #[test]
    fn minecraft_sha1_server_hash_uses_signed_big_integer_hex() {
        assert_eq!(
            signed_sha1_hex(&[0xff, 0x00, 0x00]),
            "-10000"
        );
        assert_eq!(signed_sha1_hex(&[0x00, 0x01, 0x0f]), "10f");
    }
}
