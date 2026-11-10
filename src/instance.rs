//! Talking between player windows. Each window listens on its own local socket
//! in a shared folder.
//!
//! - A song opened from the file manager first offers itself to the windows
//!   already running; an empty, silent one takes it. Otherwise it opens a new
//!   window.
//! - Whenever a window starts playing, it tells the others to pause.

use std::path::PathBuf;
#[cfg(not(unix))]
use std::sync::{Arc, atomic::AtomicBool, mpsc::Sender};

pub enum Message {
    /// From the file manager: add and start playing.
    Open(Vec<PathBuf>),
    /// From the file picker: add, play only if nothing is loaded.
    Add(Vec<PathBuf>),
    Pause,
    /// A keyboard media key or the desktop's media controls.
    Media(souvlaki::MediaControlEvent),
    /// Another media player (by MPRIS name) just started playing.
    OtherPlayerStarted(String),
    /// Other players have paused; tell the desktop again that this one plays.
    Reannounce,
}

#[cfg(unix)]
pub use unix::{Instance, hand_over};

#[cfg(not(unix))]
pub struct Instance;

#[cfg(not(unix))]
pub fn hand_over(_files: &[PathBuf]) -> bool {
    false
}

#[cfg(not(unix))]
impl Instance {
    pub fn start(_busy: Arc<AtomicBool>, _ctx: eframe::egui::Context, _tx: Sender<Message>) -> Self {
        Instance
    }
    pub fn pause_others(&self) {}
}

#[cfg(unix)]
mod unix {
    use super::Message;
    use std::ffi::OsStr;
    use std::io::{Read, Write};
    use std::net::Shutdown;
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::net::{UnixListener, UnixStream};
    use std::path::{Path, PathBuf};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::mpsc::Sender;
    use std::time::Duration;

    const OPEN: &[u8] = b"OPEN\n";
    const PAUSE: &[u8] = b"PAUSE\n";
    const TAKEN: &[u8] = b"TAKEN";

    fn socket_dir() -> PathBuf {
        let base = std::env::var_os("XDG_RUNTIME_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(std::env::temp_dir);
        // A Flatpak sandbox has its own runtime folder; app/<id> under it is
        // the part all windows of the app share.
        match std::env::var_os("FLATPAK_ID") {
            Some(id) => base.join("app").join(id),
            None => base.join(crate::APP_ID),
        }
    }

    /// Sends `request` and returns the reply; removes sockets nobody listens on.
    fn ask(socket: &Path, request: &[u8]) -> Option<Vec<u8>> {
        let Ok(mut stream) = UnixStream::connect(socket) else {
            let _ = std::fs::remove_file(socket);
            return None;
        };
        let timeout = Some(Duration::from_secs(2));
        stream.set_read_timeout(timeout).ok()?;
        stream.set_write_timeout(timeout).ok()?;
        stream.write_all(request).ok()?;
        stream.shutdown(Shutdown::Write).ok()?;
        let mut reply = Vec::new();
        stream.read_to_end(&mut reply).ok()?;
        Some(reply)
    }

    fn other_sockets(own: Option<&Path>) -> Vec<PathBuf> {
        std::fs::read_dir(socket_dir())
            .into_iter()
            .flatten()
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| path.extension().is_some_and(|ext| ext == "sock"))
            .filter(|path| Some(path.as_path()) != own)
            .collect()
    }

    /// True when an idle window took the files, so this process can exit.
    pub fn hand_over(files: &[PathBuf]) -> bool {
        let mut request = OPEN.to_vec();
        for file in files {
            request.extend_from_slice(file.as_os_str().as_bytes());
            request.push(b'\n');
        }
        other_sockets(None)
            .iter()
            .any(|socket| ask(socket, &request).is_some_and(|reply| reply == TAKEN))
    }

    pub struct Instance {
        socket: Option<PathBuf>,
    }

    impl Instance {
        /// `busy` is true while the window has songs or is playing; the window keeps it current.
        pub fn start(busy: Arc<AtomicBool>, ctx: eframe::egui::Context, tx: Sender<Message>) -> Self {
            let dir = socket_dir();
            let _ = std::fs::create_dir_all(&dir);
            let path = dir.join(format!("{}.sock", crate::instance_token()));
            let _ = std::fs::remove_file(&path);
            let listener = match UnixListener::bind(&path) {
                Ok(listener) => listener,
                Err(err) => {
                    eprintln!("easy-player: cannot listen for other windows: {err}");
                    return Self { socket: None };
                }
            };
            std::thread::spawn(move || {
                for stream in listener.incoming() {
                    let Ok(mut stream) = stream else { continue };
                    let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
                    let mut request = Vec::new();
                    if stream.read_to_end(&mut request).is_err() {
                        continue;
                    }
                    if request.starts_with(PAUSE) {
                        let _ = tx.send(Message::Pause);
                    } else if let Some(body) = request.strip_prefix(OPEN) {
                        // Claim the files before replying so two launches can't both land here.
                        if busy.swap(true, Ordering::SeqCst) {
                            let _ = stream.write_all(b"BUSY");
                            continue;
                        }
                        let paths = body
                            .split(|b| *b == b'\n')
                            .filter(|line| !line.is_empty())
                            .map(|line| PathBuf::from(OsStr::from_bytes(line)))
                            .collect();
                        let _ = tx.send(Message::Open(paths));
                        let _ = stream.write_all(TAKEN);
                    }
                    ctx.request_repaint();
                }
            });
            Self { socket: Some(path) }
        }

        pub fn pause_others(&self) {
            let others = other_sockets(self.socket.as_deref());
            if others.is_empty() {
                return;
            }
            std::thread::spawn(move || {
                for socket in others {
                    ask(&socket, PAUSE);
                }
            });
        }
    }

    impl Drop for Instance {
        fn drop(&mut self) {
            if let Some(path) = &self.socket {
                let _ = std::fs::remove_file(path);
            }
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use std::sync::mpsc::channel;

        #[test]
        fn idle_window_takes_files_busy_one_does_not() {
            let dir = std::env::temp_dir().join(format!("easy-player-test-{}", std::process::id()));
            // SAFETY: the only test that touches the environment.
            unsafe { std::env::set_var("XDG_RUNTIME_DIR", &dir) };

            let busy = Arc::new(AtomicBool::new(false));
            let (tx, rx) = channel();
            let instance = Instance::start(busy.clone(), eframe::egui::Context::default(), tx);
            let song = PathBuf::from("/music/a song.mp3");

            assert!(hand_over(std::slice::from_ref(&song)));
            match rx.recv_timeout(Duration::from_secs(2)) {
                Ok(Message::Open(paths)) => assert_eq!(paths, vec![song.clone()]),
                _ => panic!("window did not receive the song"),
            }
            assert!(busy.load(Ordering::SeqCst));
            assert!(!hand_over(&[song]), "a busy window must not take a second song");

            drop(instance);
            let _ = std::fs::remove_dir_all(&dir);
        }
    }
}
