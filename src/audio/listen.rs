//! 聆听回合结束策略 — 纯逻辑, host 可单测
//!
//! 输入每个采集块的 VAD 有声时长与块时长, 输出是否应收尾 (auto 模式)。
//! 时间语义与固件原实现一致:
//! - 前 `warmup_ms` 内不把有声计入 (codec/DMA 上电瞬态);
//! - 累计有声 ≥ `min_voiced_ms` 才认为用户说过话;
//! - 说过话后持续静音 `silence_ms` → 收尾;
//! - 从未说过话则 `no_voice_ms` 后收尾;
//! - `mic_muted` 阻止"有声"判定 (但不阻止静音累计);
//! - `in_grace` 同时阻止有声与静音累计 (播报尾音保护)。

/// 回合结束参数
#[derive(Debug, Clone, Copy)]
pub struct ListenTurnConfig {
    pub warmup_ms: u64,
    pub min_voiced_ms: u64,
    pub silence_ms: u64,
    pub no_voice_ms: u64,
}

impl Default for ListenTurnConfig {
    fn default() -> Self {
        Self {
            warmup_ms: 600,
            min_voiced_ms: 250,
            silence_ms: 1_500,
            no_voice_ms: 12_000,
        }
    }
}

/// 聆听回合跟踪
#[derive(Debug, Clone, Copy)]
pub struct ListenTurn {
    cfg: ListenTurnConfig,
    total_ms: u64,
    voiced_ms: u64,
    silence_ms: u64,
    heard: bool,
}

impl ListenTurn {
    pub const fn new(cfg: ListenTurnConfig) -> Self {
        Self {
            cfg,
            total_ms: 0,
            voiced_ms: 0,
            silence_ms: 0,
            heard: false,
        }
    }

    pub fn reset(&mut self) {
        self.total_ms = 0;
        self.voiced_ms = 0;
        self.silence_ms = 0;
        self.heard = false;
    }

    pub fn total_ms(&self) -> u64 {
        self.total_ms
    }

    pub fn voiced_ms(&self) -> u64 {
        self.voiced_ms
    }

    pub fn silence_ms(&self) -> u64 {
        self.silence_ms
    }

    pub fn heard(&self) -> bool {
        self.heard
    }

    /// 是否应结束本回合 (auto 模式)
    pub fn should_stop(&self) -> bool {
        (self.heard && self.silence_ms >= self.cfg.silence_ms)
            || (!self.heard && self.total_ms >= self.cfg.no_voice_ms)
    }

    /// 喂入一个采集块
    ///
    /// `block_voiced_ms`: 块内 60ms 子块的有声时长之和;
    /// `block_ms`: 块总时长; `mic_muted`: 提示音/唤醒静音窗; `in_grace`:
    /// TTS 结束后的尾音保护窗。返回是否应结束回合。
    pub fn feed(
        &mut self,
        block_voiced_ms: u64,
        block_ms: u64,
        mic_muted: bool,
        in_grace: bool,
    ) -> bool {
        self.total_ms = self.total_ms.saturating_add(block_ms);
        let warmup = self.total_ms <= self.cfg.warmup_ms;
        let voiced_ms = if warmup { 0 } else { block_voiced_ms };
        self.voiced_ms = self.voiced_ms.saturating_add(voiced_ms);
        if self.voiced_ms >= self.cfg.min_voiced_ms {
            self.heard = true;
        }
        let voiced = voiced_ms > 0 && !mic_muted && !in_grace;
        if voiced {
            self.silence_ms = 0;
        } else if self.heard && !in_grace {
            self.silence_ms = self.silence_ms.saturating_add(block_ms);
        }
        self.should_stop()
    }
}

impl Default for ListenTurn {
    fn default() -> Self {
        Self::new(ListenTurnConfig::default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FRAME: u64 = 60;

    #[test]
    fn speech_then_silence_stops() {
        let mut t = ListenTurn::default();
        // warmup: 即使有声也不计入
        for _ in 0..10 {
            assert!(!t.feed(FRAME, FRAME, false, false));
        }
        // 连续 5 帧有声 (300ms) → heard
        for _ in 0..5 {
            assert!(!t.feed(FRAME, FRAME, false, false));
        }
        assert!(t.heard());
        assert_eq!(t.voiced_ms(), 300);
        // 静音 25 帧 (1500ms) → 第 25 帧收尾
        for i in 0..25 {
            let stop = t.feed(0, FRAME, false, false);
            assert_eq!(stop, i == 24, "frame {i}");
        }
    }

    #[test]
    fn silence_without_speech_times_out() {
        let mut t = ListenTurn::default();
        // warmup 之后仍无有声, 12s 收尾
        let frames = 12_000 / FRAME;
        for i in 0..frames {
            let stop = t.feed(0, FRAME, false, false);
            assert!(!stop || i + 1 == frames);
        }
        assert!(!t.heard());
        assert!(t.should_stop());
    }

    #[test]
    fn mic_mute_blocks_voiced_but_not_silence() {
        let mut t = ListenTurn::default();
        for _ in 0..10 {
            t.feed(0, FRAME, false, false);
        }
        for _ in 0..5 {
            t.feed(FRAME, FRAME, false, false); // 300ms 有声 → heard
        }
        // 静音窗内: 有声判定被阻止, 静音继续累计
        for _ in 0..10 {
            t.feed(FRAME, FRAME, true, false);
        }
        assert!(t.silence_ms() >= 600);
        assert!(t.voiced_ms() >= 300);
    }

    #[test]
    fn grace_blocks_both() {
        let mut t = ListenTurn::default();
        for _ in 0..10 {
            t.feed(0, FRAME, false, false);
        }
        for _ in 0..5 {
            t.feed(FRAME, FRAME, false, false);
        }
        let before = t.silence_ms();
        for _ in 0..10 {
            t.feed(FRAME, FRAME, false, true); // grace: 不累计静音
        }
        assert_eq!(t.silence_ms(), before);
        assert!(!t.should_stop());
    }

    #[test]
    fn short_voice_does_not_set_heard() {
        let mut t = ListenTurn::default();
        for _ in 0..10 {
            t.feed(0, FRAME, false, false);
        }
        // 仅 120ms 有声 (<250ms) → 不认为说过话
        t.feed(FRAME, FRAME, false, false);
        t.feed(FRAME, FRAME, false, false);
        assert!(!t.heard());
        // 之后静音不会按 1500ms 收尾, 而是等 12s
        for _ in 0..30 {
            assert!(!t.feed(0, FRAME, false, false));
        }
    }
}
