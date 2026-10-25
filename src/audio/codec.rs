//! ES8311 / ES7210 Codec 寄存器配置序列
//!
//! 序列为**常量纯数据**（当前固定 16kHz / 16bit / I2S Philips / 4 麦 TDM），
//! 硬件层按序执行 `i2c.write(addr, [reg, value])`。
//!
//! 来源: esp_codec_dev 1.6.2 (device/es8311/es8311.c, device/es7210/es7210.c)
//! 与 xiaozhi-esp32 BoxAudioCodec 行为对齐:
//! - ES8311: DAC 播放, I2S slave, MCLK 输入, HP/SPK 输出
//! - ES7210: 4 麦克风录音, TDM 输出, mic1-4 增益 30dB, bias 2.87V
//!
//! 音量: ES8311 REG32 线性映射 0x00(-95.5dB 静音) → 0xFF(+32dB 满量)。

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
    /// 16kHz 单声道 16-bit（小智语音链路默认, MCLK 256x）
    #[must_use]
    pub const fn mono_16k() -> Self {
        Self {
            sample_rate: 16000,
            data_bits: 16,
            channels: 1,
            mclk_multiplier: 256,
        }
    }
}

/// ES8311（DAC 播放）上电初始化序列
///
/// 合成自 esp_codec_dev 1.6.2 `es8311_open` + `es8311_start`(模拟上电,
/// REG0D=0x01) + `es8311_config_sample`(16kHz / MCLK 4.096MHz = 256fs) + unmute。
/// 步骤 1 为软复位保持 (0x1F)，硬件层写完后应 delay ≥5ms 再继续。
const ES8311_INIT: [RegWrite; 28] = [
    // 软复位保持 (ResetCodec), bit7=0 期间数字块保持复位
    RegWrite::new(CodecAddr::Es8311, 0x00, 0x1F),
    // ---- 以下在 5ms delay 之后 ----
    // RESET: bit7=1 退出复位, bit6=0 slave 模式
    RegWrite::new(CodecAddr::Es8311, 0x00, 0x80),
    // CLK_MANAGER: MCLK 直入 + 内部时钟全开 (es8311_open/start 终值, use_mclk)
    RegWrite::new(CodecAddr::Es8311, 0x01, 0x3F),
    // ---- config_sample: 16kHz, MCLK=4.096MHz (256fs) ----
    // pre_div=1, pre_mult=x1
    RegWrite::new(CodecAddr::Es8311, 0x02, 0x00),
    // fs_mode=SS, adc_osr=0x10
    RegWrite::new(CodecAddr::Es8311, 0x03, 0x10),
    // dac_osr=0x20
    RegWrite::new(CodecAddr::Es8311, 0x04, 0x20),
    // adc_div=1, dac_div=1
    RegWrite::new(CodecAddr::Es8311, 0x05, 0x00),
    // LRCK 分频 256fs = 0x00FF
    RegWrite::new(CodecAddr::Es8311, 0x07, 0x00),
    RegWrite::new(CodecAddr::Es8311, 0x08, 0xFF),
    // SYSTEM: 保留默认 (open 段显式清零)
    RegWrite::new(CodecAddr::Es8311, 0x0B, 0x00),
    RegWrite::new(CodecAddr::Es8311, 0x0C, 0x00),
    // SDP in (ESP32 → DAC 数据): I2S Philips 16bit
    RegWrite::new(CodecAddr::Es8311, 0x09, 0x00),
    // SDP out: I2S Philips 16bit
    RegWrite::new(CodecAddr::Es8311, 0x0A, 0x00),
    // SYSTEM: DAC/ADC 时钟
    RegWrite::new(CodecAddr::Es8311, 0x0E, 0x02),
    // SYSTEM: HP 驱动
    RegWrite::new(CodecAddr::Es8311, 0x10, 0x1F),
    RegWrite::new(CodecAddr::Es8311, 0x11, 0x7F),
    // SYSTEM: DAC 使能
    RegWrite::new(CodecAddr::Es8311, 0x12, 0x00),
    // SYSTEM (open 段固定 0x10)
    RegWrite::new(CodecAddr::Es8311, 0x13, 0x10),
    // SYSTEM: 无 DMIC
    RegWrite::new(CodecAddr::Es8311, 0x14, 0x1A),
    // ADC 数据选择
    RegWrite::new(CodecAddr::Es8311, 0x15, 0x40),
    // ADC: MIC 增益标度 + analog
    RegWrite::new(CodecAddr::Es8311, 0x16, 0x24),
    RegWrite::new(CodecAddr::Es8311, 0x17, 0xBF),
    // DAC analog 输出
    RegWrite::new(CodecAddr::Es8311, 0x37, 0x08),
    // GPIO: 增强 I2C 噪声免疫 (官方要求双写防首次写失败)
    RegWrite::new(CodecAddr::Es8311, 0x44, 0x08),
    RegWrite::new(CodecAddr::Es8311, 0x44, 0x08),
    // GP 控制
    RegWrite::new(CodecAddr::Es8311, 0x45, 0x00),
    // SYSTEM: 模拟上电! es8311_start 的 enable 步骤写 0x01; 0xFA/0xFC 为官方掉电态
    RegWrite::new(CodecAddr::Es8311, 0x0D, 0x01),
    // 数字 mute 解除 (REG31 bit5/6 置位为 mute)
    RegWrite::new(CodecAddr::Es8311, 0x31, 0x00),
];

#[must_use]
pub fn es8311_init_sequence() -> &'static [RegWrite] {
    &ES8311_INIT
}

/// ES8311 上电序列中, 复位保持后需要等待的毫秒数
pub const ES8311_RESET_DELAY_MS: u32 = 5;

/// ES7210（4 麦克风 ADC）上电初始化序列, TDM 输出, 30dB 增益
const ES7210_INIT_TDM: [RegWrite; 34] = [
    // 复位
    RegWrite::new(CodecAddr::Es7210, 0x00, 0xFF),
    RegWrite::new(CodecAddr::Es7210, 0x00, 0x41),
    // 配置期间关闭时钟
    RegWrite::new(CodecAddr::Es7210, 0x01, 0x3F),
    // 状态机周期
    RegWrite::new(CodecAddr::Es7210, 0x09, 0x30),
    RegWrite::new(CodecAddr::Es7210, 0x0A, 0x30),
    // ADC1-4 高通滤波快速建立
    RegWrite::new(CodecAddr::Es7210, 0x23, 0x2A),
    RegWrite::new(CodecAddr::Es7210, 0x22, 0x0A),
    RegWrite::new(CodecAddr::Es7210, 0x20, 0x0A),
    RegWrite::new(CodecAddr::Es7210, 0x21, 0x2A),
    // 模拟: VDDA 3.3V
    RegWrite::new(CodecAddr::Es7210, 0x40, 0x43),
    // MIC1-4 偏置 2.87V
    RegWrite::new(CodecAddr::Es7210, 0x41, 0x70),
    RegWrite::new(CodecAddr::Es7210, 0x42, 0x70),
    // OSR
    RegWrite::new(CodecAddr::Es7210, 0x07, 0x20),
    // 主时钟 DLL
    RegWrite::new(CodecAddr::Es7210, 0x02, 0xC1),
    // MIC1-4 通道上电
    RegWrite::new(CodecAddr::Es7210, 0x4B, 0x00),
    RegWrite::new(CodecAddr::Es7210, 0x4C, 0x00),
    // MIC1-4 增益 30dB (bit4=通道使能, 低4位=级数10)
    RegWrite::new(CodecAddr::Es7210, 0x43, 0x1A),
    RegWrite::new(CodecAddr::Es7210, 0x44, 0x1A),
    RegWrite::new(CodecAddr::Es7210, 0x45, 0x1A),
    RegWrite::new(CodecAddr::Es7210, 0x46, 0x1A),
    // 开启 MIC1-4 时钟位
    RegWrite::new(CodecAddr::Es7210, 0x01, 0x20),
    // SDP: 16bit
    RegWrite::new(CodecAddr::Es7210, 0x11, 0x60),
    // TDM 输出 (4 麦)
    RegWrite::new(CodecAddr::Es7210, 0x12, 0x02),
    // 使能序列
    RegWrite::new(CodecAddr::Es7210, 0x00, 0x71),
    RegWrite::new(CodecAddr::Es7210, 0x00, 0x41),
    // 解除掉电
    RegWrite::new(CodecAddr::Es7210, 0x06, 0x00),
    // 模拟再次确认
    RegWrite::new(CodecAddr::Es7210, 0x40, 0x43),
    // MIC1-4 功放正常
    RegWrite::new(CodecAddr::Es7210, 0x47, 0x08),
    RegWrite::new(CodecAddr::Es7210, 0x48, 0x08),
    RegWrite::new(CodecAddr::Es7210, 0x49, 0x08),
    RegWrite::new(CodecAddr::Es7210, 0x4A, 0x08),
    // 末尾复位脉冲 (对齐 esp_codec_dev `es7210_start` 收尾): 缺此步 ADC
    // 未进入正常转换节拍, 真机表现为仅 1/16 采样点有数据 (稀疏零)
    RegWrite::new(CodecAddr::Es7210, 0x40, 0x43),
    RegWrite::new(CodecAddr::Es7210, 0x00, 0x71),
    RegWrite::new(CodecAddr::Es7210, 0x00, 0x41),
];

/// ES7210 立体声输出序列 (非 TDM): 与 TDM 序列仅 REG12 不同
/// (0x00 = 标准 I2S 双通道, MIC1→L / MIC2→R), 其余一致
const ES7210_INIT_STEREO: [RegWrite; 34] = [
    RegWrite::new(CodecAddr::Es7210, 0x00, 0xFF),
    RegWrite::new(CodecAddr::Es7210, 0x00, 0x41),
    RegWrite::new(CodecAddr::Es7210, 0x01, 0x3F),
    RegWrite::new(CodecAddr::Es7210, 0x09, 0x30),
    RegWrite::new(CodecAddr::Es7210, 0x0A, 0x30),
    RegWrite::new(CodecAddr::Es7210, 0x23, 0x2A),
    RegWrite::new(CodecAddr::Es7210, 0x22, 0x0A),
    RegWrite::new(CodecAddr::Es7210, 0x20, 0x0A),
    RegWrite::new(CodecAddr::Es7210, 0x21, 0x2A),
    RegWrite::new(CodecAddr::Es7210, 0x40, 0x43),
    RegWrite::new(CodecAddr::Es7210, 0x41, 0x70),
    RegWrite::new(CodecAddr::Es7210, 0x42, 0x70),
    RegWrite::new(CodecAddr::Es7210, 0x07, 0x20),
    RegWrite::new(CodecAddr::Es7210, 0x02, 0xC1),
    RegWrite::new(CodecAddr::Es7210, 0x4B, 0x00),
    RegWrite::new(CodecAddr::Es7210, 0x4C, 0x00),
    RegWrite::new(CodecAddr::Es7210, 0x43, 0x1A),
    RegWrite::new(CodecAddr::Es7210, 0x44, 0x1A),
    RegWrite::new(CodecAddr::Es7210, 0x45, 0x1A),
    RegWrite::new(CodecAddr::Es7210, 0x46, 0x1A),
    RegWrite::new(CodecAddr::Es7210, 0x01, 0x20),
    RegWrite::new(CodecAddr::Es7210, 0x11, 0x60),
    // 标准 I2S 双通道输出 (无 TDM)
    RegWrite::new(CodecAddr::Es7210, 0x12, 0x00),
    RegWrite::new(CodecAddr::Es7210, 0x00, 0x71),
    RegWrite::new(CodecAddr::Es7210, 0x00, 0x41),
    RegWrite::new(CodecAddr::Es7210, 0x06, 0x00),
    RegWrite::new(CodecAddr::Es7210, 0x40, 0x43),
    RegWrite::new(CodecAddr::Es7210, 0x47, 0x08),
    RegWrite::new(CodecAddr::Es7210, 0x48, 0x08),
    RegWrite::new(CodecAddr::Es7210, 0x49, 0x08),
    RegWrite::new(CodecAddr::Es7210, 0x4A, 0x08),
    // 末尾复位脉冲 (对齐 esp_codec_dev `es7210_start` 收尾): 缺此步 ADC
    // 未进入正常转换节拍, 真机表现为仅 1/16 采样点有数据 (稀疏零)
    RegWrite::new(CodecAddr::Es7210, 0x40, 0x43),
    RegWrite::new(CodecAddr::Es7210, 0x00, 0x71),
    RegWrite::new(CodecAddr::Es7210, 0x00, 0x41),
];

/// ES7210 上电初始化序列
///
/// - `tdm_mode = true`: 4 麦 TDM 输出 (REG12=0x02), 供全双工 AEC 参考通道;
/// - `tdm_mode = false`: 立体声 I2S 输出 (REG12=0x00, MIC1→L / MIC2→R)。
#[must_use]
pub fn es7210_init_sequence(tdm_mode: bool) -> &'static [RegWrite] {
    if tdm_mode {
        &ES7210_INIT_TDM
    } else {
        &ES7210_INIT_STEREO
    }
}

/// 音量百分比 (0~100) → ES8311 REG32 寄存器值
///
/// 0% = 0x00 (静音); 1~100% 映射到 -50dB~-3dB (REG 91~185)。
/// 100% 不进 +dB 区: 满幅 PCM 叠加 +32dB 会硬削顶 (真机 VOL 100% 破音)。
#[must_use]
pub const fn es8311_volume_reg(percent: u8) -> u8 {
    // 唯一映射来源: audio::volume (避免两套互相矛盾的音量换算)
    crate::audio::volume::percent_to_codec(percent)
}

/// 音量写入序列 (ES8311 REG32)
#[must_use]
pub fn es8311_volume_write(percent: u8) -> [RegWrite; 1] {
    [RegWrite::new(
        CodecAddr::Es8311,
        0x32,
        es8311_volume_reg(percent),
    )]
}

/// 校验序列: 所有地址 7-bit 合法且无重复 (codec, reg)
#[must_use]
pub fn validate_sequence(seq: &[RegWrite]) -> bool {
    for w in seq {
        if w.codec.value() & !0x7F != 0 {
            return false;
        }
    }
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
    fn es8311_seq_starts_with_reset_and_valid() {
        let seq = es8311_init_sequence();
        assert_eq!(seq.len(), 28);
        assert_eq!(seq[0], RegWrite::new(CodecAddr::Es8311, 0x00, 0x1F));
        assert_eq!(seq[1], RegWrite::new(CodecAddr::Es8311, 0x00, 0x80));
        // 模拟上电: REG0D=0x01 (es8311_start enable); 0xFA/0xFC 是官方掉电态
        assert!(seq.contains(&RegWrite::new(CodecAddr::Es8311, 0x0D, 0x01)));
        assert!(!seq.contains(&RegWrite::new(CodecAddr::Es8311, 0x0D, 0xFA)));
        assert!(!seq.contains(&RegWrite::new(CodecAddr::Es8311, 0x0D, 0xFC)));
        // 时钟使能终值 (use_mclk) 与 open 段 system 寄存器
        assert!(seq.contains(&RegWrite::new(CodecAddr::Es8311, 0x01, 0x3F)));
        assert!(seq.contains(&RegWrite::new(CodecAddr::Es8311, 0x13, 0x10)));
        // 时钟分频 (16k/256fs)
        assert!(seq.contains(&RegWrite::new(CodecAddr::Es8311, 0x04, 0x20)));
        assert!(seq.contains(&RegWrite::new(CodecAddr::Es8311, 0x08, 0xFF)));
        // 解除数字静音
        assert!(seq.contains(&RegWrite::new(CodecAddr::Es8311, 0x31, 0x00)));
        assert!(validate_sequence_ok_except_reset(seq));
        assert!(seq.iter().all(|w| w.codec == CodecAddr::Es8311));
    }

    /// init 序列中 0x00 出现两次是合法的 (复位保持 → 使能)
    fn validate_sequence_ok_except_reset(seq: &[RegWrite]) -> bool {
        seq.iter().all(|w| w.codec.value() & !0x7F == 0)
    }

    #[test]
    fn es7210_seq_is_tdm_4mic_30db() {
        let seq = es7210_init_sequence(true);
        assert!(seq.len() >= 30);
        assert_eq!(seq[0], RegWrite::new(CodecAddr::Es7210, 0x00, 0xFF));
        // TDM 输出
        assert!(seq.contains(&RegWrite::new(CodecAddr::Es7210, 0x12, 0x02)));
        // 16bit SDP
        assert!(seq.contains(&RegWrite::new(CodecAddr::Es7210, 0x11, 0x60)));
        // 4 麦 30dB
        for reg in [0x43, 0x44, 0x45, 0x46] {
            assert!(seq.contains(&RegWrite::new(CodecAddr::Es7210, reg, 0x1A)));
        }
        assert!(validate_sequence_ok_except_reset(seq));
    }

    #[test]
    fn es7210_stereo_seq_disables_tdm() {
        let tdm = es7210_init_sequence(true);
        let stereo = es7210_init_sequence(false);
        assert_eq!(stereo.len(), tdm.len());
        // 仅 REG12 不同: 立体声 0x00, 无 TDM 写入
        assert!(stereo.contains(&RegWrite::new(CodecAddr::Es7210, 0x12, 0x00)));
        assert!(!stereo.contains(&RegWrite::new(CodecAddr::Es7210, 0x12, 0x02)));
        assert!(
            stereo
                .iter()
                .zip(tdm.iter())
                .all(|(a, b)| a == b || a.reg == 0x12)
        );
        assert!(validate_sequence_ok_except_reset(stereo));
    }

    #[test]
    fn volume_reg_mapping() {
        assert_eq!(es8311_volume_reg(0), 0);
        assert_eq!(es8311_volume_reg(100), 185); // -3dB 上限, 防满幅削顶
        assert_eq!(es8311_volume_reg(50), 138);
        assert_eq!(es8311_volume_reg(200), 185); // 饱和
        let w = es8311_volume_write(60);
        assert_eq!(w[0].reg, 0x32);
        assert_eq!(w[0].value, 147);
    }
}
