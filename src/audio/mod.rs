//! 音频纯逻辑
//!
//! - [`volume`]: 音量/增益映射（dB 步进 → 寄存器值）
//! - [`sample_rate`]: 采样率与 I2S 时钟分频计算
//! - [`codec`]: ES8311/ES7210 寄存器配置序列
//! - [`state`]: 音频子系统状态机

pub mod codec;
pub mod sample_rate;
pub mod state;
pub mod volume;
