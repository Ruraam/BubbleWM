//! Pure window-layout algorithms.
//!
//! This module deliberately has no Smithay or Wayland dependencies so layout
//! behavior can be tested deterministically without starting a compositor.

/// A rectangle expressed in compositor logical coordinates.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

impl Rect {
    pub const fn new(x: i32, y: i32, width: i32, height: i32) -> Self {
        Self { x, y, width, height }
    }

    pub const fn is_empty(self) -> bool {
        self.width <= 0 || self.height <= 0
    }
}

/// Parameters for the master/stack layout.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MasterStackConfig {
    /// Fraction of the usable width assigned to the master column.
    pub master_ratio: f64,
    /// Logical-pixel spacing between tiled windows and columns.
    pub gap: i32,
}

impl Default for MasterStackConfig {
    fn default() -> Self {
        Self { master_ratio: 0.55, gap: 8 }
    }
}

/// Calculate master/stack rectangles in stable input order.
///
/// The first window is the master and occupies the full available height.
/// Remaining windows are stacked vertically in the right column. Rounding
/// remainders are distributed to the first stack entries. Gaps are reduced
/// when necessary so every rectangle stays inside the work area.
///
/// Empty work areas produce no rectangles. A single window fills the area.
/// Negative gaps become zero; the master ratio is clamped to 20–80%.
pub fn master_stack(area: Rect, window_count: usize, config: MasterStackConfig) -> Vec<Rect> {
    if window_count == 0 || area.is_empty() {
        return Vec::new();
    }
    if window_count == 1 {
        return vec![area];
    }

    let gap = config.gap.max(0);
    let horizontal_gap = gap.min(area.width.saturating_sub(1).max(0));
    let inner_width = area.width - horizontal_gap;
    let ratio = if config.master_ratio.is_finite() {
        config.master_ratio.clamp(0.2, 0.8)
    } else {
        MasterStackConfig::default().master_ratio
    };
    let master_width = if inner_width <= 1 {
        inner_width
    } else {
        ((inner_width as f64 * ratio).round() as i32).clamp(1, inner_width - 1)
    };
    let stack_x = area.x.saturating_add(master_width).saturating_add(horizontal_gap);
    let stack_width = inner_width - master_width;

    let stack_count = window_count - 1;
    let vertical_gap = if stack_count > 1 {
        gap.min(area.height.saturating_sub(1).max(0) / (stack_count - 1) as i32)
    } else {
        0
    };
    let stack_height = area.height - vertical_gap * (stack_count - 1) as i32;
    let stack_count_i32 = stack_count as i32;
    let base_height = stack_height / stack_count_i32;
    let remainder = stack_height % stack_count_i32;

    let mut result = Vec::with_capacity(window_count);
    result.push(Rect::new(area.x, area.y, master_width, area.height));

    let mut y = area.y;
    for index in 0..stack_count {
        let height = base_height + i32::from((index as i32) < remainder);
        result.push(Rect::new(stack_x, y, stack_width, height));
        y = y.saturating_add(height).saturating_add(vertical_gap);
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;

    const AREA: Rect = Rect::new(10, 20, 1200, 800);

    #[test]
    fn empty_layout_has_no_windows() {
        assert!(master_stack(AREA, 0, MasterStackConfig::default()).is_empty());
    }

    #[test]
    fn one_window_fills_the_work_area() {
        assert_eq!(master_stack(AREA, 1, MasterStackConfig::default()), vec![AREA]);
    }

    #[test]
    fn two_windows_create_master_and_stack_columns() {
        let layout = master_stack(AREA, 2, MasterStackConfig::default());
        assert_eq!(layout.len(), 2);
        assert_eq!(layout[0], Rect::new(10, 20, 656, 800));
        assert_eq!(layout[1], Rect::new(674, 20, 536, 800));
    }

    #[test]
    fn stack_remainder_is_distributed_without_overlap() {
        let layout = master_stack(
            Rect::new(0, 0, 1000, 503),
            4,
            MasterStackConfig { master_ratio: 0.5, gap: 7 },
        );
        assert_eq!(layout.len(), 4);
        assert_eq!(layout[1].height, 163);
        assert_eq!(layout[2].height, 163);
        assert_eq!(layout[3].height, 163);
        assert_eq!(layout[1].y, 0);
        assert_eq!(layout[2].y, 170);
        assert_eq!(layout[3].y, 340);
        assert_eq!(layout[3].y + layout[3].height, 503);
    }

    #[test]
    fn ratio_and_gap_are_safely_clamped() {
        let layout = master_stack(
            Rect::new(0, 0, 100, 100),
            2,
            MasterStackConfig { master_ratio: 9.0, gap: -10 },
        );
        assert_eq!(layout[0].width, 80);
        assert_eq!(layout[1].x, 80);
        assert_eq!(layout[1].width, 20);
    }

    #[test]
    fn non_finite_ratio_uses_the_default() {
        let layout = master_stack(
            Rect::new(0, 0, 100, 100),
            2,
            MasterStackConfig { master_ratio: f64::NAN, gap: 0 },
        );
        assert_eq!(layout[0].width, 55);
    }

    #[test]
    fn empty_work_areas_are_rejected() {
        assert!(master_stack(Rect::new(0, 0, 0, 100), 2, MasterStackConfig::default()).is_empty());
        assert!(master_stack(Rect::new(0, 0, 100, -1), 2, MasterStackConfig::default()).is_empty());
    }

    #[test]
    fn tiny_work_area_never_returns_negative_dimensions_or_overflows() {
        let layout = master_stack(
            Rect::new(0, 0, 1, 1),
            4,
            MasterStackConfig { master_ratio: 0.5, gap: 8 },
        );
        assert_eq!(layout.len(), 4);
        assert!(layout.iter().all(|rect| rect.width >= 0 && rect.height >= 0));
        assert!(layout.iter().all(|rect| rect.x <= 1 && rect.y <= 1));
    }
}
