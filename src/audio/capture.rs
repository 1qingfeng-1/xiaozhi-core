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

/// 从 2 通道 i16-LE 交织帧中提取全部样本, 保持 L,R,L,R... 顺序
///
/// - `frames`: DMA 原始字节, 每帧 4B (L 2B + R 2B, 小端);
/// - `out_lr`: 输出缓冲, 写入 L,R 交织样本; 不足部分截断。
///
/// 返回写入的 i16 数量 (= 2 × 帧数); 奇数字节尾部被丢弃。
/// 用于 2 麦 WakeNet: 模型需要真实的 2 通道交织输入, 不能复制单通道。
pub fn extract_two_slots(frames: &[u8], out_lr: &mut [i16]) -> usize {
    let frames_n = (frames.len() / 4).min(out_lr.len() / 2);
    for i in 0..frames_n {
        let o = i * 4;
        out_lr[2 * i] = i16::from_le_bytes([frames[o], frames[o + 1]]);
        out_lr[2 * i + 1] = i16::from_le_bytes([frames[o + 2], frames[o + 3]]);
    }
    frames_n * 2
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

/// 上行前处理: 软件增益 + 峰值限幅
///
/// 返回是否触发限幅。聆听、播报 AEC、预录补发三条上行路径共用同一顺序，
/// 避免固件侧多处调用时顺序不一致。
pub fn condition_for_uplink(samples: &mut [i16], gain_percent: u16, ceiling: i16) -> bool {
    apply_gain_percent(samples, gain_percent);
    limit_peak(samples, ceiling)
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

/// WakeNet 输入块条件化: 去 DC + **峰值限制增益**（单块单增益，波形不变）。
///
/// 背景（真机 2026-11-08 定位）：
/// - 麦克风数字电平仅 1~5% 满幅，WakeNet 需要 1500% 级增益；
/// - 用户贴近设备/大声说唤醒词时，固定 15x 会把大量样本削顶成方波，
///   MFCC 被谐波破坏 → 模型不命中（表现为“播放提示音后无法唤醒”）；
/// - 旧实现去 DC 用 `i16::wrapping_sub`，削顶块减去较大的 DC 均值后
///   负半周回绕成正值，整块被折叠（日志里 `mean=±8000, rms≈18000`
///   的“饱和块”），进一步毁掉特征。
///
/// 本函数在 i32 域去 DC 并按 AC 峰值等比限制增益：正常块保持
/// `gain_percent`（默认 1500%），只有会超过 `target_peak` 的块才降增益，
/// 输出永不削顶。返回实际使用的增益（%）。
pub fn condition_wakenet_block(frame: &mut [i16], gain_percent: i32, target_peak: i32) -> i32 {
    let n = frame.len();
    if n == 0 {
        return gain_percent;
    }
    let sum = frame.iter().map(|&s| s as i32).sum::<i32>();
    // 必须在 i32 域保留均值: 削顶块的 AC 分量很大, 但均值仍可为任一 i16 值;
    // 真正的问题是减均值时用 i16 wrapping 会回绕。
    let mean = sum / n as i32;
    let mut peak = 0i32;
    for &s in frame.iter() {
        peak = peak.max((s as i32 - mean).abs());
    }
    // 纯 DC 块: 输出去 DC 后全零, 增益无意义
    let gain = if peak <= 0 {
        0
    } else {
        ((target_peak.max(1) as i64 * 100) / peak as i64).min(gain_percent.max(0) as i64) as i32
    };
    for s in frame.iter_mut() {
        let d = *s as i32 - mean;
        *s = ((d * gain) / 100).clamp(i16::MIN as i32, i16::MAX as i32) as i16;
    }
    gain
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wakenet_block_normal_gain_and_dc_removal() {
        // DC 3000 + ±100 交流: 正常电平块用满增益 1500%，输出以 0 为中心
        let mut f = [0i16; 64];
        for (i, s) in f.iter_mut().enumerate() {
            *s = 3000 + if i % 2 == 0 { 100 } else { -100 };
        }
        let gain = condition_wakenet_block(&mut f, 1500, 30_000);
        assert_eq!(gain, 1500);
        assert_eq!(f[0], 1500);
        assert_eq!(f[1], -1500);
        let (mean, _) = level_stats(&f);
        assert_eq!(mean, 0);
    }

    #[test]
    fn wakenet_block_limits_loud_blocks_without_clipping() {
        // ±3000 的响度: 1500% 会到 ±45000 削顶 → 限到 ±30000
        let mut f = [0i16; 64];
        for (i, s) in f.iter_mut().enumerate() {
            *s = if i % 2 == 0 { 3000 } else { -3000 };
        }
        let gain = condition_wakenet_block(&mut f, 1500, 30_000);
        assert_eq!(gain, 1000);
        assert_eq!(f[0], 30000);
        assert_eq!(f[1], -30000);
    }

    #[test]
    fn wakenet_block_clipped_square_wave_is_not_folded() {
        // 回归: 旧实现 i16::wrapping_sub 会把负半周 +32768 回绕成正值,
        // 整块均值变 +8000 级别。现在应从 i32 域去除 DC, 两侧对称。
        let mut f = [0i16; 512];
        for (i, s) in f.iter_mut().enumerate() {
            *s = if i % 2 == 0 { 32767 } else { -32768 };
        }
        let gain = condition_wakenet_block(&mut f, 1500, 30_000);
        assert_eq!(gain, 91); // AC 峰值 32768 → 30000*100/32768 = 91
        assert!(f.iter().all(|&s| s.abs() <= 30_000));
        let (mean, _) = level_stats(&f);
        assert!(mean.abs() <= 1);

        // 非对称削顶块 (1/3 负样本): 旧代码会因回绕得到正的虚假 DC
        let mut f = [0i16; 512];
        for (i, s) in f.iter_mut().enumerate() {
            *s = if i % 3 == 0 { -32768 } else { 32767 };
        }
        let gain = condition_wakenet_block(&mut f, 1500, 30_000);
        assert!(gain > 0 && gain < 100);
        let (mean, _) = level_stats(&f);
        assert!(mean.abs() <= 2);
        assert!(f.iter().all(|&s| s.abs() <= 30_000));
    }

    #[test]
    fn wakenet_block_handles_silence_and_empty() {
        let mut f = [123i16; 32];
        let gain = condition_wakenet_block(&mut f, 1500, 30_000);
        assert_eq!(gain, 0);
        assert!(f.iter().all(|&s| s == 0));
        let mut empty: [i16; 0] = [];
        assert_eq!(condition_wakenet_block(&mut empty, 1500, 30_000), 1500);
    }

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

    /// 构造 2 通道交织帧字节: 每帧 L,R 写入 buf
    fn write_stereo_frames(buf: &mut [u8], frames: &[[i16; 2]]) {
        for (i, f) in frames.iter().enumerate() {
            for (j, s) in f.iter().enumerate() {
                let le = s.to_le_bytes();
                let o = (i * 2 + j) * 2;
                buf[o..o + 2].copy_from_slice(&le);
            }
        }
    }

    #[test]
    fn extract_two_slots_keeps_interleaved_lr() {
        let mut bytes = [0u8; 16];
        write_stereo_frames(
            &mut bytes,
            &[[100, -200], [-300, 400], [500, -600], [700, -800]],
        );
        let mut out = [0i16; 8];
        assert_eq!(extract_two_slots(&bytes, &mut out), 8);
        assert_eq!(out, [100, -200, -300, 400, 500, -600, 700, -800]);
    }

    #[test]
    fn extract_two_slots_truncates_to_output_and_drops_odd_bytes() {
        let mut bytes = [0u8; 12];
        write_stereo_frames(&mut bytes, &[[1, 2], [3, 4], [5, 6]]);
        // 输出只能放 2 帧 (4 个 i16)
        let mut out = [0i16; 4];
        assert_eq!(extract_two_slots(&bytes, &mut out), 4);
        assert_eq!(out, [1, 2, 3, 4]);
        // 奇数字节: 只有完整 4B 帧参与, 尾部 3 字节丢弃
        let mut out2 = [0i16; 8];
        assert_eq!(extract_two_slots(&bytes[..7], &mut out2), 2);
        assert_eq!(&out2[..2], &[1, 2]);
    }

    #[test]
    fn extract_two_slots_handles_empty_and_negative_values() {
        let mut out = [0i16; 4];
        assert_eq!(extract_two_slots(&[], &mut out), 0);
        assert_eq!(extract_two_slots(&[0u8; 3], &mut out), 0);
        // 小端与负数: FF 7F -> 32767, 00 80 -> -32768
        let bytes = [0xFF, 0x7F, 0x00, 0x80];
        assert_eq!(extract_two_slots(&bytes, &mut out), 2);
        assert_eq!(&out[..2], &[32767, -32768]);
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

    #[test]
    fn condition_for_uplink_applies_gain_then_limit() {
        // 150% 增益后峰值为 6000, 超过 ceiling 3000 → 整帧缩放 0.5
        let mut s = [1000, -1000, 4000, -4000];
        assert!(condition_for_uplink(&mut s, 150, 3000));
        assert_eq!(s, [750, -750, 3000, -3000]);

        // 未超限时不缩放, 返回 false
        let mut s = [1000, -1000];
        assert!(!condition_for_uplink(&mut s, 150, 3000));
        assert_eq!(s, [1500, -1500]);
    }
}
