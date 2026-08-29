//! 音频纯逻辑
//!
//! 子模块：
//! - 音量/增益映射: [`volume`]
//! - 采样率与 I2S 时钟分频计算: [`sample_rate`]
//! - ES8311/ES7210 寄存器配置序列: [`codec`]
//! - 采集处理 (TDM 解包/增益/VAD): [`capture`]
//! - 音频子系统状态机: [`state`]

pub mod capture;
pub mod codec;
pub mod sample_rate;
pub mod state;
pub mod volume;
