#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = xiaozhi_core::mqtt::decode(data);
});
