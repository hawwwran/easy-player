//! The app icon. `data/window-icon.png` is the SVG rendered at 128 px; render
//! it again when the SVG changes.

use eframe::egui::IconData;

pub const SVG: &str = include_str!("../data/com.hawwwran.EasyPlayer.svg");
const WINDOW_PNG: &[u8] = include_bytes!("../data/window-icon.png");

pub fn window_icon() -> IconData {
    match image::load_from_memory_with_format(WINDOW_PNG, image::ImageFormat::Png) {
        Ok(icon) => {
            let icon = icon.into_rgba8();
            let (width, height) = icon.dimensions();
            IconData { rgba: icon.into_raw(), width, height }
        }
        Err(err) => {
            eprintln!("easy-player: window icon unreadable: {err}");
            IconData::default()
        }
    }
}
