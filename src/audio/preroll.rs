//! 唤醒预录环 — 纯逻辑, host 可单测
//!
//! 保存最近 `buf.len()` 个原始 PCM 样本; 唤醒建连期间语音不会丢。
//! 用全局递增序号 `seq` 定位样本, 支持从任意历史点分段读取。

/// 固定容量预录环
#[derive(Debug)]
pub struct PrerollRing<'a> {
    buf: &'a mut [i16],
    w: usize,
    valid: usize,
    seq: u64,
}

impl<'a> PrerollRing<'a> {
    pub fn new(buf: &'a mut [i16]) -> Self {
        Self {
            buf,
            w: 0,
            valid: 0,
            seq: 0,
        }
    }

    /// 追加原始样本 (环形覆盖最旧数据)
    pub fn push(&mut self, samples: &[i16]) {
        if self.buf.is_empty() {
            self.seq = self.seq.wrapping_add(samples.len() as u64);
            return;
        }
        for &s in samples {
            self.buf[self.w] = s;
            self.seq = self.seq.wrapping_add(1);
            self.w = (self.w + 1) % self.buf.len();
            if self.valid < self.buf.len() {
                self.valid += 1;
            }
        }
    }

    /// 已写入的总样本数 (全局序号)
    pub fn seq(&self) -> u64 {
        self.seq
    }

    /// 当前环内有效样本数
    pub fn valid(&self) -> usize {
        self.valid
    }

    /// 从 `from_seq` 起仍可读的样本数 (被覆盖的旧数据自动截断)
    pub fn available_from(&self, from_seq: u64) -> usize {
        (self.seq.saturating_sub(from_seq) as usize).min(self.valid)
    }

    /// 读取 `[from_seq + offset, ...)` 到 `out`, 返回写入样本数
    pub fn read_from(&self, from_seq: u64, offset: usize, out: &mut [i16]) -> usize {
        if self.buf.is_empty() {
            return 0;
        }
        let total = self.available_from(from_seq);
        if offset >= total {
            return 0;
        }
        // 最旧可读样本的环内下标
        let start = (self.w + self.buf.len() - total) % self.buf.len();
        let n = out.len().min(total - offset);
        for (i, slot) in out[..n].iter_mut().enumerate() {
            *slot = self.buf[(start + offset + i) % self.buf.len()];
        }
        n
    }
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;

    #[test]
    fn keeps_latest_samples_and_reports_seq() {
        let mut backing = [0i16; 4];
        let mut ring = PrerollRing::new(&mut backing);
        ring.push(&[1, 2, 3, 4, 5]);
        assert_eq!(ring.seq(), 5);
        assert_eq!(ring.valid(), 4);
        let mut out = [0i16; 4];
        assert_eq!(ring.read_from(0, 0, &mut out), 4);
        assert_eq!(out, [2, 3, 4, 5]);
    }

    #[test]
    fn reads_from_history_and_offset() {
        let mut backing = [0i16; 4];
        let mut ring = PrerollRing::new(&mut backing);
        ring.push(&[1, 2, 3, 4, 5]);
        let mut out = [0i16; 4];
        assert_eq!(ring.read_from(3, 0, &mut out), 2);
        assert_eq!(&out[..2], &[4, 5]);
        assert_eq!(ring.read_from(3, 1, &mut out), 1);
        assert_eq!(out[0], 5);
        assert_eq!(ring.read_from(5, 0, &mut out), 0);
        assert_eq!(ring.available_from(3), 2);
        assert_eq!(ring.available_from(0), 4); // 被覆盖的截断
    }

    #[test]
    fn chunked_reads_cover_interval() {
        let mut backing = [0i16; 8];
        let mut ring = PrerollRing::new(&mut backing);
        for i in 0..20i16 {
            ring.push(&[i]);
        }
        // 最新 8 个: 12..19
        let mut out = [0i16; 3];
        let mut got = std::vec::Vec::new();
        let mut off = 0;
        loop {
            let n = ring.read_from(12, off, &mut out);
            if n == 0 {
                break;
            }
            got.extend_from_slice(&out[..n]);
            off += n;
        }
        assert_eq!(got, (12i16..20).collect::<std::vec::Vec<_>>());
    }

    #[test]
    fn empty_ring_is_safe() {
        let mut backing: [i16; 0] = [];
        let mut ring = PrerollRing::new(&mut backing);
        ring.push(&[1, 2, 3]);
        assert_eq!(ring.seq(), 3);
        assert_eq!(ring.valid(), 0);
        let mut out = [0i16; 2];
        assert_eq!(ring.read_from(0, 0, &mut out), 0);
    }
}
