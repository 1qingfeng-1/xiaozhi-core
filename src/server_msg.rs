//! 服务器下行 JSON 消息分类 — 纯逻辑, host 可单测
//!
//! 对齐 C++ `application.cc` 的消息分发: `hello / tts / stt / llm / mcp /
//! alert / system / custom / goodbye`。本模块只做**分类与顶层字段提取**,
//! 不做业务副作用 (音频/显示由固件落实)。
//!
//! 解析用定向扫描而非完整 JSON 解析器: 固件只需要顶层字段, 且文本可能含
//! 转义引号 (`\"`), 扫描时跳过反斜杠即可。

/// 服务器下行消息
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServerMessage<'a> {
    /// 握手确认
    Hello,
    /// 服务器关闭音频通道 (MQTT)
    Goodbye,
    /// TTS 开始 (进入 Speaking)
    TtsStart,
    /// TTS 结束
    TtsStop,
    /// 当前朗读句子 (用于显示)
    TtsSentenceStart(&'a str),
    /// 用户语音识别结果 (用于显示)
    Stt(&'a str),
    /// 大模型回复: 情绪 (表情)
    Llm { emotion: &'a str },
    /// MCP JSON-RPC 请求
    Mcp,
    /// 服务器告警: 状态 + 文案 + 情绪
    Alert {
        status: &'a str,
        message: &'a str,
        emotion: &'a str,
    },
    /// 系统命令 (目前只有 `reboot`)
    System { command: &'a str },
    /// 服务器错误 (部分服务端实现会下发)
    Error { message: &'a str },
    /// 自定义消息 (固件按需显示)
    Custom,
    /// 未知/不完整消息
    Unknown(&'a str),
}

/// 解析一条服务器文本帧
///
/// 字段缺失时返回空串而不是 `None`: 例如 `tts sentence_start` 没带 text
/// 仍应按「句子开始」处理, 只是没有可显示的文本。
pub fn parse(text: &str) -> ServerMessage<'_> {
    let Some(ty) = field(text, "type") else {
        return ServerMessage::Unknown("");
    };
    match ty {
        "hello" => ServerMessage::Hello,
        "goodbye" => ServerMessage::Goodbye,
        "tts" => match field(text, "state") {
            Some("start") => ServerMessage::TtsStart,
            Some("stop") => ServerMessage::TtsStop,
            Some("sentence_start") => {
                ServerMessage::TtsSentenceStart(field(text, "text").unwrap_or(""))
            },
            _ => ServerMessage::Unknown(ty),
        },
        "stt" => ServerMessage::Stt(field(text, "text").unwrap_or("")),
        "llm" => ServerMessage::Llm {
            emotion: field(text, "emotion").unwrap_or(""),
        },
        "mcp" => ServerMessage::Mcp,
        "alert" => ServerMessage::Alert {
            status: field(text, "status").unwrap_or(""),
            message: field(text, "message").unwrap_or(""),
            emotion: field(text, "emotion").unwrap_or(""),
        },
        "system" => ServerMessage::System {
            command: field(text, "command").unwrap_or(""),
        },
        "error" => ServerMessage::Error {
            message: field(text, "message")
                .or_else(|| field(text, "text"))
                .unwrap_or(""),
        },
        "custom" => ServerMessage::Custom,
        other => ServerMessage::Unknown(other),
    }
}

/// 提取顶层 JSON 字符串字段 (`"key": "value"`)
///
/// - 只接受形如 `{ "key": ...` 或 `, "key": ...` 的位置, 避免误匹配到
///   其他字符串值内部的同名文本;
/// - 值内的 `\"` 不截断, 返回值保留转义原样 (只用于显示/日志)。
pub fn field<'a>(json: &'a str, key: &str) -> Option<&'a str> {
    let bytes = json.as_bytes();
    let key_len = key.len();
    let mut i = 0usize;
    while i + key_len + 2 <= bytes.len() {
        if bytes[i] == b'"' && is_key_start(bytes, i) {
            let start = i + 1;
            let end = start + key_len;
            if end < bytes.len()
                && &bytes[start..end] == key.as_bytes()
                && bytes[end] == b'"'
                && let Some(value) = value_after_key(json, end + 1)
            {
                return Some(value);
            }
        }
        i += 1;
    }
    None
}

/// 键名前必须是 `{` 或 `,` (中间允许空白)
fn is_key_start(bytes: &[u8], quote: usize) -> bool {
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

/// 跳过 `:` 与空白, 解析引号字符串值
fn value_after_key(json: &str, mut i: usize) -> Option<&str> {
    let bytes = json.as_bytes();
    while i < bytes.len() && bytes[i].is_ascii_whitespace() {
        i += 1;
    }
    if i >= bytes.len() || bytes[i] != b':' {
        return None;
    }
    i += 1;
    while i < bytes.len() && bytes[i].is_ascii_whitespace() {
        i += 1;
    }
    if i >= bytes.len() || bytes[i] != b'"' {
        return None;
    }
    let start = i + 1;
    i = start;
    while i < bytes.len() {
        match bytes[i] {
            b'\\' => i += 2,
            b'"' => return Some(&json[start..i]),
            _ => i += 1,
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_tts_states() {
        assert_eq!(
            parse(r#"{"type":"tts","state":"start"}"#),
            ServerMessage::TtsStart
        );
        assert_eq!(
            parse(r#"{"type":"tts","state":"stop"}"#),
            ServerMessage::TtsStop
        );
        assert_eq!(
            parse(r#"{"type":"tts","state":"sentence_start","text":"你好"}"#),
            ServerMessage::TtsSentenceStart("你好")
        );
        assert_eq!(
            parse(r#"{"type":"tts","state":"weird"}"#),
            ServerMessage::Unknown("tts")
        );
    }

    #[test]
    fn parses_stt_and_llm() {
        assert_eq!(
            parse(r#"{"session_id":"s","type":"stt","text":"今天天气"}"#),
            ServerMessage::Stt("今天天气")
        );
        assert_eq!(
            parse(r#"{"type":"llm","emotion":"happy","text":"😀"}"#),
            ServerMessage::Llm { emotion: "happy" }
        );
        // 字段缺失不丢弃消息
        assert_eq!(
            parse(r#"{"type":"llm"}"#),
            ServerMessage::Llm { emotion: "" }
        );
    }

    #[test]
    fn parses_alert_system_and_misc() {
        assert_eq!(
            parse(r#"{"type":"alert","status":"错误","message":"网络断开","emotion":"sad"}"#),
            ServerMessage::Alert {
                status: "错误",
                message: "网络断开",
                emotion: "sad",
            }
        );
        assert_eq!(
            parse(r#"{"type":"system","command":"reboot"}"#),
            ServerMessage::System { command: "reboot" }
        );
        assert_eq!(
            parse(r#"{"type":"hello","transport":"websocket"}"#),
            ServerMessage::Hello
        );
        assert_eq!(parse(r#"{"type":"goodbye"}"#), ServerMessage::Goodbye);
        assert_eq!(parse(r#"{"type":"mcp","payload":{}}"#), ServerMessage::Mcp);
        assert_eq!(
            parse(r#"{"type":"custom","payload":{}}"#),
            ServerMessage::Custom
        );
        assert_eq!(
            parse(r#"{"type":"error","message":"bad request"}"#),
            ServerMessage::Error {
                message: "bad request"
            }
        );
        assert_eq!(
            parse(r#"{"type":"srv_foo"}"#),
            ServerMessage::Unknown("srv_foo")
        );
        assert_eq!(parse("not json"), ServerMessage::Unknown(""));
    }

    #[test]
    fn escaped_quotes_do_not_end_value() {
        let msg = r#"{"type":"stt","text":"他说\"你好\"然后走了"}"#;
        assert_eq!(parse(msg), ServerMessage::Stt(r#"他说\"你好\"然后走了"#));
    }

    #[test]
    fn ignores_lookalike_text_inside_values() {
        // 值里出现 `"type":"stt"` 文本不应改变分类
        let msg = r#"{"type":"mcp","payload":{"note":"\"type\":\"stt\""}}"#;
        assert_eq!(parse(msg), ServerMessage::Mcp);
    }

    #[test]
    fn field_requires_key_position_and_colon() {
        assert_eq!(field(r#"{"a":"b"}"#, "a"), Some("b"));
        assert_eq!(field(r#"{ "a" : "b" }"#, "a"), Some("b"));
        // 作为值的一部分出现不算字段
        assert_eq!(field(r#"{"x":"\"a\":\"b\""}"#, "a"), None);
        // 缺冒号/缺引号
        assert_eq!(field(r#"{"a" "b"}"#, "a"), None);
        assert_eq!(field(r#"{"a":1}"#, "a"), None);
        assert_eq!(field(r#"{"a":"unterminated}"#, "a"), None);
    }
}
