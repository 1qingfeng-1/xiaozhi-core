//! 音量/增益映射
//!
//! ES8311 的音量寄存器采用 dB 步进（0~63 对应 0~-63dB，每步 1dB），
//! 而用户界面通常用 0~100 的线性百分比。本模块提供两者之间的纯映射函数，
//! 便于在 host 上单元测试，硬件层只需把映射结果写入对应寄存器。

/// 音量百分比范围（0 = 静音，100 = 最大）
pub const VOLUME_MIN: u8 = 0;
pub const VOLUME_MAX: u8 = 100;

/// ES8311 音量寄存器范围（0 = 最大增益，63 = 静音，每步 1dB）
///
/// 参考 ES8311 datasheet：音量寄存器 0x12/0x13，
/// 取值 0~63 对应 +12dB ~ -51dB（步进 1dB），63 为静音。
pub const CODEC_REG_MAX: u8 = 63;

/// 把用户音量百分比 (0~100) 映射到 ES8311 音量寄存器值 (0~63)。
///
/// 采用线性映射并做边界钳制：
/// - `0`   → `63`（静音）
/// - `100` → `0`  （最大增益）
/// - 中间值线性插值
///
/// 返回的寄存器值越大，音量越小。
///
/// # 纯函数
/// 无副作用，可在 host 上单元测试。
#[must_use]
pub fn percent_to_codec(percent: u8) -> u8 {
    // 钳制到有效范围
    let clamped = percent.min(VOLUME_MAX);
    // 反向线性映射: percent=100 → reg=0, percent=0 → reg=63
    // reg = 63 * (100 - percent) / 100
    let reg = (CODEC_REG_MAX as u32) * (VOLUME_MAX as u32 - clamped as u32) / (VOLUME_MAX as u32);
    reg as u8
}

/// 把 ES8311 音量寄存器值 (0~63) 反向映射到用户音量百分比 (0~100)。
///
/// `percent_to_codec` 的逆函数（取整后可能不严格互逆，用于显示）。
#[must_use]
pub fn codec_to_percent(reg: u8) -> u8 {
    let clamped = reg.min(CODEC_REG_MAX);
    let percent =
        (VOLUME_MAX as u32) * (CODEC_REG_MAX as u32 - clamped as u32) / (CODEC_REG_MAX as u32);
    percent as u8
}

/// 在给定当前百分比基础上增减 `delta`，返回钳制后的新百分比。
///
/// 用于音量 +/- 按键：`adjust_volume(current, +5)` 或 `adjust_volume(current, -5)`。
#[must_use]
pub fn adjust_volume(current: u8, delta: i16) -> u8 {
    let cur = current as i32;
    let next = cur + delta as i32;
    let clamped = next.clamp(VOLUME_MIN as i32, VOLUME_MAX as i32);
    clamped as u8
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percent_to_codec_bounds() {
        assert_eq!(percent_to_codec(0), CODEC_REG_MAX);
        assert_eq!(percent_to_codec(100), 0);
    }

    #[test]
    fn percent_to_codec_clamps_high() {
        // 超出 100 应钳制到 0 寄存器
        assert_eq!(percent_to_codec(150), 0);
    }

    #[test]
    fn percent_to_codec_monotonic() {
        // 百分比越大，寄存器值越小（音量越大）
        let a = percent_to_codec(20);
        let b = percent_to_codec(50);
        let c = percent_to_codec(80);
        assert!(a > b);
        assert!(b > c);
    }

    #[test]
    fn codec_to_percent_bounds() {
        assert_eq!(codec_to_percent(0), 100);
        assert_eq!(codec_to_percent(CODEC_REG_MAX), 0);
    }

    #[test]
    fn codec_to_percent_clamps_high() {
        assert_eq!(codec_to_percent(99), 0);
    }

    #[test]
    fn roundtrip_approx() {
        // 映射应近似互逆（取整误差内）
        for p in [0, 10, 25, 50, 75, 90, 100] {
            let reg = percent_to_codec(p);
            let back = codec_to_percent(reg);
            assert!(
                (p as i32 - back as i32).abs() <= 2,
                "roundtrip failed: {p} -> {reg} -> {back}"
            );
        }
    }

    #[test]
    fn adjust_volume_clamps() {
        assert_eq!(adjust_volume(0, -10), 0);
        assert_eq!(adjust_volume(100, 10), 100);
        assert_eq!(adjust_volume(50, 10), 60);
        assert_eq!(adjust_volume(50, -10), 40);
    }

    #[test]
    fn adjust_volume_from_silence() {
        assert_eq!(adjust_volume(0, 5), 5);
    }
}
