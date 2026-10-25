//! WebSocket 帧编解码 (客户端侧) — 纯逻辑, host 可单测
//!
//! 只支持单帧 (FIN=1)、7/16 位长度、客户端掩码; 拒绝分片/continuation/
//! 保留 opcode/64 位长度/超长控制帧。RFC 6455 §5.2/§5.5。

/// 最大头部长度: 2 + 8(64 位长度, 拒绝但计入) + 4(掩码)
pub const MAX_HEADER: usize = 14;

/// 帧头
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameHeader {
    pub fin: bool,
    pub opcode: u8,
    pub masked: bool,
    pub payload_len: usize,
    /// 完整头长度 (含扩展长度与掩码)
    pub header_len: usize,
    pub mask: [u8; 4],
}

/// 解码错误
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WsError {
    TooShort,
    ReservedBits,
    Fragmented,
    Continuation,
    ReservedOpcode,
    PayloadTooLarge,
    ControlTooLong,
    OutputTooSmall,
}

impl WsError {
    pub fn describe(self) -> &'static str {
        match self {
            WsError::TooShort => "帧头不完整",
            WsError::ReservedBits => "RSV 非零",
            WsError::Fragmented => "分片帧不支持",
            WsError::Continuation => "continuation 不支持",
            WsError::ReservedOpcode => "保留 opcode",
            WsError::PayloadTooLarge => "帧过长/64 位长度不支持",
            WsError::ControlTooLong => "控制帧过长",
            WsError::OutputTooSmall => "输出缓冲不足",
        }
    }
}

/// 由前 2 字节计算完整头长度 (含扩展长度与掩码)
///
/// 同时完成可从固定头判断的合法性检查 (RSV/FIN/opcode)。
pub fn header_len(first2: &[u8]) -> Result<usize, WsError> {
    if first2.len() < 2 {
        return Err(WsError::TooShort);
    }
    validate_first(first2[0], first2[1])?;
    let masked = first2[1] & 0x80 != 0;
    let ext = match first2[1] & 0x7F {
        126 => 2,
        127 => return Err(WsError::PayloadTooLarge),
        _ => 0,
    };
    Ok(2 + ext + if masked { 4 } else { 0 })
}

fn validate_first(b0: u8, b1: u8) -> Result<(), WsError> {
    if b0 & 0x70 != 0 {
        return Err(WsError::ReservedBits);
    }
    if b0 & 0x80 == 0 {
        return Err(WsError::Fragmented);
    }
    let opcode = b0 & 0x0F;
    if opcode == 0 {
        return Err(WsError::Continuation);
    }
    if matches!(opcode, 3..=7 | 11..=15) {
        return Err(WsError::ReservedOpcode);
    }
    // 控制帧 (8/9/10) 长度位只能是 0..=125, 不能是 126/127
    if opcode >= 8 && b1 & 0x7F >= 126 {
        return Err(WsError::ControlTooLong);
    }
    Ok(())
}

/// 解析完整帧头 (`hdr.len()` 应 ≥ [`header_len`] 的返回值)
pub fn parse_header(hdr: &[u8], max_payload: usize) -> Result<FrameHeader, WsError> {
    if hdr.len() < 2 {
        return Err(WsError::TooShort);
    }
    validate_first(hdr[0], hdr[1])?;
    let masked = hdr[1] & 0x80 != 0;
    let (payload_len, mut off) = match hdr[1] & 0x7F {
        126 => {
            if hdr.len() < 4 {
                return Err(WsError::TooShort);
            }
            (u16::from_be_bytes([hdr[2], hdr[3]]) as usize, 4)
        },
        127 => return Err(WsError::PayloadTooLarge),
        n => (n as usize, 2),
    };
    let mut mask = [0u8; 4];
    if masked {
        if hdr.len() < off + 4 {
            return Err(WsError::TooShort);
        }
        mask.copy_from_slice(&hdr[off..off + 4]);
        off += 4;
    }
    if payload_len > max_payload {
        return Err(WsError::PayloadTooLarge);
    }
    Ok(FrameHeader {
        fin: true,
        opcode: hdr[0] & 0x0F,
        masked,
        payload_len,
        header_len: off,
        mask,
    })
}

/// 编码客户端帧 (FIN=1, 必带掩码), 返回总长度
pub fn encode_client_frame(
    opcode: u8,
    payload: &[u8],
    mask: [u8; 4],
    out: &mut [u8],
) -> Result<usize, WsError> {
    let (len_bytes, header_len) = if payload.len() < 126 {
        (1usize, 2usize)
    } else if payload.len() <= u16::MAX as usize {
        (2, 4)
    } else {
        return Err(WsError::PayloadTooLarge);
    };
    let total = header_len + 4 + payload.len();
    if out.len() < total {
        return Err(WsError::OutputTooSmall);
    }
    out[0] = 0x80 | opcode;
    out[1] = 0x80
        | if len_bytes == 1 {
            payload.len() as u8
        } else {
            126
        };
    if len_bytes == 2 {
        out[2..4].copy_from_slice(&(payload.len() as u16).to_be_bytes());
    }
    let mut o = header_len;
    out[o..o + 4].copy_from_slice(&mask);
    o += 4;
    for (i, b) in payload.iter().enumerate() {
        out[o + i] = b ^ mask[i % 4];
    }
    Ok(total)
}

/// 就地解掩码
pub fn unmask(payload: &mut [u8], mask: [u8; 4]) {
    for (i, b) in payload.iter_mut().enumerate() {
        *b ^= mask[i % 4];
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_len_variants() {
        assert_eq!(header_len(&[0x81, 0x05]), Ok(2)); // 文本, 5B, 无掩码
        assert_eq!(header_len(&[0x81, 0x7E]), Ok(4)); // 16 位长度
        assert_eq!(header_len(&[0x81, 0x85]), Ok(6)); // 小帧 + 掩码
        assert_eq!(header_len(&[0x81, 0x7F]), Err(WsError::PayloadTooLarge));
        assert_eq!(header_len(&[0xC1, 0x00]), Err(WsError::ReservedBits));
        assert_eq!(header_len(&[0x01, 0x00]), Err(WsError::Fragmented));
        assert_eq!(header_len(&[0x80, 0x00]), Err(WsError::Continuation));
        assert_eq!(header_len(&[0x83, 0x00]), Err(WsError::ReservedOpcode));
        assert_eq!(header_len(&[0x89, 0x7E]), Err(WsError::ControlTooLong));
    }

    #[test]
    fn parse_header_fields() {
        let hdr = [0x82, 0x7E, 0x01, 0x02];
        let f = parse_header(&hdr, 4096).unwrap();
        assert_eq!(f.opcode, 2);
        assert_eq!(f.payload_len, 258);
        assert_eq!(f.header_len, 4);
        assert!(!f.masked);
        let hdr = [0x81, 0x83, 1, 2, 3, 4];
        let f = parse_header(&hdr, 4096).unwrap();
        assert!(f.masked);
        assert_eq!(f.mask, [1, 2, 3, 4]);
        assert_eq!(f.payload_len, 3);
        assert!(parse_header(&hdr, 2).is_err());
    }

    #[test]
    fn encode_decode_roundtrip() {
        let mask = [0x37, 0xFA, 0x21, 0x3D];
        let mut buf = [0u8; 256];
        let payload = b"hello";
        let n = encode_client_frame(1, payload, mask, &mut buf).unwrap();
        let f = parse_header(&buf[..n], 256).unwrap();
        assert_eq!(f.opcode, 1);
        assert!(f.masked);
        assert_eq!(f.payload_len, payload.len());
        let mut body = buf[f.header_len..n].to_vec();
        unmask(&mut body, f.mask);
        assert_eq!(body, payload);

        // 16 位长度
        let payload = [0xA5u8; 200];
        let n = encode_client_frame(2, &payload, mask, &mut buf).unwrap();
        let f = parse_header(&buf[..n], 256).unwrap();
        assert_eq!(f.payload_len, 200);
        assert_eq!(f.header_len, 8); // 4B 头 + 4B 掩码
        let mut body = buf[f.header_len..n].to_vec();
        unmask(&mut body, f.mask);
        assert_eq!(body, payload);

        // 输出不足
        let mut tiny = [0u8; 4];
        assert_eq!(
            encode_client_frame(1, &[1, 2, 3], mask, &mut tiny),
            Err(WsError::OutputTooSmall)
        );
    }
}
