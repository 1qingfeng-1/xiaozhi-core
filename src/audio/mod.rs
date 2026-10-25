//! 音频纯逻辑
//!
//! 子模块：
//! - 音量映射 (ES8311 REG32): [`volume`]
//! - 采样率与 I2S 时钟分频计算: [`sample_rate`]
//! - ES8311/ES7210 寄存器配置序列: [`codec`]
//! - 采集处理 (TDM 解包/增益/VAD): [`capture`]
//! - 声音唤醒 (检测器 trait/唤醒策略): [`wake`]
//! - 播报播放流程 (预缓冲/排空门控): [`playback`]

pub mod capture;
pub mod codec;
pub mod listen;
pub mod playback;
pub mod preroll;
pub mod sample_rate;
pub mod volume;
pub mod wake;
