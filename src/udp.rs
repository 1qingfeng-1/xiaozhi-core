//! xiaozhi MQTT/UDP 音频包头 (16 字节) — 纯逻辑, host 可单测
//!
//! 布局 (大端, 对齐 C++ `mqtt_protocol.cc` 与网关 `app.js`):
//!
//! ```text
//! | type 1B | flags 1B | payload_len 2B | ssrc 4B | timestamp 4B | sequence 4B |
//! ```
//!
//! 服务端 hello 下发的 `nonce` 就是一条这样格式的 16 字节模板 (type=1,
//! ssrc=连接 id); 设备每帧只改写长度/时间戳/序列号, 整条 16 字节头同时作为
//! AES-128-CTR 的初始计数块。

/// UDP 包头长度 (也是 AES-CTR 的 IV 长度)
pub const UDP_HEADER_LEN: usize = 16;

/// UDP 包头视图
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UdpHeader {
    pub msg_type: u8,
    pub flags: u8,
    pub payload_len: u16,
    pub ssrc: u32,
    pub timestamp: u32,
    pub sequence: u32,
}

/// 以服务端 nonce 为模板生成一帧包头
///
/// 只改写 `payload_len` / `timestamp` / `sequence`; type/flags/ssrc 沿用模板。
/// `payload_len` 超过 u16 上限时返回 `None`。
pub fn build_header(
    base: &[u8; UDP_HEADER_LEN],
    payload_len: usize,
    timestamp: u32,
    sequence: u32,
) -> Option<[u8; UDP_HEADER_LEN]> {
    let len = u16::try_from(payload_len).ok()?;
    let mut h = *base;
    h[2..4].copy_from_slice(&len.to_be_bytes());
    h[8..12].copy_from_slice(&timestamp.to_be_bytes());
    h[12..16].copy_from_slice(&sequence.to_be_bytes());
    Some(h)
}

/// 解析包头
pub fn parse_header(buf: &[u8]) -> Option<UdpHeader> {
    if buf.len() < UDP_HEADER_LEN {
        return None;
    }
    Some(UdpHeader {
        msg_type: buf[0],
        flags: buf[1],
        payload_len: u16::from_be_bytes([buf[2], buf[3]]),
        ssrc: u32::from_be_bytes([buf[4], buf[5], buf[6], buf[7]]),
        timestamp: u32::from_be_bytes([buf[8], buf[9], buf[10], buf[11]]),
        sequence: u32::from_be_bytes([buf[12], buf[13], buf[14], buf[15]]),
    })
}

/// 十六进制串 → 定长字节数组 (服务端 hello 的 key/nonce 字段)
pub fn decode_hex<const N: usize>(s: &str) -> Option<[u8; N]> {
    if s.len() != N * 2 {
        return None;
    }
    let bytes = s.as_bytes();
    let mut out = [0u8; N];
    for (i, b) in out.iter_mut().enumerate() {
        let hi = hex_val(bytes[i * 2])?;
        let lo = hex_val(bytes[i * 2 + 1])?;
        *b = (hi << 4) | lo;
    }
    Some(out)
}

fn hex_val(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_header_preserves_template_and_sets_fields() {
        let mut base = [0u8; 16];
        base[0] = 1;
        base[1] = 0;
        base[4..8].copy_from_slice(&42u32.to_be_bytes());
        let h = build_header(&base, 100, 0x1122_3344, 7).unwrap();
        assert_eq!(h[0], 1);
        assert_eq!(&h[2..4], &100u16.to_be_bytes());
        assert_eq!(&h[4..8], &42u32.to_be_bytes());
        assert_eq!(&h[8..12], &0x1122_3344u32.to_be_bytes());
        assert_eq!(&h[12..16], &7u32.to_be_bytes());

        let parsed = parse_header(&h).unwrap();
        assert_eq!(parsed.msg_type, 1);
        assert_eq!(parsed.payload_len, 100);
        assert_eq!(parsed.ssrc, 42);
        assert_eq!(parsed.timestamp, 0x1122_3344);
        assert_eq!(parsed.sequence, 7);
    }

    #[test]
    fn build_header_rejects_oversized_payload() {
        assert!(build_header(&[0u8; 16], 65_536, 0, 0).is_none());
        assert!(build_header(&[0u8; 16], 65_535, 0, 0).is_some());
    }

    #[test]
    fn parse_header_rejects_short_buffer() {
        assert!(parse_header(&[0u8; 15]).is_none());
        assert!(parse_header(&[0u8; 16]).is_some());
    }

    #[test]
    fn decode_hex_accepts_mixed_case() {
        let v: [u8; 4] = decode_hex("00aB10Ff").unwrap();
        assert_eq!(v, [0x00, 0xAB, 0x10, 0xFF]);
        assert!(decode_hex::<4>("00ab10f").is_none());
        assert!(decode_hex::<4>("00ab10fg").is_none());
    }
}
