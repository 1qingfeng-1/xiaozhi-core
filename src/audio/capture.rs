//! 采集处理纯逻辑 — TDM 槽位解包 / 软件增益 / 能量 VAD
//!
//! 与硬件无关: 输入为 DMA 读出的 i16-LE 交错字节流, host 可单元测试。
//! 固件层 (esp-hal I2S RX) 只负责搬运字节, 槽位语义由本模块解释。

/// 从交错 i16-LE TDM 帧中提取一个槽位
///
/// - `frames`: DMA 原始字节, 长度 = n_frames × `slot_count` × 2;
/// - `slot_index`: 目标槽位 (0 起始);
/// - `out`: 输出样本缓冲 (单槽位声道), 不足部分截断。
///
/// 返回写入 `out` 的样本数; 参数非法返回 0。
pub fn extract_slot(frames: &[u8], slot_count: usize, slot_index: usize, out: &mut [i16]) -> usize {
    if slot_count == 0 || slot_index >= slot_count {
        return 0;
    }
    let frame_bytes = slot_count * 2;
    if frames.len() < frame_bytes {
        return 0;
    }
    let n = (frames.len() / frame_bytes).min(out.len());
    for (i, sample) in out[..n].iter_mut().enumerate() {
        let o = i * frame_bytes + slot_index * 2;
        *sample = i16::from_le_bytes([frames[o], frames[o + 1]]);
    }
    n
}

/// 软件增益 (百分比, 100 = 0 dB), 饱和处理防溢出
///
/// 支持 >100 (放大) 与 0..100 (衰减); 0 = 数字静音。
pub fn apply_gain_percent(samples: &mut [i16], gain_percent: u16) {
    for s in samples {
        let amplified = (*s as i32) * (gain_percent as i32) / 100;
        *s = amplified.clamp(i16::MIN as i32, i16::MAX as i32) as i16;
    }
}

/// 峰值限幅: 整帧峰值超过 `ceiling` 时按比例缩回 (防削顶失真)
///
/// 语音帧的原始峰值动态范围大 (安静句 rms 200 / 峰值 2700+), 固定软件增益
/// 要么太小声要么削顶。逐帧限幅在保电平的同时避免 i16 饱和 — 削顶会让
/// 服务端 VAD/ASR 把语音当噪声 (真机 2026-09-26 定位)。
/// 返回是否发生了限幅 (供诊断)。
pub fn limit_peak(samples: &mut [i16], ceiling: i16) -> bool {
    let peak = samples.iter().map(|&s| (s as i32).abs()).max().unwrap_or(0);
    let ceiling = ceiling.max(1) as i32;
    if peak <= ceiling {
        return false;
    }
    for s in samples {
        *s = ((*s as i32) * ceiling / peak) as i16;
    }
    true
}

/// 能量 VAD: 去除 DC (帧均值) 后的均方幅度 (RMS) ≥ `threshold` 视为有声音
///
/// 原始采集无高通 (C++ 参考靠 AFE 内 2.8Hz 高通), 故先减帧均值再测能量:
/// DC 偏置/50Hz 哼声不会顶高能量致假触发 (真机开机假唤醒的根因之一)。
/// 帧长 ~64ms, 等效高通 ~15Hz, 人声基频 (85Hz+) 不受影响。
/// 极简实现 (无降噪/频域), 供回环测试与半双工状态机粗判。
pub fn is_voice_active(samples: &[i16], threshold: i16) -> bool {
    if samples.is_empty() {
        return false;
    }
    let n = samples.len() as i64;
    let sum: i64 = samples.iter().map(|&s| s as i64).sum();
    let mean = sum / n;
    let sum_dev_sq: u64 = samples
        .iter()
        .map(|&s| {
            let d = (s as i64) - mean;
            (d * d) as u64
        })
        .sum();
    let mean_dev_sq = sum_dev_sq / n as u64;
    mean_dev_sq >= (threshold as u64) * (threshold as u64)
}

/// 帧均值 (DC) 与均方幅度 (RMS), 供电平诊断/阈值调参
pub fn level_stats(samples: &[i16]) -> (i32, i32) {
    let n = samples.len();
    if n == 0 {
        return (0, 0);
    }
    let sum = samples.iter().map(|&s| s as i32).sum::<i32>();
    let mean = sum / n as i32;
    let sum_dev_sq: i64 = samples
        .iter()
        .map(|&s| {
            let d = (s as i64) - mean as i64;
            d * d
        })
        .sum();
    (mean, (sum_dev_sq / n as i64).isqrt() as i32)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 构造交错帧字节: 每帧 3 个 i16-LE 写入 buf
    fn write_frames(buf: &mut [u8], frames: &[[i16; 3]]) {
        for (i, f) in frames.iter().enumerate() {
            for (j, s) in f.iter().enumerate() {
                let le = s.to_le_bytes();
                let o = (i * 3 + j) * 2;
                buf[o..o + 2].copy_from_slice(&le);
            }
        }
    }

    #[test]
    fn extract_slot_picks_requested_slot() {
        let mut bytes = [0u8; 12];
        write_frames(&mut bytes, &[[100, -200, 300], [-400, 500, -600]]);
        let mut out = [0i16; 8];
        // 3 槽位: 槽 0 / 1 / 2
        assert_eq!(extract_slot(&bytes, 3, 0, &mut out), 2);
        assert_eq!(&out[..2], &[100, -400]);
        let mut out2 = [0i16; 8];
        assert_eq!(extract_slot(&bytes, 3, 1, &mut out2), 2);
        assert_eq!(&out2[..2], &[-200, 500]);
        let mut out3 = [0i16; 8];
        assert_eq!(extract_slot(&bytes, 3, 2, &mut out3), 2);
        assert_eq!(&out3[..2], &[300, -600]);
    }

    #[test]
    fn extract_slot_truncates_to_output_len() {
        let mut bytes = [0u8; 18];
        write_frames(&mut bytes, &[[1, 2, 3], [4, 5, 6], [7, 8, 9]]);
        let mut out = [0i16; 2];
        assert_eq!(extract_slot(&bytes, 3, 1, &mut out), 2);
        assert_eq!(&out, &[2, 5]);
    }

    #[test]
    fn extract_slot_rejects_bad_args() {
        let mut out = [0i16; 4];
        assert_eq!(extract_slot(&[], 2, 0, &mut out), 0); // 无数据
        assert_eq!(extract_slot(&[0u8; 3], 2, 0, &mut out), 0); // 不足一帧
        assert_eq!(extract_slot(&[0u8; 8], 2, 2, &mut out), 0); // 槽位越界
        assert_eq!(extract_slot(&[0u8; 8], 0, 0, &mut out), 0); // 槽数为 0
    }

    #[test]
    fn gain_100_is_identity() {
        let mut s = [0, 1, -1, 32767, -32768];
        apply_gain_percent(&mut s, 100);
        assert_eq!(s, [0, 1, -1, 32767, -32768]);
    }

    #[test]
    fn gain_saturates_and_mutes() {
        let mut s = [20000, -20000];
        apply_gain_percent(&mut s, 200); // x2 → 饱和
        assert_eq!(s, [32767, -32768]);
        let mut m = [1234, -5678];
        apply_gain_percent(&mut m, 0); // 数字静音
        assert_eq!(m, [0, 0]);
        let mut h = [100, -100];
        apply_gain_percent(&mut h, 50); // 半增益
        assert_eq!(h, [50, -50]);
    }

    /// 零 DC 交替信号 (前 n 个样本 ±amp, 其余 0)
    fn alternating(amp: i16, n: usize) -> [i16; 16] {
        let mut s = [0i16; 16];
        for (i, v) in s.iter_mut().enumerate().take(n) {
            *v = if i % 2 == 0 { amp } else { -amp };
        }
        s
    }

    #[test]
    fn vad_detects_silence_and_voice() {
        assert!(!is_voice_active(&[], 100));
        assert!(!is_voice_active(&[0, 0, 0], 100));
        assert!(!is_voice_active(&[10, -10, 10, -10], 100)); // RMS 10 < 100
        assert!(is_voice_active(&[100, -100, 100, -100], 100)); // RMS 100
        assert!(is_voice_active(&alternating(1000, 16), 500)); // RMS 1000 > 500
    }

    #[test]
    fn vad_ignores_dc_offset() {
        // 纯 DC: 电平再高无 AC 分量也不触发
        assert!(!is_voice_active(&[5000; 16], 100));
        // DC + AC: 去 DC 后 AC 分量仍触发
        let mut s = [0i16; 16];
        for (i, v) in s.iter_mut().enumerate() {
            *v = if i % 2 == 0 { 2000 + 300 } else { 2000 - 300 };
        }
        assert!(is_voice_active(&s, 100));
    }

    #[test]
    fn level_stats_reports_mean_and_rms() {
        assert_eq!(level_stats(&[100, -100, 100, -100]), (0, 100));
        assert_eq!(level_stats(&[1000; 4]), (1000, 0));
        assert_eq!(level_stats(&[]), (0, 0));
    }

    #[test]
    fn limit_peak_scales_without_clipping() {
        // 未超限: 原样返回, 无限幅
        let mut s = [100, -200, 300];
        assert!(!limit_peak(&mut s, 1000));
        assert_eq!(s, [100, -200, 300]);
        // 超限: 整帧等比缩到 ceiling, 波形形状保持
        let mut s = [30000, -15000, 0];
        assert!(limit_peak(&mut s, 3000));
        assert_eq!(s, [3000, -1500, 0]);
        // 负峰更大时也正确
        let mut s = [100, -32768];
        assert!(limit_peak(&mut s, 16384));
        assert_eq!(s, [50, -16384]);
    }
}
