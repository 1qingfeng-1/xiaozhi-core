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

/// 能量 VAD: 均方幅度 ≥ `threshold` 视为有声音
///
/// 极简实现 (无降噪/频域), 供回环测试与半双工状态机粗判。
pub fn is_voice_active(samples: &[i16], threshold: i16) -> bool {
    if samples.is_empty() {
        return false;
    }
    let sum_sq: u64 = samples
        .iter()
        .map(|&s| {
            let v = s as i64;
            (v * v) as u64
        })
        .sum();
    let mean_sq = sum_sq / samples.len() as u64;
    mean_sq >= (threshold as u64) * (threshold as u64)
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

    #[test]
    fn vad_detects_silence_and_voice() {
        assert!(!is_voice_active(&[], 100));
        assert!(!is_voice_active(&[0, 0, 0], 100));
        assert!(!is_voice_active(&[10, -10, 10], 100)); // 均方 100 < 100^2
        assert!(is_voice_active(&[100, -100, 100], 100)); // 均方 100^2
        assert!(is_voice_active(&[1000; 16], 500));
    }
}
