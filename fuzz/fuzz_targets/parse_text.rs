#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    // 服务器消息/OTA 清单/HTTP 头都是 UTF-8 文本; 非法字节直接跳过
    let Ok(text) = core::str::from_utf8(data) else {
        return;
    };
    let _ = xiaozhi_core::server_msg::parse(text);
    let _ = xiaozhi_core::ota::parse_manifest(text);
    let _ = xiaozhi_core::http::split_response(text);
    let _ = xiaozhi_core::http::parse_status(text);
    let _ = xiaozhi_core::json_scan::str_field(text, "type");
    let _ = xiaozhi_core::json_scan::num_field(text, "volume");
});
