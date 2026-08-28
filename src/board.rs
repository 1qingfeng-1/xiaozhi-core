//! 板级纯逻辑 — 与硬件无关的板级判断规则，host 可单元测试
//!
//! 硬件读取 (GPIO/ADC) 在 `xiaozhi-s3` 的 BSP 层，本模块只做分类/换算。

/// 电源状态
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PowerStatus {
    /// 充电中
    Charging,
    /// 充电完成 (充电器已接, 充电 IC 报告终止)
    Charged,
    /// 放电中 (电池供电)
    Discharging,
    /// 空闲 (未接充电器或状态未知)
    Idle,
}

/// 由充电 IC 的两根状态脚 (低电平有效) 分类电源状态
///
/// | CHRG 有效 | DONE 有效 | 状态        |
/// |-----------|-----------|-------------|
/// | 是        | 否        | Charging    |
/// | 否        | 是        | Charged     |
/// | 否        | 否        | Idle        |
/// | 是        | 是        | Discharging (异常组合, 按电池供电处理) |
///
/// 注: 官方 gezipai_board.cc 将 DONE 有效笼统标记为 discharging;
/// Rust 版单独区分出 Charged, 供"充电完成显示 100%"等 UI 规则使用。
pub const fn charge_status(chrg_active: bool, done_active: bool) -> PowerStatus {
    match (chrg_active, done_active) {
        (true, false) => PowerStatus::Charging,
        (false, true) => PowerStatus::Charged,
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

/// 上报电量百分比 (供 UI/日志显示)
///
/// 充电完成时钳位 100%: 充电 IC DONE 有效即为满电, 而充满后静置电压
/// (~4.0~4.15V) 低于 100% 映射点 (4.2V), 纯电压法会低估; 其余状态按
/// 电压线性映射 [`battery_percentage`]。
///
/// 返回 `None` 表示配置非法 (`min_mv >= max_mv`) 且非充电完成状态。
pub const fn reported_battery_percentage(
    status: PowerStatus,
    voltage_mv: i32,
    min_mv: i32,
    max_mv: i32,
) -> Option<i32> {
    if matches!(status, PowerStatus::Charged) {
        return Some(100);
    }
    let p = battery_percentage(voltage_mv, min_mv, max_mv);
    if p >= 0 { Some(p) } else { None }
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
    fn charged_when_chrg_high_done_low() {
        assert_eq!(charge_status(false, true), PowerStatus::Charged);
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

    #[test]
    fn reported_clamps_to_100_when_charged() {
        // 钳位不依赖电压读数与量程配置
        assert_eq!(
            reported_battery_percentage(PowerStatus::Charged, 4050, 3000, 4200),
            Some(100)
        );
        assert_eq!(
            reported_battery_percentage(PowerStatus::Charged, 0, 3000, 4200),
            Some(100)
        );
        assert_eq!(
            reported_battery_percentage(PowerStatus::Charged, 4050, 4200, 3000),
            Some(100)
        );
    }

    #[test]
    fn reported_uses_voltage_mapping_otherwise() {
        assert_eq!(
            reported_battery_percentage(PowerStatus::Discharging, 3600, 3000, 4200),
            Some(50)
        );
        assert_eq!(
            reported_battery_percentage(PowerStatus::Charging, 4199, 3000, 4200),
            Some(99)
        );
        assert_eq!(
            reported_battery_percentage(PowerStatus::Idle, 2999, 3000, 4200),
            Some(0)
        );
        // 配置非法时维持 None (非充电完成状态)
        assert_eq!(
            reported_battery_percentage(PowerStatus::Discharging, 3600, 4200, 3000),
            None
        );
    }
}
