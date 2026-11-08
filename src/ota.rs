//! OTA 升级 — 纯逻辑层 (host 单测)
//!
//! 三块:
//! 1. **清单解析**: 协议 (对齐 C++ ota.cc) POST http://<server>:8003/xiaozhi/ota/
//!    返回 `{"firmware":{"version":"1.2.3","url":"http://..."},"activation":{...},`
//!    `"server_time":{...},"websocket":{...}}`; `parse_manifest` 取固件字段及可选的
//!    force/server_time/websocket, `compare_versions` 语义化比较,
//!    `validate_image_header` 校验 e9 镜像头 (magic + app_desc → 版本)。
//! 2. **otadata 选择条目**: `OtaDataEntry` (32B, 对齐 IDF `esp_ota_select_entry_t`)
//!    的编解码 + `ota_select_crc` (reflected CRC-32, init 0, xorout 0xFFFFFFFF) +
//!    `active_seq`/`active_slot`/`next_write` (双 app 乒乓切换 + 回滚保留)。
//!    供固件烧写新镜像后写 otadata 分区, 二级 bootloader 据此选活动槽。

/// OTA 清单 — 固件更新信息 + 可选服务端时间/WebSocket 配置
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OtaManifest<'a> {
    pub version: &'a str,
    pub url: &'a str,
    /// 可选镜像 SHA-256 (服务端支持时下发; 兼容 `sha256` / `hash` 两种键)
    pub sha256: Option<&'a str>,
    /// `firmware.force == 1`: 强制升级 (字段缺失或为 0 → false)
    pub force: bool,
    /// `server_time` 对象 (缺失 → None, 不影响清单其余字段)
    pub server_time: Option<ServerTime>,
    /// `websocket` 对象 (缺失/不完整 → None, 不影响清单其余字段)
    pub websocket: Option<WebsocketConfig<'a>>,
}

/// 兼容旧名 (原 `parse_manifest` 返回类型)
pub type UpdateInfo<'a> = OtaManifest<'a>;

/// 服务端时间 (对齐 C++ ota.cc `server_time` 对象)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ServerTime {
    /// 原始 Unix 毫秒时间戳 (UTC 基准)
    pub timestamp_ms: u64,
    /// 时区偏移 (分钟, 可为负; 东八区 = 480)
    pub timezone_offset_min: i32,
}

impl ServerTime {
    /// 本地时间 (Unix 毫秒): `timestamp_ms + timezone_offset_min * 60_000`
    ///
    /// 对齐 C++ `ota.cc` 的 `ts += timezone_offset * 60 * 1000` 后再
    /// `settimeofday` 的语义。用 `i64` 避免负偏移/极端时间戳溢出,
    /// 时间戳超出 `i64` 时饱和到 `i64::MAX`。
    pub fn epoch_ms(&self) -> i64 {
        i64::try_from(self.timestamp_ms)
            .unwrap_or(i64::MAX)
            .saturating_add(i64::from(self.timezone_offset_min) * 60_000)
    }
}

/// 服务端下发的 WebSocket 连接参数
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WebsocketConfig<'a> {
    pub url: &'a str,
    /// 缺 token 时为 `""` (对齐 C++: 只是不加 Authorization 头)
    pub token: &'a str,
    /// 协议版本; 缺失/非法/0 时默认 1 (C++ `if (version != 0)`)
    pub version: u8,
}

/// 找到对象成员 `"key":` 的值起点 (冒号后, 跳过空白)
///
/// 只在根对象 (`depth == 1`) 的成员位置匹配, 并在扫描时跳过完整字符串字面量
/// (含 `\"` 转义), 避免把字符串值里的 `\"key\":` 文本或嵌套对象中的同名键
/// 误当成清单顶层字段。`json` 必须从根对象的 `{` 开始。
fn value_pos(json: &str, key: &str) -> Option<usize> {
    let bytes = json.as_bytes();
    let mut depth = 0usize;
    let mut i = 0usize;
    while i < bytes.len() {
        match bytes[i] {
            b'"' => {
                let end = string_end(bytes, i + 1)?;
                if let Some(pos) = key_value_pos(bytes, key, i, depth, end) {
                    return Some(pos);
                }
                i = end + 1;
            },
            b'{' | b'[' => {
                depth += 1;
                i += 1;
            },
            b'}' | b']' => {
                depth = depth.saturating_sub(1);
                i += 1;
            },
            _ => i += 1,
        }
    }
    None
}

/// 字符串字面量闭合引号下标 (从 `start` 开始, 跳过 `\"` 转义); 未闭合 → None
fn string_end(bytes: &[u8], start: usize) -> Option<usize> {
    let mut j = start;
    let mut esc = false;
    while j < bytes.len() {
        if esc {
            esc = false;
        } else if bytes[j] == b'\\' {
            esc = true;
        } else if bytes[j] == b'"' {
            return Some(j);
        }
        j += 1;
    }
    None
}

/// 若 `quote..end` 是根对象的成员键 `key`, 返回值起点 (冒号后)
fn key_value_pos(bytes: &[u8], key: &str, quote: usize, depth: usize, end: usize) -> Option<usize> {
    if depth != 1 || &bytes[quote + 1..end] != key.as_bytes() || !is_member_key(bytes, quote) {
        return None;
    }
    let k = skip_ws(bytes, end + 1);
    if k < bytes.len() && bytes[k] == b':' {
        Some(skip_ws(bytes, k + 1))
    } else {
        None
    }
}

/// 跳过空白, 返回下一个非空白字符下标
fn skip_ws(bytes: &[u8], mut i: usize) -> usize {
    while i < bytes.len() && bytes[i].is_ascii_whitespace() {
        i += 1;
    }
    i
}

/// 键名前必须是 `{` 或 `,` (中间允许空白), 排除值内文本
fn is_member_key(bytes: &[u8], quote: usize) -> bool {
    let mut i = quote;
    while i > 0 {
        i -= 1;
        match bytes[i] {
            b' ' | b'\t' | b'\r' | b'\n' => continue,
            b'{' | b',' => return true,
            _ => return false,
        }
    }
    false
}

/// 取根对象成员 `"key": { ... }` 的窗口 (含大括号), 正确处理字符串内 `{}`/转义
fn object_window<'a>(body: &'a str, key: &str) -> Option<&'a str> {
    let start = value_pos(body, key)?;
    let rest = &body[start..];
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

/// 取窗口内字符串成员 (值保留原样, 不解转义; 与 `server_msg::field` 一致)
fn str_field<'a>(json: &'a str, key: &str) -> Option<&'a str> {
    let rest = &json[value_pos(json, key)?..];
    let body = rest.strip_prefix('"')?;
    let bytes = body.as_bytes();
    let mut i = 0usize;
    let mut esc = false;
    while i < bytes.len() {
        if esc {
            esc = false;
        } else if bytes[i] == b'\\' {
            esc = true;
        } else if bytes[i] == b'"' {
            return Some(&body[..i]);
        }
        i += 1;
    }
    None
}

/// 把 JSON 字符串值反转义后写入定长缓冲
///
/// 支持 `\" \\ \/ \b \f \n \r \t \uXXXX`; 代理对/非法转义/容量不足返回
/// None。大多数服务端 (Python `json.dumps`) 不转义 `/`, 但 PHP 默认会输出
/// `ws:\/\/...`, 不解转义会让 URL 匹配失败并静默回退默认服务器。
pub fn unescape_json<const N: usize>(src: &str, out: &mut heapless::String<N>) -> Option<()> {
    out.clear();
    let mut it = src.chars();
    while let Some(c) = it.next() {
        if c != '\\' {
            out.push(c).ok()?;
            continue;
        }
        let e = it.next()?;
        let ch = match e {
            '"' => '"',
            '\\' => '\\',
            '/' => '/',
            'b' => '\u{8}',
            'f' => '\u{c}',
            'n' => '\n',
            'r' => '\r',
            't' => '\t',
            'u' => {
                let mut v = 0u32;
                for _ in 0..4 {
                    v = v.checked_mul(16)?.checked_add(it.next()?.to_digit(16)?)?;
                }
                // 代理对不在清单字段范围内, from_u32 对孤立代理返回 None
                char::from_u32(v)?
            },
            _ => return None,
        };
        out.push(ch).ok()?;
    }
    Some(())
}

/// 取窗口内数字成员 (返回数字字面量)
///
/// 只接受十进制整数或小数 (`-` 允许, `+` 拒绝); `1e2`/`1.7e12` 这类指数形式
/// 返回 None, 避免把指数误截断成尾数 (1e2 不能读成 1)。
fn num_field<'a>(json: &'a str, key: &str) -> Option<&'a str> {
    let rest = &json[value_pos(json, key)?..];
    let bytes = rest.as_bytes();
    let mut i = 0usize;
    if i < bytes.len() && bytes[i] == b'-' {
        i += 1;
    }
    let digits = i;
    while i < bytes.len() && bytes[i].is_ascii_digit() {
        i += 1;
    }
    if i == digits {
        return None;
    }
    // 小数部分: 保留字面量, 是否接受由 parse_whole 判定
    if i < bytes.len() && bytes[i] == b'.' {
        let mut j = i + 1;
        while j < bytes.len() && bytes[j].is_ascii_digit() {
            j += 1;
        }
        if j > i + 1 {
            i = j;
        }
    }
    if i < bytes.len() && (bytes[i] == b'e' || bytes[i] == b'E') {
        return None;
    }
    Some(&rest[..i])
}

/// 解析“整数值”数字: 允许 `1`/`1.0`/`-2`, 拒绝 `1.5`/`1e2` (失败 → None)
fn parse_whole<T: core::str::FromStr>(v: &str) -> Option<T> {
    let mut parts = v.splitn(2, '.');
    let int = parts.next()?;
    if let Some(frac) = parts.next()
        && !frac.bytes().all(|b| b == b'0')
    {
        return None;
    }
    int.parse().ok()
}

fn parse_server_time(body: &str) -> Option<ServerTime> {
    let window = object_window(body, "server_time")?;
    // timestamp 是毫秒 (u64), 缺失/负数/溢出/非整数 → 整个 server_time 视为缺失
    let timestamp_ms = num_field(window, "timestamp").and_then(parse_whole::<u64>)?;
    // 时区缺失 (或非数字) 按 0 处理, 对齐 C++ (不应用偏移)
    let timezone_offset_min = num_field(window, "timezone_offset")
        .and_then(parse_whole::<i32>)
        .unwrap_or(0);
    Some(ServerTime {
        timestamp_ms,
        timezone_offset_min,
    })
}

fn parse_websocket(body: &str) -> Option<WebsocketConfig<'_>> {
    let window = object_window(body, "websocket")?;
    Some(WebsocketConfig {
        // 缺 url 视为没有可用配置; 缺 token 仍可连接 (C++ 只是不加 Authorization)
        url: str_field(window, "url")?,
        token: str_field(window, "token").unwrap_or(""),
        // C++: version != 0 才覆盖默认; 缺失/0/非法 → 1
        version: num_field(window, "version")
            .and_then(parse_whole::<u8>)
            .filter(|&v| v != 0)
            .unwrap_or(1),
    })
}

/// 解析 OTA 清单, 提取 firmware.version/url/可选 sha256/force 及
/// 可选的 server_time / websocket (缺失返回 None, 不使整体解析失败)
///
/// 对齐 C++: `firmware`/`server_time`/`websocket` 三块独立解析。若响应没有
/// `firmware` (例如仅下发校时/WS 参数), 返回 `version == "" && url == ""` 的
/// 清单, 下游按空版本判定无更新; 若三块都没有则返回 None。
pub fn parse_manifest(body: &str) -> Option<OtaManifest<'_>> {
    let server_time = parse_server_time(body);
    let websocket = parse_websocket(body);
    let Some(window) = object_window(body, "firmware") else {
        let has_sections = server_time.is_some() || websocket.is_some();
        return if has_sections {
            Some(OtaManifest {
                version: "",
                url: "",
                sha256: None,
                force: false,
                server_time,
                websocket,
            })
        } else {
            None
        };
    };
    let version = str_field(window, "version")?;
    let url = str_field(window, "url")?;
    let sha256 = str_field(window, "sha256").or_else(|| str_field(window, "hash"));
    // 对齐 C++: 只有数值 1 才算强制 (字符串 "1"/1.5 都不算)
    let force = matches!(
        num_field(window, "force").and_then(parse_whole::<i64>),
        Some(1)
    );
    Some(OtaManifest {
        version,
        url,
        sha256,
        force,
        server_time,
        websocket,
    })
}

/// 解析激活码字段 (C++ 返回的 activation.code, 设备端目前仅日志)
pub fn parse_activation_code(body: &str) -> Option<&str> {
    str_field(object_window(body, "activation")?, "code")
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
        endpoint: str_field(window, "endpoint")?,
        client_id: str_field(window, "client_id")?,
        username: str_field(window, "username")?,
        password: str_field(window, "password").unwrap_or(""),
        publish_topic: str_field(window, "publish_topic")?,
        subscribe_topic: str_field(window, "subscribe_topic")?,
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
// 旧 valid 条目保留作回滚候选; 新条目写 NEW, 若引导器启用 APP_ROLLBACK
// 会在首次启动时转为 PENDING_VERIFY; 应用连通 WiFi 后由
// confirm_running_image 置 VALID。

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

    /// 可启动条目 = 擦除态排除 + crc 匹配 + 状态不是 Invalid/Aborted。
    ///
    /// 对齐 ESP-IDF v5.5.2 `bootloader_common_loader.c`: NEW/PENDING_VERIFY/
    /// VALID 都视为可启动。用于 [`active_seq`] 判断"当前实际运行的槽",
    /// 避免确认失败 (仍为 PENDING_VERIFY) 时把镜像写进正在运行的槽。
    pub fn is_bootable(&self) -> bool {
        self.ota_seq != u32::MAX
            && self.crc == ota_select_crc(self.ota_seq)
            && !matches!(
                self.ota_state,
                OtaImageState::Invalid | OtaImageState::Aborted
            )
    }

    /// 待确认 = 状态为 PendingVerify 且 crc 匹配 (回滚引导器首次启动后)
    pub fn is_pending(&self) -> bool {
        self.ota_state == OtaImageState::PendingVerify && self.crc == ota_select_crc(self.ota_seq)
    }

    /// 需要应用确认 = New（未启用回滚引导器）或 PendingVerify（已启用）
    /// 且 crc 匹配。ota 写入方统一写 `New`，由引导器按配置转为
    /// `PendingVerify`；应用确认后写 `Valid`。
    pub fn needs_confirm(&self) -> bool {
        matches!(
            self.ota_state,
            OtaImageState::New | OtaImageState::PendingVerify
        ) && self.crc == ota_select_crc(self.ota_seq)
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

/// 活动序列号 = 两条中 `is_bootable()` 且 `ota_seq` 最大者; 无可启动条目 → `None`
///
/// 与二级 bootloader 的判定一致: NEW/PENDING_VERIFY/VALID 都算可启动。
/// 若只认 Valid, 运行条目仍是 PENDING_VERIFY (确认失败) 时会回落到另一槽,
/// `next_write` 可能把镜像写进**正在运行**的槽。
pub fn active_seq(entries: &[OtaDataEntry; 2]) -> Option<u32> {
    let mut best: Option<u32> = None;
    for e in entries.iter() {
        if e.is_bootable() {
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
/// 偶→slot1)。新条目写 NEW (回滚引导器首启转 PENDING_VERIFY, 应用确认后
/// 转 VALID), 旧 valid 条目保留作回滚候选。
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
    fn parses_force_flag() {
        let one = r#"{"firmware":{"version":"1.0.0","url":"http://x","force":1}}"#;
        assert!(parse_manifest(one).unwrap().force);

        let zero = r#"{"firmware":{"version":"1.0.0","url":"http://x","force":0}}"#;
        assert!(!parse_manifest(zero).unwrap().force);

        // 缺失 → false (现有 MANIFEST 亦覆盖)
        assert!(!parse_manifest(MANIFEST).unwrap().force);

        // 对齐 C++ cJSON_IsNumber: 字符串 "1"/其它数值都不算强制
        let quoted = r#"{"firmware":{"version":"1.0.0","url":"http://x","force":"1"}}"#;
        assert!(!parse_manifest(quoted).unwrap().force);
        let other = r#"{"firmware":{"version":"1.0.0","url":"http://x","force":2}}"#;
        assert!(!parse_manifest(other).unwrap().force);

        // 1.0 是整数值; 1.5 / 1e2 不是 1 (不能静默截断)
        let whole = r#"{"firmware":{"version":"1.0.0","url":"http://x","force":1.0}}"#;
        assert!(parse_manifest(whole).unwrap().force);
        let fraction = r#"{"firmware":{"version":"1.0.0","url":"http://x","force":1.5}}"#;
        assert!(!parse_manifest(fraction).unwrap().force);
        let exp = r#"{"firmware":{"version":"1.0.0","url":"http://x","force":1e2}}"#;
        assert!(!parse_manifest(exp).unwrap().force);
        let plus = r#"{"firmware":{"version":"1.0.0","url":"http://x","force":+1}}"#;
        assert!(!parse_manifest(plus).unwrap().force);
    }

    #[test]
    fn parses_server_time_timezone() {
        // 毫秒时间戳 (超出 u32) + 东八区
        let east = r#"{"firmware":{"version":"1.0.0","url":"http://x"},"server_time":{"timestamp":1700000000123,"timezone_offset":480}}"#;
        let st = parse_manifest(east).unwrap().server_time.unwrap();
        assert_eq!(st.timestamp_ms, 1_700_000_000_123);
        assert_eq!(st.timezone_offset_min, 480);
        // 对齐 C++: 本地时间 = timestamp + offset*60000
        assert_eq!(st.epoch_ms(), 1_700_028_800_123);

        // 负时区
        let west = r#"{"firmware":{"version":"1.0.0","url":"http://x"},"server_time":{"timestamp":1700000000123,"timezone_offset":-300}}"#;
        let st = parse_manifest(west).unwrap().server_time.unwrap();
        assert_eq!(st.timezone_offset_min, -300);
        assert_eq!(st.epoch_ms(), 1_699_982_000_123);

        // timezone_offset 缺失 → 0 (对齐 C++, 不应用偏移)
        let no_tz =
            r#"{"firmware":{"version":"1.0.0","url":"http://x"},"server_time":{"timestamp":42}}"#;
        let st = parse_manifest(no_tz).unwrap().server_time.unwrap();
        assert_eq!((st.timestamp_ms, st.timezone_offset_min), (42, 0));
        assert_eq!(st.epoch_ms(), 42);
    }

    #[test]
    fn server_time_missing_or_invalid_is_none() {
        // 整个 server_time 缺失
        assert_eq!(parse_manifest(MANIFEST).unwrap().server_time, None);
        // 有对象但缺 timestamp → None, 清单其余字段仍可用
        let no_ts = r#"{"firmware":{"version":"1.0.0","url":"http://x"},"server_time":{"timezone_offset":480}}"#;
        let m = parse_manifest(no_ts).unwrap();
        assert_eq!(m.server_time, None);
        assert_eq!(m.version, "1.0.0");
        // timestamp 非数字 / 负数 → None
        let bad = r#"{"firmware":{"version":"1.0.0","url":"http://x"},"server_time":{"timestamp":"1700000000123","timezone_offset":480}}"#;
        assert_eq!(parse_manifest(bad).unwrap().server_time, None);
        let neg =
            r#"{"firmware":{"version":"1.0.0","url":"http://x"},"server_time":{"timestamp":-1}}"#;
        assert_eq!(parse_manifest(neg).unwrap().server_time, None);
        // 指数形式 (1.7e12) 必须视为非法, 不能截断成 1
        let exp = r#"{"firmware":{"version":"1.0.0","url":"http://x"},"server_time":{"timestamp":1.7e12}}"#;
        assert_eq!(parse_manifest(exp).unwrap().server_time, None);
        // 非零小数同样不接受
        let frac =
            r#"{"firmware":{"version":"1.0.0","url":"http://x"},"server_time":{"timestamp":1.5}}"#;
        assert_eq!(parse_manifest(frac).unwrap().server_time, None);
        // + 前缀非法
        let plus =
            r#"{"firmware":{"version":"1.0.0","url":"http://x"},"server_time":{"timestamp":+42}}"#;
        assert_eq!(parse_manifest(plus).unwrap().server_time, None);
    }

    #[test]
    fn parses_websocket_config() {
        let full = r#"{"firmware":{"version":"1.0.0","url":"http://x"},"websocket":{"url":"ws://192.168.1.7:8000/xiaozhi/v1/","token":"secret","version":2}}"#;
        let ws = parse_manifest(full).unwrap().websocket.unwrap();
        assert_eq!(ws.url, "ws://192.168.1.7:8000/xiaozhi/v1/");
        assert_eq!(ws.token, "secret");
        assert_eq!(ws.version, 2);

        // version 缺失 → 默认 1
        let no_ver = r#"{"firmware":{"version":"1.0.0","url":"http://x"},"websocket":{"url":"ws://h/","token":"t"}}"#;
        assert_eq!(
            parse_manifest(no_ver).unwrap().websocket.unwrap().version,
            1
        );

        // 对齐 C++ `if (version != 0)`: 0 / 越界 / 非整数 → 回退 1
        for v in ["0", "256", "-1", "1.5", "1e2"] {
            let body = std::format!(
                r#"{{"firmware":{{"version":"1.0.0","url":"http://x"}},"websocket":{{"url":"ws://h/","version":{v}}}}}"#
            );
            assert_eq!(
                parse_manifest(&body).unwrap().websocket.unwrap().version,
                1,
                "version={v} 应回退 1"
            );
        }
    }

    #[test]
    fn websocket_missing_sections_do_not_fail_manifest() {
        // 无 websocket 对象
        assert_eq!(parse_manifest(MANIFEST).unwrap().websocket, None);
        // 缺 token → token 为空串, url 仍有效 (C++ 只是不加 Authorization)
        let no_token =
            r#"{"firmware":{"version":"1.0.0","url":"http://x"},"websocket":{"url":"ws://h/"}}"#;
        let m = parse_manifest(no_token).unwrap();
        let ws = m.websocket.unwrap();
        assert_eq!((ws.url, ws.token, ws.version), ("ws://h/", "", 1));
        assert_eq!(m.url, "http://x");
        // 缺 url → 视为无可用 websocket 配置
        let no_url =
            r#"{"firmware":{"version":"1.0.0","url":"http://x"},"websocket":{"token":"t"}}"#;
        assert_eq!(parse_manifest(no_url).unwrap().websocket, None);
        // websocket 不是对象 → None
        let not_obj = r#"{"firmware":{"version":"1.0.0","url":"http://x"},"websocket":"off"}"#;
        assert_eq!(parse_manifest(not_obj).unwrap().websocket, None);
    }

    #[test]
    fn manifest_key_scanning_skips_string_contents() {
        // 字符串值里伪造的 "firmware"/"version" 不能被当成真实字段
        let fake_top = r#"{"note":"evil,\"firmware\":{\"version\":\"9.9.9\",\"url\":\"evil\"}","firmware":{"version":"1.0.0","url":"http://good"}}"#;
        let m = parse_manifest(fake_top).unwrap();
        assert_eq!((m.version, m.url), ("1.0.0", "http://good"));

        // firmware 对象内字符串值里的 "version" 同理
        let fake_inner = r#"{"firmware":{"note":"x, \"version\":\"9.9.9\"","version":"1.0.0","url":"http://good"}}"#;
        assert_eq!(parse_manifest(fake_inner).unwrap().version, "1.0.0");
    }

    #[test]
    fn manifest_without_firmware_parses_optional_sections() {
        // 对齐 C++: 三块独立解析, 仅下发校时/WS 参数也应成功
        let body = r#"{"server_time":{"timestamp":1700000000123,"timezone_offset":480},"websocket":{"url":"ws://h/","token":"t"}}"#;
        let m = parse_manifest(body).unwrap();
        assert_eq!((m.version, m.url), ("", ""));
        assert_eq!((m.sha256, m.force), (None, false));
        assert_eq!(m.server_time.unwrap().timestamp_ms, 1_700_000_000_123);
        assert_eq!(m.websocket.unwrap().url, "ws://h/");

        // 仅 websocket 也可
        let only_ws = r#"{"websocket":{"url":"ws://h/"}}"#;
        assert!(parse_manifest(only_ws).unwrap().websocket.is_some());

        // 没有任何已知 section → None (保持严格)
        assert_eq!(parse_manifest(r#"{"activation":{"code":"123"}}"#), None);
        assert_eq!(parse_manifest("{}"), None);
        assert_eq!(parse_manifest("not json"), None);
    }

    #[test]
    fn manifest_nested_duplicate_and_escaped_values() {
        // 嵌套对象中的同名 key 不算顶层
        let nested = r#"{"wrapper":{"firmware":{"version":"9.9.9","url":"evil"},"server_time":{"timestamp":7}},"firmware":{"version":"1.0.0","url":"http://good"}}"#;
        let m = parse_manifest(nested).unwrap();
        assert_eq!((m.version, m.url), ("1.0.0", "http://good"));
        assert_eq!(m.server_time, None);

        // 重复 key 取首个成员 (对齐 cJSON_GetObjectItem)
        let dup = r#"{"firmware":{"version":"1.0.0","url":"http://first","version":"2.0.0","url":"http://second"}}"#;
        let m = parse_manifest(dup).unwrap();
        assert_eq!((m.version, m.url), ("1.0.0", "http://first"));

        // 转义斜杠不截断值; 零拷贝契约下保留原始转义形式 (`\/` 不解码)
        let escaped = r#"{"firmware":{"version":"1.0.0","url":"http://x"},"websocket":{"url":"ws:\/\/h","token":"t"}}"#;
        assert_eq!(
            parse_manifest(escaped).unwrap().websocket.unwrap().url,
            r"ws:\/\/h"
        );
    }

    #[test]
    fn mqtt_config_ignores_nested_same_name_keys() {
        let body = r#"{"wrapper":{"mqtt":{"endpoint":"evil:1","client_id":"e","username":"e","publish_topic":"e","subscribe_topic":"e"}},"mqtt":{"endpoint":"good:1883","client_id":"c","username":"u","publish_topic":"p","subscribe_topic":"s"}}"#;
        let m = parse_mqtt_config(body).unwrap();
        assert_eq!(m.endpoint, "good:1883");
        assert_eq!(m.client_id, "c");
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
    fn new_and_pending_both_need_confirmation() {
        let new = OtaDataEntry::new(2, "1.0.1", OtaImageState::New);
        assert!(new.needs_confirm());
        assert!(
            !new.is_pending(),
            "New 是写入态, 不是引导器转换后的 PendingVerify"
        );
        assert!(!new.is_valid());

        let pending = OtaDataEntry::new(2, "1.0.1", OtaImageState::PendingVerify);
        assert!(pending.needs_confirm());

        let valid = OtaDataEntry::new(2, "1.0.1", OtaImageState::Valid);
        assert!(!valid.needs_confirm());

        let mut bad = new;
        bad.crc ^= 1;
        assert!(!bad.needs_confirm());
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
    fn active_seq_counts_pending_verify_as_running() {
        // 运行中的新镜像 seq2 仍是 PENDING_VERIFY (确认失败), 另一槽 seq1 VALID。
        // 引导器把 PENDING_VERIFY 视为可启动: active 必须是 seq2, 否则
        // next_write 会把镜像写进正在运行的 slot1。
        let mut buf = erased_otadata();
        let e0 = OtaDataEntry::new(1, "1.0.0", OtaImageState::Valid);
        let e1 = OtaDataEntry::new(2, "2.0.0", OtaImageState::PendingVerify);
        buf[0..32].copy_from_slice(&e0.to_bytes());
        buf[0x1000..0x1020].copy_from_slice(&e1.to_bytes());
        let [a, b] = read_otadata(&buf);
        assert_eq!(active_seq(&[a, b]), Some(2));
        assert_eq!(active_slot(&[a, b], 2), Some(1));
        assert_eq!(next_write(&[a, b], 2), (3, 0)); // 目标 ota_0, 非运行中的 ota_1

        // ABORTED 条目不可启动: active 回落到 VALID 的 seq3
        let mut buf2 = erased_otadata();
        let e0 = OtaDataEntry::new(4, "3.0.0", OtaImageState::Aborted);
        let e1 = OtaDataEntry::new(3, "2.0.0", OtaImageState::Valid);
        buf2[0..32].copy_from_slice(&e0.to_bytes());
        buf2[0x1000..0x1020].copy_from_slice(&e1.to_bytes());
        let [a, b] = read_otadata(&buf2);
        assert_eq!(active_seq(&[a, b]), Some(3));
    }

    #[test]
    fn unescape_json_handles_escaped_slash_and_unicode() {
        let mut out = heapless::String::<64>::new();
        assert!(unescape_json("ws:\\/\\/192.168.1.7:8000\\/xiaozhi", &mut out).is_some());
        assert_eq!(out.as_str(), "ws://192.168.1.7:8000/xiaozhi");

        assert!(unescape_json("token\\u0031\\u0032\\\"x\\\\y", &mut out).is_some());
        assert_eq!(out.as_str(), "token12\"x\\y");

        // 非法转义 / 孤立代理 / 容量不足 → None
        assert!(unescape_json("a\\q", &mut out).is_none());
        assert!(unescape_json("\\uD83D", &mut out).is_none());
        let mut tiny = heapless::String::<2>::new();
        assert!(unescape_json("abcd", &mut tiny).is_none());
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
