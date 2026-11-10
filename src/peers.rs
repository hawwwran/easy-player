//! Sharing the media keys with other players (VLC, browsers, other Easy
//! Player windows) so the last one started is the one that listens.
//!
//! GNOME (gsd-media-keys 46, mpris-controller.c) sends the keys to one MPRIS
//! player and only moves them to another player that starts playing while
//! the current one is *not* playing. Two players playing at once would keep
//! the keys on the older one, so one player plays at a time:
//!
//! - When this window starts playing it pauses the other players, then
//!   re-announces "Playing" once they have paused, which makes GNOME switch.
//! - When another player starts, this window pauses. GNOME may still route
//!   the keys here (it never saw a reason to switch), so keys that arrive
//!   while another player is the newest get forwarded to it.

use std::sync::mpsc::Sender;
use std::time::Duration;

use crate::instance::Message;

/// The MPRIS name suffix this window registers (`org.mpris.MediaPlayer2.<this>`).
pub fn own_mpris_suffix() -> String {
    format!("easy_player.instance{}", crate::instance_token())
}

#[cfg(not(target_os = "linux"))]
pub struct Peers;

#[cfg(not(target_os = "linux"))]
impl Peers {
    pub fn start(_tx: Sender<Message>, _ctx: eframe::egui::Context) -> Option<Self> {
        None
    }
    pub fn pause_others(&self, _then: impl FnOnce() + Send + 'static) {}
    pub fn is_present(&self, _name: &str) -> bool {
        false
    }
    pub fn forward(&self, _player: &str, _event: &souvlaki::MediaControlEvent) -> bool {
        false
    }
}

#[cfg(target_os = "linux")]
pub use linux::Peers;

#[cfg(target_os = "linux")]
mod linux {
    use super::*;
    use std::collections::HashMap;
    use zbus::blocking::{Connection, MessageIterator, fdo::DBusProxy};
    use zbus::names::BusName;
    use zbus::zvariant::OwnedValue;

    const MPRIS_PREFIX: &str = "org.mpris.MediaPlayer2.";
    const MPRIS_PATH: &str = "/org/mpris/MediaPlayer2";
    const PLAYER_IFACE: &str = "org.mpris.MediaPlayer2.Player";
    /// playerctld mirrors whichever player is active, including this one.
    const MIRRORS: &[&str] = &["org.mpris.MediaPlayer2.playerctld"];

    #[derive(Clone)]
    pub struct Peers {
        conn: Connection,
        own_name: String,
    }

    impl Peers {
        pub fn start(tx: Sender<Message>, ctx: eframe::egui::Context) -> Option<Self> {
            let conn = match Connection::session() {
                Ok(conn) => conn,
                Err(err) => {
                    eprintln!("easy-player: no session bus, media keys stay with GNOME's choice: {err}");
                    return None;
                }
            };
            let peers = Self { conn, own_name: format!("{MPRIS_PREFIX}{}", own_mpris_suffix()) };
            let watcher = peers.clone();
            std::thread::spawn(move || {
                if let Err(err) = watcher.watch(&tx, &ctx) {
                    eprintln!("easy-player: stopped watching other players: {err}");
                }
            });
            Some(peers)
        }

        /// Other MPRIS players currently on the bus: (well-known name, unique owner).
        pub(super) fn others(&self) -> Vec<(String, String)> {
            let Ok(dbus) = DBusProxy::new(&self.conn) else { return Vec::new() };
            let Ok(names) = dbus.list_names() else { return Vec::new() };
            names
                .into_iter()
                .map(|n| n.to_string())
                .filter(|n| n.starts_with(MPRIS_PREFIX) && *n != self.own_name && !MIRRORS.contains(&n.as_str()))
                .filter_map(|name| {
                    let bus_name = BusName::try_from(name.as_str()).ok()?;
                    let owner = dbus.get_name_owner(bus_name).ok()?;
                    Some((name, owner.to_string()))
                })
                .collect()
        }

        fn watch(&self, tx: &Sender<Message>, ctx: &eframe::egui::Context) -> zbus::Result<()> {
            let rule = zbus::MatchRule::builder()
                .msg_type(zbus::message::Type::Signal)
                .interface("org.freedesktop.DBus.Properties")?
                .member("PropertiesChanged")?
                .path(MPRIS_PATH)?
                .build();
            for message in MessageIterator::for_match_rule(rule, &self.conn, Some(64))? {
                let message = message?;
                let Some(sender) = message.header().sender().map(|s| s.to_string()) else { continue };
                let Ok((iface, changed, _)) =
                    message.body().deserialize::<(String, HashMap<String, OwnedValue>, Vec<String>)>()
                else {
                    continue;
                };
                let started = iface == PLAYER_IFACE
                    && changed.get("PlaybackStatus").and_then(|v| v.downcast_ref::<&str>().ok()) == Some("Playing");
                if !started {
                    continue;
                }
                // Signals carry the unique sender; map it back to a player name.
                // Our own window and mirrors aren't in `others`, so they drop out here.
                if let Some((name, _)) = self.others().into_iter().find(|(_, owner)| *owner == sender) {
                    if tx.send(Message::OtherPlayerStarted(name)).is_err() {
                        break;
                    }
                    ctx.request_repaint();
                }
            }
            Ok(())
        }

        pub(super) fn playback_status(&self, name: &str) -> Option<String> {
            let reply = self
                .conn
                .call_method(
                    Some(name),
                    MPRIS_PATH,
                    Some("org.freedesktop.DBus.Properties"),
                    "Get",
                    &(PLAYER_IFACE, "PlaybackStatus"),
                )
                .ok()?;
            let value: OwnedValue = reply.body().deserialize().ok()?;
            value.downcast_ref::<&str>().ok().map(str::to_owned)
        }

        fn call(&self, name: &str, method: &str) -> bool {
            self.conn.call_method(Some(name), MPRIS_PATH, Some(PLAYER_IFACE), method, &()).is_ok()
        }

        /// Pauses every other player that is playing. Calls `then` afterwards
        /// so this window can re-announce itself once GNOME saw them pause.
        pub fn pause_others(&self, then: impl FnOnce() + Send + 'static) {
            let peers = self.clone();
            std::thread::spawn(move || {
                let mut paused_any = false;
                for (name, _) in peers.others() {
                    if peers.playback_status(&name).as_deref() == Some("Playing") {
                        paused_any |= peers.call(&name, "Pause");
                    }
                }
                if paused_any {
                    std::thread::sleep(Duration::from_millis(400));
                    then();
                }
            });
        }

        pub fn is_present(&self, name: &str) -> bool {
            self.others().iter().any(|(other, _)| other == name)
        }

        /// Hands a media key to `player`. False when the key isn't a
        /// transport key or the player is gone.
        pub fn forward(&self, player: &str, event: &souvlaki::MediaControlEvent) -> bool {
            use souvlaki::MediaControlEvent as E;
            let method = match event {
                E::Toggle => "PlayPause",
                E::Play => "Play",
                E::Pause => "Pause",
                E::Next => "Next",
                E::Previous => "Previous",
                E::Stop => "Stop",
                _ => return false,
            };
            self.is_present(player) && self.call(player, method)
        }
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use souvlaki::{MediaControlEvent, MediaControls, MediaPlayback, PlatformConfig};
    use std::sync::mpsc::channel;

    /// Uses the real session bus with a stand-in player:
    /// `cargo test -- --ignored`. Skips the pause step if a real player is
    /// playing, so it never pauses your music.
    #[test]
    #[ignore]
    fn newest_player_gets_the_keys() {
        let (tx, rx) = channel();
        let peers = Peers::start(tx, eframe::egui::Context::default()).expect("session bus");
        std::thread::sleep(Duration::from_millis(300));

        let suffix = format!("easy_player_test_peer{}", std::process::id());
        let fake_name = format!("org.mpris.MediaPlayer2.{suffix}");
        let (fake_tx, fake_rx) = channel();
        let mut fake = MediaControls::new(PlatformConfig { display_name: "Fake", dbus_name: &suffix, hwnd: None })
            .unwrap();
        fake.attach(move |event| {
            let _ = fake_tx.send(event);
        })
        .unwrap();
        std::thread::sleep(Duration::from_millis(500));
        fake.set_playback(MediaPlayback::Playing { progress: None }).unwrap();

        let started = loop {
            match rx.recv_timeout(Duration::from_secs(3)) {
                Ok(Message::OtherPlayerStarted(name)) if name == fake_name => break true,
                Ok(_) => continue,
                Err(_) => break false,
            }
        };
        assert!(started, "did not notice the other player starting");

        assert!(peers.forward(&fake_name, &MediaControlEvent::Next));
        assert!(matches!(fake_rx.recv_timeout(Duration::from_secs(2)), Ok(MediaControlEvent::Next)));

        let real_playing = peers
            .others()
            .iter()
            .any(|(name, _)| *name != fake_name && peers.playback_status(name).as_deref() == Some("Playing"));
        if !real_playing {
            let (done_tx, done_rx) = channel();
            peers.pause_others(move || {
                let _ = done_tx.send(());
            });
            assert!(matches!(fake_rx.recv_timeout(Duration::from_secs(3)), Ok(MediaControlEvent::Pause)));
            assert!(done_rx.recv_timeout(Duration::from_secs(3)).is_ok());
        }
        fake.detach().unwrap();
    }
}
