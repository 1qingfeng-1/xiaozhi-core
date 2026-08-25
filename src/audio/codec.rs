//! ES8311 / ES7210 Codec 寄存器配置序列
//!
//! 本模块把"上电后需要写哪些寄存器"建模为纯数据结构 + 纯函数，
//! 硬件层只需按序列逐条执行 I2C 写入。
//!
//! 寄存器地址/值参考：
//! - ES8311 datasheet（输出 codec，I2C addr 0x18）
//! - ES7210 datasheet（输入 codec，I2C addr 0x40）
//!
//! 这些序列是纯数据，可在 host 上单元测试（顺序、地址、值是否正确）。

/// I2C 7 位设备地址
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CodecAddr {
    /// ES8311 输出 codec (I2C 7-bit)
    Es8311 = 0x18,
    /// ES7210 输入 codec (I2C 7-bit)
    Es7210 = 0x40,
}

impl CodecAddr {
    #[must_use]
    pub const fn value(self) -> u8 {
        match self {
            Self::Es8311 => 0x18,
            Self::Es7210 => 0x40,
        }
    }
}

/// 一条寄存器写操作
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RegWrite {
    /// 目标 codec
    pub codec: CodecAddr,
    /// 寄存器地址（8-bit）
    pub reg: u8,
    /// 写入值（8-bit）
    pub value: u8,
}

impl RegWrite {
    #[must_use]
    pub const fn new(codec: CodecAddr, reg: u8, value: u8) -> Self {
        Self { codec, reg, value }
    }
}

/// 音频子系统运行配置
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct AudioConfig {
    /// 采样率 (Hz)
    pub sample_rate: u32,
    /// 每通道位宽
    pub data_bits: u32,
    /// 通道数
    pub channels: u32,
    /// MCLK 倍频 (256 或 384)
    pub mclk_multiplier: u32,
}

impl AudioConfig {
    /// 立体声 16-bit 48kHz（小智音箱默认）
    #[must_use]
    pub const fn stereo_16k_mono() -> Self {
        Self {
            sample_rate: 16000,
            data_bits: 16,
            channels: 1,
            mclk_multiplier: 256,
        }
    }

    #[must_use]
    pub const fn stereo_16bit_48k() -> Self {
        Self {
            sample_rate: 48000,
            data_bits: 16,
            channels: 2,
            mclk_multiplier: 256,
        }
    }
}

/// ES8311（输出 codec）上电初始化序列
///
/// 纯函数：给定配置，返回需要依次写入的寄存器操作列表。
/// 硬件层按顺序执行 `i2c.write(codec.addr, [reg, value])`。
#[must_use]
pub fn es8311_init_sequence(_cfg: AudioConfig) -> [RegWrite; 8] {
    [
        // 0x00 Reset: bit7=1 触发软复位
        RegWrite::new(CodecAddr::Es8311, 0x00, 0x80),
        // 0x02/0x03 系统时钟：MCLK 来自外部（bit 设置 MCLK 源）
        RegWrite::new(CodecAddr::Es8311, 0x02, 0x02),
        // 0x04 系统时钟分频（默认）
        RegWrite::new(CodecAddr::Es8311, 0x04, 0x14),
        // 0x06 主时钟选择
        RegWrite::new(CodecAddr::Es8311, 0x06, 0x02),
        // 0x12 主音量（静音起步，硬件层后续按音量映射写入）
        RegWrite::new(CodecAddr::Es8311, 0x12, 0x00),
        // 0x13 右声道音量
        RegWrite::new(CodecAddr::Es8311, 0x13, 0x00),
        // 0x15 数字接口：I2S Philips, MSB first
        RegWrite::new(CodecAddr::Es8311, 0x15, 0x30),
        // 0x17 输出通路使能（DAC + 耳机驱动）
        RegWrite::new(CodecAddr::Es8311, 0x17, 0x00),
    ]
}

/// ES7210（输入 codec / 麦克风）上电初始化序列
#[must_use]
pub fn es7210_init_sequence(_cfg: AudioConfig) -> [RegWrite; 8] {
    [
        // 0x00 Reset
        RegWrite::new(CodecAddr::Es7210, 0x00, 0x80),
        // 0x01 系统时钟源
        RegWrite::new(CodecAddr::Es7210, 0x01, 0x02),
        // 0x02 ADC 采样率
        RegWrite::new(CodecAddr::Es7210, 0x02, 0x14),
        // 0x04 数字接口：I2S Philips
        RegWrite::new(CodecAddr::Es7210, 0x04, 0x30),
        // 0x05 输入通路
        RegWrite::new(CodecAddr::Es7210, 0x05, 0x00),
        // 0x06 麦克风增益（MIC gain，起步 0dB）
        RegWrite::new(CodecAddr::Es7210, 0x06, 0x00),
        // 0x07 ADC 使能
        RegWrite::new(CodecAddr::Es7210, 0x07, 0x00),
        // 0x08 输入通路使能
        RegWrite::new(CodecAddr::Es7210, 0x08, 0x00),
    ]
}

/// 把音量映射结果（0~63 寄存器值）写成 ES8311 的左右声道音量寄存器操作。
///
/// 配合 [`super::volume::percent_to_codec`] 使用。
#[must_use]
pub fn es8311_volume_write(percent: u8) -> [RegWrite; 2] {
    let reg = super::volume::percent_to_codec(percent);
    [
        RegWrite::new(CodecAddr::Es8311, 0x12, reg), // 左声道主音量
        RegWrite::new(CodecAddr::Es8311, 0x13, reg), // 右声道音量
    ]
}

/// 校验一个寄存器序列：
/// - 没有重复的 (codec, reg) 冲突（同 codec 同 reg 出现多次视为可疑）
/// - 所有地址值合法（7-bit）
///
/// 返回 `true` 表示序列合法。纯函数，可单元测试。
#[must_use]
pub fn validate_sequence(seq: &[RegWrite]) -> bool {
    for w in seq {
        // I2C 7-bit 地址范围
        if w.codec.value() & !0x7F != 0 {
            return false;
        }
    }
    // 检查重复 (codec, reg)
    for i in 0..seq.len() {
        for j in (i + 1)..seq.len() {
            if seq[i].codec == seq[j].codec && seq[i].reg == seq[j].reg {
                return false;
            }
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codec_addr_values() {
        assert_eq!(CodecAddr::Es8311.value(), 0x18);
        assert_eq!(CodecAddr::Es7210.value(), 0x40);
    }

    #[test]
    fn es8311_seq_is_valid() {
        let seq = es8311_init_sequence(AudioConfig::stereo_16bit_48k());
        assert!(
            validate_sequence(&seq),
            "ES8311 init sequence must be valid"
        );
        // 第一条应是复位
        assert_eq!(seq[0], RegWrite::new(CodecAddr::Es8311, 0x00, 0x80));
        // 全部针对 Es8311
        assert!(seq.iter().all(|w| w.codec == CodecAddr::Es8311));
    }

    #[test]
    fn es7210_seq_is_valid() {
        let seq = es7210_init_sequence(AudioConfig::stereo_16bit_48k());
        assert!(
            validate_sequence(&seq),
            "ES7210 init sequence must be valid"
        );
        assert_eq!(seq[0], RegWrite::new(CodecAddr::Es7210, 0x00, 0x80));
        assert!(seq.iter().all(|w| w.codec == CodecAddr::Es7210));
    }

    #[test]
    fn volume_write_uses_mapping() {
        let writes = es8311_volume_write(100);
        // 100% → reg 0
        assert_eq!(writes[0].value, 0);
        assert_eq!(writes[1].value, 0);
        let writes0 = es8311_volume_write(0);
        assert_eq!(writes0[0].value, 63);
    }

    #[test]
    fn validate_detects_duplicate() {
        let bad = [
            RegWrite::new(CodecAddr::Es8311, 0x02, 0x00),
            RegWrite::new(CodecAddr::Es8311, 0x02, 0x01), // 同 reg 重复
        ];
        assert!(!validate_sequence(&bad));
    }

    #[test]
    fn config_defaults() {
        let cfg = AudioConfig::stereo_16bit_48k();
        assert_eq!(cfg.sample_rate, 48000);
        assert_eq!(cfg.data_bits, 16);
        assert_eq!(cfg.channels, 2);
        let mono = AudioConfig::stereo_16k_mono();
        assert_eq!(mono.sample_rate, 16000);
        assert_eq!(mono.channels, 1);
    }
}
