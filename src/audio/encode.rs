//! 定长帧组装 — 纯逻辑, host 可单测
//!
//! 固件每轮 pop 到的 PCM 长度不固定 (64~448ms), 需要累积到固定帧长
//! (60ms) 才能交给 opus。本模块把"填缓冲 → 凑满 → 回调 → 移位"的状态机
//! 从固件热路径抽出来, 保证跨轮余量、顺序与边界行为可测试。

/// 定长帧组装器
///
/// `acc` 与 `acc_len` 由调用方持有 (固件里是常驻缓冲与长度), 跨多次
/// [`push`](Self::push) 保留未凑满的余量。只在凑满一帧时调用回调,
/// 回调可安全地编码/入队。
pub struct FrameQueue<'a> {
    acc: &'a mut [i16],
    acc_len: &'a mut usize,
    frame: usize,
}

impl<'a> FrameQueue<'a> {
    /// `frame` 必须满足 `0 < frame <= acc.len()`, 否则 [`push`](Self::push)
    /// 直接返回 0 (防御配置错误, 避免固件热路径死循环)。
    pub fn new(acc: &'a mut [i16], acc_len: &'a mut usize, frame: usize) -> Self {
        Self {
            acc,
            acc_len,
            frame,
        }
    }

    /// 当前未凑满一帧的样本数
    pub fn pending(&self) -> usize {
        *self.acc_len
    }

    /// 追加一块 PCM, 每凑满一帧调用一次 `on_frame`, 返回本次产出的帧数。
    pub fn push(&mut self, src: &[i16], mut on_frame: impl FnMut(&[i16])) -> usize {
        if self.frame == 0 || self.frame > self.acc.len() || *self.acc_len > self.acc.len() {
            return 0;
        }
        let mut frames = 0usize;
        let mut src = src;
        while !src.is_empty() {
            let space = self.acc.len() - *self.acc_len;
            if space == 0 {
                // frame <= acc.len() 时不可达; 防御异常状态避免死循环。
                break;
            }
            let take = space.min(src.len());
            self.acc[*self.acc_len..*self.acc_len + take].copy_from_slice(&src[..take]);
            *self.acc_len += take;
            src = &src[take..];

            while *self.acc_len >= self.frame {
                on_frame(&self.acc[..self.frame]);
                self.acc.copy_within(self.frame..*self.acc_len, 0);
                *self.acc_len -= self.frame;
                frames += 1;
            }
        }
        frames
    }
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use std::vec;
    use std::vec::Vec;

    #[test]
    fn splits_into_frames_and_keeps_remainder_across_pushes() {
        let mut acc = [0i16; 8];
        let mut len = 0usize;
        let mut out: Vec<Vec<i16>> = Vec::new();

        let mut q = FrameQueue::new(&mut acc, &mut len, 4);
        assert_eq!(q.push(&[1, 2, 3], |f| out.push(f.to_vec())), 0);
        assert_eq!(q.pending(), 3);
        assert!(out.is_empty());

        assert_eq!(q.push(&[4, 5, 6], |f| out.push(f.to_vec())), 1);
        assert_eq!(q.pending(), 2);
        assert_eq!(out, vec![vec![1, 2, 3, 4]]);

        assert_eq!(q.push(&[7, 8], |f| out.push(f.to_vec())), 1);
        assert_eq!(q.pending(), 0);
        assert_eq!(out, vec![vec![1, 2, 3, 4], vec![5, 6, 7, 8]]);
    }

    #[test]
    fn emits_multiple_frames_from_one_push() {
        let mut acc = [0i16; 4];
        let mut len = 0usize;
        let mut out: Vec<Vec<i16>> = Vec::new();
        let mut q = FrameQueue::new(&mut acc, &mut len, 2);
        assert_eq!(q.push(&[1, 2, 3, 4, 5, 6], |f| out.push(f.to_vec())), 3);
        assert_eq!(out, vec![vec![1, 2], vec![3, 4], vec![5, 6]]);
        assert_eq!(q.pending(), 0);
    }

    #[test]
    fn degenerate_configuration_is_ignored() {
        let mut acc = [0i16; 4];
        let mut len = 0usize;

        let mut q = FrameQueue::new(&mut acc, &mut len, 0);
        assert_eq!(q.push(&[1, 2], |_| panic!("不应产出帧")), 0);

        let mut q = FrameQueue::new(&mut acc, &mut len, 5);
        assert_eq!(q.push(&[1, 2], |_| panic!("不应产出帧")), 0);
        assert_eq!(q.pending(), 0);
    }

    #[test]
    fn oversized_acc_len_is_rejected() {
        let mut acc = [0i16; 4];
        let mut len = 9usize;
        let mut q = FrameQueue::new(&mut acc, &mut len, 2);
        assert_eq!(q.push(&[1, 2], |_| panic!("不应产出帧")), 0);
    }
}
