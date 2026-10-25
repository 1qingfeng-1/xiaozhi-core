#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    // 任意字节流: 只要求不 panic / 不越界
    let _ = xiaozhi_core::ws::header_len(data);
    let _ = xiaozhi_core::ws::parse_header(data, 4096);
});
