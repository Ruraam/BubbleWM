//! BubbleOS icon helpers.

use std::path::Path;

/// Loads an existing Bubble SVG icon without changing its original colors.
///
/// Non-positive sizes are clamped to one pixel.
pub fn from_file(path: impl AsRef<Path>, size: i32) -> gtk::Image {
    let image = gtk::Image::from_file(path);
    image.set_pixel_size(normalize_size(size));
    image
}

fn normalize_size(size: i32) -> i32 {
    size.max(1)
}

#[cfg(test)]
mod tests {
    use super::normalize_size;

    #[test]
    fn icon_size_is_clamped_to_at_least_one_pixel() {
        assert_eq!(normalize_size(i32::MIN), 1);
        assert_eq!(normalize_size(-1), 1);
        assert_eq!(normalize_size(0), 1);
        assert_eq!(normalize_size(1), 1);
        assert_eq!(normalize_size(24), 24);
    }
}
