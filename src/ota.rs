//! OTA 升级 — 纯逻辑层 (host 单测)
//!
//! 三块:
//! 1. **清单解析**: 协议 (对齐 C++ ota.cc) POST http://<server>:8003/xiaozhi/ota/
//!    返回 `{"firmware":{"version":"1.2.3","url":"http://..."},"activation":{...}}`;
//!    `parse_manifest` 取版本/url, `compare_versions` 语义化比较, `validate_image_header`
//!    校验 e9 镜像头 (magic + app_desc → 版本)。
//! 2. **otadata 选择条目**: `OtaDataEntry` (32B, 对齐 IDF `esp_ota_select_entry_t`)
//!    的编解码 + `ota_select_crc` (reflected CRC-32, init 0, xorout 0xFFFFFFFF) +
//!    `active_seq`/`active_slot`/`next_write` (双 app 乒乓切换 + 回滚保留)。
//!    供固件烧写新镜像后写 otadata 分区, 二级 bootloader 据此选活动槽。

/// 固件更新信息
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UpdateInfo<'a> {
    pub version: &'a str,
    pub url: &'a str,
    /// 可选镜像 SHA-256 (服务端支持时下发; 兼容 `sha256` / `hash` 两种键)
    pub sha256: Option<&'a str>,
}

/// 取 `"key": { ... }` 对象窗口 (含大括号), 正确处理字符串内的 `{}` 与转义
fn object_window<'a>(body: &'a str, key: &str) -> Option<&'a str> {
    let mut pat = heapless::String::<40>::new();
    pat.push('"').ok()?;
    pat.push_str(key).ok()?;
    pat.push_str("\":").ok()?;
    let idx = body.find(pat.as_str())? + pat.len();
    let rest = body[idx..].trim_start();
    if !rest.starts_with('{') {
        return None;
    }
    let mut depth = 0usize;
    let mut in_str = false;
    let mut esc = false;
    for (i, &b) in rest.as_bytes().iter().enumerate() {
        if in_str {
            if esc {
                esc = false;
            } else if b == b'\\' {
                esc = true;
            } else if b == b'"' {
                in_str = false;
            }
            continue;
        }
        match b {
            b'"' => in_str = true,
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(&rest[..=i]);
                }
            },
            _ => {},
        }
    }
    None
}

/// 解析 OTA 清单, 提取 firmware.version / firmware.url / 可选 sha256
pub fn parse_manifest(body: &str) -> Option<UpdateInfo<'_>> {
    let window = object_window(body, "firmware")?;
    let version = crate::server_msg::field(window, "version")?;
    let url = crate::server_msg::field(window, "url")?;
    let sha256 = crate::server_msg::field(window, "sha256")
        .or_else(|| crate::server_msg::field(window, "hash"));
    Some(UpdateInfo {
        version,
        url,
        sha256,
    })
}

/// 解析激活码字段 (C++ 返回的 activation.code, 设备端目前仅日志)
pub fn parse_activation_code(body: &str) -> Option<&str> {
    crate::server_msg::field(object_window(body, "activation")?, "code")
}

/// OTA 清单下发的 MQTT 网关配置 (存在时设备改走 MQTT+UDP 传输)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MqttConfig<'a> {
    /// `host:port` (网关同时是 MQTT broker 与 UDP 服务端)
    pub endpoint: &'a str,
    /// `GID_<model>@@@<mac>@@@<uuid>`
    pub client_id: &'a str,
    pub username: &'a str,
    pub password: &'a str,
    pub publish_topic: &'a str,
    pub subscribe_topic: &'a str,
}

/// 解析 OTA 清单中的 `mqtt` 对象; 未配置网关时返回 None (继续走 WebSocket)
pub fn parse_mqtt_config(body: &str) -> Option<MqttConfig<'_>> {
    let window = object_window(body, "mqtt")?;
    Some(MqttConfig {
        endpoint: crate::server_msg::field(window, "endpoint")?,
        client_id: crate::server_msg::field(window, "client_id")?,
        username: crate::server_msg::field(window, "username")?,
        password: crate::server_msg::field(window, "password").unwrap_or(""),
        publish_topic: crate::server_msg::field(window, "publish_topic")?,
        subscribe_topic: crate::server_msg::field(window, "subscribe_topic")?,
    })
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

// ===== otadata 选择条目 (对齐 IDF esp_ota_select_entry_t / 二级 bootloader) =====
//
// otadata 分区 (0x2000 = 2 扇区) 含两条 32B 条目, **各占一个扇区**:
//   slot0 @+0x0000, slot1 @+0x1000; 每条: ota_seq u32 @0 | seq_label[20] @4
//   | ota_state u32 @24 | crc u32 @28; 扇区其余 0xFF。
// 二级 bootloader 启动时读两条 (偏移 0 与 0x1000), 取 ota_seq 最大且 crc 通过
// 者为活动镜像, slot = (ota_seq - 1) % app_count (我们 app_count=2 →
// ota_0/ota_1 交替)。无工厂分区且 otadata 全 0xFF 时自愈 (启动 ota_0 并写
// slot0={seq1,valid})。我们 OTA 时把**更高** seq 写入新 seq 映射的槽 (乒乓),
// 旧 valid 条目保留作回滚候选; 新条目直接置 VALID (不走 PENDING_VERIFY)。

/// otadata 未初始化序列号 (擦除态 0xFF)
pub const UNINITIALIZED_SEQUENCE: u32 = 0xFFFF_FFFF;

/// otadata 单扇区大小 (0x1000 = 4096B)
pub const OTADATA_SECTOR_SIZE: u32 = 0x1000;
/// otadata 分区大小 (2 扇区 = 0x2000)
pub const OTADATA_PARTITION_SIZE: u32 = 0x2000;
/// otadata 单条目大小 (32B, 对齐 `esp_ota_select_entry_t`)
pub const OTADATA_ENTRY_SIZE: usize = 32;

/// OTA 镜像状态 (对齐 esp_ota_img_states_t)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum OtaImageState {
    /// 未定义 (0xFFFFFFFF): 擦除态 / 从未写入
    Undefined = 0xFFFF_FFFF,
    /// 新镜像 (未确认)
    New = 0,
    /// 待验证 (回滚候选; 我们不用)
    PendingVerify = 1,
    /// 有效 (确认)
    Valid = 2,
    /// 无效
    Invalid = 3,
    /// 中止
    Aborted = 4,
}

impl OtaImageState {
    pub const fn from_u32(v: u32) -> Self {
        match v {
            0 => Self::New,
            1 => Self::PendingVerify,
            2 => Self::Valid,
            3 => Self::Invalid,
            4 => Self::Aborted,
            _ => Self::Undefined,
        }
    }

    pub const fn as_u32(self) -> u32 {
        self as u32
    }
}

/// otadata 单条目 (32B, 布局对齐 IDF `esp_ota_select_entry_t`)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OtaDataEntry {
    /// OTA 序列号 (活动镜像取最大值; UNINITIALIZED = 未写入)
    pub ota_seq: u32,
    /// 序列标签 (我们存版本串, ≤19 字符 + NUL)
    pub seq_label: [u8; 20],
    /// 镜像状态
    pub ota_state: OtaImageState,
    /// `ota_seq` 的 CRC32 (校验用)
    pub crc: u32,
}

impl OtaDataEntry {
    /// 构造条目: 自动算 `crc = ota_select_crc(ota_seq)`; `seq_label` 截断到 19 字符
    pub fn new(ota_seq: u32, seq_label: &str, ota_state: OtaImageState) -> Self {
        let mut label = [0u8; 20];
        let b = seq_label.as_bytes();
        // 按字符边界截断: 切断 UTF-8 会让 label_str() 返回空串,
        // 导致 confirm_running_image 永远匹配不上 PENDING 条目。
        let mut n = b.len().min(19);
        while n > 0 && !seq_label.is_char_boundary(n) {
            n -= 1;
        }
        label[..n].copy_from_slice(&b[..n]);
        Self {
            ota_seq,
            seq_label: label,
            ota_state,
            crc: ota_select_crc(ota_seq),
        }
    }

    /// 有效 = 状态为 Valid 且 crc 与 `ota_seq` 匹配
    pub fn is_valid(&self) -> bool {
        self.ota_state == OtaImageState::Valid && self.crc == ota_select_crc(self.ota_seq)
    }

    /// 待确认 = 状态为 PendingVerify 且 crc 匹配 (新镜像首次启动)
    pub fn is_pending(&self) -> bool {
        self.ota_state == OtaImageState::PendingVerify && self.crc == ota_select_crc(self.ota_seq)
    }

    /// 序列标签 (版本串, 截止 NUL)
    pub fn label_str(&self) -> &str {
        let n = self.seq_label.iter().position(|&b| b == 0).unwrap_or(20);
        core::str::from_utf8(&self.seq_label[..n]).unwrap_or("")
    }

    /// 编码为 32B 小端 (字段布局对齐 IDF struct)
    pub fn to_bytes(&self) -> [u8; 32] {
        let mut b = [0u8; 32];
        b[0..4].copy_from_slice(&self.ota_seq.to_le_bytes());
        b[4..24].copy_from_slice(&self.seq_label);
        b[24..28].copy_from_slice(&self.ota_state.as_u32().to_le_bytes());
        b[28..32].copy_from_slice(&self.crc.to_le_bytes());
        b
    }

    /// 从 32B 解码 (`buf` 不足 32B 返回全零/Undefined 条目)
    pub fn from_bytes(buf: &[u8]) -> Self {
        let mut e = Self {
            ota_seq: 0,
            seq_label: [0u8; 20],
            ota_state: OtaImageState::Undefined,
            crc: 0,
        };
        if buf.len() >= 32 {
            e.ota_seq = u32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]);
            e.seq_label.copy_from_slice(&buf[4..24]);
            e.ota_state =
                OtaImageState::from_u32(u32::from_le_bytes([buf[24], buf[25], buf[26], buf[27]]));
            e.crc = u32::from_le_bytes([buf[28], buf[29], buf[30], buf[31]]);
        }
        e
    }

    /// `seq_label` 作为字符串 (到第一个 NUL; 非 UTF-8 → 空串)
    pub fn label(&self) -> &str {
        let end = self.seq_label.iter().position(|&b| b == 0).unwrap_or(20);
        core::str::from_utf8(&self.seq_label[..end]).unwrap_or("")
    }
}

/// 从 otadata 分区 (≥8KB: 两条各占一扇区) 解析出两条
///
/// entry0 @ buf[0..32], entry1 @ buf[0x1000..0x1020] (对齐 IDF
/// `bootloader_common_read_otadata`: 偏移 0 与 `SPI_SEC_SIZE`)。
/// `buf` 不足 8KB 时缺失部分按未初始化处理。
pub fn read_otadata(buf: &[u8]) -> [OtaDataEntry; 2] {
    let e0 = OtaDataEntry::from_bytes(buf);
    let off = OTADATA_SECTOR_SIZE as usize;
    let e1 = if buf.len() >= off + OTADATA_ENTRY_SIZE {
        OtaDataEntry::from_bytes(&buf[off..off + OTADATA_ENTRY_SIZE])
    } else {
        OtaDataEntry::from_bytes(&[])
    };
    [e0, e1]
}

/// otadata 条目在分区内的字节偏移 (slot 0 → 0, slot 1 → 0x1000)
pub const fn otadata_entry_offset(slot: usize) -> u32 {
    slot as u32 * OTADATA_SECTOR_SIZE
}

/// otadata 条目的 CRC32: reflected CRC-32 (poly 0xEDB88320, init 0, xorout 0xFFFFFFFF)
/// 对 `ota_seq` 的 4 字节小端。真机/IDF 参考值: seq1→0x4743989A, seq2→0x55F63774,
/// seq3→0xED4A5011, seq=0xFFFFFFFF→0x2144DF1C。
pub fn ota_select_crc(ota_seq: u32) -> u32 {
    crc32_init_xorout(&ota_seq.to_le_bytes(), 0, 0xFFFF_FFFF)
}

/// 表驱动 reflected CRC-32, 给定 `init` / `xorout` (通用, 供 otadata 及他处复用)
pub fn crc32_init_xorout(data: &[u8], init: u32, xorout: u32) -> u32 {
    let mut c = init;
    for &b in data {
        c = (c >> 8) ^ CRC32_TABLE[((c ^ (b as u32)) & 0xFF) as usize];
    }
    c ^ xorout
}

const fn build_crc32_table() -> [u32; 256] {
    let mut t = [0u32; 256];
    let mut i = 0;
    while i < 256 {
        let mut c = i as u32;
        let mut k = 0;
        while k < 8 {
            c = if c & 1 != 0 {
                (c >> 1) ^ 0xEDB8_8320
            } else {
                c >> 1
            };
            k += 1;
        }
        t[i] = c;
        i += 1;
    }
    t
}

const CRC32_TABLE: [u32; 256] = build_crc32_table();

/// 活动序列号 = 两条中 `is_valid()` 且 `ota_seq` 最大者; 无有效条目 → `None`
///
/// 注意: 这里按"valid 且 crc 通过"筛选, 与二级 bootloader 的选择一致 (未初始化
/// 0xFF 的条目 crc 必然不符, 自然被排除)。
pub fn active_seq(entries: &[OtaDataEntry; 2]) -> Option<u32> {
    let mut best: Option<u32> = None;
    for e in entries.iter() {
        if e.is_valid() {
            best = Some(best.map_or(e.ota_seq, |b| b.max(e.ota_seq)));
        }
    }
    best
}

/// 由序列号推 app slot: `(seq - 1) % app_count` (`seq` 必须 ≥1)
pub fn slot_for_seq(seq: u32, app_count: usize) -> usize {
    // seq=0 只可能来自被构造/损坏的 otadata; 按 1 处理而不是下溢/调试 panic
    let seq = seq.max(1);
    ((seq - 1) % app_count as u32) as usize
}

/// 活动 app slot (0-based; 我们 0=ota_0, 1=ota_1); 无有效条目 → `None`
pub fn active_slot(entries: &[OtaDataEntry; 2], app_count: usize) -> Option<usize> {
    active_seq(entries).map(|s| slot_for_seq(s, app_count))
}

/// OTA 目标计算: 返回 `(新 seq, 写哪条 otadata 槽)`。
///
/// 对齐 IDF `esp_ota_end`: 新 seq = 活动 seq + 1 (无活动条目时从 1 起);
/// 目标槽 = `slot_for_seq(新 seq, app_count)` (双 app 乒乓: seq 奇→slot0,
/// 偶→slot1)。新条目直接置 VALID, 旧 valid 条目保留作回滚候选。
pub fn next_write(entries: &[OtaDataEntry; 2], app_count: usize) -> (u32, usize) {
    let active = active_seq(entries).unwrap_or(0);
    let new_seq = active.saturating_add(1);
    (new_seq, slot_for_seq(new_seq, app_count))
}

#[cfg(test)]
mod tests {
    extern crate std;
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
    fn parses_optional_sha256_hash() {
        const H: &str = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
        let body =
            std::format!(r#"{{"firmware":{{"version":"1.0.0","url":"http://x","sha256":"{H}"}}}}"#);
        assert_eq!(parse_manifest(&body).unwrap().sha256, Some(H));
        let body =
            std::format!(r#"{{"firmware":{{"version":"1.0.0","url":"http://x","hash":"{H}"}}}}"#);
        assert_eq!(parse_manifest(&body).unwrap().sha256, Some(H));
        assert_eq!(parse_manifest(MANIFEST).unwrap().sha256, None);
    }

    #[test]
    fn object_window_handles_braces_inside_strings() {
        let body = r#"{"firmware":{"version":"1.0.0","url":"http://x/a}b","note":"{\"k\":\"v\"}"},"mqtt":{"endpoint":"h:1","client_id":"c","username":"u","password":"p}q","publish_topic":"t","subscribe_topic":"s"}}"#;
        let info = parse_manifest(body).unwrap();
        assert_eq!(info.version, "1.0.0");
        assert_eq!(info.url, "http://x/a}b");
        let m = parse_mqtt_config(body).unwrap();
        assert_eq!(m.password, "p}q");
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
    fn mqtt_config_parsed_when_gateway_configured() {
        let body = r#"{
            "firmware": {"version": "1.0.0", "url": ""},
            "mqtt": {
                "endpoint": "192.168.123.14:1883",
                "client_id": "GID_gezipai@@@44_1B_F6_D9_58_B0@@@44_1B_F6_D9_58_B0",
                "username": "dXNlcg==",
                "password": "c2ln",
                "publish_topic": "device-server",
                "subscribe_topic": "devices/p2p/44_1B_F6_D9_58_B0"
            }
        }"#;
        let m = parse_mqtt_config(body).unwrap();
        assert_eq!(m.endpoint, "192.168.123.14:1883");
        assert_eq!(m.publish_topic, "device-server");
        assert_eq!(m.subscribe_topic, "devices/p2p/44_1B_F6_D9_58_B0");
        assert_eq!(m.password, "c2ln");
    }

    #[test]
    fn mqtt_config_absent_returns_none() {
        assert_eq!(parse_mqtt_config(MANIFEST), None);
        assert_eq!(parse_mqtt_config("not json"), None);
        // mqtt 对象缺少必要字段
        assert_eq!(parse_mqtt_config(r#"{"mqtt":{"endpoint":"h:1883"}}"#), None);
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

    // ===== otadata / CRC 参考值回归 =====

    #[test]
    fn ota_crc_matches_idf_reference_values() {
        // 真机/IDF otatool 参考 (reflected CRC-32, init 0, xorout 0xFFFFFFFF)
        assert_eq!(ota_select_crc(1), 0x4743_989A);
        assert_eq!(ota_select_crc(2), 0x55F6_3774);
        assert_eq!(ota_select_crc(3), 0xED4A_5011);
        assert_eq!(ota_select_crc(UNINITIALIZED_SEQUENCE), 0x2144_DF1C);
    }

    #[test]
    fn crc_generic_matches_zlib_for_known_vec() {
        // 标准 zlib CRC-32 (init 0xFFFFFFFF, xorout 0xFFFFFFFF) of "123456789" = 0xCBF43926
        assert_eq!(
            crc32_init_xorout(b"123456789", 0xFFFF_FFFF, 0xFFFF_FFFF),
            0xCBF4_3926
        );
        // 空输入: init ^ xorout
        assert_eq!(crc32_init_xorout(&[], 0, 0xFFFF_FFFF), 0xFFFF_FFFF);
        assert_eq!(crc32_init_xorout(&[], 0xFFFF_FFFF, 0xFFFF_FFFF), 0);
    }

    #[test]
    fn pending_entry_detected() {
        let e = OtaDataEntry::new(3, "1.1.0", OtaImageState::PendingVerify);
        assert!(e.is_pending());
        assert!(!e.is_valid());
        assert_eq!(e.label_str(), "1.1.0");
        let mut bad = e;
        bad.crc ^= 1;
        assert!(!bad.is_pending());
    }

    #[test]
    fn entry_encode_decode_roundtrip() {
        let e = OtaDataEntry::new(7, "1.2.3", OtaImageState::Valid);
        let b = e.to_bytes();
        assert_eq!(b.len(), 32);
        // 字段偏移: seq@0, label@4, state@24, crc@28
        assert_eq!(&b[0..4], &7u32.to_le_bytes());
        assert_eq!(&b[24..28], &2u32.to_le_bytes()); // Valid
        assert_eq!(&b[28..32], &e.crc.to_le_bytes());
        let d = OtaDataEntry::from_bytes(&b);
        assert_eq!(d, e);
        assert_eq!(d.label(), "1.2.3");
        assert!(d.is_valid());
    }

    #[test]
    fn entry_truncates_long_label_and_invalid_crc_detected() {
        let e = OtaDataEntry::new(1, "a-very-long-version-string-42", OtaImageState::Valid);
        assert_eq!(e.label().len(), 19); // 截断
        // 破坏 crc → is_valid 失败
        let mut bad = e;
        bad.crc ^= 0xDEAD;
        assert!(!bad.is_valid());
        // 状态非 Valid → is_valid 失败
        let pend = OtaDataEntry::new(1, "x", OtaImageState::PendingVerify);
        assert!(!pend.is_valid());
    }

    fn erased_otadata() -> [u8; 0x2000] {
        [0xFFu8; 0x2000]
    }

    #[test]
    fn read_otadata_erased_gives_uninitialized() {
        let [a, b] = read_otadata(&erased_otadata());
        assert_eq!(a.ota_seq, UNINITIALIZED_SEQUENCE);
        assert_eq!(a.ota_state, OtaImageState::Undefined);
        assert_eq!(b.ota_seq, UNINITIALIZED_SEQUENCE);
        assert!(!a.is_valid());
        assert_eq!(active_seq(&[a, b]), None);
    }

    #[test]
    fn read_otadata_short_buffer_tolerated() {
        // 不足 8KB: 只有 entry0 可读
        let mut buf = [0xFFu8; 64];
        let e0 = OtaDataEntry::new(1, "1.0.0", OtaImageState::Valid);
        buf[..32].copy_from_slice(&e0.to_bytes());
        let [a, b] = read_otadata(&buf);
        assert_eq!(a.ota_seq, 1);
        assert_eq!(b.ota_seq, 0); // 不足 8KB → 未初始化 (from_bytes(&[]))
        assert!(!b.is_valid());
    }

    #[test]
    fn active_seq_picks_max_valid() {
        // slot0 未初始化, slot1 @+0x1000 = seq5 valid
        let mut buf = erased_otadata();
        let e1 = OtaDataEntry::new(5, "2.0.0", OtaImageState::Valid);
        buf[0x1000..0x1020].copy_from_slice(&e1.to_bytes());
        let [a, b] = read_otadata(&buf);
        assert_eq!(active_seq(&[a, b]), Some(5));
        assert_eq!(active_slot(&[a, b], 2), Some(0)); // (5-1)%2=0 → ota_0
    }

    #[test]
    fn otadata_entry_offset_layout() {
        assert_eq!(otadata_entry_offset(0), 0);
        assert_eq!(otadata_entry_offset(1), 0x1000);
        assert_eq!(OTADATA_PARTITION_SIZE, 0x2000);
        assert_eq!(OTADATA_SECTOR_SIZE, 0x1000);
    }

    #[test]
    fn slot_for_seq_pingpong_two_slots() {
        assert_eq!(slot_for_seq(1, 2), 0); // ota_0
        assert_eq!(slot_for_seq(2, 2), 1); // ota_1
        assert_eq!(slot_for_seq(3, 2), 0); // ota_0
        assert_eq!(slot_for_seq(4, 2), 1); // ota_1
        assert_eq!(slot_for_seq(0, 2), 0); // 损坏的 seq=0 不下溢
    }

    #[test]
    fn next_write_pingpongs_and_bumps_seq() {
        // 起点: 仅 slot0 = seq1 valid (首次自愈后态), 活动=ota_0
        let mut buf = erased_otadata();
        buf[0..32].copy_from_slice(&OtaDataEntry::new(1, "1.0.0", OtaImageState::Valid).to_bytes());
        let [a, b] = read_otadata(&buf);

        // OTA #1: 活动 seq1 → 新 seq2, 目标 slot=(2-1)%2=1
        let (seq, slot) = next_write(&[a, b], 2);
        assert_eq!((seq, slot), (2, 1));

        // 模拟写入: slot1 @+0x1000 = seq2 valid
        let mut buf2 = buf;
        buf2[0x1000..0x1020]
            .copy_from_slice(&OtaDataEntry::new(2, "1.1.0", OtaImageState::Valid).to_bytes());
        let [c, d] = read_otadata(&buf2);
        assert_eq!(active_seq(&[c, d]), Some(2));
        assert_eq!(active_slot(&[c, d], 2), Some(1)); // 现在活动 ota_1

        // OTA #2: 活动 seq2 (slot1) → 新 seq3, 目标 slot=(3-1)%2=0
        let (seq2, slot2) = next_write(&[c, d], 2);
        assert_eq!((seq2, slot2), (3, 0));
        // 写后两条: slot0=seq3, slot1=seq2 → 活动 seq3 → (3-1)%2=0 → ota_0
        let mut buf3 = buf2;
        buf3[0..32]
            .copy_from_slice(&OtaDataEntry::new(3, "1.2.0", OtaImageState::Valid).to_bytes());
        let [e3, f3] = read_otadata(&buf3);
        assert_eq!(active_seq(&[e3, f3]), Some(3));
        assert_eq!(active_slot(&[e3, f3], 2), Some(0));
    }
}
