//! 空闲提示策略 — 纯逻辑, host 可单测
//!
//! 用于「一段时间无交互后播放提示音并进入低功耗」：活动（唤醒/按键/会话）
//! 会重置窗口；窗口到点后只触发一次，直到下一次活动。

/// 空闲提示策略
#[derive(Debug, Clone, Copy)]
pub struct IdlePromptPolicy {
    window_ms: u64,
    last_activity_ms: u64,
    prompted: bool,
}

impl IdlePromptPolicy {
    /// `now_ms` 为创建时刻（通常开机时刻），窗口从此刻开始计时
    pub const fn new(window_ms: u64, now_ms: u64) -> Self {
        Self {
            window_ms,
            last_activity_ms: now_ms,
            prompted: false,
        }
    }

    /// 记录一次活动: 重置窗口并允许下次再提示
    pub fn note_activity(&mut self, now_ms: u64) {
        self.last_activity_ms = now_ms;
        self.prompted = false;
    }

    /// 轮询: 窗口到点且本窗口内未提示过 → 返回 true (只触发一次)
    pub fn poll(&mut self, now_ms: u64) -> bool {
        if self.prompted {
            return false;
        }
        if now_ms.saturating_sub(self.last_activity_ms) >= self.window_ms {
            self.prompted = true;
            true
        } else {
            false
        }
    }

    /// 本次空闲窗口是否已提示过
    pub fn prompted(&self) -> bool {
        self.prompted
    }

    /// 距离上次活动的时间
    pub fn idle_for_ms(&self, now_ms: u64) -> u64 {
        now_ms.saturating_sub(self.last_activity_ms)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn triggers_once_after_window() {
        let mut p = IdlePromptPolicy::new(60_000, 0);
        assert!(!p.poll(59_999));
        assert!(p.poll(60_000));
        // 不重复触发
        assert!(!p.poll(120_000));
        assert!(!p.poll(600_000));
        assert!(p.prompted());
    }

    #[test]
    fn activity_resets_window_and_rearms() {
        let mut p = IdlePromptPolicy::new(60_000, 0);
        assert!(!p.poll(30_000));
        p.note_activity(30_000);
        assert!(!p.poll(89_999));
        assert!(p.poll(90_000));
        // 再次活动后可再次提示
        p.note_activity(100_000);
        assert!(!p.poll(159_999));
        assert!(p.poll(160_000));
    }

    #[test]
    fn idle_for_reports_elapsed() {
        let mut p = IdlePromptPolicy::new(1_000, 5_000);
        assert_eq!(p.idle_for_ms(6_200), 1_200);
        p.note_activity(10_000);
        assert_eq!(p.idle_for_ms(10_001), 1);
    }
}
