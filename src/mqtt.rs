//! 最小 MQTT 3.1.1 编解码 (QoS 0) — 纯逻辑, host 可单测
//!
//! 只覆盖 xiaozhi MQTT 网关用到的报文: CONNECT / SUBSCRIBE / PUBLISH(QoS0) /
//! PINGREQ / DISCONNECT 的编码, 以及 CONNACK / SUBACK / PUBLISH / PINGRESP 的
//! 增量解析。解析函数在数据不完整时返回 `Ok(None)`, 由调用方继续读 socket。

/// 报文类型 (固定头高 4 位)
pub const CONNECT: u8 = 1;
pub const CONNACK: u8 = 2;
pub const PUBLISH: u8 = 3;
pub const PUBACK: u8 = 4;
pub const SUBSCRIBE: u8 = 8;
pub const SUBACK: u8 = 9;
pub const PINGREQ: u8 = 12;
pub const PINGRESP: u8 = 13;
pub const DISCONNECT: u8 = 14;

/// 固定头 PINGREQ / PINGRESP / DISCONNECT 常量报文
pub const PINGREQ_FRAME: [u8; 2] = [PINGREQ << 4, 0];
pub const PINGRESP_FRAME: [u8; 2] = [PINGRESP << 4, 0];
pub const DISCONNECT_FRAME: [u8; 2] = [DISCONNECT << 4, 0];

/// 编解码错误
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MqttError {
    /// 输出缓冲不足
    BufferTooSmall,
    /// 长度字段非法 (超过 4 字节 / 溢出)
    Malformed,
    /// 字段不是合法 UTF-8
    BadUtf8,
}

/// CONNECT 参数
pub struct ConnectParams<'a> {
    pub client_id: &'a str,
    pub username: &'a str,
    pub password: &'a str,
    pub keep_alive_secs: u16,
}

/// 报文视图 (借用输入缓冲)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Packet<'a> {
    ConnAck { session_present: bool, code: u8 },
    SubAck { packet_id: u16, granted: u8 },
    Publish(Publish<'a>),
    PubAck { packet_id: u16 },
    PingResp,
    Other { kind: u8 },
}

/// PUBLISH 报文内容
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Publish<'a> {
    pub topic: &'a str,
    pub payload: &'a [u8],
    pub qos: u8,
    pub retain: bool,
    pub dup: bool,
}

/// 编码剩余长度 (变长整数, 1~4 字节)
fn encode_remaining_len(mut len: usize, out: &mut [u8]) -> Result<usize, MqttError> {
    let mut n = 0usize;
    loop {
        let mut b = (len % 128) as u8;
        len /= 128;
        if len > 0 {
            b |= 0x80;
        }
        *out.get_mut(n).ok_or(MqttError::BufferTooSmall)? = b;
        n += 1;
        if len == 0 {
            return Ok(n);
        }
        if n == 4 {
            return Err(MqttError::Malformed);
        }
    }
}

/// 解析剩余长度: 返回 (值, 占用字节数); 数据不足返回 Ok(None)
fn decode_remaining_len(buf: &[u8]) -> Result<Option<(usize, usize)>, MqttError> {
    let mut value = 0usize;
    let mut multiplier = 1usize;
    for (i, &b) in buf.iter().enumerate() {
        value += (b & 0x7F) as usize * multiplier;
        if b & 0x80 == 0 {
            return Ok(Some((value, i + 1)));
        }
        multiplier *= 128;
        if i == 3 {
            return Err(MqttError::Malformed);
        }
    }
    Ok(None)
}

/// 计算固定头长度并写入 `out` 起始处, 返回 (头部字节数, 总长度)
fn begin_frame(first: u8, body_len: usize, out: &mut [u8]) -> Result<(usize, usize), MqttError> {
    let mut hdr = [0u8; 5];
    hdr[0] = first;
    let n = 1 + encode_remaining_len(body_len, &mut hdr[1..])?;
    let total = n + body_len;
    if total > out.len() {
        return Err(MqttError::BufferTooSmall);
    }
    out[..n].copy_from_slice(&hdr[..n]);
    Ok((n, total))
}

/// 写入大端 u16 长度前缀字符串, 返回新偏移
fn write_str(s: &str, out: &mut [u8], at: usize) -> Result<usize, MqttError> {
    let len = u16::try_from(s.len()).map_err(|_| MqttError::Malformed)?;
    out[at..at + 2].copy_from_slice(&len.to_be_bytes());
    out[at + 2..at + 2 + s.len()].copy_from_slice(s.as_bytes());
    Ok(at + 2 + s.len())
}

/// 编码 CONNECT (clean session, 用户名/密码认证)
pub fn encode_connect(p: &ConnectParams<'_>, out: &mut [u8]) -> Result<usize, MqttError> {
    let body_len = 10 + 3 * 2 + p.client_id.len() + p.username.len() + p.password.len();
    let (mut at, total) = begin_frame(CONNECT << 4, body_len, out)?;
    // 协议名 + 级别
    out[at..at + 7].copy_from_slice(&[0x00, 0x04, b'M', b'Q', b'T', b'T', 0x04]);
    at += 7;
    // clean session + username + password
    out[at] = 0b1100_0010;
    at += 1;
    out[at..at + 2].copy_from_slice(&p.keep_alive_secs.to_be_bytes());
    at += 2;
    for s in [p.client_id, p.username, p.password] {
        at = write_str(s, out, at)?;
    }
    debug_assert_eq!(at, total);
    Ok(total)
}

/// 编码 SUBSCRIBE (单主题, QoS 0, packet_id 由调用方维护)
pub fn encode_subscribe(packet_id: u16, topic: &str, out: &mut [u8]) -> Result<usize, MqttError> {
    let body_len = 2 + 2 + topic.len() + 1;
    let (mut at, total) = begin_frame(SUBSCRIBE << 4 | 0b0010, body_len, out)?;
    out[at..at + 2].copy_from_slice(&packet_id.to_be_bytes());
    at += 2;
    at = write_str(topic, out, at)?;
    out[at] = 0;
    at += 1;
    debug_assert_eq!(at, total);
    Ok(total)
}

/// 编码 QoS0 PUBLISH
pub fn encode_publish(topic: &str, payload: &[u8], out: &mut [u8]) -> Result<usize, MqttError> {
    let body_len = 2 + topic.len() + payload.len();
    let (mut at, total) = begin_frame(PUBLISH << 4, body_len, out)?;
    at = write_str(topic, out, at)?;
    out[at..at + payload.len()].copy_from_slice(payload);
    at += payload.len();
    debug_assert_eq!(at, total);
    Ok(total)
}

/// 增量解析一个报文
///
/// 返回 `Ok(None)` 表示缓冲里还不够一个完整报文 (调用方继续读 socket)。
/// 返回 `Ok(Some((packet, consumed)))` 时 `consumed` 为整包字节数。
pub fn decode(buf: &[u8]) -> Result<Option<(Packet<'_>, usize)>, MqttError> {
    if buf.is_empty() {
        return Ok(None);
    }
    let kind = buf[0] >> 4;
    let flags = buf[0] & 0x0F;
    let Some((remaining, len_bytes)) = decode_remaining_len(&buf[1..])? else {
        return Ok(None);
    };
    let hlen = 1 + len_bytes;
    let total = hlen + remaining;
    if buf.len() < total {
        return Ok(None);
    }
    let body = &buf[hlen..total];
    let packet = match kind {
        CONNACK => {
            if body.len() < 2 {
                return Err(MqttError::Malformed);
            }
            Packet::ConnAck {
                session_present: body[0] & 1 != 0,
                code: body[1],
            }
        },
        SUBACK => {
            if body.len() < 3 {
                return Err(MqttError::Malformed);
            }
            Packet::SubAck {
                packet_id: u16::from_be_bytes([body[0], body[1]]),
                granted: body[2],
            }
        },
        PUBLISH => {
            if body.len() < 2 {
                return Err(MqttError::Malformed);
            }
            let topic_len = u16::from_be_bytes([body[0], body[1]]) as usize;
            if body.len() < 2 + topic_len {
                return Err(MqttError::Malformed);
            }
            let topic =
                core::str::from_utf8(&body[2..2 + topic_len]).map_err(|_| MqttError::BadUtf8)?;
            let qos = (flags >> 1) & 0x03;
            let mut off = 2 + topic_len;
            if qos > 0 {
                if body.len() < off + 2 {
                    return Err(MqttError::Malformed);
                }
                off += 2;
            }
            Packet::Publish(Publish {
                topic,
                payload: &body[off..],
                qos,
                retain: flags & 1 != 0,
                dup: flags & 0x08 != 0,
            })
        },
        PUBACK => {
            if body.len() < 2 {
                return Err(MqttError::Malformed);
            }
            Packet::PubAck {
                packet_id: u16::from_be_bytes([body[0], body[1]]),
            }
        },
        PINGRESP => Packet::PingResp,
        other => Packet::Other { kind: other },
    };
    Ok(Some((packet, total)))
}

/// 便捷: 拼接 CONNECT 报文所需的最大长度 (客户端 id/用户名/密码各按 128 计)
pub const CONNECT_MAX: usize = 512;

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_connect() -> (heapless::Vec<u8, 256>, usize) {
        let p = ConnectParams {
            client_id: "cid",
            username: "user",
            password: "pass",
            keep_alive_secs: 60,
        };
        let mut buf = [0u8; 256];
        let n = encode_connect(&p, &mut buf).unwrap();
        let mut v = heapless::Vec::new();
        v.extend_from_slice(&buf[..n]).unwrap();
        (v, n)
    }

    #[test]
    fn connect_layout_is_mqtt311() {
        let (frame, _) = sample_connect();
        assert_eq!(frame[0], 0x10);
        // 可变头
        assert_eq!(&frame[2..9], b"\x00\x04MQTT\x04");
        // clean session + username + password
        assert_eq!(frame[9], 0b1100_0010);
        assert_eq!(&frame[10..12], &[0x00, 0x3C]);
        // 客户端 id
        assert_eq!(&frame[12..17], b"\x00\x03cid");
        // 用户名/密码
        assert_eq!(&frame[17..23], b"\x00\x04user");
        assert_eq!(&frame[23..29], b"\x00\x04pass");
        assert_eq!(frame.len(), 29);
    }

    #[test]
    fn remaining_len_varint_roundtrip_via_publish() {
        // 200 字节载荷 → 剩余长度需要 2 字节
        let payload = [0xAB; 200];
        let mut buf = [0u8; 512];
        let n = encode_publish("t", &payload, &mut buf).unwrap();
        assert_eq!(buf[0], 0x30);
        assert_eq!(n, 1 + 2 + 2 + 1 + 200);
        let (pkt, used) = decode(&buf[..n]).unwrap().unwrap();
        assert_eq!(used, n);
        match pkt {
            Packet::Publish(p) => {
                assert_eq!(p.topic, "t");
                assert_eq!(p.payload, &payload);
                assert_eq!(p.qos, 0);
            },
            other => panic!("期望 Publish, 实际 {other:?}"),
        }
    }

    #[test]
    fn decode_handles_incremental_input() {
        let mut buf = [0u8; 128];
        let n = encode_publish("topic", b"hello", &mut buf).unwrap();
        // 少一个字节 → 需要更多数据
        assert!(decode(&buf[..n - 1]).unwrap().is_none());
        assert!(decode(&[0x30]).unwrap().is_none());
        assert!(decode(&[]).unwrap().is_none());
        assert!(decode(&buf[..n]).unwrap().is_some());
    }

    #[test]
    fn decode_connack_suback_pingresp() {
        let (pkt, used) = decode(&[0x20, 0x02, 0x00, 0x00]).unwrap().unwrap();
        assert_eq!(used, 4);
        assert_eq!(
            pkt,
            Packet::ConnAck {
                session_present: false,
                code: 0
            }
        );

        let (pkt, _) = decode(&[0x90, 0x03, 0x00, 0x01, 0x00]).unwrap().unwrap();
        assert_eq!(
            pkt,
            Packet::SubAck {
                packet_id: 1,
                granted: 0
            }
        );

        let (pkt, _) = decode(&PINGRESP_FRAME).unwrap().unwrap();
        assert_eq!(pkt, Packet::PingResp);
    }

    #[test]
    fn subscribe_encoding_and_decode_qos1_publish() {
        let mut buf = [0u8; 128];
        let n = encode_subscribe(7, "devices/p2p/aa_bb", &mut buf).unwrap();
        assert_eq!(buf[0], 0x82);
        let (pkt, used) = decode(&buf[..n]).unwrap().unwrap();
        assert_eq!(used, n);
        match pkt {
            Packet::Other { kind } => assert_eq!(kind, SUBSCRIBE),
            other => panic!("期望 Other, 实际 {other:?}"),
        }

        // QoS1 PUBLISH: 固定头 flags=0b0010 + packet id
        let frame = [
            0x32, 0x0B, 0x00, 0x03, b'a', b'b', b'c', 0x00, 0x09, 1, 2, 3, 4,
        ];
        let (pkt, used) = decode(&frame).unwrap().unwrap();
        assert_eq!(used, frame.len());
        match pkt {
            Packet::Publish(p) => {
                assert_eq!(p.topic, "abc");
                assert_eq!(p.qos, 1);
                assert_eq!(p.payload, &[1, 2, 3, 4]);
            },
            other => panic!("期望 Publish, 实际 {other:?}"),
        }
    }

    #[test]
    fn rejects_malformed_remaining_length() {
        assert_eq!(
            decode(&[0x30, 0xFF, 0xFF, 0xFF, 0xFF, 0x01]).unwrap_err(),
            MqttError::Malformed
        );
    }
}
