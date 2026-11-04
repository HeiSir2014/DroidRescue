//! Pure geometry for custom-chrome hit-testing (and the rail side).
//! All values are physical pixels in screen (or window-local) coordinates.

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Side {
    Left,
    #[default]
    Right,
}

impl Side {
    pub const fn flipped(self) -> Side {
        match self {
            Side::Left => Side::Right,
            Side::Right => Side::Left,
        }
    }
}

/// New window left edge when its width changes from `width` to `new_width`
/// so that the control rail (on `rail` side) stays put on screen; the video
/// stage grows/shrinks on the other side. Clamped into `[work_left, work_right]`.
pub fn resize_keep_rail(
    left: i32,
    width: i32,
    new_width: i32,
    rail: Side,
    work_left: i32,
    work_right: i32,
) -> i32 {
    let anchored = match rail {
        Side::Right => left + width - new_width, // keep the right edge
        Side::Left => left,                      // keep the left edge
    };
    anchored.min(work_right - new_width).max(work_left)
}

// ───────────────────────── hit-testing ─────────────────────────

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hit {
    Client,
    Caption,
    Left,
    Right,
    Top,
    Bottom,
    TopLeft,
    TopRight,
    BottomLeft,
    BottomRight,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Edges {
    pub left: bool,
    pub right: bool,
    pub top: bool,
    pub bottom: bool,
}

impl Edges {
    pub const ALL: Edges = Edges {
        left: true,
        right: true,
        top: true,
        bottom: true,
    };
    pub const NONE: Edges = Edges {
        left: false,
        right: false,
        top: false,
        bottom: false,
    };
}

/// Non-client layout published by the UI every frame (window-local physical px).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ChromeLayout {
    pub caption_height: i32,
    /// Interactive caption controls (buttons) occupy `[controls_left, right)`.
    pub controls_left: i32,
    pub border: i32,
    pub resizable: Edges,
}

impl Default for ChromeLayout {
    fn default() -> Self {
        Self {
            caption_height: 40,
            controls_left: i32::MAX,
            border: 6,
            resizable: Edges::ALL,
        }
    }
}

/// Classify a window-local point. Resize borders win over the caption so the
/// top edge stays grabbable; caption controls stay client so the UI gets clicks.
pub fn hit_test(x: i32, y: i32, width: i32, height: i32, layout: &ChromeLayout) -> Hit {
    let b = layout.border;
    let e = layout.resizable;
    let left = e.left && x < b;
    let right = e.right && x >= width - b;
    let top = e.top && y < b;
    let bottom = e.bottom && y >= height - b;
    match (top, bottom, left, right) {
        (true, _, true, _) => return Hit::TopLeft,
        (true, _, _, true) => return Hit::TopRight,
        (_, true, true, _) => return Hit::BottomLeft,
        (_, true, _, true) => return Hit::BottomRight,
        (true, ..) => return Hit::Top,
        (_, true, ..) => return Hit::Bottom,
        (_, _, true, _) => return Hit::Left,
        (_, _, _, true) => return Hit::Right,
        _ => {}
    }
    if y < layout.caption_height && x < layout.controls_left {
        Hit::Caption
    } else {
        Hit::Client
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn growing_keeps_a_right_rail_in_place() {
        // 340-wide rail at x=1500 grows to 1300: stage opens to the left.
        assert_eq!(resize_keep_rail(1500, 340, 1300, Side::Right, 0, 2560), 540);
    }

    #[test]
    fn shrinking_keeps_a_right_rail_in_place() {
        assert_eq!(resize_keep_rail(540, 1300, 340, Side::Right, 0, 2560), 1500);
    }

    #[test]
    fn left_rail_keeps_its_left_edge() {
        assert_eq!(resize_keep_rail(100, 340, 1300, Side::Left, 0, 2560), 100);
    }

    #[test]
    fn result_is_clamped_into_the_work_area() {
        assert_eq!(
            resize_keep_rail(200, 340, 1300, Side::Right, 0, 2560),
            0,
            "would go off the left"
        );
        assert_eq!(
            resize_keep_rail(2000, 340, 1300, Side::Left, 0, 2560),
            1260,
            "would go off the right"
        );
    }

    fn layout(edges: Edges) -> ChromeLayout {
        ChromeLayout {
            caption_height: 40,
            controls_left: 200,
            border: 6,
            resizable: edges,
        }
    }

    #[test]
    fn caption_area_is_draggable() {
        assert_eq!(
            hit_test(50, 20, 300, 800, &layout(Edges::ALL)),
            Hit::Caption
        );
    }

    #[test]
    fn caption_buttons_are_client() {
        assert_eq!(
            hit_test(250, 20, 300, 800, &layout(Edges::ALL)),
            Hit::Client
        );
    }

    #[test]
    fn body_is_client() {
        assert_eq!(
            hit_test(150, 400, 300, 800, &layout(Edges::ALL)),
            Hit::Client
        );
    }

    #[test]
    fn borders_and_corners_resize() {
        let l = layout(Edges::ALL);
        assert_eq!(hit_test(2, 400, 300, 800, &l), Hit::Left);
        assert_eq!(hit_test(297, 400, 300, 800, &l), Hit::Right);
        assert_eq!(hit_test(150, 2, 300, 800, &l), Hit::Top);
        assert_eq!(hit_test(150, 797, 300, 800, &l), Hit::Bottom);
        assert_eq!(hit_test(1, 1, 300, 800, &l), Hit::TopLeft);
        assert_eq!(hit_test(299, 799, 300, 800, &l), Hit::BottomRight);
    }
}
