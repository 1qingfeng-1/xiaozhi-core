//! 正弦音生成 — 纯逻辑, host 可单测
//!
//! 64 点正弦表 + 32 位相位累加器，支持任意频率的分块连续输出。
//! 主固件 `Audio::beep` 与 HIL agent 的 `PLAY_TONE` 共用同一实现，
//! 避免固件里的私有 `fill_sine` 与测试音行为漂移。

/// 正弦表长度 (一个周期)
pub const TABLE_LEN: usize = 64;

/// 64 点正弦表: `32767 * sin(2*pi*n/64)`
pub const SINE_TABLE: [i16; TABLE_LEN] = [
    0, 3212, 6393, 9512, 12539, 15446, 18204, 20787, 23170, 25329, 27245, 28898, 30273, 31356,
    32137, 32609, 32767, 32609, 32137, 31356, 30273, 28898, 27245, 25329, 23170, 20787, 18204,
    15446, 12539, 9512, 6393, 3212, 0, -3212, -6393, -9512, -12539, -15446, -18204, -20787, -23170,
    -25329, -27245, -28898, -30273, -31356, -32137, -32609, -32767, -32609, -32137, -31356, -30273,
    -28898, -27245, -25329, -23170, -20787, -18204, -15446, -12539, -9512, -6393, -3212,
];

const PHASE_BITS: u32 = 32;
const TABLE_SHIFT: u32 = PHASE_BITS - 6; // 64 = 2^6

/// 任意频率正弦生成器 (单声道, 跨块保持相位)
#[derive(Debug, Clone, Copy)]
pub struct ToneGen {
    phase: u32,
    step: u32,
    amp: i16,
}

impl ToneGen {
    /// `sample_rate == 0` 时 step 为 0，输出静音（防御异常配置）。
    pub fn new(freq_hz: u32, sample_rate: u32, amp: i16) -> Self {
        let step = if sample_rate == 0 {
            0
        } else {
            ((freq_hz as u64 * (1u64 << PHASE_BITS)) / sample_rate as u64) as u32
        };
        Self {
            phase: 0,
            step,
            amp: amp.clamp(0, i16::MAX),
        }
    }

    /// 生成下一个单声道样本
    pub fn next_sample(&mut self) -> i16 {
        let idx = (self.phase >> TABLE_SHIFT) as usize & (TABLE_LEN - 1);
        let sample = SINE_TABLE[idx] as i32 * self.amp as i32 / i16::MAX as i32;
        self.phase = self.phase.wrapping_add(self.step);
        sample as i16
    }

    /// 填充单声道 PCM
    pub fn fill(&mut self, out: &mut [i16]) {
        for s in out {
            *s = self.next_sample();
        }
    }

    /// 填充立体声 i16-LE (L=R), 返回写入字节数 (总是 4 的倍数)
    pub fn fill_stereo_le(&mut self, out: &mut [u8]) -> usize {
        let samples = out.len() / 4;
        for i in 0..samples {
            let v = self.next_sample().to_le_bytes();
            let o = i * 4;
            out[o..o + 2].copy_from_slice(&v);
            out[o + 2..o + 4].copy_from_slice(&v);
        }
        samples * 4
    }
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use std::vec::Vec;

    fn samples(tone: &mut ToneGen, n: usize) -> Vec<i16> {
        (0..n).map(|_| tone.next_sample()).collect()
    }

    #[test]
    fn tone_250hz_matches_table_exactly() {
        let mut tone = ToneGen::new(250, 16_000, i16::MAX);
        assert_eq!(samples(&mut tone, TABLE_LEN), SINE_TABLE.to_vec());
    }

    #[test]
    fn frequency_matches_requested_rate() {
        // 1kHz @16kHz: 1 秒 16000 样本, 正向过零约 1000 次
        let mut tone = ToneGen::new(1000, 16_000, 10_000);
        let data = samples(&mut tone, 16_000);
        let mut crossings = 0;
        for pair in data.windows(2) {
            if pair[0] < 0 && pair[1] >= 0 {
                crossings += 1;
            }
        }
        assert!((999..=1001).contains(&crossings), "crossings={crossings}");
    }

    #[test]
    fn amplitude_scaling_is_bounded() {
        let mut tone = ToneGen::new(250, 16_000, 1000);
        let data = samples(&mut tone, TABLE_LEN);
        let peak = data.iter().map(|s| (*s as i32).abs()).max().unwrap();
        assert!((998..=1001).contains(&peak), "peak={peak}");
    }

    #[test]
    fn chunked_generation_is_continuous() {
        let mut whole = ToneGen::new(750, 16_000, 5000);
        let expected = samples(&mut whole, 128);

        let mut chunked = ToneGen::new(750, 16_000, 5000);
        let mut got = Vec::new();
        for _ in 0..2 {
            let mut buf = [0i16; 64];
            chunked.fill(&mut buf);
            got.extend_from_slice(&buf);
        }
        assert_eq!(got, expected);
    }

    #[test]
    fn stereo_output_duplicates_channels() {
        let mut tone = ToneGen::new(1000, 16_000, i16::MAX);
        let mut buf = [0u8; 8];
        assert_eq!(tone.fill_stereo_le(&mut buf), 8);
        for frame in buf.as_chunks::<4>().0 {
            assert_eq!(frame[0..2], frame[2..4]);
        }
    }

    #[test]
    fn zero_sample_rate_is_silent() {
        let mut tone = ToneGen::new(1000, 0, i16::MAX);
        assert_eq!(samples(&mut tone, 8), std::vec![0; 8]);
    }
}
