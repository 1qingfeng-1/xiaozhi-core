//! SHA-1 (FIPS 180-1) — 仅用于 WebSocket 握手 `Sec-WebSocket-Accept` 校验
//!
//! 纯逻辑、无分配, host 可单测。WebSocket 的 Accept = base64(SHA1(key||GUID)),
//! 其中 GUID 固定为 `258EAFA5-E914-47DA-95CA-C5AB0DC85B11`。

/// 计算 SHA-1 摘要
pub fn sha1(data: &[u8]) -> [u8; 20] {
    let mut h: [u32; 5] = [
        0x6745_2301,
        0xEFCD_AB89,
        0x98BA_DCFE,
        0x1032_5476,
        0xC3D2_E1F0,
    ];

    let mut i = 0usize;
    while i + 64 <= data.len() {
        let mut block = [0u8; 64];
        block.copy_from_slice(&data[i..i + 64]);
        process(&mut h, &block);
        i += 64;
    }

    let rem = &data[i..];
    let bit_len = (data.len() as u64).wrapping_mul(8);
    let mut tail = [0u8; 128];
    tail[..rem.len()].copy_from_slice(rem);
    tail[rem.len()] = 0x80;
    let pad_len = if rem.len() < 56 { 64 } else { 128 };
    tail[pad_len - 8..pad_len].copy_from_slice(&bit_len.to_be_bytes());
    for block in tail[..pad_len].as_chunks::<64>().0 {
        process(&mut h, block);
    }

    let mut out = [0u8; 20];
    for (i, v) in h.iter().enumerate() {
        out[i * 4..i * 4 + 4].copy_from_slice(&v.to_be_bytes());
    }
    out
}

fn process(h: &mut [u32; 5], block: &[u8; 64]) {
    let mut w = [0u32; 80];
    for (i, word) in w.iter_mut().take(16).enumerate() {
        *word = u32::from_be_bytes([
            block[i * 4],
            block[i * 4 + 1],
            block[i * 4 + 2],
            block[i * 4 + 3],
        ]);
    }
    for i in 16..80 {
        w[i] = (w[i - 3] ^ w[i - 8] ^ w[i - 14] ^ w[i - 16]).rotate_left(1);
    }

    let (mut a, mut b, mut c, mut d, mut e) = (h[0], h[1], h[2], h[3], h[4]);
    for (i, wi) in w.iter().enumerate() {
        let (f, k) = match i {
            0..=19 => ((b & c) | ((!b) & d), 0x5A82_7999),
            20..=39 => (b ^ c ^ d, 0x6ED9_EBA1),
            40..=59 => ((b & c) | (b & d) | (c & d), 0x8F1B_BCDC),
            _ => (b ^ c ^ d, 0xCA62_C1D6),
        };
        let temp = a
            .rotate_left(5)
            .wrapping_add(f)
            .wrapping_add(e)
            .wrapping_add(k)
            .wrapping_add(*wi);
        e = d;
        d = c;
        c = b.rotate_left(30);
        b = a;
        a = temp;
    }

    h[0] = h[0].wrapping_add(a);
    h[1] = h[1].wrapping_add(b);
    h[2] = h[2].wrapping_add(c);
    h[3] = h[3].wrapping_add(d);
    h[4] = h[4].wrapping_add(e);
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;

    fn hex(bytes: &[u8]) -> std::string::String {
        let mut s = std::string::String::new();
        for b in bytes {
            s.push_str(&std::format!("{b:02x}"));
        }
        s
    }

    #[test]
    fn fips_vectors() {
        assert_eq!(hex(&sha1(b"")), "da39a3ee5e6b4b0d3255bfef95601890afd80709");
        assert_eq!(
            hex(&sha1(b"abc")),
            "a9993e364706816aba3e25717850c26c9cd0d89d"
        );
        assert_eq!(
            hex(&sha1(
                b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"
            )),
            "84983e441c3bd26ebaae4aa1f95129e5e54670f1"
        );
    }

    #[test]
    fn padding_boundaries() {
        // 55/56/64 字节是 padding 的分支边界
        for len in [54usize, 55, 56, 63, 64, 65, 119, 120, 128] {
            let data = std::vec![0x61u8; len];
            let digest = sha1(&data);
            assert_eq!(digest.len(), 20);
            // 同样输入两次结果一致 (纯函数)
            assert_eq!(digest, sha1(&data));
        }
    }
}
