//! Easy Player: a small audio player. Files dropped on the window or opened from
//! the file manager go into a playlist; one button pulls in the rest of the
//! playing song's folder.

mod app;
mod desktop;
mod icon;
mod instance;
mod library;
mod media;
mod peers;
mod player;
mod settings;
mod widgets;

use std::path::PathBuf;
use std::sync::OnceLock;
use std::time::{SystemTime, UNIX_EPOCH};

pub const APP_ID: &str = "com.hawwwran.EasyPlayer";
pub const APP_NAME: &str = "Easy Player";

pub fn in_flatpak() -> bool {
    std::env::var_os("FLATPAK_ID").is_some()
}

/// Names this window on the session bus and in the socket folder. The process
/// id alone doesn't do: every Flatpak sandbox counts its own pids, so two
/// windows can have the same one.
pub fn instance_token() -> &'static str {
    static TOKEN: OnceLock<String> = OnceLock::new();
    TOKEN.get_or_init(|| {
        let nanos = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.subsec_nanos()).unwrap_or(0);
        format!("{}_{nanos:08x}", std::process::id())
    })
}

struct Args {
    screenshot: Option<PathBuf>,
    files: Vec<PathBuf>,
}

fn parse_args() -> Args {
    let mut args = Args { screenshot: None, files: Vec::new() };
    let mut it = std::env::args_os().skip(1);
    while let Some(arg) = it.next() {
        if arg == "--screenshot" {
            args.screenshot = it.next().map(PathBuf::from);
        } else if let Some(path) = library::path_from_arg(&arg) {
            args.files.push(path);
        }
    }
    args
}

fn main() -> eframe::Result<()> {
    let args = parse_args();
    let screenshot_mode = args.screenshot.is_some();

    // A song opened from the file manager goes to an idle window if there is one.
    if !screenshot_mode && !args.files.is_empty() && instance::hand_over(&args.files) {
        return Ok(());
    }

    let mut viewport = eframe::egui::ViewportBuilder::default()
        .with_title(APP_NAME)
        .with_app_id(APP_ID)
        .with_inner_size([520.0, 700.0])
        .with_min_inner_size([380.0, 460.0])
        .with_icon(icon::window_icon())
        .with_drag_and_drop(true);
    if screenshot_mode {
        viewport = viewport.with_active(false);
        #[cfg(windows)]
        {
            viewport = viewport.with_window_level(eframe::egui::WindowLevel::AlwaysOnBottom);
        }
    }

    let options = eframe::NativeOptions {
        viewport,
        event_loop_builder: Some(Box::new(move |builder| {
            // winit only delivers dropped files on X11, so run through XWayland
            // when it is available.
            #[cfg(target_os = "linux")]
            if std::env::var_os("DISPLAY").is_some()
                && std::env::var_os("EASY_PLAYER_NATIVE_WAYLAND").is_none()
            {
                use winit::platform::x11::EventLoopBuilderExtX11;
                builder.with_x11();
            }
            #[cfg(target_os = "macos")]
            if screenshot_mode {
                use winit::platform::macos::EventLoopBuilderExtMacOS;
                builder.with_activate_ignoring_other_apps(false);
            }
            let _ = (builder, screenshot_mode);
        })),
        ..Default::default()
    };

    eframe::run_native(
        APP_NAME,
        options,
        Box::new(move |cc| Ok(Box::new(app::EasyPlayer::new(cc, args.files, args.screenshot)))),
    )
}
