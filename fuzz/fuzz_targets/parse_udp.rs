#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = xiaozhi_core::udp::parse_header(data);
    // 16 字节头正好时, 再走一遍序号跟踪器
    if let Some(h) = xiaozhi_core::udp::parse_header(data) {
        let mut tracker = xiaozhi_core::udp::SequenceTracker::new();
        let _ = tracker.observe(h.sequence);
    }
});
