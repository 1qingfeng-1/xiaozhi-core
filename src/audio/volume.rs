//! 音量映射 — 用户百分比 (0~100) ↔ ES8311 REG32
//!
//! ES8311 REG32 是 dB 线性映射: 0 = -95.5dB, 191 = 0dB, 255 = +32dB
//! (步进 0.5dB)。满幅 PCM 再叠加 +32dB 会硬削顶 (真机 VOL 100% 破音)，
//! 因此 100% 只到 **-3dB** (REG 185) 留一点模拟余量：
//! - 0%: REG 0 = -95.5dB (实际静音)
//! - 1%~100%: REG 91 (-50dB) ~ 185 (-3dB)
//!
//! 60% ≈ -21.8dB，与旧线性映射的 60% (-19dB) 接近；低音量段比旧映射
//! 更可用 (旧映射 30% 只有 -57dB，几乎不可闻)。

/// 用户音量百分比范围 (0 = 静音, 100 = 最大)
pub const VOLUME_MIN: u8 = 0;
pub const VOLUME_MAX: u8 = 100;

/// 最大音量对应的 REG32 (-3dB; 留模拟余量防削顶)
pub const CODEC_REG_MAX: u8 = 185;
/// 最小非零音量对应的 REG32 (-50dB)
pub const CODEC_REG_MIN: u8 = 91;

/// 百分比 → REG32 (0% 静音, 1~100% 映射到 -50dB~-3dB)
#[must_use]
pub const fn percent_to_codec(percent: u8) -> u8 {
    if percent == 0 {
        return 0;
    }
    let p = if percent > VOLUME_MAX {
        VOLUME_MAX
    } else {
        percent
    } as u16;
    let span = (CODEC_REG_MAX - CODEC_REG_MIN) as u16;
    CODEC_REG_MIN + (p * span / VOLUME_MAX as u16) as u8
}

/// REG32 → 百分比 (用于显示; 取整误差 ±1)
#[must_use]
pub const fn codec_to_percent(reg: u8) -> u8 {
    if reg == 0 {
        0
    } else if reg <= CODEC_REG_MIN {
        1
    } else {
        let span = (CODEC_REG_MAX - CODEC_REG_MIN) as u16;
        let p = (reg - CODEC_REG_MIN) as u16 * VOLUME_MAX as u16 / span;
        if p > VOLUME_MAX as u16 {
            VOLUME_MAX
        } else {
            p as u8
        }
    }
}

/// 在当前百分比上增减 `delta`, 结果钳制到 [0, 100]
#[must_use]
pub fn adjust_volume(current: u8, delta: i16) -> u8 {
    let next = current as i32 + delta as i32;
    next.clamp(VOLUME_MIN as i32, VOLUME_MAX as i32) as u8
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percent_to_codec_bounds() {
        assert_eq!(percent_to_codec(0), 0);
        assert_eq!(percent_to_codec(1), CODEC_REG_MIN);
        assert_eq!(percent_to_codec(100), CODEC_REG_MAX);
        assert_eq!(percent_to_codec(150), CODEC_REG_MAX);
        // 100% 不能进入 +dB 区 (REG > 191 会削顶): 由 CODEC_REG_MAX 常量保证
    }

    #[test]
    fn percent_to_codec_monotonic() {
        let a = percent_to_codec(20);
        let b = percent_to_codec(50);
        let c = percent_to_codec(80);
        assert!(a < b && b < c);
        assert_eq!(percent_to_codec(60), 147);
    }

    #[test]
    fn codec_to_percent_bounds() {
        assert_eq!(codec_to_percent(0), 0);
        assert_eq!(codec_to_percent(CODEC_REG_MAX), 100);
        assert_eq!(codec_to_percent(255), 100);
    }

    #[test]
    fn roundtrip_approx() {
        for p in [0, 1, 10, 25, 50, 60, 75, 90, 100] {
            let reg = percent_to_codec(p);
            let back = codec_to_percent(reg);
            assert!(
                (p as i32 - back as i32).abs() <= 1,
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
        assert_eq!(adjust_volume(0, 5), 5);
    }
}
