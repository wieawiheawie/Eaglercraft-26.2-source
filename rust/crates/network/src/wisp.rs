use std::collections::VecDeque;

const HEADER_LEN: usize = 5;
const MAX_PAYLOAD: usize = 65_536;
const MAX_PENDING_BYTES: usize = 4 * 1024 * 1024;
const MAX_INBOUND_BYTES: usize = 4 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WispState {
    AwaitingGreeting,
    Ready,
    Closed,
    Failed,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum WispError {
    InvalidUri,
    MissingTarget,
    InvalidTarget,
    InvalidPort,
    InvalidPacket(&'static str),
    UnexpectedPacket { kind: u8, stream: u32 },
    RelayRejected(u8),
    QueueLimit,
    NotReady,
    Closed,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WispTarget {
    pub endpoint: String,
    pub host: String,
    pub port: u16,
}

pub fn parse_wisp_uri(uri: &str) -> Result<WispTarget, WispError> {
    let raw = uri.strip_prefix("eagler-wisp:").ok_or(WispError::InvalidUri)?;
    let (endpoint_encoded, query) = raw.split_once('?').ok_or(WispError::MissingTarget)?;
    let endpoint = percent_decode(endpoint_encoded)?;
    if !(endpoint.starts_with("ws://") || endpoint.starts_with("wss://"))
        || endpoint.len() > 2048
        || endpoint.contains('@')
        || endpoint.contains('#')
    {
        return Err(WispError::InvalidUri);
    }
    let target_encoded = query
        .split('&')
        .filter_map(|part| part.split_once('='))
        .find_map(|(key, value)| (key == "target").then_some(value))
        .ok_or(WispError::MissingTarget)?;
    let target = percent_decode(target_encoded)?;
    let (host, port) = parse_host_port(&target)?;
    Ok(WispTarget { endpoint, host, port })
}

pub fn build_wisp_uri(endpoint: &str, host: &str, port: u16) -> Result<String, WispError> {
    if !(endpoint.starts_with("ws://") || endpoint.starts_with("wss://"))
        || endpoint.contains('@')
        || endpoint.contains('#')
        || endpoint.len() > 2048
    {
        return Err(WispError::InvalidUri);
    }
    if host.is_empty() || host.len() > 253 || host.chars().any(char::is_whitespace) || port == 0 {
        return Err(WispError::InvalidTarget);
    }
    let target = if host.contains(':') && !host.starts_with('[') {
        format!("[{host}]:{port}")
    } else {
        format!("{host}:{port}")
    };
    Ok(format!(
        "eagler-wisp:{}?target={}",
        percent_encode(endpoint),
        percent_encode(&target)
    ))
}

fn parse_host_port(target: &str) -> Result<(String, u16), WispError> {
    if target.is_empty() || target.len() > 280 || target.chars().any(char::is_whitespace) {
        return Err(WispError::InvalidTarget);
    }
    let (host, port) = if let Some(rest) = target.strip_prefix('[') {
        let close = rest.find(']').ok_or(WispError::InvalidTarget)?;
        let host = &rest[..close];
        let suffix = &rest[close + 1..];
        if host.is_empty() {
            return Err(WispError::InvalidTarget);
        }
        let port = if suffix.is_empty() {
            25565
        } else {
            suffix
                .strip_prefix(':')
                .ok_or(WispError::InvalidTarget)?
                .parse::<u16>()
                .map_err(|_| WispError::InvalidPort)?
        };
        (host.to_owned(), port)
    } else if target.matches(':').count() == 1 {
        let (host, port_text) = target.rsplit_once(':').ok_or(WispError::InvalidTarget)?;
        (
            host.to_owned(),
            port_text.parse::<u16>().map_err(|_| WispError::InvalidPort)?,
        )
    } else {
        (target.to_owned(), 25565)
    };
    if host.is_empty() || host.len() > 253 || port == 0 {
        return Err(if port == 0 { WispError::InvalidPort } else { WispError::InvalidTarget });
    }
    Ok((host, port))
}

fn percent_encode(value: &str) -> String {
    let mut output = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            output.push(byte as char);
        } else {
            output.push('%');
            output.push(char::from_digit(u32::from(byte >> 4), 16).unwrap().to_ascii_uppercase());
            output.push(char::from_digit(u32::from(byte & 0x0f), 16).unwrap().to_ascii_uppercase());
        }
    }
    output
}

fn percent_decode(value: &str) -> Result<String, WispError> {
    let bytes = value.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut cursor = 0;
    while cursor < bytes.len() {
        if bytes[cursor] == b'%' {
            if cursor + 2 >= bytes.len() {
                return Err(WispError::InvalidUri);
            }
            let high = hex(bytes[cursor + 1]).ok_or(WispError::InvalidUri)?;
            let low = hex(bytes[cursor + 2]).ok_or(WispError::InvalidUri)?;
            decoded.push((high << 4) | low);
            cursor += 3;
        } else {
            decoded.push(bytes[cursor]);
            cursor += 1;
        }
    }
    String::from_utf8(decoded).map_err(|_| WispError::InvalidUri)
}

fn hex(value: u8) -> Option<u8> {
    match value {
        b'0'..=b'9' => Some(value - b'0'),
        b'a'..=b'f' => Some(value - b'a' + 10),
        b'A'..=b'F' => Some(value - b'A' + 10),
        _ => None,
    }
}

#[derive(Debug)]
pub struct WispSession {
    host: String,
    port: u16,
    state: WispState,
    credits: u64,
    pending: VecDeque<Vec<u8>>,
    pending_bytes: usize,
    outbound: VecDeque<Vec<u8>>,
    outbound_pool: Vec<Vec<u8>>,
    inbound: Vec<u8>,
    inbound_cursor: usize,
    failure: Option<WispError>,
}

impl WispSession {
    pub fn new(host: impl Into<String>, port: u16) -> Result<Self, WispError> {
        let host = host.into();
        if host.is_empty() || host.len() > 253 || host.chars().any(char::is_whitespace) || port == 0 {
            return Err(WispError::InvalidTarget);
        }
        Ok(Self {
            host,
            port,
            state: WispState::AwaitingGreeting,
            credits: 0,
            pending: VecDeque::new(),
            pending_bytes: 0,
            outbound: VecDeque::new(),
            outbound_pool: Vec::with_capacity(8),
            inbound: Vec::with_capacity(64 * 1024),
            inbound_cursor: 0,
            failure: None,
        })
    }

    pub fn state(&self) -> WispState {
        self.state
    }

    pub fn failure(&self) -> Option<&WispError> {
        self.failure.as_ref()
    }

    pub fn queued_send_bytes(&self) -> usize {
        self.pending_bytes
    }

    pub fn buffered_receive_bytes(&self) -> usize {
        self.inbound.len().saturating_sub(self.inbound_cursor)
    }

    pub fn receive_packet(&mut self, packet: &[u8]) -> Result<(), WispError> {
        if packet.len() < HEADER_LEN {
            return self.fail(WispError::InvalidPacket("packet shorter than header"));
        }
        if self.state == WispState::Closed || self.state == WispState::Failed {
            return Err(WispError::Closed);
        }
        let kind = packet[0];
        let stream = read_u32_le(packet, 1);

        if self.state == WispState::AwaitingGreeting {
            if kind != 3 || stream != 0 || packet.len() != 9 {
                return self.fail(WispError::InvalidPacket("expected Wisp v1 greeting"));
            }
            self.credits = u64::from(read_u32_le(packet, 5));
            let hostname = self.host.as_bytes();
            let mut open = self.take_packet_buffer(HEADER_LEN + 3 + hostname.len());
            open.push(1);
            open.extend_from_slice(&1_u32.to_le_bytes());
            open.push(1); // TCP
            open.extend_from_slice(&self.port.to_le_bytes());
            open.extend_from_slice(hostname);
            self.outbound.push_back(open);
            self.state = WispState::Ready;
            return Ok(());
        }

        match (kind, stream) {
            (2, 1) => {
                if packet.len() == HEADER_LEN {
                    return Ok(());
                }
                let incoming = &packet[HEADER_LEN..];
                if self.buffered_receive_bytes().saturating_add(incoming.len()) > MAX_INBOUND_BYTES {
                    return self.fail(WispError::QueueLimit);
                }
                self.compact_inbound();
                self.inbound.extend_from_slice(incoming);
                Ok(())
            }
            (3, 1) if packet.len() == 9 => {
                self.credits = u64::from(read_u32_le(packet, 5));
                self.flush_pending();
                Ok(())
            }
            (4, 0 | 1) if packet.len() == 6 => {
                let reason = packet[5];
                if reason == 2 {
                    self.state = WispState::Closed;
                    self.pending.clear();
                    self.pending_bytes = 0;
                    Ok(())
                } else {
                    self.fail(WispError::RelayRejected(reason))
                }
            }
            _ => self.fail(WispError::UnexpectedPacket { kind, stream }),
        }
    }

    pub fn send_tcp(&mut self, bytes: &[u8]) -> Result<(), WispError> {
        if self.state == WispState::Closed || self.state == WispState::Failed {
            return Err(WispError::Closed);
        }
        if self.state != WispState::Ready {
            return Err(WispError::NotReady);
        }
        let mut offset = 0;
        while offset < bytes.len() {
            let length = (bytes.len() - offset).min(MAX_PAYLOAD);
            let mut packet = self.take_packet_buffer(HEADER_LEN + length);
            packet.push(2);
            packet.extend_from_slice(&1_u32.to_le_bytes());
            packet.extend_from_slice(&bytes[offset..offset + length]);
            if self.credits > 0 && self.pending.is_empty() {
                self.credits -= 1;
                self.outbound.push_back(packet);
            } else {
                if self.pending_bytes.saturating_add(packet.len()) > MAX_PENDING_BYTES {
                    return self.fail(WispError::QueueLimit);
                }
                self.pending_bytes += packet.len();
                self.pending.push_back(packet);
            }
            offset += length;
        }
        Ok(())
    }

    pub fn read_tcp(&mut self, destination: &mut [u8]) -> usize {
        let count = self.buffered_receive_bytes().min(destination.len());
        if count == 0 {
            return 0;
        }
        destination[..count]
            .copy_from_slice(&self.inbound[self.inbound_cursor..self.inbound_cursor + count]);
        self.inbound_cursor += count;
        if self.inbound_cursor == self.inbound.len() {
            self.inbound.clear();
            self.inbound_cursor = 0;
        } else if self.inbound_cursor >= 64 * 1024 && self.inbound_cursor >= self.inbound.len() / 2 {
            self.compact_inbound();
        }
        count
    }

    pub fn pop_outbound(&mut self) -> Option<Vec<u8>> {
        self.outbound.pop_front()
    }

    /// Returns a packet buffer after WebSocket.send has synchronously copied it.
    /// A small pool avoids repeated heap allocations for the common relay frames.
    pub fn recycle_outbound(&mut self, mut packet: Vec<u8>) {
        packet.clear();
        if self.outbound_pool.len() < 8 {
            self.outbound_pool.push(packet);
        }
    }

    fn take_packet_buffer(&mut self, capacity: usize) -> Vec<u8> {
        let mut packet = self.outbound_pool.pop().unwrap_or_default();
        packet.clear();
        if packet.capacity() < capacity {
            packet.reserve(capacity - packet.capacity());
        }
        packet
    }

    pub fn close(&mut self) {
        if self.state == WispState::Closed || self.state == WispState::Failed {
            return;
        }
        if self.state == WispState::Ready {
            let mut packet = self.take_packet_buffer(6);
            packet.push(4);
            packet.extend_from_slice(&1_u32.to_le_bytes());
            packet.push(2);
            self.outbound.push_back(packet);
        }
        self.state = WispState::Closed;
        self.pending.clear();
        self.pending_bytes = 0;
    }

    fn flush_pending(&mut self) {
        while self.credits > 0 {
            let Some(packet) = self.pending.pop_front() else { break };
            self.pending_bytes = self.pending_bytes.saturating_sub(packet.len());
            self.credits -= 1;
            self.outbound.push_back(packet);
        }
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

    fn fail<T>(&mut self, error: WispError) -> Result<T, WispError> {
        self.state = WispState::Failed;
        self.failure = Some(error.clone());
        self.pending.clear();
        self.pending_bytes = 0;
        Err(error)
    }
}

fn read_u32_le(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes([
        bytes[offset],
        bytes[offset + 1],
        bytes[offset + 2],
        bytes[offset + 3],
    ])
}

#[cfg(test)]
mod tests {
    use super::{build_wisp_uri, parse_wisp_uri, WispError, WispSession, WispState};

    fn greeting(credits: u32) -> [u8; 9] {
        let mut packet = [0_u8; 9];
        packet[0] = 3;
        packet[5..9].copy_from_slice(&credits.to_le_bytes());
        packet
    }

    #[test]
    fn parses_and_builds_wisp_addresses() {
        let uri = build_wisp_uri("wss://relay.example/wisp", "mc.example.net", 25565).unwrap();
        let parsed = parse_wisp_uri(&uri).unwrap();
        assert_eq!(parsed.endpoint, "wss://relay.example/wisp");
        assert_eq!(parsed.host, "mc.example.net");
        assert_eq!(parsed.port, 25565);
    }

    #[test]
    fn parses_ipv6_targets_and_default_ports() {
        let parsed = parse_wisp_uri("eagler-wisp:wss%3A%2F%2Frelay.example?target=%5B2001%3Adb8%3A%3A1%5D").unwrap();
        assert_eq!(parsed.host, "2001:db8::1");
        assert_eq!(parsed.port, 25565);
    }

    #[test]
    fn rejects_missing_or_invalid_targets() {
        assert_eq!(parse_wisp_uri("eagler-wisp:wss%3A%2F%2Frelay"), Err(WispError::MissingTarget));
        assert_eq!(parse_wisp_uri("eagler-wisp:wss%3A%2F%2Frelay?target="), Err(WispError::InvalidTarget));
        assert!(parse_wisp_uri("eagler-wisp:http%3A%2F%2Frelay?target=mc").is_err());
    }

    #[test]
    fn greeting_opens_tcp_stream_one_and_consumes_no_credit() {
        let mut session = WispSession::new("mc.example.net", 25565).unwrap();
        session.receive_packet(&greeting(2)).unwrap();
        assert_eq!(session.state(), WispState::Ready);
        let open = session.pop_outbound().unwrap();
        assert_eq!(&open[..8], &[1, 1, 0, 0, 0, 1, 0xDD, 0x63]);
        assert_eq!(&open[8..], b"mc.example.net");
    }

    #[test]
    fn splits_tcp_data_and_respects_absolute_credit_window() {
        let mut session = WispSession::new("mc.example.net", 25565).unwrap();
        session.receive_packet(&greeting(1)).unwrap();
        session.pop_outbound();
        let data = vec![7_u8; 70_000];
        session.send_tcp(&data).unwrap();
        let first = session.pop_outbound().unwrap();
        assert_eq!(first.len(), 5 + 65_536);
        assert_eq!(first[0], 2);
        assert_eq!(first[5], 7);
        assert!(session.pop_outbound().is_none());
        assert_eq!(session.queued_send_bytes(), 5 + (70_000 - 65_536));

        let mut update = [0_u8; 9];
        update[0] = 3;
        update[1] = 1;
        update[5..9].copy_from_slice(&1_u32.to_le_bytes());
        session.receive_packet(&update).unwrap();
        let second = session.pop_outbound().unwrap();
        assert_eq!(second.len(), 5 + (70_000 - 65_536));
        assert!(session.pop_outbound().is_none());
        assert_eq!(session.queued_send_bytes(), 0);
    }

    #[test]
    fn reads_tcp_data_into_a_reusable_buffer() {
        let mut session = WispSession::new("mc.example.net", 25565).unwrap();
        session.receive_packet(&greeting(0)).unwrap();
        session.pop_outbound();
        let mut packet = vec![2, 1, 0, 0, 0];
        packet.extend_from_slice(b"minecraft");
        session.receive_packet(&packet).unwrap();

        let mut first = [0_u8; 5];
        assert_eq!(session.read_tcp(&mut first), 5);
        assert_eq!(&first, b"minec");
        let mut tail = [0_u8; 8];
        assert_eq!(session.read_tcp(&mut tail), 4);
        assert_eq!(&tail[..4], b"raft");
    }

    #[test]
    fn invalid_packets_fail_the_stream() {
        let mut session = WispSession::new("mc.example.net", 25565).unwrap();
        assert!(session.receive_packet(&[3, 0]).is_err());
        assert_eq!(session.state(), WispState::Failed);
    }

    #[test]
    fn pending_queue_limit_is_enforced() {
        let mut session = WispSession::new("mc.example.net", 25565).unwrap();
        session.receive_packet(&greeting(0)).unwrap();
        session.pop_outbound();
        let data = vec![4_u8; 4 * 1024 * 1024 + 1];
        assert_eq!(session.send_tcp(&data), Err(WispError::QueueLimit));
        assert_eq!(session.state(), WispState::Failed);
    }
}
