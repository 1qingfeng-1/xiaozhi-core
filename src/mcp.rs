//! MCP 设备控制 — JSON-RPC 2.0 消息解析/构造 (xiaozhi "mcp" 包裹层)
//!
//! 协议 (对齐 C++ mcp_server.cc):
//! - 服务器文本帧: `{"type":"mcp","payload":{jsonrpc 请求}}`
//! - 设备回应:     `{"type":"mcp","payload":{"jsonrpc":"2.0","id":N,"result":...}}`
//! - method: `initialize` / `tools/list` / `tools/call`; `notifications/*` 忽略
//!
//! 纯逻辑, heapless 缓冲, host 可单测。解析用定向扫描 (字段位置固定),
//! 不引入 serde。

/// 待发响应上限 (tools/list 描述较长)
const RESP_MAX: usize = 1024;

/// 解析出的 JSON-RPC 调用
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum McpMethod<'a> {
    /// initialize 握手
    Initialize,
    /// tools/list 枚举
    ToolsList,
    /// tools/call 调用工具
    ToolsCall {
        /// 工具名 (params.name)
        name: &'a str,
        /// params.arguments.volume (set_volume 用; 其余工具忽略)
        volume: Option<i32>,
    },
    /// 未实现的方法 (固件回 error 帧)
    Unknown(&'a str),
}

/// 解析结果: notifications 无响应, 解析失败返回 None
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct McpRequest<'a> {
    pub id: i64,
    pub method: McpMethod<'a>,
}

/// 解析 jsonrpc 请求载荷 (可传入整个 "mcp" 文本帧, 字段扫描与嵌套无关)
pub fn parse_request(payload: &str) -> Option<McpRequest<'_>> {
    // notifications/* 无需响应
    let method_str = json_str(payload, "method")?;
    if method_str.starts_with("notifications") {
        return None;
    }
    let id = json_num(payload, "id")?;
    let method = match method_str {
        "initialize" => McpMethod::Initialize,
        "tools/list" => McpMethod::ToolsList,
        "tools/call" => {
            let name = json_str(payload, "name")?;
            let volume = json_num(payload, "volume").map(|v| v as i32);
            McpMethod::ToolsCall { name, volume }
        },
        // 未实现的方法: 保留名字供固件回错误帧
        other => McpMethod::Unknown(other),
    };
    Some(McpRequest { id, method })
}

/// 错误响应包装 (未实现的方法用)
pub fn error_response(id: i64, message: &str) -> heapless::String<RESP_MAX> {
    let mut s = heapless::String::<RESP_MAX>::new();
    let _ = core::fmt::write(
        &mut s,
        format_args!(
            "{{\"type\":\"mcp\",\"payload\":{{\"jsonrpc\":\"2.0\",\"id\":{},\"error\":{{\"message\":\"{}\"}}}}}}",
            id, message
        ),
    );
    s
}

/// 成功响应包装
fn result_response(id: i64, result: &str) -> heapless::String<RESP_MAX> {
    let mut s = heapless::String::<RESP_MAX>::new();
    let _ = core::fmt::write(
        &mut s,
        format_args!(
            "{{\"type\":\"mcp\",\"payload\":{{\"jsonrpc\":\"2.0\",\"id\":{},\"result\":{}}}}}",
            id, result
        ),
    );
    s
}

/// initialize 响应
pub fn initialize_response(id: i64, board_name: &str, version: &str) -> heapless::String<RESP_MAX> {
    let mut result = heapless::String::<256>::new();
    let _ = core::fmt::write(
        &mut result,
        format_args!(
            "{{\"protocolVersion\":\"2024-11-05\",\"capabilities\":{{\"tools\":{{}}}},\"serverInfo\":{{\"name\":\"{}\",\"version\":\"{}\"}}}}",
            board_name, version
        ),
    );
    result_response(id, result.as_str())
}

/// tools/list 响应 — 本设备支持的工具集
pub fn tools_list_response(id: i64) -> heapless::String<RESP_MAX> {
    const TOOLS: &str = concat!(
        r#"{"tools":["#,
        r#"{"name":"self.get_device_status","#,
        r#""description":"Get device status: speaker volume, battery level and charging state, network.","#,
        r#""inputSchema":{"type":"object","properties":{}}},"#,
        r#"{"name":"self.audio_speaker.set_volume","#,
        r#""description":"Set the speaker volume (0-100). Call self.get_device_status first if volume is unknown.","#,
        r#""inputSchema":{"type":"object","properties":{"volume":{"type":"integer","minimum":0,"maximum":100}},"required":["volume"]}}"#,
        r#"]}"#,
    );
    result_response(id, TOOLS)
}

/// tools/call 结果包装: `{"content":[{"type":"text","text":...}]}`
pub fn text_result_response(id: i64, text: &str) -> heapless::String<RESP_MAX> {
    let mut result = heapless::String::<256>::new();
    let _ = core::fmt::write(
        &mut result,
        format_args!(
            "{{\"content\":[{{\"type\":\"text\",\"text\":\"{}\"}}]}}",
            text
        ),
    );
    result_response(id, result.as_str())
}

/// 设备状态 JSON (get_device_status 返回值, 形状对齐 C++ WifiBoard)
pub fn status_json(volume: u8, battery_pct: Option<u8>, charging: bool) -> heapless::String<256> {
    // battery: None (未读到有效电压) → level 用 -1 表达未知
    let level = battery_pct.map(|v| v as i32).unwrap_or(-1);
    let mut s = heapless::String::<256>::new();
    let _ = core::fmt::write(
        &mut s,
        format_args!(
            "{{\"audio_speaker\":{{\"volume\":{}}},\"battery\":{{\"level\":{},\"charging\":{}}}}}",
            volume,
            level,
            if charging { "true" } else { "false" }
        ),
    );
    s
}

/// 定位 `"key":` 字面模式, 返回其后的文本片段 (值区)
///
/// 注: xiaozhi 服务器的 JSON 为无空格 minified (cJSON_PrintUnformatted),
/// 故按 `"key":` 精确匹配即可; 多次出现取第一个能解析出值的位置。
fn find_value<'a>(payload: &'a str, key: &str) -> Option<&'a str> {
    // 构造 "key": 字面模式 (core 无 alloc, 用 heapless 拼接)
    let mut pat = heapless::String::<48>::new();
    pat.push('"').ok()?;
    pat.push_str(key).ok()?;
    pat.push('"').ok()?;
    pat.push(':').ok()?;
    let at = payload.find(pat.as_str())?;
    let v = &payload[at + pat.len()..];
    Some(v.trim_start())
}

/// 在 JSON 文本中查找 `"key":` 后的数字
pub fn json_num(payload: &str, key: &str) -> Option<i64> {
    let after = find_value(payload, key)?;
    let digits = after
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == '-')
        .count();
    if digits > 0 {
        after[..digits].parse().ok()
    } else {
        None
    }
}

/// 在 JSON 文本中查找 `"key":"字符串"` 的值
pub fn json_str<'a>(payload: &'a str, key: &str) -> Option<&'a str> {
    let v = find_value(payload, key)?.strip_prefix('"')?;
    let end = v.find('"')?;
    Some(&v[..end])
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;

    #[test]
    fn parses_initialize() {
        let payload =
            r#"{"jsonrpc":"2.0","id":7,"method":"initialize","params":{"capabilities":{}}}"#;
        let r = parse_request(payload).unwrap();
        assert_eq!(r.id, 7);
        assert_eq!(r.method, McpMethod::Initialize);
    }

    #[test]
    fn parses_tools_call_with_volume() {
        let payload = r#"{"jsonrpc":"2.0","id":12,"method":"tools/call","params":{"name":"self.audio_speaker.set_volume","arguments":{"volume":45}}}"#;
        let r = parse_request(payload).unwrap();
        assert_eq!(r.id, 12);
        match r.method {
            McpMethod::ToolsCall { name, volume } => {
                assert_eq!(name, "self.audio_speaker.set_volume");
                assert_eq!(volume, Some(45));
            },
            _ => std::panic!("expected ToolsCall"),
        }
    }

    #[test]
    fn notifications_ignored() {
        let payload = r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#;
        assert_eq!(parse_request(payload), None);
    }

    #[test]
    fn malformed_returns_none() {
        assert_eq!(parse_request(r#"{"jsonrpc":"2.0"}"#), None);
        assert_eq!(
            parse_request(r#"{"jsonrpc":"2.0","id":1,"method":"tools/call"}"#),
            None
        );
    }

    #[test]
    fn wraps_result_and_error() {
        let r = result_response(3, "{\"ok\":true}");
        assert!(r.contains(r#""id":3,"result":{"ok":true}"#));
        let e = error_response(4, "boom");
        assert!(e.contains(r#""error":{"message":"boom"}"#));
    }

    #[test]
    fn initialize_and_tools_list_shapes() {
        let r = initialize_response(1, "gezipai", "0.1.0");
        assert!(r.contains(r#""protocolVersion":"2024-11-05""#));
        assert!(r.contains(r#""name":"gezipai""#));
        let t = tools_list_response(2);
        assert!(t.contains("self.get_device_status"));
        assert!(t.contains("self.audio_speaker.set_volume"));
        assert!(t.contains(r#""required":["volume"]"#));
    }

    #[test]
    fn text_and_status_results() {
        let r = text_result_response(5, "true");
        assert!(r.contains(r#""content":[{"type":"text","text":"true"}]"#));
        let s = status_json(70, Some(50), false);
        assert!(s.contains(r#""volume":70"#));
        assert!(s.contains(r#""level":50"#));
        assert!(s.contains(r#""charging":false"#));
        let unknown = status_json(10, None, true);
        assert!(unknown.contains(r#""level":-1"#));
        assert!(unknown.contains(r#""charging":true"#));
    }

    #[test]
    fn json_helpers_scan_nested() {
        // 传整帧也能解析 (嵌套字段)
        let frame = r#"{"type":"mcp","payload":{"jsonrpc":"2.0","id":9,"method":"tools/call","params":{"name":"self.get_device_status"}}}"#;
        assert_eq!(json_num(frame, "id"), Some(9));
        assert_eq!(json_str(frame, "name"), Some("self.get_device_status"));
        // get_device_status 无 volume 参数
        let r = parse_request(frame).unwrap();
        match r.method {
            McpMethod::ToolsCall { name, volume } => {
                assert_eq!(name, "self.get_device_status");
                assert_eq!(volume, None);
            },
            _ => std::panic!("expected ToolsCall"),
        }
    }
}
