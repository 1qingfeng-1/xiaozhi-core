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

/// 零交叉率上限 (千分比)：人声浊音集中在低频，ZCR 通常 <150‰；
/// 键盘敲击/摩擦音/宽带噪声接近 400~500‰，用它滤掉瞬态噪声。
pub const MAX_ZCR_PERMILLE: u32 = 250;

/// 计算去均值后的零交叉率 (千分比, 0~1000)
pub fn zero_crossing_permille(pcm: &[i16]) -> u32 {
    if pcm.len() < 2 {
        return 0;
    }
    let sum: i64 = pcm.iter().map(|&s| s as i64).sum();
    let mean = (sum / pcm.len() as i64) as i32;
    let mut prev = pcm[0] as i32 - mean;
    let mut crossings = 0u32;
    for &s in &pcm[1..] {
        let v = s as i32 - mean;
        if (v >= 0) != (prev >= 0) {
            crossings += 1;
        }
        prev = v;
    }
    (crossings * 1000 / (pcm.len() as u32 - 1)).min(1000)
}

/// 能量 VAD 检测器：帧均方幅度 ≥ 阈值，且需要足够多"低零交叉率"的
/// 人声帧。键盘敲击/宽带瞬态能量足够但 ZCR 高，不会累积到触发门槛；
/// 说话中的摩擦音偶尔 ZCR 高只做衰减，不会立刻清零。
pub struct VadWakeDetector {
    threshold: i16,
    /// 当前有声段内累计的低 ZCR 证据 (0~8)
    low_zcr_run: u32,
}

impl VadWakeDetector {
    /// 触发所需的低 ZCR 证据帧数 (≈180ms 浊音)
    const LOW_ZCR_REQUIRED: u32 = 3;

    pub fn new(threshold: i16) -> Self {
        Self {
            threshold,
            low_zcr_run: 0,
        }
    }

    /// 动态调整阈值 (环境噪声自适应): 固件按待机噪声底定期更新
    pub fn set_threshold(&mut self, threshold: i16) {
        self.threshold = threshold;
    }

    pub fn threshold(&self) -> i16 {
        self.threshold
    }
}

impl WakeWordSource for VadWakeDetector {
    fn detect(&mut self, pcm: &[i16]) -> bool {
        if !is_voice_active(pcm, self.threshold) {
            self.low_zcr_run = 0;
            return false;
        }
        if zero_crossing_permille(pcm) <= MAX_ZCR_PERMILLE {
            self.low_zcr_run = (self.low_zcr_run + 1).min(8);
        } else {
            // 摩擦音/瞬态: 衰减而不是清零, 容忍正常说话中的高频段
            self.low_zcr_run = self.low_zcr_run.saturating_sub(1);
        }
        self.low_zcr_run >= Self::LOW_ZCR_REQUIRED
    }
}

/// 噪声底更新: 快降慢升 (安静时快速下调, 说话/瞬态只缓慢抬高)
pub fn update_noise_floor(ema: i32, rms: i32) -> i32 {
    if ema == 0 {
        rms
    } else if rms < ema {
        ema - (ema - rms) / 8
    } else {
        ema + (rms - ema) / 64
    }
}

/// 由噪声底计算唤醒阈值: 1.5x 噪声 + 100, 限制在 [floor, ceiling]
///
/// 相比 2x 更贴近安静房间的说话电平 (噪声 250 → 475), 避免阈值过高
/// 导致正常说话唤不醒; 噪声大时由 ceiling 兜底。
pub fn adaptive_threshold(noise_floor: i32, floor: i16, ceiling: i16) -> i16 {
    (noise_floor * 3 / 2 + 100).clamp(floor as i32, ceiling as i32) as i16
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

    pub fn source_mut(&mut self) -> &mut S {
        &mut self.source
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

    /// 零 DC 交替信号 (最高频, 模拟键盘/宽带噪声)
    fn alternating(amp: i16) -> [i16; 1024] {
        let mut s = [0i16; 1024];
        for (i, v) in s.iter_mut().enumerate() {
            *v = if i % 2 == 0 { amp } else { -amp };
        }
        s
    }

    /// 低频正弦 (200Hz, 模拟人声浊音)
    fn sine_200hz(amp: i16) -> [i16; 1024] {
        let mut s = [0i16; 1024];
        for (i, v) in s.iter_mut().enumerate() {
            // 16kHz / 200Hz = 80 点/周期; 用整数相位避免浮点
            let phase = (i % 80) as i32;
            *v = if phase < 40 { amp } else { -amp };
        }
        s
    }

    #[test]
    fn vad_threshold_levels() {
        let mut det = VadWakeDetector::new(300);
        let quiet = [10i16; 1024]; // 纯 DC, 去 DC 后为静音
        let loud = sine_200hz(500);
        assert!(!det.detect(&quiet));
        // 需要累计 3 帧浊音证据
        assert!(!det.detect(&loud));
        assert!(!det.detect(&loud));
        assert!(det.detect(&loud));
        assert!(!det.detect(&[]));
    }

    #[test]
    fn zero_crossing_rejects_keyboard_like_noise() {
        let mut det = VadWakeDetector::new(300);
        // 人声浊音: 低零交叉率, 累计 3 帧后通过
        assert!(!det.detect(&sine_200hz(2000)));
        assert!(!det.detect(&sine_200hz(2000)));
        assert!(det.detect(&sine_200hz(2000)));
        // 键盘敲击/宽带瞬态: 能量足够但零交叉率高, 始终拒绝
        let mut clicks = VadWakeDetector::new(300);
        for _ in 0..20 {
            assert!(!clicks.detect(&alternating(2000)));
        }
        assert!(zero_crossing_permille(&sine_200hz(2000)) < 100);
        assert_eq!(zero_crossing_permille(&alternating(2000)), 1000);
        assert_eq!(zero_crossing_permille(&[7i16; 1024]), 0);
    }

    #[test]
    fn short_sibilant_between_vowels_does_not_reset() {
        let mut det = VadWakeDetector::new(300);
        assert!(!det.detect(&sine_200hz(2000)));
        assert!(!det.detect(&sine_200hz(2000)));
        // 一帧摩擦音只衰减，不清零
        assert!(!det.detect(&alternating(2000)));
        assert!(!det.detect(&sine_200hz(2000)));
        assert!(det.detect(&sine_200hz(2000)));
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

#[cfg(test)]
mod adaptive_tests {
    use super::*;

    #[test]
    fn threshold_setter_updates_detection() {
        let mut det = VadWakeDetector::new(1000);
        // 低频方波 (200Hz): 能量足够且 ZCR 像人声
        let mut loud = [0i16; 320];
        for (i, v) in loud.iter_mut().enumerate() {
            *v = if (i % 80) < 40 { 500 } else { -500 };
        }
        assert!(!det.detect(&loud));
        det.set_threshold(400);
        assert_eq!(det.threshold(), 400);
        // 需要累计 3 帧低 ZCR 证据
        assert!(!det.detect(&loud));
        assert!(!det.detect(&loud));
        assert!(det.detect(&loud));
    }

    #[test]
    fn noise_floor_drops_fast_rises_slow() {
        // 初始值直接采用
        assert_eq!(update_noise_floor(0, 800), 800);
        // 变安静: 一次下降 1/8
        assert_eq!(update_noise_floor(800, 0), 700);
        // 变吵: 一次上升 1/64
        assert_eq!(update_noise_floor(700, 1700), 715);
    }

    #[test]
    fn adaptive_threshold_clamps_and_scales() {
        assert_eq!(adaptive_threshold(100, 400, 700), 400);
        assert_eq!(adaptive_threshold(250, 400, 700), 475);
        assert_eq!(adaptive_threshold(1000, 400, 700), 700);
    }
}
