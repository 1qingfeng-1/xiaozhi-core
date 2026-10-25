//! 播报播放流程纯逻辑：预缓冲放行、TtsStop 排空门控、播放时间戳配对
//!
//! 固件侧持有 PCM 队列与 I2S DMA；这里只决定「何时开始播」「TtsStop 后
//! 何时算排空」以及「哪一帧服务器音频正在出声」，便于在 host 上单测。

use crate::protocol::AecClock;

/// 播放流程状态机
#[derive(Debug)]
pub struct PlaybackFlow {
    prebuffer_bytes: usize,
    drain_ms: u64,
    /// TX DMA 卡死时的兜底上限（必须 ≥ 软件队列时长 + TX 环时长）
    force_ms: u64,
    prebuffering: bool,
    pending_stop: bool,
    drain_at: u64,
    stop_at: u64,
    stop_started: bool,
    prebuffer_done: bool,
}

impl PlaybackFlow {
    /// `drain_ms`: 队列空后等待 DMA 排空的余量（应 ≥ TX 环形时长）
    /// `force_ms`: TX 卡死兜底上限（应 ≥ 队列容量时长 + TX 环时长 + 余量）
    pub fn new(prebuffer_bytes: usize, drain_ms: u64, force_ms: u64) -> Self {
        Self {
            prebuffer_bytes,
            drain_ms,
            force_ms,
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
    ///
    /// 兜底 `force_ms` 只在 TX DMA 真卡死时才有意义：正常路径下软件队列
    /// 可能仍有 2–3s 音频（服务器合成快于实时），不能用短超时提前结束播报，
    /// 否则会一边外放一边开麦，形成自问自答。
    pub fn poll_drained(&mut self, queue_empty: bool, now_ms: u64) -> bool {
        if !self.pending_stop {
            return false;
        }
        if !self.stop_started {
            self.stop_started = true;
            self.stop_at = now_ms;
        }
        let forced = now_ms.saturating_sub(self.stop_at) >= self.force_ms;
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

/// 播放时间戳队列条目: 该帧还有多少字节未播出
#[derive(Debug, Clone, Copy)]
struct TsEntry {
    bytes: usize,
    ts: u32,
}

/// 播放时间戳配对 (服务端 AEC)
///
/// PCM 入队时登记 `(字节数, 服务器时间戳)`; 每次实际写入 DMA 时按消费
/// 字节数推进, 一帧**播完**才把时间戳交给 [`AecClock`], 上行帧随之携带
/// 该时间戳, 服务端即可对齐"正在播放的参考声音"。
#[derive(Debug)]
pub struct PlayoutTimestamps {
    entries: [TsEntry; Self::CAP],
    head: usize,
    len: usize,
}

impl PlayoutTimestamps {
    /// 队列深度: 每帧一条, 192 条 ≈ 11.5s (60ms/帧) > PCM 队列 8s
    ///
    /// 必须覆盖整个 PCM 环: 服务端 TTS 快于实时, 队列里可能压着 8s 音频;
    /// 容量不足时新帧时间戳会被并入最旧条目, 服务端 AEC 参考帧随之错位/
    /// 丢失 (真机表现为播报中上行时间戳大量为 0, AEC 基本失效)。
    pub const CAP: usize = 192;

    pub fn new() -> Self {
        Self {
            entries: [TsEntry { bytes: 0, ts: 0 }; Self::CAP],
            head: 0,
            len: 0,
        }
    }

    /// 登记一帧 (bytes = 该帧 PCM 字节数; ts = 0 表示无时间戳, 忽略)
    pub fn push_frame(&mut self, bytes: usize, ts: u32) {
        if ts == 0 || bytes == 0 {
            return;
        }
        if self.len == Self::CAP {
            // 满: 并入最早条目 (偏差 < 一帧, 不丢对齐信息)
            self.entries[self.head].bytes += bytes;
            return;
        }
        let idx = (self.head + self.len) % Self::CAP;
        self.entries[idx] = TsEntry { bytes, ts };
        self.len += 1;
    }

    /// 已播出 `bytes` 字节: 播完的条目按序交给 `clock`
    pub fn consume(&mut self, mut bytes: usize, clock: &mut AecClock) {
        while bytes > 0 && self.len > 0 {
            let e = &mut self.entries[self.head];
            let take = e.bytes.min(bytes);
            e.bytes -= take;
            bytes -= take;
            if e.bytes == 0 {
                clock.push(e.ts);
                self.head = (self.head + 1) % Self::CAP;
                self.len -= 1;
            }
        }
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn clear(&mut self) {
        self.head = 0;
        self.len = 0;
    }
}

impl Default for PlayoutTimestamps {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prebuffer_blocks_until_threshold() {
        let mut f = PlaybackFlow::new(1000, 150, 600);
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
        let mut f = PlaybackFlow::new(1000, 150, 600);
        f.on_tts_start(5000);
        assert!(!f.is_prebuffering());
        assert!(f.allow_audio(5000));
    }

    #[test]
    fn drain_waits_until_queue_empty_plus_delay() {
        let mut f = PlaybackFlow::new(1000, 150, 600);
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
        let mut f = PlaybackFlow::new(1000, 150, 600);
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
        let mut f = PlaybackFlow::new(1000, 150, 600);
        f.on_tts_stop();
        assert!(!f.poll_drained(false, 0));
        assert!(!f.poll_drained(false, 599));
        assert!(f.poll_drained(false, 600));
        assert!(!f.pending_stop());
    }

    #[test]
    fn long_queue_is_not_cut_short_by_force_timeout() {
        // 队列里还有 3s 音频时, 兜底阈值必须大于队列时长, 否则会在
        // 外放未结束时切回 Listening (自问自答的根因)。
        let mut f = PlaybackFlow::new(1000, 150, 4_000);
        f.on_tts_stop();
        assert!(!f.poll_drained(false, 0));
        assert!(!f.poll_drained(false, 599));
        assert!(!f.poll_drained(false, 3_999));
        assert!(f.poll_drained(false, 4_000)); // 仅 TX 卡死时兜底
    }

    #[test]
    fn reset_clears_all_state() {
        let mut f = PlaybackFlow::new(1000, 150, 600);
        f.on_tts_start(0);
        f.on_tts_stop();
        f.reset();
        assert!(!f.is_prebuffering());
        assert!(!f.pending_stop());
        assert!(!f.take_prebuffer_done());
    }

    #[test]
    fn playout_ts_fires_only_after_frame_fully_played() {
        let mut q = PlayoutTimestamps::new();
        let mut clock = AecClock::new();
        q.push_frame(100, 11);
        q.push_frame(100, 22);
        q.push_frame(100, 33);
        assert_eq!(q.len(), 3);
        // 播了半帧: 不应产生时间戳
        q.consume(50, &mut clock);
        assert!(clock.is_empty());
        // 播完第一帧
        q.consume(50, &mut clock);
        assert_eq!(clock.pop(), 11);
        assert!(clock.is_empty());
        // 一次播完剩余两帧
        q.consume(200, &mut clock);
        assert_eq!(clock.pop(), 22);
        assert_eq!(clock.pop(), 33);
        assert!(q.is_empty());
    }

    #[test]
    fn playout_ts_ignores_zero_and_merges_when_full() {
        let mut q = PlayoutTimestamps::new();
        q.push_frame(100, 0);
        assert!(q.is_empty());
        for i in 1..=PlayoutTimestamps::CAP as u32 + 2 {
            q.push_frame(10, i);
        }
        assert_eq!(q.len(), PlayoutTimestamps::CAP);
        // 溢出并入队首: 首帧字节数变大, 时间戳仍为 1
        let mut clock = AecClock::new();
        q.consume(10, &mut clock);
        assert!(clock.is_empty());
        q.consume(20, &mut clock);
        assert_eq!(clock.pop(), 1);
    }

    #[test]
    fn playout_ts_clear_resets() {
        let mut q = PlayoutTimestamps::new();
        q.push_frame(10, 5);
        q.clear();
        let mut clock = AecClock::new();
        q.consume(10, &mut clock);
        assert!(clock.is_empty());
    }
}
