//! 极简 JSON 字段扫描 — 用于 WS/MQTT 控制消息
//!
//! xiaozhi 的服务端消息是受控格式, 完整 JSON 解析器代价过大。这里只做
//! 「定位 `"key":` 后的第一个值」:
//! - 支持任意空白 (`"key": 123` / `"key" : "x"`);
//! - 支持嵌套 (UDP 参数在 `udp` 对象内, 需要能跨层找到);
//! - **不处理字符串内的转义/花括号**, 因此只应用于 type/state/session_id/
//!   server/port/key/nonce 这类受控字段 (展示/日志字段请用 `server_msg`)。

/// 构造 `"key"` 模式串 (栈上, 避免每条消息一次堆分配)
fn key_pattern(key: &str) -> Option<heapless::String<48>> {
    let mut pat = heapless::String::<48>::new();
    pat.push('"').ok()?;
    pat.push_str(key).ok()?;
    pat.push('"').ok()?;
    Some(pat)
}

/// 找到 `"key"` 后跳过空白与冒号, 返回值的起始位置
fn value_start<'a>(json: &'a str, key: &str) -> Option<&'a str> {
    let pat = key_pattern(key)?;
    let pat = pat.as_str();
    let mut rest = json;
    while let Some(i) = rest.find(pat) {
        let after_key = &rest[i + pat.len()..];
        if let Some(after_colon) = after_key.trim_start().strip_prefix(':') {
            return Some(after_colon.trim_start());
        }
        rest = after_key;
    }
    None
}

/// 抽取 `"key": <数字>` 字段值
pub fn num_field(json: &str, key: &str) -> Option<u32> {
    let after = value_start(json, key)?;
    let digits = after.chars().take_while(|c| c.is_ascii_digit()).count();
    if digits == 0 {
        return None;
    }
    after[..digits].parse().ok()
}

/// 抽取 `"key": "字符串"` 字段值 (取第一个匹配)
pub fn str_field<'a>(json: &'a str, key: &str) -> Option<&'a str> {
    let after = value_start(json, key)?;
    after
        .strip_prefix('"')
        .and_then(|v| v.split_once('"').map(|(s, _)| s))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn num_field_finds_audio_params() {
        let hello = r#"{"type": "hello", "audio_params": {"format": "opus", "sample_rate": 16000, "frame_duration": 60}}"#;
        assert_eq!(num_field(hello, "sample_rate"), Some(16000));
        assert_eq!(num_field(hello, "frame_duration"), Some(60));
        assert_eq!(num_field(hello, "missing"), None);
    }

    #[test]
    fn num_field_takes_first_match() {
        let s = r#"{"a":1,"b":{"a":2}}"#;
        assert_eq!(num_field(s, "a"), Some(1));
    }

    #[test]
    fn str_field_extracts_value() {
        let msg = r#"{"type":"tts","state":"start","session_id":"abc"}"#;
        assert_eq!(str_field(msg, "type"), Some("tts"));
        assert_eq!(str_field(msg, "state"), Some("start"));
        assert_eq!(str_field(msg, "session_id"), Some("abc"));
        assert_eq!(str_field(msg, "nope"), None);
    }

    #[test]
    fn nested_udp_fields_are_found() {
        let hello = r#"{"type":"hello","transport":"udp","udp":{"server":"192.168.1.7","port":8884,"key":"00112233445566778899aabbccddeeff","nonce":"01000000000000000000000000000000"}}"#;
        assert_eq!(str_field(hello, "server"), Some("192.168.1.7"));
        assert_eq!(num_field(hello, "port"), Some(8884));
        assert_eq!(
            str_field(hello, "key"),
            Some("00112233445566778899aabbccddeeff")
        );
    }

    #[test]
    fn whitespace_and_missing_values() {
        assert_eq!(str_field(r#"{ "type" : "hello" }"#, "type"), Some("hello"));
        assert_eq!(num_field(r#"{ "port" :  8884 }"#, "port"), Some(8884));
        // 键存在但值不是数字/字符串: 继续找下一条, 最终 None
        assert_eq!(num_field(r#"{"port":null}"#, "port"), None);
        assert_eq!(str_field(r#"{"type":null}"#, "type"), None);
    }
}
