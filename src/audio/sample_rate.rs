//! 采样率与 I2S 时钟计算
//!
//! I2S 时钟关系（Philips/TDM 标准）：
//! - `BCLK = SAMPLE_RATE * DATA_BITS * CHANNELS`
//! - `MCLK = BCLK * MCLK_MULTIPLIER`（ES8311/ES7210 通常要求 MCLK = 256 * FS 或 384 * FS）
//!
//! 这些是纯计算，可在 host 上单元测试，硬件层只需把算出的分频值写入寄存器。

/// ES8311/ES7210 要求的 MCLK 倍频（MCLK = 256 * SAMPLE_RATE）
///
/// 参考 ES8311 datasheet：MCLK 推荐为 256 或 384 倍采样率。
pub const MCLK_MULTIPLIER_256: u32 = 256;
pub const MCLK_MULTIPLIER_384: u32 = 384;

/// 常见音频采样率（Hz）
#[must_use]
pub const fn common_sample_rates() -> [u32; 6] {
    [8000, 16000, 22050, 44100, 48000, 96000]
}

/// 计算 BCLK（位时钟）频率
///
/// `BCLK = SAMPLE_RATE * DATA_BITS * CHANNELS`
///
/// # 参数
/// - `sample_rate`: 采样率 (Hz)
/// - `data_bits`: 每通道数据位宽（16/24/32）
/// - `channels`: 通道数（1=单声道，2=立体声）
///
/// # 纯函数
#[must_use]
pub fn bclk_frequency(sample_rate: u32, data_bits: u32, channels: u32) -> u32 {
    sample_rate * data_bits * channels
}

/// 计算 MCLK（主时钟）频率
///
/// `MCLK = BCLK * multiplier`
///
/// # 纯函数
#[must_use]
pub fn mclk_frequency(sample_rate: u32, data_bits: u32, channels: u32, multiplier: u32) -> u32 {
    bclk_frequency(sample_rate, data_bits, channels) * multiplier
}

/// 给定源时钟和 MCLK 目标，计算整数分频系数和误差 (Hz)。
///
/// 返回 `(divider, abs_error_hz)`：`source / divider ≈ target`，误差为 |source/divider - target|。
/// 若 source < target 则无法分频，返回 `(1, source - target)`。
///
/// # 纯函数
#[must_use]
pub fn clock_divider(source_hz: u32, target_hz: u32) -> (u32, u32) {
    if target_hz == 0 {
        return (1, source_hz);
    }
    if source_hz < target_hz {
        return (1, target_hz - source_hz);
    }
    let divider = source_hz / target_hz;
    let error = source_hz / divider - target_hz;
    (divider, error)
}

/// 校验采样率是否在 I2S 支持的常用范围内。
#[must_use]
pub fn is_valid_sample_rate(hz: u32) -> bool {
    (8000..=192000).contains(&hz)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bclk_16bit_stereo_48k() {
        // 48000 * 16 * 2 = 1_536_000
        assert_eq!(bclk_frequency(48000, 16, 2), 1_536_000);
    }

    #[test]
    fn bclk_32bit_stereo_44_1k() {
        // 44100 * 32 * 2 = 2_822_400
        assert_eq!(bclk_frequency(44100, 32, 2), 2_822_400);
    }

    #[test]
    fn mclk_256x() {
        // 48000 * 16 * 2 * 256 = 393_216_000
        let mclk = mclk_frequency(48000, 16, 2, MCLK_MULTIPLIER_256);
        assert_eq!(mclk, 393_216_000);
    }

    #[test]
    fn mclk_384x() {
        // 44100 * 16 * 2 = 1_411_200 (BCLK), * 384 = 541_900_800 (MCLK)
        let mclk = mclk_frequency(44100, 16, 2, MCLK_MULTIPLIER_384);
        assert_eq!(mclk, 1_411_200u32 * 384);
    }

    #[test]
    fn divider_exact() {
        // 192MHz / 48MHz = 4
        let (div, err) = clock_divider(192_000_000, 48_000_000);
        assert_eq!(div, 4);
        assert_eq!(err, 0);
    }

    #[test]
    fn divider_with_error() {
        // 100 / 7 = 14, 100/14 = 7.14 → err = |7 - 7| 用整除: 100/14=7, err=0
        let (div, _) = clock_divider(100, 7);
        assert_eq!(div, 14);
    }

    #[test]
    fn divider_source_less_than_target() {
        let (div, err) = clock_divider(1000, 2000);
        assert_eq!(div, 1);
        assert_eq!(err, 1000);
    }

    #[test]
    fn valid_sample_rates() {
        assert!(is_valid_sample_rate(44100));
        assert!(is_valid_sample_rate(48000));
        assert!(!is_valid_sample_rate(1000));
        assert!(!is_valid_sample_rate(500000));
    }
}
