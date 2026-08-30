//! 小智音箱 - 纯逻辑核心层
//!
//! 本 crate 只包含**与硬件无关**的纯逻辑，可在 host (std) 上单元测试：
//! - 音量/增益映射
//! - 采样率与 I2S 时钟计算
//! - Codec 寄存器配置序列
//! - 音频状态机
//! - 板级规则 (电源状态分类等)
//!
//! 硬件相关代码（I2S/I2C 寄存器读写、DMA）放在 `xiaozhi-s3` 固件 crate。

#![no_std]
#![deny(unsafe_code)]

pub mod audio;
pub mod board;
pub mod dialog;
pub mod display;
pub mod mcp;
pub mod ota;
