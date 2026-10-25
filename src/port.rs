//! 端口 (ports) — 固件编排层的同步抽象边界 (ADR-0005 步骤 2)
//!
//! 这里只定义与硬件无关的接口与一个可 host 测试的搬运函数。
//! 真实适配器 (I2S / codec / DMA) 仍在固件侧实现; 本模块**不改变现有
//! 音频路径**, 供后续把 `app::run` 的编排抽到 host 测试时使用。
//!
//! 设计约束:
//! - 只做单声道 PCM (16kHz `i16`), 立体声/字节序转换属于适配器;
//! - 不使用 trait object / async, 避免固件侧额外的动态分发与状态机;
//! - 错误类型保持 `Copy`, 便于在无堆环境传递。

/// 端口错误
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PortError {
    /// 设备未就绪 (未上电/未配置)
    Unavailable,
    /// 底层 I/O 失败 (DMA/总线)
    Io,
    /// 暂时无空间/忙, 调用方应稍后重试
    Busy,
}

/// 单声道 PCM 采集源
pub trait PcmSource {
    /// 读取最多 `out.len()` 个样本, 返回实际写入的样本数。
    fn read(&mut self, out: &mut [i16]) -> Result<usize, PortError>;
}

/// 单声道 PCM 播放汇
pub trait PcmSink {
    /// 写入一块单声道 PCM; 实现方负责立体声/字节序转换。
    fn write(&mut self, pcm: &[i16]) -> Result<(), PortError>;
}

/// 采集后处理参数
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CaptureConfig {
    /// 软件增益百分比 (100 = 不放大)
    pub gain_percent: u16,
    /// 限幅峰值
    pub ceiling: i16,
}

impl Default for CaptureConfig {
    fn default() -> Self {
        Self {
            gain_percent: 100,
            ceiling: i16::MAX,
        }
    }
}

/// 从采集源读一块 PCM, 应用软件增益与限幅后写入 sink。
///
/// 返回处理的样本数。源返回 0 时不写 sink; 源返回超过缓冲长度时按缓冲截断
/// (防御异常适配器)。错误原样向上传递。
pub fn pump_capture<S: PcmSource, K: PcmSink>(
    src: &mut S,
    sink: &mut K,
    buf: &mut [i16],
    cfg: CaptureConfig,
) -> Result<usize, PortError> {
    let n = src.read(buf)?.min(buf.len());
    if n == 0 {
        return Ok(0);
    }
    crate::audio::capture::apply_gain_percent(&mut buf[..n], cfg.gain_percent);
    crate::audio::capture::limit_peak(&mut buf[..n], cfg.ceiling);
    sink.write(&buf[..n])?;
    Ok(n)
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use std::vec::Vec;

    struct MockSource {
        samples: Vec<i16>,
        reported: Option<usize>,
        error: Option<PortError>,
    }

    impl MockSource {
        fn new(samples: &[i16]) -> Self {
            Self {
                samples: samples.to_vec(),
                reported: None,
                error: None,
            }
        }
    }

    impl PcmSource for MockSource {
        fn read(&mut self, out: &mut [i16]) -> Result<usize, PortError> {
            if let Some(e) = self.error {
                return Err(e);
            }
            let n = self.samples.len().min(out.len());
            out[..n].copy_from_slice(&self.samples[..n]);
            Ok(self.reported.unwrap_or(self.samples.len()))
        }
    }

    struct MockSink {
        got: Vec<i16>,
        error: Option<PortError>,
    }

    impl MockSink {
        fn new() -> Self {
            Self {
                got: Vec::new(),
                error: None,
            }
        }
    }

    impl PcmSink for MockSink {
        fn write(&mut self, pcm: &[i16]) -> Result<(), PortError> {
            if let Some(e) = self.error {
                return Err(e);
            }
            self.got.extend_from_slice(pcm);
            Ok(())
        }
    }

    #[test]
    fn default_config_is_neutral() {
        let mut src = MockSource::new(&[100, -100, 30000, -30000]);
        let mut sink = MockSink::new();
        let mut buf = [0i16; 8];
        let n = pump_capture(&mut src, &mut sink, &mut buf, CaptureConfig::default()).unwrap();
        assert_eq!(n, 4);
        assert_eq!(sink.got, [100, -100, 30000, -30000]);
    }

    #[test]
    fn gain_and_limit_are_applied_before_sink() {
        // 先 150% 增益 → [1500, -1500, 6000, -6000];
        // 峰值 6000 超过 ceiling 3000, 整帧按比例缩放 0.5 → [750, -750, 3000, -3000]
        let mut src = MockSource::new(&[1000, -1000, 4000, -4000]);
        let mut sink = MockSink::new();
        let mut buf = [0i16; 8];
        let cfg = CaptureConfig {
            gain_percent: 150,
            ceiling: 3000,
        };
        let n = pump_capture(&mut src, &mut sink, &mut buf, cfg).unwrap();
        assert_eq!(n, 4);
        assert_eq!(sink.got, [750, -750, 3000, -3000]);
    }

    #[test]
    fn source_longer_than_buffer_is_clamped() {
        let mut src = MockSource::new(&[1, 2, 3, 4, 5, 6, 7, 8]);
        src.reported = Some(8);
        let mut sink = MockSink::new();
        let mut buf = [0i16; 4];
        let n = pump_capture(&mut src, &mut sink, &mut buf, CaptureConfig::default()).unwrap();
        assert_eq!(n, 4);
        assert_eq!(sink.got.len(), 4);
    }

    #[test]
    fn empty_read_does_not_touch_sink() {
        let mut src = MockSource::new(&[]);
        let mut sink = MockSink::new();
        let mut buf = [0i16; 4];
        assert_eq!(
            pump_capture(&mut src, &mut sink, &mut buf, CaptureConfig::default()),
            Ok(0)
        );
        assert!(sink.got.is_empty());
    }

    #[test]
    fn source_error_propagates() {
        let mut src = MockSource::new(&[1, 2, 3]);
        src.error = Some(PortError::Io);
        let mut sink = MockSink::new();
        let mut buf = [0i16; 4];
        assert_eq!(
            pump_capture(&mut src, &mut sink, &mut buf, CaptureConfig::default()),
            Err(PortError::Io)
        );
        assert!(sink.got.is_empty());
    }

    #[test]
    fn sink_error_propagates() {
        let mut src = MockSource::new(&[1, 2, 3]);
        let mut sink = MockSink::new();
        sink.error = Some(PortError::Busy);
        let mut buf = [0i16; 4];
        assert_eq!(
            pump_capture(&mut src, &mut sink, &mut buf, CaptureConfig::default()),
            Err(PortError::Busy)
        );
    }
}
