//! OTA 升级 — 清单解析与版本比较 (纯逻辑, host 单测)
//!
//! 协议 (对齐 C++ ota.cc): GET http://<server>:8003/xiaozhi/ota/ 返回
//! `{"firmware":{"version":"1.2.3","url":"http://..."},"activation":{...}}`。
//! 固件比较版本后决定是否下载烧写 (烧写需分区表改造, 见 ROADMAP M7)。

/// 固件更新信息
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UpdateInfo<'a> {
    pub version: &'a str,
    pub url: &'a str,
}

/// 解析 OTA 清单, 提取 firmware.version / firmware.url
pub fn parse_manifest(body: &str) -> Option<UpdateInfo<'_>> {
    // 在 "firmware" 对象内查找两个字段: 取 "firmware":{ 之后到配对大括号前的窗口
    let pat = "\"firmware\":";
    let idx = body.find(pat)? + pat.len();
    let window = body[idx..].trim_start();
    let window = window.strip_prefix('{')?;
    let end = window.find('}')?;
    let window = &window[..end];

    let version = find_str_value(window, "version")?;
    let url = find_str_value(window, "url")?;
    Some(UpdateInfo { version, url })
}

/// 解析激活码字段 (C++ 返回的 activation.code, 设备端目前仅日志)
pub fn parse_activation_code(body: &str) -> Option<&str> {
    let pat = "\"activation\":";
    let idx = body.find(pat)? + pat.len();
    let window = body[idx..].trim_start();
    let window = window.strip_prefix('{')?;
    let end = window.find('}')?;
    find_str_value(&window[..end], "code")
}

/// 语义化版本比较 (点分十进制, 各段按数值比; 段数不足按 0 补齐)
///
/// 返回 Ordering: `a` 相对 `b` 的大小。非数字段按 0 处理。
pub fn compare_versions(a: &str, b: &str) -> core::cmp::Ordering {
    use core::cmp::Ordering;
    let mut sa = a.split('.');
    let mut sb = b.split('.');
    loop {
        match (sa.next(), sb.next()) {
            (None, None) => return Ordering::Equal,
            (x, y) => {
                let xa = x.map(parse_num).unwrap_or(0);
                let xb = y.map(parse_num).unwrap_or(0);
                match xa.cmp(&xb) {
                    Ordering::Equal => continue,
                    other => return other,
                }
            },
        }
    }
}

fn parse_num(s: &str) -> u64 {
    // 去掉可能的非数字后缀 (如 "3-beta")
    let digits: &str = s.split(|c: char| !c.is_ascii_digit()).next().unwrap_or("0");
    digits.parse().unwrap_or(0)
}

/// 在 JSON 文本窗口中取 `"key":"值"`
fn find_str_value<'a>(window: &'a str, key: &str) -> Option<&'a str> {
    let mut pat = heapless::String::<40>::new();
    pat.push('"').ok()?;
    pat.push_str(key).ok()?;
    pat.push('"').ok()?;
    pat.push(':').ok()?;
    let at = window.find(pat.as_str())?;
    let v = window[at + pat.len()..].trim_start();
    let v = v.strip_prefix('"')?;
    let end = v.find('"')?;
    Some(&v[..end])
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::cmp::Ordering;

    const MANIFEST: &str = r#"{
        "firmware": {"version": "2.1.0", "url": "http://192.168.123.14:8003/firmware/xiaozhi.bin"},
        "activation": {"challenge": "abc", "code": "123456", "message": "请输入配网码"}
    }"#;

    #[test]
    fn parses_firmware_version_and_url() {
        let info = parse_manifest(MANIFEST).unwrap();
        assert_eq!(info.version, "2.1.0");
        assert_eq!(info.url, "http://192.168.123.14:8003/firmware/xiaozhi.bin");
    }

    #[test]
    fn manifest_missing_url_is_none() {
        assert_eq!(parse_manifest(r#"{"firmware":{"version":"1.0"}}"#), None);
        assert_eq!(parse_manifest("not json"), None);
    }

    #[test]
    fn activation_code_parsed() {
        assert_eq!(parse_activation_code(MANIFEST), Some("123456"));
    }

    #[test]
    fn version_compare_semantics() {
        assert_eq!(compare_versions("1.2.3", "1.2.3"), Ordering::Equal);
        assert_eq!(compare_versions("1.2.4", "1.2.3"), Ordering::Greater);
        assert_eq!(compare_versions("1.2", "1.2.1"), Ordering::Less);
        assert_eq!(compare_versions("2.0.0", "1.9.9"), Ordering::Greater);
        // 多位数逐段比较 (字符串比较会错: "10" < "9")
        assert_eq!(compare_versions("1.10.0", "1.9.0"), Ordering::Greater);
        assert_eq!(compare_versions("0.1.0", "0.1.0-beta"), Ordering::Equal);
    }

    #[test]
    fn same_version_means_no_update() {
        let info = parse_manifest(MANIFEST).unwrap();
        let current = "2.1.0";
        assert_ne!(compare_versions(info.version, current), Ordering::Less);
        let older_current = "1.0.0";
        assert_eq!(
            compare_versions(info.version, older_current),
            Ordering::Greater
        );
    }
}
