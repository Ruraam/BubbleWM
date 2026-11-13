//! BubbleOS icon helpers.

use std::path::Path;

/// Loads an existing Bubble SVG icon without changing its original colors.
pub fn from_file(path: impl AsRef<Path>, size: i32) -> gtk::Image {
    let image = gtk::Image::from_file(path);
    image.set_pixel_size(size);
    image
}
