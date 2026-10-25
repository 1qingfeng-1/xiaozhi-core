//! 显示点阵 — 3x5 ASCII 字库与文本度量 (纯逻辑, host 单测)
//!
//! 字形为 5 行 × 3bit (bit2 = 最左列), 逐字符渲染到面板窗口。
//! 小尺寸字库只保留状态文本需要的 ASCII 子集 (数字/大写字母/少量符号)。

/// 字形宽高 (像素)
pub const GLYPH_W: usize = 3;
pub const GLYPH_H: usize = 5;
/// 字符间距
pub const GLYPH_GAP: usize = 1;

/// 取字形 (返回 5 行, 每行 3bit; 无字形返回 None → 渲染为空白)
pub fn glyph_rows(c: u8) -> Option<[u8; GLYPH_H]> {
    const _: () = assert!(GLYPH_W == 3);
    let rows = match c {
        b'A' => [0b010, 0b101, 0b111, 0b101, 0b101],
        b'B' => [0b110, 0b101, 0b110, 0b101, 0b110],
        b'C' => [0b011, 0b100, 0b100, 0b100, 0b011],
        b'D' => [0b110, 0b101, 0b101, 0b101, 0b110],
        b'E' => [0b111, 0b100, 0b110, 0b100, 0b111],
        b'F' => [0b111, 0b100, 0b110, 0b100, 0b100],
        b'G' => [0b011, 0b100, 0b101, 0b101, 0b011],
        b'H' => [0b101, 0b101, 0b111, 0b101, 0b101],
        b'I' => [0b111, 0b010, 0b010, 0b010, 0b111],
        b'J' => [0b001, 0b001, 0b001, 0b101, 0b010],
        b'K' => [0b101, 0b101, 0b110, 0b101, 0b101],
        b'L' => [0b100, 0b100, 0b100, 0b100, 0b111],
        b'M' => [0b101, 0b111, 0b101, 0b101, 0b101],
        b'N' => [0b110, 0b101, 0b101, 0b101, 0b101],
        b'O' => [0b010, 0b101, 0b101, 0b101, 0b010],
        b'P' => [0b110, 0b101, 0b110, 0b100, 0b100],
        b'R' => [0b110, 0b101, 0b110, 0b101, 0b101],
        b'S' => [0b011, 0b100, 0b010, 0b001, 0b110],
        b'T' => [0b111, 0b010, 0b010, 0b010, 0b010],
        b'U' => [0b101, 0b101, 0b101, 0b101, 0b111],
        b'V' => [0b101, 0b101, 0b101, 0b101, 0b010],
        b'W' => [0b101, 0b101, 0b101, 0b111, 0b101],
        b'X' => [0b101, 0b101, 0b010, 0b101, 0b101],
        b'Y' => [0b101, 0b101, 0b010, 0b010, 0b010],
        b'Z' => [0b111, 0b001, 0b010, 0b100, 0b111],
        b'0' => [0b111, 0b101, 0b101, 0b101, 0b111],
        b'1' => [0b010, 0b110, 0b010, 0b010, 0b111],
        b'2' => [0b111, 0b001, 0b111, 0b100, 0b111],
        b'3' => [0b111, 0b001, 0b011, 0b001, 0b111],
        b'4' => [0b101, 0b101, 0b111, 0b001, 0b001],
        b'5' => [0b111, 0b100, 0b111, 0b001, 0b111],
        b'6' => [0b111, 0b100, 0b111, 0b101, 0b111],
        b'7' => [0b111, 0b001, 0b001, 0b001, 0b001],
        b'8' => [0b111, 0b101, 0b111, 0b101, 0b111],
        b'9' => [0b111, 0b101, 0b111, 0b001, 0b111],
        b'-' => [0b000, 0b000, 0b111, 0b000, 0b000],
        b'.' => [0b000, 0b000, 0b000, 0b000, 0b010],
        b':' => [0b000, 0b010, 0b000, 0b010, 0b000],
        b'%' => [0b101, 0b001, 0b010, 0b100, 0b101],
        b'/' => [0b001, 0b001, 0b010, 0b100, 0b100],
        b'+' => [0b000, 0b010, 0b111, 0b010, 0b000],
        b' ' => [0b000, 0b000, 0b000, 0b000, 0b000],
        _ => return None,
    };
    Some(rows)
}

/// 文本渲染后的像素尺寸 (宽 = n×3 + (n-1)×1)
pub fn text_size(text: &str) -> (usize, usize) {
    let n = text.chars().count();
    if n == 0 {
        return (0, 0);
    }
    (n * GLYPH_W + (n - 1) * GLYPH_GAP, GLYPH_H)
}

/// 文本是否全部可渲染 (避免运行期出现空白占位)
pub fn is_renderable(text: &str) -> bool {
    text.chars().all(|c| glyph_rows(c as u8).is_some())
}

/// 将一行 3bit 数据展开为像素掩码 (bit2 → 第 0 列)
pub fn row_pixels(row: u8) -> [bool; GLYPH_W] {
    [row & 0b100 != 0, row & 0b010 != 0, row & 0b001 != 0]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn needed_charset_is_renderable() {
        for text in [
            "XIAOZHI S3",
            "IDLE",
            "CONN",
            "LISTEN",
            "WAIT",
            "TTS",
            "VOL:70",
            "BATT:85%",
            "0123456789",
            "V 0.1.0",
        ] {
            assert!(is_renderable(text), "缺字形: {}", text);
        }
        assert!(!is_renderable("中文"));
    }

    #[test]
    fn glyph_rows_within_bounds() {
        for c in b' '..=b'~' {
            let Some(rows) = glyph_rows(c) else {
                continue;
            };
            for row in rows {
                assert!(row <= 0b111, "字形超出 3bit: {} ({})", row, c as char);
            }
        }
    }

    #[test]
    fn known_glyph_shape() {
        assert_eq!(glyph_rows(b'E'), Some([0b111, 0b100, 0b110, 0b100, 0b111]));
        assert_eq!(glyph_rows(b'8'), Some([0b111, 0b101, 0b111, 0b101, 0b111]));
        assert_eq!(glyph_rows(b'~'), None);
    }

    #[test]
    fn row_pixel_expansion() {
        assert_eq!(row_pixels(0b101), [true, false, true]);
        assert_eq!(row_pixels(0b010), [false, true, false]);
    }

    #[test]
    fn text_metrics() {
        assert_eq!(text_size(""), (0, 0));
        assert_eq!(text_size("A"), (3, 5));
        // 3 字符: 3×3 + 2 间隔
        assert_eq!(text_size("ABC"), (11, 5));
    }
}
