/// Release protocol version exposed by the checked-in Minecraft source.
pub const MINECRAFT_PROTOCOL_VERSION: i32 = 776;

const MAX_PACKET_BYTES: usize = 2 * 1024 * 1024;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CodecError {
    VarIntTooLong,
    NegativeLength,
    PacketTooLarge(usize),
    InvalidUtf8,
    UnexpectedPacketId { expected: i32, actual: i32 },
    InvalidPacket(&'static str),
    TransportClosed,
}

pub fn read_varint(bytes: &[u8]) -> Result<Option<(i32, usize)>, CodecError> {
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
    Err(CodecError::VarIntTooLong)
}

pub fn write_varint(mut value: i32, output: &mut Vec<u8>) {
    loop {
        if value & !0x7f == 0 {
            output.push(value as u8);
            return;
        }
        output.push(((value & 0x7f) | 0x80) as u8);
        value = ((value as u32) >> 7) as i32;
    }
}

fn append_packet(body: &[u8], output: &mut Vec<u8>) {
    output.clear();
    write_varint(body.len() as i32, output);
    output.extend_from_slice(body);
}

fn append_string(value: &str, output: &mut Vec<u8>) {
    write_varint(value.len() as i32, output);
    output.extend_from_slice(value.as_bytes());
}

/// Builds the vanilla status handshake and status request for protocol 776.
pub fn build_status_start(host: &str, port: u16, output: &mut Vec<u8>) -> Result<(), CodecError> {
    if host.is_empty() || host.len() > 255 || host.chars().any(char::is_whitespace) {
        return Err(CodecError::InvalidPacket("invalid server hostname"));
    }

    let mut body = Vec::with_capacity(host.len() + 16);
    write_varint(0, &mut body); // Handshake packet id.
    write_varint(MINECRAFT_PROTOCOL_VERSION, &mut body);
    append_string(host, &mut body);
    body.extend_from_slice(&port.to_be_bytes());
    write_varint(1, &mut body); // Next state: status.
    append_packet(&body, output);

    body.clear();
    body.push(0); // Status request packet id.
    let mut request = Vec::with_capacity(1);
    append_packet(&body, &mut request);
    output.extend_from_slice(&request);
    Ok(())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StatusProbeState {
    AwaitingStatus,
    AwaitingPong,
    Complete,
    Failed,
}

/// Incremental Minecraft status probe with reusable input/output/packet buffers.
#[derive(Debug)]
pub struct MinecraftStatusProbe {
    input: Vec<u8>,
    read_cursor: usize,
    packet_scratch: Vec<u8>,
    output: Vec<u8>,
    state: StatusProbeState,
    nonce: i64,
    status_json: Option<String>,
}

impl MinecraftStatusProbe {
    pub fn new(host: &str, port: u16, nonce: i64) -> Result<Self, CodecError> {
        let mut output = Vec::with_capacity(host.len() + 32);
        build_status_start(host, port, &mut output)?;
        Ok(Self {
            input: Vec::with_capacity(16 * 1024),
            read_cursor: 0,
            packet_scratch: Vec::with_capacity(16 * 1024),
            output,
            state: StatusProbeState::AwaitingStatus,
            nonce,
            status_json: None,
        })
    }

    pub fn state(&self) -> StatusProbeState {
        self.state
    }

    pub fn status_json(&self) -> Option<&str> {
        self.status_json.as_deref()
    }

    /// Copies raw TCP bytes into a bounded stream buffer and consumes complete
    /// Minecraft frames. Returns true only after a matching status pong.
    pub fn receive_tcp(&mut self, bytes: &[u8]) -> Result<bool, CodecError> {
        if self.state == StatusProbeState::Complete {
            return Ok(true);
        }
        if self.state == StatusProbeState::Failed {
            return Err(CodecError::TransportClosed);
        }
        if self.input.len().saturating_sub(self.read_cursor) + bytes.len() > MAX_PACKET_BYTES * 2 {
            self.state = StatusProbeState::Failed;
            return Err(CodecError::PacketTooLarge(bytes.len()));
        }
        self.compact_input();
        self.input.extend_from_slice(bytes);

        loop {
            let remaining = &self.input[self.read_cursor..];
            let Some((frame_len, prefix_len)) = read_varint(remaining)? else {
                return Ok(false);
            };
            if frame_len < 0 {
                self.state = StatusProbeState::Failed;
                return Err(CodecError::NegativeLength);
            }
            let frame_len = frame_len as usize;
            if frame_len > MAX_PACKET_BYTES {
                self.state = StatusProbeState::Failed;
                return Err(CodecError::PacketTooLarge(frame_len));
            }
            if remaining.len() < prefix_len + frame_len {
                return Ok(false);
            }

            let packet_start = self.read_cursor + prefix_len;
            let packet_end = packet_start + frame_len;
            self.packet_scratch.clear();
            self.packet_scratch.extend_from_slice(&self.input[packet_start..packet_end]);
            self.read_cursor = packet_end;

            // Move the reusable packet buffer out briefly to avoid aliasing
            // an immutable slice of self while the packet handler mutates self.
            let mut packet = std::mem::take(&mut self.packet_scratch);
            let handled = self.handle_packet(&packet);
            packet.clear();
            self.packet_scratch = packet;
            handled?;

            if self.state == StatusProbeState::Complete {
                self.input.clear();
                self.read_cursor = 0;
                return Ok(true);
            }
        }
    }

    /// Copies pending bytes into a caller-owned reusable send buffer.
    pub fn take_output(&mut self, destination: &mut Vec<u8>) {
        destination.clear();
        destination.extend_from_slice(&self.output);
        self.output.clear();
    }

    fn handle_packet(&mut self, packet: &[u8]) -> Result<(), CodecError> {
        let Some((packet_id, id_len)) = read_varint(packet)? else {
            return self.fail(CodecError::InvalidPacket("missing packet id"));
        };
        match self.state {
            StatusProbeState::AwaitingStatus => {
                if packet_id != 0 {
                    return self.fail(CodecError::UnexpectedPacketId {
                        expected: 0,
                        actual: packet_id,
                    });
                }
                let Some((json_len, len_len)) = read_varint(&packet[id_len..])? else {
                    return self.fail(CodecError::InvalidPacket("truncated status string length"));
                };
                if json_len < 0 {
                    return self.fail(CodecError::NegativeLength);
                }
                let json_len = json_len as usize;
                let start = id_len + len_len;
                if packet.len() != start + json_len {
                    return self.fail(CodecError::InvalidPacket("status JSON length mismatch"));
                }
                let json = std::str::from_utf8(&packet[start..]).map_err(|_| CodecError::InvalidUtf8)?;
                self.status_json = Some(json.to_owned());

                let mut ping = Vec::with_capacity(9);
                ping.push(1);
                ping.extend_from_slice(&self.nonce.to_be_bytes());
                append_packet(&ping, &mut self.output);
                self.state = StatusProbeState::AwaitingPong;
                Ok(())
            }
            StatusProbeState::AwaitingPong => {
                if packet_id != 1 {
                    return self.fail(CodecError::UnexpectedPacketId {
                        expected: 1,
                        actual: packet_id,
                    });
                }
                if packet.len() != id_len + 8 {
                    return self.fail(CodecError::InvalidPacket("status pong must contain one i64"));
                }
                let response = i64::from_be_bytes(
                    packet[id_len..id_len + 8]
                        .try_into()
                        .expect("validated eight-byte status nonce"),
                );
                if response != self.nonce {
                    return self.fail(CodecError::InvalidPacket("status pong nonce mismatch"));
                }
                self.state = StatusProbeState::Complete;
                Ok(())
            }
            StatusProbeState::Complete => Ok(()),
            StatusProbeState::Failed => Err(CodecError::TransportClosed),
        }
    }

    fn compact_input(&mut self) {
        if self.read_cursor == 0 {
            return;
        }
        let remaining = self.input.len() - self.read_cursor;
        self.input.copy_within(self.read_cursor.., 0);
        self.input.truncate(remaining);
        self.read_cursor = 0;
    }

    fn fail<T>(&mut self, error: CodecError) -> Result<T, CodecError> {
        self.state = StatusProbeState::Failed;
        Err(error)
    }
}

#[cfg(test)]
mod tests {
    use super::{
        append_packet, build_status_start, read_varint, write_varint, CodecError,
        MinecraftStatusProbe, StatusProbeState, MINECRAFT_PROTOCOL_VERSION,
    };

    #[test]
    fn varints_round_trip_protocol_version_and_large_values() {
        for value in [0, 1, 127, 128, 255, 776, 2_097_151, i32::MAX, -1] {
            let mut bytes = Vec::new();
            write_varint(value, &mut bytes);
            assert_eq!(read_varint(&bytes), Ok(Some((value, bytes.len()))));
        }
        assert_eq!(read_varint(&[0x80]), Ok(None));
        assert_eq!(
            read_varint(&[0x80, 0x80, 0x80, 0x80, 0x80]),
            Err(CodecError::VarIntTooLong)
        );
    }

    #[test]
    fn status_start_uses_checked_in_protocol_and_network_order_port() {
        let mut bytes = Vec::new();
        build_status_start("play.example.net", 25565, &mut bytes).unwrap();
        assert_eq!(MINECRAFT_PROTOCOL_VERSION, 776);
        assert_eq!(bytes[0], 0x17); // Handshake packet body length.
        assert!(bytes.windows(2).any(|pair| pair == [0x63, 0xDD]));
        assert!(bytes.ends_with(&[1, 0])); // Status request.
    }

    #[test]
    fn status_probe_handles_fragmented_response_and_verifies_pong() {
        let nonce = 0x0102_0304_0506_0708_i64;
        let mut probe = MinecraftStatusProbe::new("example.net", 25565, nonce).unwrap();
        assert_eq!(probe.state(), StatusProbeState::AwaitingStatus);
        let mut initial = Vec::new();
        probe.take_output(&mut initial);
        assert!(!initial.is_empty());

        let json = br#"{"version":{"name":"26.2","protocol":776},"players":{"max":20,"online":2}}"#;
        let mut response_body = vec![0];
        write_varint(json.len() as i32, &mut response_body);
        response_body.extend_from_slice(json);
        let mut response = Vec::new();
        append_packet(&response_body, &mut response);

        assert_eq!(probe.receive_tcp(&response[..4]), Ok(false));
        assert_eq!(probe.receive_tcp(&response[4..]), Ok(false));
        assert_eq!(probe.status_json(), Some(std::str::from_utf8(json).unwrap()));

        let mut ping = Vec::new();
        probe.take_output(&mut ping);
        assert_eq!(ping[0], 9);
        assert_eq!(ping[1], 1);

        let mut pong_body = vec![1];
        pong_body.extend_from_slice(&nonce.to_be_bytes());
        let mut pong = Vec::new();
        append_packet(&pong_body, &mut pong);
        assert_eq!(probe.receive_tcp(&pong), Ok(true));
        assert_eq!(probe.state(), StatusProbeState::Complete);
    }

    #[test]
    fn status_probe_rejects_invalid_packet_ids() {
        let mut probe = MinecraftStatusProbe::new("example.net", 25565, 1).unwrap();
        let mut bad = Vec::new();
        append_packet(&[1], &mut bad);
        assert_eq!(
            probe.receive_tcp(&bad),
            Err(CodecError::UnexpectedPacketId { expected: 0, actual: 1 })
        );
        assert_eq!(probe.state(), StatusProbeState::Failed);
    }
}
