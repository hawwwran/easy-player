//! Keyboard media keys and the desktop's media controls. The desktop sends
//! play/pause, next and previous to players over MPRIS (Linux), the system
//! media transport controls (Windows) or Now Playing (macOS); souvlaki covers
//! all three. Works while the window is unfocused or minimized.

use std::path::PathBuf;
use std::sync::mpsc::Sender;
use std::time::Duration;

use souvlaki::{MediaControlEvent, MediaControls, MediaMetadata, MediaPlayback, MediaPosition, PlatformConfig};

use crate::instance::Message;

#[derive(Clone, PartialEq, Default)]
pub struct NowPlaying {
    pub path: Option<PathBuf>,
    pub title: String,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub duration: Option<Duration>,
    pub playing: bool,
    pub loaded: bool,
}

pub struct MediaKeys {
    controls: MediaControls,
    shown: Option<NowPlaying>,
}

impl MediaKeys {
    pub fn start(tx: Sender<Message>, ctx: eframe::egui::Context) -> Option<Self> {
        // Each window registers its own name so several windows can coexist.
        let dbus_name = crate::peers::own_mpris_suffix();
        let config = PlatformConfig { display_name: crate::APP_NAME, dbus_name: &dbus_name, hwnd: None };
        let mut controls = match MediaControls::new(config) {
            Ok(controls) => controls,
            Err(err) => {
                eprintln!("easy-player: media keys unavailable: {err:?}");
                return None;
            }
        };
        let attached = controls.attach(move |event| {
            if tx.send(Message::Media(event)).is_ok() {
                ctx.request_repaint();
            }
        });
        if let Err(err) = attached {
            eprintln!("easy-player: media keys unavailable: {err:?}");
            return None;
        }
        Some(Self { controls, shown: None })
    }

    /// Tells the desktop what is playing; only sends when something changed.
    pub fn sync(&mut self, now: NowPlaying, position: Duration) {
        if self.shown.as_ref() == Some(&now) {
            return;
        }
        let previous = self.shown.replace(now.clone());
        if previous.as_ref().map(|p| (&p.path, &p.title, &p.artist, &p.album, p.duration))
            != Some((&now.path, &now.title, &now.artist, &now.album, now.duration))
        {
            let _ = self.controls.set_metadata(MediaMetadata {
                title: now.path.is_some().then_some(now.title.as_str()),
                artist: now.artist.as_deref(),
                album: now.album.as_deref(),
                duration: now.duration,
                cover_url: None,
            });
        }
        let progress = Some(MediaPosition(position));
        let playback = match (now.loaded, now.playing) {
            (false, _) => MediaPlayback::Stopped,
            (true, true) => MediaPlayback::Playing { progress },
            (true, false) => MediaPlayback::Paused { progress },
        };
        let _ = self.controls.set_playback(playback);
    }

    /// The person moved the seek bar; the desktop's progress needs a refresh.
    pub fn invalidate(&mut self) {
        self.shown = None;
    }
}

pub fn seek_step(event: &MediaControlEvent) -> Option<(bool, Duration)> {
    use souvlaki::SeekDirection::Forward;
    match event {
        MediaControlEvent::Seek(dir) => Some((*dir == Forward, Duration::from_secs(5))),
        MediaControlEvent::SeekBy(dir, by) => Some((*dir == Forward, *by)),
        _ => None,
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use std::sync::mpsc::channel;

    /// Sends PlayPause and Next over the session bus, as the desktop does for
    /// keyboard media keys: `cargo test -- --ignored`.
    #[test]
    #[ignore]
    fn desktop_play_pause_reaches_the_player() {
        let (tx, rx) = channel();
        let keys = MediaKeys::start(tx, eframe::egui::Context::default()).expect("media controls");
        let bus_name = format!("org.mpris.MediaPlayer2.{}", crate::peers::own_mpris_suffix());
        std::thread::sleep(Duration::from_millis(500));
        for method in ["PlayPause", "Next"] {
            let status = std::process::Command::new("busctl")
                .args(["--user", "call", &bus_name, "/org/mpris/MediaPlayer2", "org.mpris.MediaPlayer2.Player", method])
                .status()
                .expect("busctl");
            assert!(status.success(), "{method} call failed");
        }
        let received: Vec<_> = (0..2).filter_map(|_| rx.recv_timeout(Duration::from_secs(2)).ok()).collect();
        assert!(matches!(received.as_slice(), [Message::Media(MediaControlEvent::Toggle), Message::Media(MediaControlEvent::Next)]));
        drop(keys);
    }
}
