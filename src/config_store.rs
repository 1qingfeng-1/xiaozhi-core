//! NVS 定长配置记录 — magic + 多字段 (u16 LE 长度 + bytes) + CRC32
//!
//! 供 WiFi 凭据 / MQTT 网关配置共用。纯逻辑, host 可单测。
//! 格式与既有 `wifi::store` 完全一致, 保证旧记录可读。

use crate::ota::crc32_init_xorout;

/// 编码记录到 `out`, 返回写入长度 (含 CRC); 容量不足返回 None
pub fn encode(magic: u32, fields: &[&str], out: &mut [u8]) -> Option<usize> {
    if out.len() < 4 {
        return None;
    }
    out[..4].copy_from_slice(&magic.to_le_bytes());
    let mut o = 4usize;
    for s in fields {
        let len = u16::try_from(s.len()).ok()?;
        if o + 2 + s.len() > out.len() {
            return None;
        }
        out[o..o + 2].copy_from_slice(&len.to_le_bytes());
        o += 2;
        out[o..o + s.len()].copy_from_slice(s.as_bytes());
        o += s.len();
    }
    if o + 4 > out.len() {
        return None;
    }
    let crc = crc32_init_xorout(&out[..o], 0, 0xFFFF_FFFF);
    out[o..o + 4].copy_from_slice(&crc.to_le_bytes());
    Some(o + 4)
}

/// 解码记录; `fields.len()` 决定字段数。
///
/// `legacy_zero_crc` 为真时接受 CRC 全 0 的旧 WiFi 记录 (旧实现未写 CRC,
/// 缓冲区尾部为 0)。返回 None 表示 magic/长度/UTF-8/CRC 任一不合法。
pub fn decode<'a>(
    magic: u32,
    buf: &'a [u8],
    fields: &mut [&'a str],
    legacy_zero_crc: bool,
) -> Option<usize> {
    if buf.len() < 4 || u32::from_le_bytes(buf[..4].try_into().ok()?) != magic {
        return None;
    }
    let mut o = 4usize;
    for slot in fields.iter_mut() {
        let len = u16::from_le_bytes(buf.get(o..o + 2)?.try_into().ok()?) as usize;
        o += 2;
        let s = buf.get(o..o + len)?;
        o += len;
        *slot = core::str::from_utf8(s).ok()?;
    }
    let crc_stored = u32::from_le_bytes(buf.get(o..o + 4)?.try_into().ok()?);
    let crc_calc = crc32_init_xorout(&buf[..o], 0, 0xFFFF_FFFF);
    if crc_stored == crc_calc || (legacy_zero_crc && crc_stored == 0) {
        Some(o + 4)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MAGIC: u32 = 0x5849_4F5A;

    #[test]
    fn roundtrip_fields_and_crc() {
        let mut buf = [0u8; 128];
        let n = encode(MAGIC, &["ssid", "pass", "192.168.1.2"], &mut buf).unwrap();
        assert_eq!(&buf[..4], &MAGIC.to_le_bytes());
        let mut fields = ["", "", ""];
        let used = decode(MAGIC, &buf[..n], &mut fields, false).unwrap();
        assert_eq!(used, n);
        assert_eq!(fields, ["ssid", "pass", "192.168.1.2"]);
    }

    #[test]
    fn rejects_crc_corruption_and_truncation() {
        let mut buf = [0u8; 64];
        let n = encode(MAGIC, &["a", "bc"], &mut buf).unwrap();
        {
            let mut fields = ["", ""];
            assert!(decode(MAGIC, &buf[..n], &mut fields, false).is_some());
        }
        buf[n - 1] ^= 0xFF;
        let mut fields = ["", ""];
        assert!(decode(MAGIC, &buf[..n], &mut fields, false).is_none());
        assert!(decode(MAGIC, &buf[..n - 1], &mut fields, false).is_none());
        assert!(decode(MAGIC ^ 1, &buf[..n], &mut fields, false).is_none());
    }

    #[test]
    fn legacy_zero_crc_is_opt_in() {
        let mut buf = [0u8; 64];
        let n = encode(MAGIC, &["a"], &mut buf).unwrap();
        // 模拟旧格式: CRC 槽全 0
        buf[n - 4..n].fill(0);
        let mut fields = [""];
        assert!(decode(MAGIC, &buf[..n], &mut fields, true).is_some());
        assert_eq!(fields[0], "a");
        assert!(decode(MAGIC, &buf[..n], &mut fields, false).is_none());
    }

    #[test]
    fn rejects_bad_utf8_and_small_output() {
        let mut buf = [0u8; 32];
        let n = encode(MAGIC, &["ok"], &mut buf).unwrap();
        // 把字段内容改成非法 UTF-8 (长度仍为 2)
        buf[6] = 0xFF;
        let mut fields = [""];
        assert!(decode(MAGIC, &buf[..n], &mut fields, false).is_none());

        let mut tiny = [0u8; 5];
        assert!(encode(MAGIC, &["abc"], &mut tiny).is_none());
    }
}
