//! xiaozhi 二进制音频帧编解码 (协议版本 1/2/3)
//!
//! 帧格式对齐 C++ 参考实现 (`main/protocols/protocol.h`):
//!
//! - 版本 1: WebSocket 二进制帧载荷 = 裸 opus
//! - 版本 2: 16 字节帧头 + 载荷
//!   `version u16 BE | type u16 BE | reserved u32 BE | timestamp u32 BE | payload_size u32 BE`
//! - 版本 3: 4 字节帧头 + 载荷
//!   `type u8 | reserved u8 | payload_size u16 BE`
//!
//! 版本 2 的 `timestamp` 是服务端 AEC 的对齐依据: 下行帧携带服务器生成的
//! 毫秒时间戳, 设备每播放一帧就把它压入 [`AecClock`], 上行帧按顺序弹出
//! 该时间戳随帧发送, 服务端即可找到回声参考帧。

/// 版本 2 帧头长度
pub const V2_HEADER_LEN: usize = 16;
/// 版本 3 帧头长度
pub const V3_HEADER_LEN: usize = 4;
/// 版本 2 的协议版本号
pub const VERSION_2: u16 = 2;

/// 帧类型: opus 音频
pub const MSG_TYPE_OPUS: u16 = 0;
/// 帧类型: JSON 文本 (仅 v2/v3 帧头定义, 实际文本走 WS 文本帧)
pub const MSG_TYPE_JSON: u16 = 1;

/// 帧编解码错误
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameError {
    /// 缓冲不足帧头长度
    TooShort,
    /// 版本号与解码器不匹配
    BadVersion,
    /// 帧类型不在已知范围
    BadType,
    /// 头部声明长度与实际载荷长度不一致
    SizeMismatch,
    /// 输出缓冲不足
    OutputTooSmall,
}

/// 版本 2 帧视图
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameV2<'a> {
    pub msg_type: u16,
    pub timestamp: u32,
    pub payload: &'a [u8],
}

/// 版本 3 帧视图
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameV3<'a> {
    pub msg_type: u8,
    pub payload: &'a [u8],
}

/// 编码版本 2 帧, 返回写入长度
pub fn encode_v2(timestamp: u32, payload: &[u8], out: &mut [u8]) -> Result<usize, FrameError> {
    let need = V2_HEADER_LEN + payload.len();
    if out.len() < need {
        return Err(FrameError::OutputTooSmall);
    }
    out[0..2].copy_from_slice(&VERSION_2.to_be_bytes());
    out[2..4].copy_from_slice(&MSG_TYPE_OPUS.to_be_bytes());
    out[4..8].copy_from_slice(&0u32.to_be_bytes());
    out[8..12].copy_from_slice(&timestamp.to_be_bytes());
    out[12..16].copy_from_slice(&(payload.len() as u32).to_be_bytes());
    out[V2_HEADER_LEN..need].copy_from_slice(payload);
    Ok(need)
}

/// 解码版本 2 帧 (严格校验长度, 拒绝拼接/截断帧)
pub fn decode_v2(buf: &[u8]) -> Result<FrameV2<'_>, FrameError> {
    if buf.len() < V2_HEADER_LEN {
        return Err(FrameError::TooShort);
    }
    let version = u16::from_be_bytes([buf[0], buf[1]]);
    if version != VERSION_2 {
        return Err(FrameError::BadVersion);
    }
    let msg_type = u16::from_be_bytes([buf[2], buf[3]]);
    if msg_type > MSG_TYPE_JSON {
        return Err(FrameError::BadType);
    }
    let timestamp = u32::from_be_bytes([buf[8], buf[9], buf[10], buf[11]]);
    let size = u32::from_be_bytes([buf[12], buf[13], buf[14], buf[15]]) as usize;
    if size != buf.len() - V2_HEADER_LEN {
        return Err(FrameError::SizeMismatch);
    }
    Ok(FrameV2 {
        msg_type,
        timestamp,
        payload: &buf[V2_HEADER_LEN..],
    })
}

/// 编码版本 3 帧, 返回写入长度
pub fn encode_v3(msg_type: u8, payload: &[u8], out: &mut [u8]) -> Result<usize, FrameError> {
    let need = V3_HEADER_LEN + payload.len();
    if out.len() < need {
        return Err(FrameError::OutputTooSmall);
    }
    if payload.len() > u16::MAX as usize {
        return Err(FrameError::SizeMismatch);
    }
    out[0] = msg_type;
    out[1] = 0;
    out[2..4].copy_from_slice(&(payload.len() as u16).to_be_bytes());
    out[V3_HEADER_LEN..need].copy_from_slice(payload);
    Ok(need)
}

/// 解码版本 3 帧
pub fn decode_v3(buf: &[u8]) -> Result<FrameV3<'_>, FrameError> {
    if buf.len() < V3_HEADER_LEN {
        return Err(FrameError::TooShort);
    }
    let msg_type = buf[0];
    if msg_type > MSG_TYPE_JSON as u8 {
        return Err(FrameError::BadType);
    }
    let size = u16::from_be_bytes([buf[2], buf[3]]) as usize;
    if size != buf.len() - V3_HEADER_LEN {
        return Err(FrameError::SizeMismatch);
    }
    Ok(FrameV3 {
        msg_type,
        payload: &buf[V3_HEADER_LEN..],
    })
}

/// 允许的协议版本范围
pub const VERSION_MIN: u16 = 1;
pub const VERSION_MAX: u16 = 3;

/// 把任意版本号收敛到支持范围
pub fn clamp_version(version: u16) -> u16 {
    version.clamp(VERSION_MIN, VERSION_MAX)
}

/// 构造 WebSocket hello 消息
///
/// `features.mcp` 宣告 MCP 设备控制; 版本 ≥2 时额外置 `features.aec` —
/// 表明上行帧携带时间戳, 服务端可据此做回声消除。
pub fn hello_message(version: u16) -> heapless::String<320> {
    use core::fmt::Write as _;
    let version = clamp_version(version);
    let mut s = heapless::String::new();
    let _ = write!(
        s,
        "{{\"type\":\"hello\",\"version\":{version},\"features\":{{\"mcp\":true\
         {}}},\"transport\":\"websocket\",\"audio_params\":{{\"format\":\"opus\",\
         \"sample_rate\":16000,\"channels\":1,\"frame_duration\":60}}}}",
        if version >= 2 { ",\"aec\":true" } else { "" }
    );
    s
}

/// 下行/上行帧时间戳配对队列 (服务端 AEC)
///
/// 每播放一帧下行音频压入其时间戳, 每发一帧上行音频弹出队首。队列满时
/// 丢弃最旧时间戳 —— 对齐的是"当前正在播放的声音", 陈旧时间戳只会误导
/// 服务端的参考帧匹配。
#[derive(Debug)]
pub struct AecClock {
    slots: [u32; Self::CAP],
    head: usize,
    len: usize,
}

impl AecClock {
    /// 深缓冲深度: 播放与上行由不同节奏驱动, 留 8 帧 (≈480ms) 余量
    pub const CAP: usize = 8;

    pub const fn new() -> Self {
        Self {
            slots: [0; Self::CAP],
            head: 0,
            len: 0,
        }
    }

    /// 压入下行帧时间戳 (0 表示无时间戳, 忽略)
    pub fn push(&mut self, timestamp: u32) {
        if timestamp == 0 {
            return;
        }
        if self.len == Self::CAP {
            self.slots[self.head] = timestamp;
            self.head = (self.head + 1) % Self::CAP;
        } else {
            let idx = (self.head + self.len) % Self::CAP;
            self.slots[idx] = timestamp;
            self.len += 1;
        }
    }

    /// 弹出上行的配对时间戳; 队列空时返回 0 (v2 语义: 无参考)
    pub fn pop(&mut self) -> u32 {
        if self.len == 0 {
            return 0;
        }
        let ts = self.slots[self.head];
        self.head = (self.head + 1) % Self::CAP;
        self.len -= 1;
        ts
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// 清空 (新回合/打断时调用)
    pub fn clear(&mut self) {
        self.head = 0;
        self.len = 0;
    }
}

impl Default for AecClock {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn v2_roundtrip_preserves_timestamp_and_payload() {
        let payload = [0xE9u8, 0x01, 0x02, 0x03, 0xFE];
        let mut out = [0u8; V2_HEADER_LEN + 8];
        let n = encode_v2(123_456, &payload, &mut out).unwrap();
        assert_eq!(n, V2_HEADER_LEN + payload.len());
        let f = decode_v2(&out[..n]).unwrap();
        assert_eq!(f.msg_type, MSG_TYPE_OPUS);
        assert_eq!(f.timestamp, 123_456);
        assert_eq!(f.payload, &payload);
    }

    #[test]
    fn v2_header_is_big_endian_and_16_bytes() {
        let mut out = [0u8; V2_HEADER_LEN + 2];
        encode_v2(0x0102_0304, &[0xAA, 0xBB], &mut out).unwrap();
        assert_eq!(&out[0..2], &[0x00, 0x02]);
        assert_eq!(&out[2..4], &[0x00, 0x00]);
        assert_eq!(&out[4..8], &[0, 0, 0, 0]);
        assert_eq!(&out[8..12], &[0x01, 0x02, 0x03, 0x04]);
        assert_eq!(&out[12..16], &[0x00, 0x00, 0x00, 0x02]);
        assert_eq!(&out[16..], &[0xAA, 0xBB]);
    }

    #[test]
    fn v2_rejects_bad_frames() {
        let mut buf = [0u8; V2_HEADER_LEN];
        assert_eq!(decode_v2(&buf[..8]), Err(FrameError::TooShort));
        encode_v2(1, &[], &mut buf).unwrap();
        // 版本不符
        let mut wrong_ver = buf;
        wrong_ver[1] = 1;
        assert_eq!(decode_v2(&wrong_ver), Err(FrameError::BadVersion));
        // 类型越界
        let mut wrong_type = buf;
        wrong_type[3] = 7;
        assert_eq!(decode_v2(&wrong_type), Err(FrameError::BadType));
        // 声明长度与实际不符
        let mut wrong_size = buf;
        wrong_size[15] = 5;
        assert_eq!(decode_v2(&wrong_size), Err(FrameError::SizeMismatch));
    }

    #[test]
    fn v2_encode_rejects_small_output() {
        let mut out = [0u8; 8];
        assert_eq!(
            encode_v2(0, &[1, 2, 3], &mut out),
            Err(FrameError::OutputTooSmall)
        );
    }

    #[test]
    fn v3_roundtrip_and_validation() {
        let payload = [1u8, 2, 3, 4, 5];
        let mut out = [0u8; V3_HEADER_LEN + 5];
        let n = encode_v3(MSG_TYPE_OPUS as u8, &payload, &mut out).unwrap();
        assert_eq!(n, 9);
        let f = decode_v3(&out[..n]).unwrap();
        assert_eq!(f.msg_type, 0);
        assert_eq!(f.payload, &payload);

        assert_eq!(decode_v3(&out[..2]), Err(FrameError::TooShort));
        let mut bad = out;
        bad[3] = 9;
        assert_eq!(decode_v3(&bad), Err(FrameError::SizeMismatch));
    }

    #[test]
    fn hello_v1_has_mcp_but_no_aec() {
        let s = hello_message(1);
        assert!(s.starts_with("{\"type\":\"hello\",\"version\":1"));
        assert!(s.contains("\"mcp\":true"));
        assert!(!s.contains("aec"));
        assert!(s.contains("\"transport\":\"websocket\""));
        assert!(s.contains("\"sample_rate\":16000"));
        assert_eq!(s.matches('{').count(), s.matches('}').count());
    }

    #[test]
    fn hello_v2_advertises_server_aec_and_clamps_version() {
        let s = hello_message(2);
        assert!(s.contains("\"version\":2"));
        assert!(s.contains("\"aec\":true"));
        assert_eq!(s.matches('{').count(), s.matches('}').count());
        // 超范围版本收敛到 3
        assert!(hello_message(9).contains("\"version\":3"));
        assert_eq!(clamp_version(0), 1);
        assert_eq!(clamp_version(3), 3);
        assert_eq!(clamp_version(99), 3);
    }

    #[test]
    fn aec_clock_is_fifo_and_ignores_zero() {
        let mut c = AecClock::new();
        assert_eq!(c.pop(), 0);
        c.push(0);
        assert!(c.is_empty());
        c.push(10);
        c.push(20);
        c.push(30);
        assert_eq!(c.len(), 3);
        assert_eq!(c.pop(), 10);
        assert_eq!(c.pop(), 20);
        assert_eq!(c.pop(), 30);
        assert_eq!(c.pop(), 0);
    }

    #[test]
    fn aec_clock_drops_oldest_when_full() {
        let mut c = AecClock::new();
        for i in 1..=AecClock::CAP as u32 + 2 {
            c.push(i * 100);
        }
        assert_eq!(c.len(), AecClock::CAP);
        // 最旧的 100/200 已被丢弃
        assert_eq!(c.pop(), 300);
    }

    #[test]
    fn aec_clock_clear_and_wraparound() {
        let mut c = AecClock::new();
        for i in 0..5u32 {
            c.push(i + 1);
        }
        assert_eq!(c.pop(), 1);
        assert_eq!(c.pop(), 2);
        // 回绕写入
        for i in 5..12u32 {
            c.push(i + 1);
        }
        assert_eq!(c.len(), 8);
        // 3/4 被后续写入覆盖, 队首为 5
        assert_eq!(c.pop(), 5);
        c.clear();
        assert!(c.is_empty());
        assert_eq!(c.pop(), 0);
    }
}
