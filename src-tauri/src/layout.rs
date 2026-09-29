//! 预览浮窗的摆放计算：与平台无关，便于在任何系统上测试。

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rect {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

impl Rect {
    pub fn new(left: i32, top: i32, width: i32, height: i32) -> Self {
        Self {
            left,
            top,
            right: left + width,
            bottom: top + height,
        }
    }

    pub fn width(&self) -> i32 {
        self.right - self.left
    }

    pub fn height(&self) -> i32 {
        self.bottom - self.top
    }

    fn overlap_area(&self, other: &Rect) -> i64 {
        let width = (self.right.min(other.right) - self.left.max(other.left)).max(0) as i64;
        let height = (self.bottom.min(other.bottom) - self.top.max(other.top)).max(0) as i64;
        width * height
    }
}

/// 预览默认放在工作区右下角；如果会挡住弹出面板，则改放左下角。
/// 两侧都会遮挡时（例如面板很宽），选择遮挡面积更小的一侧，同面积优先右侧。
pub fn preview_rect(work: Rect, popup: Rect, width: i32, height: i32, gap: i32) -> Rect {
    let width = width.min(work.width() - 2 * gap).max(1);
    let height = height.min(work.height() - 2 * gap).max(1);
    let top = work.bottom - height - gap;
    let right = Rect::new(work.right - width - gap, top, width, height);
    let left = Rect::new(work.left + gap, top, width, height);
    // 给面板留出一点呼吸空间，贴边也算遮挡。
    let guard = Rect {
        left: popup.left - gap / 2,
        top: popup.top - gap / 2,
        right: popup.right + gap / 2,
        bottom: popup.bottom + gap / 2,
    };
    let right_overlap = right.overlap_area(&guard);
    if right_overlap == 0 {
        return right;
    }
    if left.overlap_area(&guard) < right_overlap {
        left
    } else {
        right
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const WORK: Rect = Rect {
        left: 0,
        top: 0,
        right: 1920,
        bottom: 1040,
    };

    #[test]
    fn preview_prefers_bottom_right_and_moves_left_when_it_would_cover_popup() {
        let popup = Rect::new(100, 100, 400, 560);
        assert_eq!(
            preview_rect(WORK, popup, 480, 560, 16),
            Rect::new(1920 - 480 - 16, 1040 - 560 - 16, 480, 560)
        );

        let popup = Rect::new(1500, 460, 400, 560);
        assert_eq!(
            preview_rect(WORK, popup, 480, 560, 16),
            Rect::new(16, 1040 - 560 - 16, 480, 560)
        );
    }

    #[test]
    fn preview_picks_smaller_overlap_and_clamps_to_small_work_area() {
        // 面板横跨整个底部时两侧都遮挡，选遮挡更少的一侧。
        let popup = Rect::new(300, 600, 1600, 400);
        let rect = preview_rect(WORK, popup, 480, 560, 16);
        assert_eq!(rect.left, 16);

        let small = Rect::new(0, 0, 300, 200);
        let rect = preview_rect(small, Rect::new(0, 0, 1, 1), 480, 560, 16);
        assert_eq!((rect.width(), rect.height()), (268, 168));
        assert_eq!((rect.left, rect.top), (16, 16));
    }
}
