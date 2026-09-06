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

// e9 固件镜像头校验 (纯逻辑, host 单测)
//
// 布局 (对齐 espflash / ESP-IDF 二级 bootloader 镜像, 已用真机镜像逐字节核对):
// - ImageHeader 24B: magic@0 (0xE9), segment_count@1, flash_mode@2,
//   flash_config@3, entry@4, ...
// - 首个 SegmentHeader 8B @24 (addr + length)
// - AppDescriptor @32: magic_word (0xABCD5432) @32, version (32B) @48,
//   project_name (32B) @80
const E9_MAGIC: u8 = 0xE9;
const APP_DESC_MAGIC: u32 = 0xABCD5432;
const DESC_VERSION_OFF: usize = 48; // 32 (descriptor 起点) + 16 (magic+secver+reserv1)
const DESC_NAME_OFF: usize = 80; // 32 + 16 + 32 (version 之后)
const DESC_STR_LEN: usize = 32;

/// 镜像头解析结果 (NUL 终止, 取到第一个 NUL 为止)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ImageInfo<'a> {
    /// 应用版本 (如 "0.1.0")
    pub version: &'a str,
    /// 项目名 (如 "xiaozhi-s3")
    pub project_name: &'a str,
}

/// 校验 e9 固件镜像头并提取版本/项目名
///
/// 需要前 80 字节 (version 字段完整)。`None` 表示不是有效镜像: 长度不足 /
/// magic 不是 0xE9 / app_desc magic 不符。供下载后先验证再烧写 (防坏固件)。
pub fn validate_image_header(img: &[u8]) -> Option<ImageInfo<'_>> {
    if img.len() < DESC_NAME_OFF + DESC_STR_LEN {
        return None;
    }
    if img[0] != E9_MAGIC {
        return None;
    }
    let magic = u32::from_le_bytes([img[32], img[33], img[34], img[35]]);
    if magic != APP_DESC_MAGIC {
        return None;
    }
    Some(ImageInfo {
        version: read_cstr(&img[DESC_VERSION_OFF..DESC_VERSION_OFF + DESC_STR_LEN]),
        project_name: read_cstr(&img[DESC_NAME_OFF..DESC_NAME_OFF + DESC_STR_LEN]),
    })
}

/// 取 C 字符串 (到第一个 NUL; 非 UTF-8 返回空串, 不 panic)
fn read_cstr(buf: &[u8]) -> &str {
    let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
    core::str::from_utf8(&buf[..end]).unwrap_or("")
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

    /// 构造一个最小 e9 镜像头 (24B ImageHeader + 8B SegmentHeader + AppDescriptor)
    fn make_image(version: &str, name: &str) -> [u8; 112] {
        let mut img = [0u8; 112];
        img[0] = E9_MAGIC;
        img[1] = 5; // segment_count
        img[2] = 2; // flash_mode
        // AppDescriptor @32
        img[32..36].copy_from_slice(&APP_DESC_MAGIC.to_le_bytes());
        let v = version.as_bytes();
        img[DESC_VERSION_OFF..DESC_VERSION_OFF + v.len()].copy_from_slice(v);
        let n = name.as_bytes();
        img[DESC_NAME_OFF..DESC_NAME_OFF + n.len()].copy_from_slice(n);
        img
    }

    #[test]
    fn validates_e9_header_and_extracts_version() {
        let img = make_image("1.2.3", "xiaozhi-s3");
        let info = validate_image_header(&img).unwrap();
        assert_eq!(info.version, "1.2.3");
        assert_eq!(info.project_name, "xiaozhi-s3");
    }

    #[test]
    fn rejects_bad_magic() {
        let mut img = make_image("1.2.3", "xiaozhi-s3");
        img[0] = 0x00;
        assert_eq!(validate_image_header(&img), None);
        img[0] = E9_MAGIC;
        img[32] = 0xff; // 破坏 app_desc magic
        assert_eq!(validate_image_header(&img), None);
    }

    #[test]
    fn rejects_short_image() {
        assert_eq!(validate_image_header(&[E9_MAGIC; 16]), None);
        assert_eq!(validate_image_header(&[]), None);
    }
}
