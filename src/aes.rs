//! AES-128 (加密方向) + CTR 模式 — 纯软件实现, host 可单测
//!
//! 用途: xiaozhi MQTT/UDP 音频通道。UDP 包格式为
//! `16B 帧头(作为 CTR 初始计数块) || AES-128-CTR 密文`, 密钥由服务端
//! hello 下发 (16 字节), 每条消息用各自的帧头作 IV (对齐 C++ 参考
//! `mqtt_protocol.cc` 与网关 `app.js` 的 `createCipheriv('aes-128-ctr')`)。
//!
//! 选软件实现而非硬件 AES 外设: 音频包 60ms 一帧 (~1KB), 量级远低于
//! 外设中断/DMA 开销, 且纯逻辑可进 core 单测。

/// S 盒 (FIPS-197)
const SBOX: [u8; 256] = [
    0x63, 0x7c, 0x77, 0x7b, 0xf2, 0x6b, 0x6f, 0xc5, 0x30, 0x01, 0x67, 0x2b, 0xfe, 0xd7, 0xab, 0x76,
    0xca, 0x82, 0xc9, 0x7d, 0xfa, 0x59, 0x47, 0xf0, 0xad, 0xd4, 0xa2, 0xaf, 0x9c, 0xa4, 0x72, 0xc0,
    0xb7, 0xfd, 0x93, 0x26, 0x36, 0x3f, 0xf7, 0xcc, 0x34, 0xa5, 0xe5, 0xf1, 0x71, 0xd8, 0x31, 0x15,
    0x04, 0xc7, 0x23, 0xc3, 0x18, 0x96, 0x05, 0x9a, 0x07, 0x12, 0x80, 0xe2, 0xeb, 0x27, 0xb2, 0x75,
    0x09, 0x83, 0x2c, 0x1a, 0x1b, 0x6e, 0x5a, 0xa0, 0x52, 0x3b, 0xd6, 0xb3, 0x29, 0xe3, 0x2f, 0x84,
    0x53, 0xd1, 0x00, 0xed, 0x20, 0xfc, 0xb1, 0x5b, 0x6a, 0xcb, 0xbe, 0x39, 0x4a, 0x4c, 0x58, 0xcf,
    0xd0, 0xef, 0xaa, 0xfb, 0x43, 0x4d, 0x33, 0x85, 0x45, 0xf9, 0x02, 0x7f, 0x50, 0x3c, 0x9f, 0xa8,
    0x51, 0xa3, 0x40, 0x8f, 0x92, 0x9d, 0x38, 0xf5, 0xbc, 0xb6, 0xda, 0x21, 0x10, 0xff, 0xf3, 0xd2,
    0xcd, 0x0c, 0x13, 0xec, 0x5f, 0x97, 0x44, 0x17, 0xc4, 0xa7, 0x7e, 0x3d, 0x64, 0x5d, 0x19, 0x73,
    0x60, 0x81, 0x4f, 0xdc, 0x22, 0x2a, 0x90, 0x88, 0x46, 0xee, 0xb8, 0x14, 0xde, 0x5e, 0x0b, 0xdb,
    0xe0, 0x32, 0x3a, 0x0a, 0x49, 0x06, 0x24, 0x5c, 0xc2, 0xd3, 0xac, 0x62, 0x91, 0x95, 0xe4, 0x79,
    0xe7, 0xc8, 0x37, 0x6d, 0x8d, 0xd5, 0x4e, 0xa9, 0x6c, 0x56, 0xf4, 0xea, 0x65, 0x7a, 0xae, 0x08,
    0xba, 0x78, 0x25, 0x2e, 0x1c, 0xa6, 0xb4, 0xc6, 0xe8, 0xdd, 0x74, 0x1f, 0x4b, 0xbd, 0x8b, 0x8a,
    0x70, 0x3e, 0xb5, 0x66, 0x48, 0x03, 0xf6, 0x0e, 0x61, 0x35, 0x57, 0xb9, 0x86, 0xc1, 0x1d, 0x9e,
    0xe1, 0xf8, 0x98, 0x11, 0x69, 0xd9, 0x8e, 0x94, 0x9b, 0x1e, 0x87, 0xe9, 0xce, 0x55, 0x28, 0xdf,
    0x8c, 0xa1, 0x89, 0x0d, 0xbf, 0xe6, 0x42, 0x68, 0x41, 0x99, 0x2d, 0x0f, 0xb0, 0x54, 0xbb, 0x16,
];

/// 轮常量
const RCON: [u8; 10] = [0x01, 0x02, 0x04, 0x08, 0x10, 0x20, 0x40, 0x80, 0x1b, 0x36];

/// 预展开的 AES-128 密钥 (11 组 16 字节轮密钥)
#[derive(Clone)]
pub struct Aes128 {
    round_keys: [u8; 176],
}

impl Aes128 {
    /// 由 16 字节密钥展开轮密钥
    pub fn new(key: &[u8; 16]) -> Self {
        let mut round_keys = [0u8; 176];
        round_keys[..16].copy_from_slice(key);
        for round in 1..11 {
            let prev = (round - 1) * 16;
            let cur = round * 16;
            let mut t = [
                round_keys[prev + 12],
                round_keys[prev + 13],
                round_keys[prev + 14],
                round_keys[prev + 15],
            ];
            // RotWord
            t.rotate_left(1);
            // SubWord + Rcon (轮常量下标 = round - 1)
            let rc = RCON[round - 1];
            t[0] = SBOX[t[0] as usize] ^ rc;
            t[1] = SBOX[t[1] as usize];
            t[2] = SBOX[t[2] as usize];
            t[3] = SBOX[t[3] as usize];
            // 首字 = 上一轮首字 XOR g(上一轮末字)
            for i in 0..4 {
                round_keys[cur + i] = round_keys[prev + i] ^ t[i];
            }
            // 后续字 = 本字前 4 字节 XOR 上一轮同位置
            for i in 4..16 {
                round_keys[cur + i] = round_keys[cur + i - 4] ^ round_keys[prev + i];
            }
        }
        Self { round_keys }
    }

    /// 加密单个 16 字节块 (就地)
    pub fn encrypt_block(&self, block: &mut [u8; 16]) {
        add_round_key(block, &self.round_keys[..16]);
        for round in 1..10 {
            sub_bytes(block);
            shift_rows(block);
            mix_columns(block);
            add_round_key(block, &self.round_keys[round * 16..round * 16 + 16]);
        }
        sub_bytes(block);
        shift_rows(block);
        add_round_key(block, &self.round_keys[160..176]);
    }

    /// CTR 模式: 以 `iv` 为初始计数块, 就地 XOR 加/解密
    pub fn ctr(&self, iv: &[u8; 16], data: &mut [u8]) {
        let mut counter = *iv;
        let mut ks = [0u8; 16];
        for chunk in data.chunks_mut(16) {
            ks.copy_from_slice(&counter);
            self.encrypt_block(&mut ks);
            for (b, k) in chunk.iter_mut().zip(ks.iter()) {
                *b ^= k;
            }
            increment_be(&mut counter);
        }
    }
}

/// 便捷函数: 一次 CTR 加/解密 (密钥展开 + 处理)
pub fn ctr_apply(key: &[u8; 16], iv: &[u8; 16], data: &mut [u8]) {
    Aes128::new(key).ctr(iv, data);
}

/// 128 位大端计数 +1
fn increment_be(counter: &mut [u8; 16]) {
    for b in counter.iter_mut().rev() {
        let (v, carry) = b.overflowing_add(1);
        *b = v;
        if !carry {
            break;
        }
    }
}

fn add_round_key(state: &mut [u8; 16], key: &[u8]) {
    for (s, k) in state.iter_mut().zip(key.iter()) {
        *s ^= *k;
    }
}

fn sub_bytes(state: &mut [u8; 16]) {
    for b in state.iter_mut() {
        *b = SBOX[*b as usize];
    }
}

/// 行移位 (状态按列优先: index = row + 4*col)
fn shift_rows(state: &mut [u8; 16]) {
    let t = *state;
    for row in 0..4 {
        for col in 0..4 {
            state[row + 4 * col] = t[row + 4 * ((col + row) % 4)];
        }
    }
}

fn xtime(x: u8) -> u8 {
    (x << 1) ^ if x & 0x80 != 0 { 0x1b } else { 0 }
}

fn mix_columns(state: &mut [u8; 16]) {
    for col in 0..4 {
        let i = col * 4;
        let (a0, a1, a2, a3) = (state[i], state[i + 1], state[i + 2], state[i + 3]);
        state[i] = xtime(a0) ^ (xtime(a1) ^ a1) ^ a2 ^ a3;
        state[i + 1] = a0 ^ xtime(a1) ^ (xtime(a2) ^ a2) ^ a3;
        state[i + 2] = a0 ^ a1 ^ xtime(a2) ^ (xtime(a3) ^ a3);
        state[i + 3] = (xtime(a0) ^ a0) ^ a1 ^ a2 ^ xtime(a3);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex<const N: usize>(s: &str) -> [u8; N] {
        let mut out = [0u8; N];
        for (i, b) in out.iter_mut().enumerate() {
            *b = u8::from_str_radix(&s[i * 2..i * 2 + 2], 16).unwrap();
        }
        out
    }

    #[test]
    fn fips197_aes128_block_vector() {
        // FIPS-197 附录 C.1
        let key: [u8; 16] = hex("000102030405060708090a0b0c0d0e0f");
        let mut block: [u8; 16] = hex("00112233445566778899aabbccddeeff");
        Aes128::new(&key).encrypt_block(&mut block);
        assert_eq!(block, hex("69c4e0d86a7b0430d8cdb78070b4c55a"));
    }

    #[test]
    fn fips197_key_expansion_last_round_key() {
        // FIPS-197 附录 A.1: 第 10 轮密钥
        let key: [u8; 16] = hex("2b7e151628aed2a6abf7158809cf4f3c");
        let aes = Aes128::new(&key);
        let expect: [u8; 16] = hex("d014f9a8c9ee2589e13f0cc8b6630ca6");
        assert_eq!(aes.round_keys[160..176], expect);
    }

    #[test]
    fn nist_sp800_38a_ctr_vector() {
        // NIST SP 800-38A F.5.1 CTR-AES-128
        let key: [u8; 16] = hex("2b7e151628aed2a6abf7158809cf4f3c");
        let iv: [u8; 16] = hex("f0f1f2f3f4f5f6f7f8f9fafbfcfdfeff");
        let mut data: [u8; 64] = hex(concat!(
            "6bc1bee22e409f96e93d7e117393172a",
            "ae2d8a571e03ac9c9eb76fac45af8e51",
            "30c81c46a35ce411e5fbc1191a0a52ef",
            "f69f2445df4f9b17ad2b417be66c3710"
        ));
        ctr_apply(&key, &iv, &mut data);
        let expect: [u8; 64] = hex(concat!(
            "874d6191b620e3261bef6864990db6ce",
            "9806f66b7970fdff8617187bb9fffdff",
            "5ae4df3edbd5d35e5b4f09020db03eab",
            "1e031dda2fbe03d1792170a0f3009cee"
        ));
        assert_eq!(data, expect);
        // 再套一次恢复明文
        ctr_apply(&key, &iv, &mut data);
        assert_eq!(data[..16], hex::<16>("6bc1bee22e409f96e93d7e117393172a"));
    }

    #[test]
    fn ctr_counter_wraps_low_bytes_only_forward() {
        let key = [0u8; 16];
        let iv = [0xffu8; 16];
        let aes = Aes128::new(&key);
        let mut data = [0u8; 32];
        aes.ctr(&iv, &mut data);
        // 两块密钥流必须不同 (计数器已进位)
        assert_ne!(data[..16], data[16..]);
    }
}
