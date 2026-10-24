//! 播报播放流程纯逻辑：预缓冲放行与 TtsStop 排空门控
//!
//! 固件侧持有 PCM 队列与 I2S DMA；这里只决定「何时开始播」和
//! 「TtsStop 后何时算排空」，便于在 host 上单测。

/// 播放流程状态机
#[derive(Debug)]
pub struct PlaybackFlow {
    prebuffer_bytes: usize,
    drain_ms: u64,
    prebuffering: bool,
    pending_stop: bool,
    drain_at: u64,
    stop_at: u64,
    stop_started: bool,
    prebuffer_done: bool,
}

impl PlaybackFlow {
    pub fn new(prebuffer_bytes: usize, drain_ms: u64) -> Self {
        Self {
            prebuffer_bytes,
            drain_ms,
            prebuffering: false,
            pending_stop: false,
            drain_at: 0,
            stop_at: 0,
            stop_started: false,
            prebuffer_done: false,
        }
    }

    /// 新回复开始：队列不足阈值则重新预缓冲
    pub fn on_tts_start(&mut self, queued: usize) {
        self.pending_stop = false;
        self.drain_at = 0;
        self.prebuffering = queued < self.prebuffer_bytes;
    }

    /// 服务器发送完毕：保留预缓冲放行，进入排空等待
    pub fn on_tts_stop(&mut self) {
        self.prebuffering = false;
        self.pending_stop = true;
        self.drain_at = 0;
        self.stop_at = 0;
        self.stop_started = false;
    }

    /// 当前是否允许播放真实音频（预缓冲期间只补静音）
    pub fn allow_audio(&mut self, queued: usize) -> bool {
        if self.prebuffering && queued >= self.prebuffer_bytes {
            self.prebuffering = false;
            self.prebuffer_done = true;
        }
        !self.prebuffering
    }

    pub fn is_prebuffering(&self) -> bool {
        self.prebuffering
    }

    pub fn pending_stop(&self) -> bool {
        self.pending_stop
    }

    /// 取出「预缓冲刚完成」事件（一次性）
    pub fn take_prebuffer_done(&mut self) -> bool {
        core::mem::take(&mut self.prebuffer_done)
    }

    /// 队列空后再等 `drain_ms`，到点返回 true 表示可以派发 TtsStop
    pub fn poll_drained(&mut self, queue_empty: bool, now_ms: u64) -> bool {
        if !self.pending_stop {
            return false;
        }
        if !self.stop_started {
            self.stop_started = true;
            self.stop_at = now_ms;
        }
        // 兜底: TX 环形卡死 (DmaError::Late 后 tx_free 恒 0) 时队列永远不空,
        // 最多等 MAX_DRAIN_MS 就强制结束播报, 防止状态机永久停在 Speaking。
        let forced = now_ms.saturating_sub(self.stop_at) >= 600;
        if !queue_empty {
            self.drain_at = 0;
        } else if self.drain_at == 0 {
            self.drain_at = now_ms + self.drain_ms;
        }
        if forced || (queue_empty && self.drain_at != 0 && now_ms >= self.drain_at) {
            self.pending_stop = false;
            self.drain_at = 0;
            self.stop_at = 0;
            self.stop_started = false;
            true
        } else {
            false
        }
    }

    pub fn reset(&mut self) {
        self.prebuffering = false;
        self.pending_stop = false;
        self.drain_at = 0;
        self.stop_at = 0;
        self.stop_started = false;
        self.prebuffer_done = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prebuffer_blocks_until_threshold() {
        let mut f = PlaybackFlow::new(1000, 150);
        f.on_tts_start(0);
        assert!(f.is_prebuffering());
        assert!(!f.allow_audio(999));
        assert!(f.allow_audio(1000));
        assert!(!f.is_prebuffering());
        assert!(f.take_prebuffer_done());
        assert!(!f.take_prebuffer_done());
    }

    #[test]
    fn tts_start_with_enough_data_skips_prebuffer() {
        let mut f = PlaybackFlow::new(1000, 150);
        f.on_tts_start(5000);
        assert!(!f.is_prebuffering());
        assert!(f.allow_audio(5000));
    }

    #[test]
    fn drain_waits_until_queue_empty_plus_delay() {
        let mut f = PlaybackFlow::new(1000, 150);
        f.on_tts_start(5000);
        f.on_tts_stop();
        assert!(f.pending_stop());
        // 队列非空：不计时
        assert!(!f.poll_drained(false, 0));
        // 队列空：从现在起等 150ms (仍在 600ms 兜底内)
        assert!(!f.poll_drained(true, 100));
        assert!(!f.poll_drained(true, 249));
        assert!(f.poll_drained(true, 250));
        assert!(!f.pending_stop());
        assert!(!f.poll_drained(true, 2000));
    }

    #[test]
    fn drain_timer_resets_if_audio_arrives() {
        let mut f = PlaybackFlow::new(1000, 150);
        f.on_tts_stop();
        assert!(!f.poll_drained(true, 0));
        // 又有音频入队，计时清零
        assert!(!f.poll_drained(false, 100));
        assert!(!f.poll_drained(true, 120));
        assert!(!f.poll_drained(true, 260));
        assert!(f.poll_drained(true, 270));
    }

    #[test]
    fn drain_forced_after_max_wait_even_if_queue_not_empty() {
        let mut f = PlaybackFlow::new(1000, 150);
        f.on_tts_stop();
        assert!(!f.poll_drained(false, 0));
        assert!(!f.poll_drained(false, 599));
        assert!(f.poll_drained(false, 600));
        assert!(!f.pending_stop());
    }

    #[test]
    fn reset_clears_all_state() {
        let mut f = PlaybackFlow::new(1000, 150);
        f.on_tts_start(0);
        f.on_tts_stop();
        f.reset();
        assert!(!f.is_prebuffering());
        assert!(!f.pending_stop());
        assert!(!f.take_prebuffer_done());
    }
}
