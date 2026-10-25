//! 音频纯逻辑
//!
//! 子模块：
//! - 音量映射 (ES8311 REG32): [`volume`]
//! - 采样率与 I2S 时钟分频计算: [`sample_rate`]
//! - ES8311/ES7210 寄存器配置序列: [`codec`]
//! - 采集处理 (TDM 解包/增益/VAD): [`capture`]
//! - 定长帧组装 (60ms opus 帧): [`encode`]
//! - 正弦测试音生成: [`tone`]
//! - 声音唤醒 (检测器 trait/唤醒策略): [`wake`]
//! - 播报播放流程 (预缓冲/排空门控): [`playback`]

pub mod capture;
pub mod codec;
pub mod encode;
pub mod listen;
pub mod playback;
pub mod preroll;
pub mod sample_rate;
pub mod tone;
pub mod volume;
pub mod wake;
