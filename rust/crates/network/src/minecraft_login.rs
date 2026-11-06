use std::{collections::VecDeque, io::{Read, Write}};

use crate::{decode_chunk_with_light, ChunkDecodeError, WorldPacket};

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
    AwaitingPlayLogin,
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
    ChunkDecode(ChunkDecodeError),
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
    decrypt_scratch: Vec<u8>,
    decompressed_scratch: Vec<u8>,
    outbound: Vec<u8>,
    world_packets: VecDeque<WorldPacket>,
    profile_uuid: Option<[u8; 16]>,
    profile_name: Option<String>,
    world_dimension: Option<String>,
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
            decrypt_scratch: Vec::with_capacity(16 * 1024),
            decompressed_scratch: Vec::with_capacity(64 * 1024),
            outbound: Vec::with_capacity(4096),
            world_packets: VecDeque::with_capacity(32),
            profile_uuid: None,
            profile_name: None,
            world_dimension: None,
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

    pub fn authenticated_profile_uuid(&self) -> Option<[u8; 16]> {
        self.profile_uuid
    }

    pub fn profile_name(&self) -> Option<&str> {
        self.profile_name.as_deref()
    }

    pub fn world_dimension(&self) -> Option<&str> {
        self.world_dimension.as_deref()
    }

    pub fn pop_world_packet(&mut self) -> Option<WorldPacket> {
        self.world_packets.pop_front()
    }

    pub fn queued_world_packets(&self) -> usize {
        self.world_packets.len()
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

    /// Feed raw TCP bytes from Wisp. Unencrypted packet bodies are borrowed
    /// directly from the reusable stream buffer; encrypted input and compressed
    /// bodies use reusable scratch vectors. No per-frame packet copy is made.
    pub fn receive_tcp(&mut self, bytes: &[u8]) -> Result<LoginState, LoginError> {
        if self.state == LoginState::Failed {
            return Err(LoginError::TransportClosed);
        }
        if self.inbound.len().saturating_add(bytes.len()) > MAX_BUFFERED_BYTES {
            return self.fail(LoginError::PacketTooLarge(bytes.len()));
        }

        if let Some(cipher) = &mut self.decryptor {
            self.decrypt_scratch.clear();
            self.decrypt_scratch.extend_from_slice(bytes);
            cipher.decrypt(&mut self.decrypt_scratch);
            self.inbound.extend_from_slice(&self.decrypt_scratch);
            self.decrypt_scratch.clear();
        } else {
            self.inbound.extend_from_slice(bytes);
        }

        // Temporarily move the owning Vec out so packet handlers can mutably
        // update session state while borrowing each frame from this buffer.
        let mut buffered = std::mem::take(&mut self.inbound);
        let mut cursor = 0_usize;
        let processed = (|| {
            loop {
                let remaining = &buffered[cursor..];
                let Some((frame_length, prefix_length)) = read_varint(remaining)? else {
                    break;
                };
                if frame_length < 0 {
                    return Err(LoginError::NegativeLength);
                }
                let frame_length = frame_length as usize;
                if frame_length > MAX_PACKET_BYTES {
                    return Err(LoginError::PacketTooLarge(frame_length));
                }
                let end_of_prefix = prefix_length;
                let end = cursor
                    .checked_add(end_of_prefix)
                    .and_then(|start| start.checked_add(frame_length))
                    .ok_or(LoginError::PacketTooLarge(frame_length))?;
                if remaining.len() < end_of_prefix + frame_length {
                    break;
                }
                let start = cursor + end_of_prefix;
                cursor = end;
                self.handle_frame(&buffered[start..end])?;
            }
            Ok(())
        })();

        let remaining = buffered.len().saturating_sub(cursor);
        buffered.copy_within(cursor.., 0);
        buffered.truncate(remaining);
        self.inbound = buffered;

        if let Err(error) = processed {
            return if self.state == LoginState::Failed {
                Err(error)
            } else {
                self.fail(error)
            };
        }
        Ok(self.state)
    }

    fn handle_frame(&mut self, frame: &[u8]) -> Result<(), LoginError> {
        let Some(threshold) = self.compression_threshold else {
            return self.handle_packet(frame);
        };
        let Some((declared_size, prefix_length)) = read_varint(frame)? else {
            return self.fail(LoginError::InvalidPacket("missing compression data length"));
        };
        if declared_size < 0 || declared_size as usize > MAX_PACKET_BYTES {
            return self.fail(LoginError::PacketTooLarge(declared_size.max(0) as usize));
        }
        if declared_size == 0 {
            if frame.len() == prefix_length {
                return self.fail(LoginError::InvalidPacket("empty uncompressed packet"));
            }
            return self.handle_packet(&frame[prefix_length..]);
        }
        if (declared_size as usize) < threshold {
            return self.fail(LoginError::InvalidPacket("packet compressed below server threshold"));
        }

        let mut decompressed = std::mem::take(&mut self.decompressed_scratch);
        decompressed.clear();
        let decode_result = {
            let decoder = ZlibDecoder::new(&frame[prefix_length..]);
            decoder
                .take((MAX_PACKET_BYTES + 1) as u64)
                .read_to_end(&mut decompressed)
                .map_err(|error| LoginError::Compression(error.to_string()))
        };
        let decode_result = decode_result.and_then(|_| {
            if decompressed.len() != declared_size as usize {
                Err(LoginError::InvalidPacket("decompressed packet length mismatch"))
            } else {
                Ok(())
            }
        });
        let handled = match decode_result {
            Ok(()) => self.handle_packet(&decompressed),
            Err(error) => Err(error),
        };
        decompressed.clear();
        self.decompressed_scratch = decompressed;
        handled
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
                    2 => self.handle_login_finished(payload),
                    3 => self.handle_compression(payload),
                    4 => self.handle_custom_query(payload),
                    5 => self.handle_cookie_request(payload, true),
                    _ => self.fail(LoginError::InvalidPacket("unknown login packet id")),
                }
            }
            // Keep the socket/session alive while an official online-mode
            // session join is pending. The UI can now display the signed hash.
            LoginState::AuthenticationRequired => Ok(()),
            LoginState::Configuring => self.handle_configuration_packet(packet_id, payload),
            LoginState::AwaitingPlayLogin | LoginState::Ready => self.handle_play_common_packet(packet_id, payload),
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

    fn handle_login_finished(&mut self, payload: &[u8]) -> Result<(), LoginError> {
        let mut cursor = 0_usize;
        let uuid: [u8; 16] = payload
            .get(cursor..cursor + 16)
            .ok_or(LoginError::InvalidPacket("truncated Login Success UUID"))?
            .try_into()
            .expect("sixteen-byte UUID");
        cursor += 16;
        let name = read_string(payload, &mut cursor, 16)?;
        if name.is_empty() {
            return self.fail(LoginError::InvalidPacket("empty Login Success username"));
        }
        let property_count = read_count(payload, &mut cursor, 16)?;
        for _ in 0..property_count {
            let _property_name = read_string(payload, &mut cursor, 64)?;
            let _property_value = read_string(payload, &mut cursor, 32767)?;
            let has_signature = read_bool(payload, &mut cursor)?;
            if has_signature {
                let _signature = read_string(payload, &mut cursor, 1024)?;
            }
        }
        // Protocol 776 appends a session UUID. A number of Velocity/ViaVersion
        // backends still send the complete legacy profile-only payload.
        if payload.len().saturating_sub(cursor) == 16 {
            cursor += 16;
        }
        if cursor != payload.len() {
            return self.fail(LoginError::InvalidPacket("trailing Login Success bytes"));
        }
        self.profile_uuid = Some(uuid);
        self.profile_name = Some(name);

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
                self.state = LoginState::AwaitingPlayLogin;
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
        // These IDs follow the protocol-776 ClientboundTemplate registration
        // order in game/.../protocol/game/GameProtocols.java.
        match packet_id {
            48 => self.handle_play_login(payload),
            44 => {
                if self.state != LoginState::Ready {
                    return self.fail(LoginError::InvalidPacket("chunk arrived before play login"));
                }
                let chunk = decode_chunk_with_light(payload)
                    .map_err(LoginError::ChunkDecode)?;
                self.world_packets.push_back(WorldPacket::Chunk(chunk));
                Ok(())
            }
            36 => {
                if payload.len() != 8 {
                    return self.fail(LoginError::InvalidPacket("invalid Forget Level Chunk packet"));
                }
                // FriendlyByteBuf.readChunkPos() uses ChunkPos.unpack(long):
                // X occupies the low 32 bits and Z the high 32 bits.
                let packed = u64::from_be_bytes(payload.try_into().expect("eight-byte chunk position"));
                let x = packed as u32 as i32;
                let z = (packed >> 32) as u32 as i32;
                self.world_packets.push_back(WorldPacket::ForgetChunk { x, z });
                Ok(())
            }
            43 => {
                if payload.len() != 8 {
                    return self.fail(LoginError::InvalidPacket("invalid play keep-alive packet"));
                }
                let mut response = Vec::with_capacity(9);
                write_varint(28, &mut response); // Serverbound keep-alive.
                response.extend_from_slice(payload);
                self.write_frame(&response, true)
            }
            60 => {
                if payload.len() != 4 {
                    return self.fail(LoginError::InvalidPacket("invalid play ping packet"));
                }
                let mut response = Vec::with_capacity(5);
                write_varint(45, &mut response); // Serverbound pong.
                response.extend_from_slice(payload);
                self.write_frame(&response, true)
            }
            31 => self.fail(LoginError::InvalidPacket("server disconnected during play")),
            10 => {
                let Some((batch_size, consumed)) = read_varint(payload)? else {
                    return self.fail(LoginError::InvalidPacket("truncated chunk batch finished packet"));
                };
                if batch_size < 0 || consumed != payload.len() {
                    return self.fail(LoginError::InvalidPacket("invalid chunk batch finished packet"));
                }
                let mut response = Vec::with_capacity(5);
                write_varint(11, &mut response); // Serverbound chunk_batch_received.
                response.extend_from_slice(&64.0_f32.to_be_bytes());
                self.write_frame(&response, true)
            }
            11 if payload.is_empty() => Ok(()), // Chunk batch start.
            47 => Ok(()), // Incremental light update; full chunk-with-light already carries light.
            71 => self.handle_player_position(payload),
            _ => Ok(()),
        }
    }

    fn handle_player_position(&mut self, payload: &[u8]) -> Result<(), LoginError> {
        if self.state != LoginState::Ready {
            return self.fail(LoginError::InvalidPacket("player teleport arrived before play login"));
        }
        let mut cursor = 0_usize;
        let teleport_id = read_nonnegative_varint(payload, &mut cursor)?;
        let mut position = [0.0_f64; 3];
        let mut delta_movement = [0.0_f64; 3];
        for value in &mut position {
            *value = read_f64(payload, &mut cursor)?;
        }
        for value in &mut delta_movement {
            *value = read_f64(payload, &mut cursor)?;
        }
        let yaw = read_f32(payload, &mut cursor)?;
        let pitch = read_f32(payload, &mut cursor)?;
        let relative_flags = read_i32(payload, &mut cursor)?;
        if cursor != payload.len() {
            return self.fail(LoginError::InvalidPacket("trailing player teleport bytes"));
        }

        // Acknowledge the teleport before the next movement packet.
        let mut acknowledgement = Vec::with_capacity(6);
        write_varint(0, &mut acknowledgement);
        write_varint(teleport_id, &mut acknowledgement);
        self.write_frame(&acknowledgement, true)?;
        self.world_packets.push_back(WorldPacket::PlayerPosition {
            teleport_id,
            position,
            delta_movement,
            yaw,
            pitch,
            relative_flags,
        });
        Ok(())
    }

    /// Queues the protocol-776 position+rotation packet and the end-of-tick
    /// marker. The browser calls this once for each 20 TPS simulation tick.
    pub fn queue_player_movement(
        &mut self,
        position: [f32; 3],
        yaw_degrees: f32,
        pitch_degrees: f32,
        on_ground: bool,
    ) -> Result<(), LoginError> {
        if self.state != LoginState::Ready {
            return Err(LoginError::InvalidPacket("movement queued before play login"));
        }
        if position.iter().any(|value| !value.is_finite())
            || !yaw_degrees.is_finite()
            || !pitch_degrees.is_finite()
        {
            return Err(LoginError::InvalidPacket("non-finite player movement"));
        }
        let mut movement = Vec::with_capacity(34);
        write_varint(31, &mut movement); // Serverbound move_player_pos_rot.
        for value in position {
            movement.extend_from_slice(&(value as f64).to_be_bytes());
        }
        movement.extend_from_slice(&yaw_degrees.to_be_bytes());
        movement.extend_from_slice(&pitch_degrees.to_be_bytes());
        movement.push(u8::from(on_ground)); // Horizontal-collision flag remains clear.
        self.write_frame(&movement, true)?;

        let tick_end = [13_u8]; // Serverbound client_tick_end.
        self.write_frame(&tick_end, true)
    }

    fn handle_play_login(&mut self, payload: &[u8]) -> Result<(), LoginError> {
        if self.state != LoginState::AwaitingPlayLogin {
            return self.fail(LoginError::InvalidPacket("duplicate or out-of-order play login"));
        }
        let mut cursor = 0_usize;
        let _player_id = read_i32(payload, &mut cursor)?;
        let _hardcore = read_bool(payload, &mut cursor)?;
        let levels = read_count(payload, &mut cursor, 256)?;
        for _ in 0..levels {
            let _level = read_string(payload, &mut cursor, 256)?;
        }
        let _max_players = read_nonnegative_varint(payload, &mut cursor)?;
        let _chunk_radius = read_nonnegative_varint(payload, &mut cursor)?;
        let _simulation_distance = read_nonnegative_varint(payload, &mut cursor)?;
        let _reduced_debug = read_bool(payload, &mut cursor)?;
        let _show_death_screen = read_bool(payload, &mut cursor)?;
        let _limited_crafting = read_bool(payload, &mut cursor)?;

        // CommonPlayerSpawnInfo: registry holder ID, dimension key, seed,
        // game-mode bytes, debug/flat flags, optional death location, and
        // portal/sea-level VarInts.
        let _dimension_type_id = read_nonnegative_varint(payload, &mut cursor)?;
        let dimension = read_string(payload, &mut cursor, 256)?;
        skip_bytes(payload, &mut cursor, 8)?; // World seed.
        skip_bytes(payload, &mut cursor, 2)?; // Current and previous game mode.
        let _debug = read_bool(payload, &mut cursor)?;
        let _flat = read_bool(payload, &mut cursor)?;
        if read_bool(payload, &mut cursor)? {
            let _death_dimension = read_string(payload, &mut cursor, 256)?;
            skip_bytes(payload, &mut cursor, 8)?; // Packed BlockPos.
        }
        let _portal_cooldown = read_nonnegative_varint(payload, &mut cursor)?;
        let _sea_level = read_nonnegative_varint(payload, &mut cursor)?;
        let _online_mode = read_bool(payload, &mut cursor)?;
        let _secure_chat = read_bool(payload, &mut cursor)?;
        if cursor != payload.len() {
            return self.fail(LoginError::InvalidPacket("trailing play login bytes"));
        }
        self.world_dimension = Some(dimension);

        // Notify protocol-776 servers that the client has created its player.
        let player_loaded = [44_u8]; // Serverbound player_loaded is an empty packet.
        self.write_frame(&player_loaded, true)?;
        self.state = LoginState::Ready;
        Ok(())
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

    fn fail<T>(&mut self, error: LoginError) -> Result<T, LoginError> {
        self.state = LoginState::Failed;
        self.failure = Some(format!("{error:?}"));
        Err(error)
    }
}

fn skip_bytes(bytes: &[u8], cursor: &mut usize, count: usize) -> Result<(), LoginError> {
    let end = cursor.checked_add(count).ok_or(LoginError::InvalidPacket("field length overflow"))?;
    if end > bytes.len() {
        return Err(LoginError::InvalidPacket("truncated packet field"));
    }
    *cursor = end;
    Ok(())
}

fn read_bool(bytes: &[u8], cursor: &mut usize) -> Result<bool, LoginError> {
    let value = *bytes.get(*cursor).ok_or(LoginError::InvalidPacket("truncated boolean"))?;
    *cursor += 1;
    Ok(value != 0)
}

fn read_f32(bytes: &[u8], cursor: &mut usize) -> Result<f32, LoginError> {
    let end = cursor.checked_add(4).ok_or(LoginError::InvalidPacket("float length overflow"))?;
    let value = bytes.get(*cursor..end).ok_or(LoginError::InvalidPacket("truncated float"))?;
    *cursor = end;
    Ok(f32::from_be_bytes(value.try_into().expect("four-byte float")))
}

fn read_f64(bytes: &[u8], cursor: &mut usize) -> Result<f64, LoginError> {
    let end = cursor.checked_add(8).ok_or(LoginError::InvalidPacket("double length overflow"))?;
    let value = bytes.get(*cursor..end).ok_or(LoginError::InvalidPacket("truncated double"))?;
    *cursor = end;
    Ok(f64::from_be_bytes(value.try_into().expect("eight-byte double")))
}

fn read_i32(bytes: &[u8], cursor: &mut usize) -> Result<i32, LoginError> {
    let end = cursor.checked_add(4).ok_or(LoginError::InvalidPacket("integer overflow"))?;
    let value = bytes.get(*cursor..end).ok_or(LoginError::InvalidPacket("truncated integer"))?;
    *cursor = end;
    Ok(i32::from_be_bytes(value.try_into().expect("four-byte integer")))
}

fn read_count(bytes: &[u8], cursor: &mut usize, maximum: usize) -> Result<usize, LoginError> {
    let remaining = bytes.get(*cursor..).ok_or(LoginError::InvalidPacket("invalid packet cursor"))?;
    let (count, length) = read_varint(remaining)?.ok_or(LoginError::InvalidPacket("truncated collection length"))?;
    if count < 0 || count as usize > maximum {
        return Err(LoginError::PacketTooLarge(count.max(0) as usize));
    }
    *cursor += length;
    Ok(count as usize)
}

fn read_nonnegative_varint(bytes: &[u8], cursor: &mut usize) -> Result<i32, LoginError> {
    let remaining = bytes.get(*cursor..).ok_or(LoginError::InvalidPacket("invalid packet cursor"))?;
    let (value, length) = read_varint(remaining)?.ok_or(LoginError::InvalidPacket("truncated VarInt"))?;
    if value < 0 {
        return Err(LoginError::NegativeLength);
    }
    *cursor += length;
    Ok(value)
}

fn offline_uuid(username: &str) -> [u8; 16] {
    let digest = md5::Md5::digest(format!("OfflinePlayer:{username}").as_bytes());
    let mut uuid = [0_u8; 16];
    uuid.copy_from_slice(&digest);
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
    use super::{offline_uuid, signed_sha1_hex, LoginState, MinecraftLoginSession, WorldPacket};

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
    fn login_success_is_validated_before_configuration_begins() {
        let mut session = MinecraftLoginSession::new("localhost", 25565, "RustPlayer").unwrap();
        // UUID, name, zero properties, and the protocol-776 session UUID.
        let mut payload = vec![0_u8; 16];
        super::write_string("RustPlayer", &mut payload).unwrap();
        super::write_varint(0, &mut payload);
        payload.extend_from_slice(&[7_u8; 16]);
        session.handle_login_finished(&payload).unwrap();
        assert_eq!(session.state(), LoginState::Configuring);
        assert_eq!(session.profile_name(), Some("RustPlayer"));
        assert_eq!(session.authenticated_profile_uuid(), Some([0_u8; 16]));
    }

    #[test]
    fn configuration_finish_does_not_claim_play_until_clientbound_login() {
        let mut session = MinecraftLoginSession::new("localhost", 25565, "RustPlayer").unwrap();
        session.state = LoginState::Configuring;
        session.handle_configuration_packet(3, &[]).unwrap();
        assert_eq!(session.state(), LoginState::AwaitingPlayLogin);
    }

    #[test]
    fn play_login_packet_is_required_before_ready_state() {
        let mut session = MinecraftLoginSession::new("localhost", 25565, "RustPlayer").unwrap();
        session.state = LoginState::AwaitingPlayLogin;
        let mut payload = Vec::new();
        payload.extend_from_slice(&1_i32.to_be_bytes()); // Player entity ID.
        payload.push(0); // Hardcore.
        super::write_varint(1, &mut payload);
        super::write_string("minecraft:overworld", &mut payload);
        super::write_varint(20, &mut payload); // Max players.
        super::write_varint(8, &mut payload); // View distance.
        super::write_varint(8, &mut payload); // Simulation distance.
        payload.extend_from_slice(&[0, 1, 0]); // Reduced debug, death screen, crafting.
        super::write_varint(0, &mut payload); // Dimension type holder ID.
        super::write_string("minecraft:overworld", &mut payload);
        payload.extend_from_slice(&0_i64.to_be_bytes()); // Seed.
        payload.extend_from_slice(&[0, 0xff, 0, 1, 0]); // Modes, debug, flat, no death position.
        super::write_varint(0, &mut payload); // Portal cooldown.
        super::write_varint(63, &mut payload); // Sea level.
        payload.extend_from_slice(&[0, 0]); // Offline mode, no secure chat.

        session.handle_play_login(&payload).unwrap();
        assert_eq!(session.state(), LoginState::Ready);
        assert_eq!(session.world_dimension(), Some("minecraft:overworld"));
    }

    #[test]
    fn forget_chunk_uses_vanilla_packed_chunk_position_order() {
        let mut session = MinecraftLoginSession::new("localhost", 25565, "RustPlayer").unwrap();
        session.state = LoginState::Ready;
        let packed = ((-9_i32 as u32 as u64) << 32) | (5_i32 as u32 as u64);
        session.handle_play_common_packet(36, &packed.to_be_bytes()).unwrap();
        assert_eq!(
            session.pop_world_packet(),
            Some(WorldPacket::ForgetChunk { x: 5, z: -9 })
        );
    }

    #[test]
    fn movement_queue_emits_position_rotation_and_client_tick_end() {
        let mut session = MinecraftLoginSession::new("localhost", 25565, "RustPlayer").unwrap();
        session.state = LoginState::Ready;
        session.queue_player_movement([1.0, 64.0, -2.0], 90.0, -10.0, true).unwrap();
        let mut output = Vec::new();
        session.take_outbound(&mut output);

        let (first_length, first_prefix) = super::read_varint(&output).unwrap().unwrap();
        let first = &output[first_prefix..first_prefix + first_length as usize];
        assert_eq!(super::read_varint(first).unwrap().unwrap().0, 31);
        let second_offset = first_prefix + first_length as usize;
        let (second_length, second_prefix) = super::read_varint(&output[second_offset..]).unwrap().unwrap();
        let second = &output[second_offset + second_prefix..second_offset + second_prefix + second_length as usize];
        assert_eq!(super::read_varint(second).unwrap().unwrap().0, 13);
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
