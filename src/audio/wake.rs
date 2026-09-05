//! 声音唤醒 — 检测器抽象 + 唤醒策略状态机
//!
//! 分层：检测器（VAD / 未来 WakeNet 关键词）只回答"本帧是否命中"，
//! 策略状态机（去抖、冷却、会话后抑制）决定"是否对外触发唤醒"。
//! 纯逻辑，时间由调用方注入（毫秒），host 可单测全部转移。

use crate::audio::capture::is_voice_active;

/// 检测器接口：喂一帧 PCM，返回本帧是否"候选命中"。
///
/// - [`VadWakeDetector`]：能量法，任何持续人声命中（默认实现，上机即用）；
/// - 关键词唤醒（feature `wakenet`，esp-sr WakeNet）：同接口接入，策略层不变。
pub trait WakeWordSource {
    fn detect(&mut self, pcm: &[i16]) -> bool;
}

/// 能量 VAD 检测器：帧均方幅度 ≥ 阈值即命中
pub struct VadWakeDetector {
    threshold: i16,
}

impl VadWakeDetector {
    pub fn new(threshold: i16) -> Self {
        Self { threshold }
    }
}

impl WakeWordSource for VadWakeDetector {
    fn detect(&mut self, pcm: &[i16]) -> bool {
        is_voice_active(pcm, self.threshold)
    }
}

/// 唤醒事件
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WakeEvent {
    /// 无事发生
    None,
    /// 触发唤醒（策略层放行）
    Triggered,
}

/// 唤醒策略配置
#[derive(Debug, Clone, Copy)]
pub struct WakePolicyConfig {
    /// 开机预热（ms）：首次喂帧后忽略检测，等 codec/DMA 稳定，滤掉上电瞬态
    /// (0 = 关闭; 纯逻辑层不做硬件假设, 由固件按 codec 上电特性注入, 如 1000ms)
    pub warmup_ms: u64,
    /// 去抖：需连续命中 N 帧才触发（滤掉瞬态噪声）
    pub debounce_frames: u32,
    /// 触发后的冷却时长（防回声/残句连环唤醒）
    pub cooldown_ms: u64,
    /// 会话结束后的抑制时长（说完话松开 → 残音再触发）
    pub refractory_ms: u64,
}

impl Default for WakePolicyConfig {
    fn default() -> Self {
        // 帧长 ~64ms（单 DMA 描述符）：2 帧 ≈ 128ms 持续人声；
        // 冷却覆盖典型 TTS 播完的尾巴回声；抑制覆盖会话收尾残音；
        // warmup 默认 0: 由固件按 codec 上电特性注入。
        Self {
            warmup_ms: 0,
            debounce_frames: 2,
            cooldown_ms: 2_000,
            refractory_ms: 1_500,
        }
    }
}

/// 唤醒策略状态机
///
/// 状态：`Idle`（监听）→ 去抖计数达标且不在抑制期 → `Triggered`；
/// 触发/会话结束/播完提示音都会设置抑制窗口，窗口走完后自动重新武装。
#[derive(Debug)]
pub struct WakePolicy {
    cfg: WakePolicyConfig,
    /// 当前连续命中帧数
    voiced_run: u32,
    /// 抑制截止时刻（ms）：冷却/回声防护/会话后抑制统一取最大值
    suppressed_until: u64,
    /// 是否已武装（抑制窗口外为 true）
    armed: bool,
    /// 首次喂帧时刻（ms）：预热期基准（None = 尚未喂帧，首帧起算）
    start_ms: Option<u64>,
}

impl WakePolicy {
    pub fn new(cfg: WakePolicyConfig) -> Self {
        Self {
            cfg,
            voiced_run: 0,
            suppressed_until: 0,
            armed: true,
            start_ms: None,
        }
    }

    /// 喂入一帧检测结论，返回策略结论
    pub fn feed(&mut self, voiced: bool, now_ms: u64) -> WakeEvent {
        // 首帧记录预热基准；预热期内强制视为静音（滤 codec/DMA 上电瞬态，
        // 核心默认 0 关闭，固件按 codec 上电特性注入）；抑制窗口照常走。
        let start = self.start_ms.get_or_insert(now_ms);
        let voiced = if now_ms.saturating_sub(*start) >= self.cfg.warmup_ms {
            voiced
        } else {
            false
        };
        if !voiced {
            self.voiced_run = 0;
            return self.rearm_if_due(now_ms);
        }
        if !self.armed {
            return self.rearm_if_due(now_ms);
        }
        self.voiced_run += 1;
        if self.voiced_run >= self.cfg.debounce_frames {
            // 触发：挂起并进入冷却
            self.voiced_run = 0;
            self.armed = false;
            self.suppressed_until = now_ms + self.cfg.cooldown_ms;
            WakeEvent::Triggered
        } else {
            WakeEvent::None
        }
    }

    /// 会话结束：进入抑制窗口并复位去抖（残音/回声防护）
    pub fn note_session_end(&mut self, now_ms: u64) {
        self.suppressed_until = self.suppressed_until.max(now_ms + self.cfg.refractory_ms);
        self.armed = false;
        self.voiced_run = 0;
    }

    /// TTS 播放结束：进入冷却窗口（回声防护）
    pub fn note_playback_end(&mut self, now_ms: u64) {
        self.suppressed_until = self.suppressed_until.max(now_ms + self.cfg.cooldown_ms);
        self.armed = false;
        self.voiced_run = 0;
    }

    /// 强制复位（调试/手动模式切回时用）
    pub fn reset(&mut self) {
        self.voiced_run = 0;
        self.suppressed_until = 0;
        self.armed = true;
        self.start_ms = None;
    }

    /// 抑制窗口走完后重新武装；重新武装发生在喂帧时（惰性），本帧不参与去抖
    fn rearm_if_due(&mut self, now_ms: u64) -> WakeEvent {
        if !self.armed && now_ms >= self.suppressed_until {
            self.armed = true;
            self.voiced_run = 0;
        }
        WakeEvent::None
    }
}

/// 组合引擎：检测器 + 策略，一次 `feed` 完成检测与判定
pub struct WakeEngine<S: WakeWordSource> {
    source: S,
    policy: WakePolicy,
}

impl<S: WakeWordSource> WakeEngine<S> {
    pub fn new(source: S, policy: WakePolicy) -> Self {
        Self { source, policy }
    }

    pub fn policy_mut(&mut self) -> &mut WakePolicy {
        &mut self.policy
    }

    pub fn feed(&mut self, pcm: &[i16], now_ms: u64) -> WakeEvent {
        let voiced = self.source.detect(pcm);
        self.policy.feed(voiced, now_ms)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FRAME_MS: u64 = 64;

    fn step_engine(engine: &mut WakeEngine<VadWakeDetector>, voiced: bool, n: u64) -> WakeEvent {
        engine.policy_mut().feed(voiced, n * FRAME_MS)
    }

    fn make_engine(cfg: WakePolicyConfig) -> WakeEngine<VadWakeDetector> {
        WakeEngine::new(VadWakeDetector::new(300), WakePolicy::new(cfg))
    }

    /// 零 DC 交替信号
    fn alternating(amp: i16) -> [i16; 1024] {
        let mut s = [0i16; 1024];
        for (i, v) in s.iter_mut().enumerate() {
            *v = if i % 2 == 0 { amp } else { -amp };
        }
        s
    }

    #[test]
    fn vad_threshold_levels() {
        let mut det = VadWakeDetector::new(300);
        let quiet = [10i16; 1024]; // 纯 DC, 去 DC 后为静音
        let loud = alternating(500);
        assert!(!det.detect(&quiet));
        assert!(det.detect(&loud));
        assert!(!det.detect(&[]));
    }

    #[test]
    fn debounce_requires_consecutive_frames() {
        let mut eng = make_engine(WakePolicyConfig {
            debounce_frames: 3,
            ..Default::default()
        });
        assert_eq!(step_engine(&mut eng, true, 0), WakeEvent::None);
        assert_eq!(step_engine(&mut eng, true, 1), WakeEvent::None);
        // 中断一次，计数清零
        assert_eq!(step_engine(&mut eng, false, 2), WakeEvent::None);
        assert_eq!(step_engine(&mut eng, true, 3), WakeEvent::None);
        assert_eq!(step_engine(&mut eng, true, 4), WakeEvent::None);
        assert_eq!(step_engine(&mut eng, true, 5), WakeEvent::Triggered);
    }

    #[test]
    fn trigger_enters_cooldown_then_rearms() {
        let mut eng = make_engine(WakePolicyConfig {
            debounce_frames: 2,
            cooldown_ms: 1_000,
            refractory_ms: 0,
            ..Default::default()
        });
        assert_eq!(step_engine(&mut eng, true, 0), WakeEvent::None);
        assert_eq!(step_engine(&mut eng, true, 1), WakeEvent::Triggered);
        // 冷却期内即使持续发声也不触发
        assert_eq!(step_engine(&mut eng, true, 2), WakeEvent::None);
        assert_eq!(step_engine(&mut eng, true, 14), WakeEvent::None);
        // 冷却结束（1000ms ≈ 第 16 帧）后重新武装；重新武装帧不参与去抖
        assert_eq!(step_engine(&mut eng, true, 20), WakeEvent::None);
        assert_eq!(step_engine(&mut eng, true, 21), WakeEvent::None);
        assert_eq!(step_engine(&mut eng, true, 22), WakeEvent::Triggered);
    }

    #[test]
    fn session_end_suppresses_retrigger() {
        let cfg = WakePolicyConfig {
            debounce_frames: 1,
            cooldown_ms: 100,
            refractory_ms: 2_000,
            ..Default::default()
        };
        let mut eng = make_engine(cfg);
        eng.policy_mut().note_session_end(5_000);
        // 抑制期内（至 7000ms）不触发
        assert_eq!(step_engine(&mut eng, true, 5_000 / 64), WakeEvent::None);
        assert_eq!(step_engine(&mut eng, true, 106), WakeEvent::None);
        // 7104ms ≥ 7000ms：重新武装帧返回 None，下一帧触发
        assert_eq!(step_engine(&mut eng, true, 111), WakeEvent::None);
        assert_eq!(step_engine(&mut eng, true, 112), WakeEvent::Triggered);
    }

    #[test]
    fn playback_end_suppresses_echo() {
        let mut eng = make_engine(WakePolicyConfig {
            debounce_frames: 1,
            cooldown_ms: 1_200,
            refractory_ms: 0,
            ..Default::default()
        });
        eng.policy_mut().note_playback_end(10_000);
        assert_eq!(step_engine(&mut eng, true, 160), WakeEvent::None);
        // 10s + 1.2s = 11.2s ≈ 第 176 帧；180 帧 > 窗口：重新武装帧
        assert_eq!(step_engine(&mut eng, true, 180), WakeEvent::None);
        assert_eq!(step_engine(&mut eng, true, 181), WakeEvent::Triggered);
    }

    #[test]
    fn reset_rearms_immediately() {
        let mut eng = make_engine(WakePolicyConfig::default());
        assert_eq!(step_engine(&mut eng, true, 0), WakeEvent::None);
        assert_eq!(step_engine(&mut eng, true, 1), WakeEvent::Triggered);
        eng.policy_mut().reset();
        assert_eq!(step_engine(&mut eng, true, 2), WakeEvent::None);
        assert_eq!(step_engine(&mut eng, true, 3), WakeEvent::Triggered);
    }

    #[test]
    fn silent_frames_keep_counter_zero() {
        let mut eng = make_engine(WakePolicyConfig {
            debounce_frames: 2,
            ..Default::default()
        });
        for n in 0..100u64 {
            assert_eq!(step_engine(&mut eng, false, n), WakeEvent::None);
        }
    }

    #[test]
    fn warmup_suppresses_boot_transient() {
        let cfg = WakePolicyConfig {
            warmup_ms: 1_000,
            debounce_frames: 2,
            ..Default::default()
        };
        let mut eng = make_engine(cfg);
        // 开机瞬态: 首帧 + 预热期内（n*64 = 0..960ms < 1000ms）的连续强命中不得触发
        for n in 0..16 {
            assert_eq!(step_engine(&mut eng, true, n), WakeEvent::None);
        }
        // 预热期结束（n=16 → 1024ms > 1000ms）: 正常去抖，连续 2 帧后触发
        assert_eq!(step_engine(&mut eng, true, 16), WakeEvent::None);
        assert_eq!(step_engine(&mut eng, true, 17), WakeEvent::Triggered);
    }
}
