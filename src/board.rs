//! 板级纯逻辑 — 与硬件无关的板级判断规则，host 可单元测试
//!
//! 硬件读取 (GPIO/ADC) 在 `xiaozhi-s3` 的 BSP 层，本模块只做分类/换算。

/// 电源状态
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PowerStatus {
    /// 充电中
    Charging,
    /// 放电中 (电池供电)
    Discharging,
    /// 空闲 (未接充电器或状态未知)
    Idle,
}

/// 由充电 IC 的两根状态脚 (低电平有效) 分类电源状态
///
/// 行为与参考实现 `gezipai_board.cc` 一致 (电平→状态):
///
/// | CHRG 电平 | DONE 电平 | 状态        | (低有效标志位)   |
/// |-----------|-----------|-------------|------------------|
/// | 0         | 1         | Charging    | (true, false)    |
/// | 1         | 1         | Idle        | (false, false)   |
/// | 0         | 0         | Discharging | (true, true)     |
/// | 1         | 0         | Discharging | (false, true)    |
pub const fn charge_status(chrg_active: bool, done_active: bool) -> PowerStatus {
    match (chrg_active, done_active) {
        (true, false) => PowerStatus::Charging,
        (false, false) => PowerStatus::Idle,
        _ => PowerStatus::Discharging,
    }
}

/// 由电池端电压 (mV) 计算电量百分比
///
/// 线性映射 `[min_mv, max_mv] → [0, 100]`，行为与参考实现
/// `gezipai_board.cc` 一致 (3.0V~4.2V 锂电池)。
///
/// 返回 -1 表示配置非法 (`min_mv >= max_mv`)。
pub const fn battery_percentage(voltage_mv: i32, min_mv: i32, max_mv: i32) -> i32 {
    if min_mv >= max_mv {
        return -1;
    }
    if voltage_mv <= min_mv {
        return 0;
    }
    if voltage_mv >= max_mv {
        return 100;
    }
    (voltage_mv - min_mv) * 100 / (max_mv - min_mv)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn charging_when_chrg_low_done_high() {
        assert_eq!(charge_status(true, false), PowerStatus::Charging);
    }

    #[test]
    fn idle_when_both_high() {
        assert_eq!(charge_status(false, false), PowerStatus::Idle);
    }

    #[test]
    fn discharging_when_both_low() {
        assert_eq!(charge_status(true, true), PowerStatus::Discharging);
    }

    #[test]
    fn discharging_when_chrg_high_done_low() {
        assert_eq!(charge_status(false, true), PowerStatus::Discharging);
    }

    #[test]
    fn battery_percentage_boundaries() {
        assert_eq!(battery_percentage(3000, 3000, 4200), 0);
        assert_eq!(battery_percentage(4200, 3000, 4200), 100);
        assert_eq!(battery_percentage(2999, 3000, 4200), 0);
        assert_eq!(battery_percentage(4201, 3000, 4200), 100);
    }

    #[test]
    fn battery_percentage_midpoints() {
        assert_eq!(battery_percentage(3600, 3000, 4200), 50);
        assert_eq!(battery_percentage(3300, 3000, 4200), 25);
        assert_eq!(battery_percentage(4199, 3000, 4200), 99);
    }

    #[test]
    fn battery_percentage_invalid_range() {
        assert_eq!(battery_percentage(3600, 4200, 3000), -1);
        assert_eq!(battery_percentage(3600, 3000, 3000), -1);
    }
}
