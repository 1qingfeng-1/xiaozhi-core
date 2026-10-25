//! 协议/清单解析器的属性测试 (host only)
//!
//! 目标不是覆盖业务逻辑 (已有单元测试), 而是验证: 任意输入不 panic、
//! 编解码往返一致、长度边界不越界。发现 panic 时 proptest 会给出最小化输入。

use proptest::prelude::*;
use xiaozhi_core::{mqtt, ota, server_msg, udp, ws};

proptest! {
    #![proptest_config(ProptestConfig { cases: 256, ..ProptestConfig::default() })]

    /// 任意字节流都不应让 WS 解析器 panic
    #[test]
    fn ws_parse_never_panics(data in prop::collection::vec(any::<u8>(), 0..128)) {
        let _ = ws::header_len(&data);
        let _ = ws::parse_header(&data, 4096);
    }

    /// 客户端帧编码 → 解析 → 解掩码应还原原始 payload
    #[test]
    fn ws_roundtrip(
        opcode in prop::sample::select(vec![0x1u8, 0x2u8]),
        payload in prop::collection::vec(any::<u8>(), 0..4096),
        mask in any::<[u8; 4]>(),
    ) {
        let mut frame = vec![0u8; payload.len() + ws::MAX_HEADER];
        let n = ws::encode_client_frame(opcode, &payload, mask, &mut frame)
            .expect("frame buffer is sized for the header + payload");
        let hdr = ws::parse_header(&frame[..n], 4096).expect("self-encoded frame must parse");
        prop_assert_eq!(hdr.opcode, opcode);
        prop_assert_eq!(hdr.payload_len, payload.len());
        prop_assert_eq!(hdr.mask, mask);

        let mut body = frame[hdr.header_len..n].to_vec();
        ws::unmask(&mut body, hdr.mask);
        prop_assert_eq!(body, payload);
    }

    /// UDP 包头字段往返一致 (payload 长度被 u16 限制)
    #[test]
    fn udp_header_roundtrip(
        base in any::<[u8; 16]>(),
        payload_len in 0u16..=u16::MAX,
        timestamp in any::<u32>(),
        sequence in any::<u32>(),
    ) {
        let built = udp::build_header(&base, payload_len as usize, timestamp, sequence)
            .expect("payload_len fits in u16");
        let parsed = udp::parse_header(&built).expect("built header is 16 bytes");
        prop_assert_eq!(parsed.payload_len, payload_len);
        prop_assert_eq!(parsed.timestamp, timestamp);
        prop_assert_eq!(parsed.sequence, sequence);
        prop_assert_eq!(parsed.msg_type, base[0]);
        prop_assert_eq!(parsed.flags, base[1]);
        prop_assert_eq!(parsed.ssrc, u32::from_be_bytes([base[4], base[5], base[6], base[7]]));
    }

    /// 任意字节流都不应让 UDP 解析器 panic
    #[test]
    fn udp_parse_never_panics(data in prop::collection::vec(any::<u8>(), 0..64)) {
        let _ = udp::parse_header(&data);
    }

    /// 十六进制解码与编码互逆
    #[test]
    fn hex_roundtrip(bytes in any::<[u8; 8]>()) {
        let text: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
        prop_assert_eq!(udp::decode_hex::<8>(&text), Some(bytes));
    }

    /// 任意 JSON/文本输入都不应让 OTA 清单解析 panic
    #[test]
    fn ota_manifest_never_panics(body in any::<String>()) {
        let _ = ota::parse_manifest(&body);
    }

    /// 任意文本输入都不应让服务器消息解析 panic
    #[test]
    fn server_msg_never_panics(text in any::<String>()) {
        let _ = server_msg::parse(&text);
    }

    /// 任意字节流都不应让 MQTT 解码 panic
    #[test]
    fn mqtt_decode_never_panics(data in prop::collection::vec(any::<u8>(), 0..256)) {
        let _ = mqtt::decode(&data);
    }
}
