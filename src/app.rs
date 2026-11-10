//! The player window: now-playing card on top, playlist below.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::{Duration, Instant};

use eframe::egui::{
    self, Align, Align2, Color32, CornerRadius, FontId, Key, Layout, Margin, RichText, Sense, Stroke,
    StrokeKind, UiBuilder, pos2, vec2,
};

use crate::instance::{Instance, Message};
use crate::library::{self, InfoReader, TrackInfo};
use crate::media::{self, MediaKeys, NowPlaying};
use crate::peers::Peers;
use crate::player::Player;
use crate::settings::{Repeat, Settings};
use crate::widgets::{self, Glyph};

struct Track {
    path: PathBuf,
    info: Option<TrackInfo>,
    error: Option<String>,
}

impl Track {
    fn new(path: PathBuf) -> Self {
        Self { path, info: None, error: None }
    }

    fn title(&self) -> String {
        self.info
            .as_ref()
            .and_then(|i| i.title.clone())
            .unwrap_or_else(|| library::file_stem(&self.path))
    }

    fn subtitle(&self) -> String {
        let info = self.info.as_ref();
        match (info.and_then(|i| i.artist.as_deref()), info.and_then(|i| i.album.as_deref())) {
            (Some(artist), Some(album)) => format!("{artist} · {album}"),
            (Some(artist), None) => artist.to_owned(),
            (None, Some(album)) => album.to_owned(),
            (None, None) => library::folder_name(&self.path),
        }
    }

    fn duration(&self) -> Option<Duration> {
        self.info.as_ref().and_then(|i| i.duration)
    }
}

/// Audio files in the playing song's folder that aren't in the playlist yet.
struct FolderScan {
    folder: PathBuf,
    files: Vec<PathBuf>,
    missing: usize,
    scanned_for: usize,
}

struct Screenshot {
    path: PathBuf,
    frames: u32,
    requested: bool,
}

enum Notice {
    Info(String),
    Error(String),
}

pub struct EasyPlayer {
    tracks: Vec<Track>,
    current: Option<usize>,
    selected: Option<usize>,
    player: Player,
    settings: Settings,
    seek_preview: Option<f32>,
    info_reader: InfoReader,
    messages: Receiver<Message>,
    message_tx: Sender<Message>,
    instance: Option<Instance>,
    media_keys: Option<MediaKeys>,
    peers: Option<Peers>,
    /// Another player that started after this window last played; media keys
    /// that land here go to it.
    newest_other: Option<String>,
    busy: Arc<AtomicBool>,
    folder_scan: Option<FolderScan>,
    /// Bumped on every playlist change so the folder scan knows to refresh.
    playlist_version: usize,
    notice: Option<(Notice, Instant)>,
    scroll_to_current: bool,
    install_state: Option<crate::desktop::InstallState>,
    screenshot: Option<Screenshot>,
}

impl EasyPlayer {
    pub fn new(cc: &eframe::CreationContext<'_>, files: Vec<PathBuf>, screenshot: Option<PathBuf>) -> Self {
        apply_style(&cc.egui_ctx);
        let live = screenshot.is_none();
        Self::create(&cc.egui_ctx, files, screenshot, live)
    }

    /// `live: false` runs without sound and without talking to other windows.
    fn create(ctx: &egui::Context, files: Vec<PathBuf>, screenshot: Option<PathBuf>, live: bool) -> Self {
        let ctx = ctx.clone();
        let settings = Settings::load();
        let (message_tx, messages) = channel();
        let busy = Arc::new(AtomicBool::new(!files.is_empty()));
        let instance = live.then(|| Instance::start(busy.clone(), ctx.clone(), message_tx.clone()));
        let media_keys = live.then(|| MediaKeys::start(message_tx.clone(), ctx.clone())).flatten();
        let peers = live.then(|| Peers::start(message_tx.clone(), ctx.clone())).flatten();

        let mut player = Player::new(live, settings.volume);
        player.set_volume(settings.volume);
        let mut app = Self {
            tracks: Vec::new(),
            current: None,
            selected: None,
            player,
            settings,
            seek_preview: None,
            info_reader: InfoReader::new(ctx),
            messages,
            message_tx,
            instance,
            media_keys,
            peers,
            newest_other: None,
            busy,
            folder_scan: None,
            playlist_version: 0,
            notice: None,
            scroll_to_current: false,
            install_state: None,
            screenshot: screenshot.map(|path| Screenshot { path, frames: 0, requested: false }),
        };
        if let Some(err) = app.player.output_error.clone() {
            app.notify(Notice::Error(err));
        }
        if !files.is_empty() {
            app.open_files(&files, true);
        }
        app
    }

    fn notify(&mut self, notice: Notice) {
        self.notice = Some((notice, Instant::now()));
    }

    /// Adds files (and folders) to the playlist. `play` starts the first of them;
    /// otherwise playback only starts if nothing is loaded yet.
    fn open_files(&mut self, paths: &[PathBuf], play: bool) {
        let expanded = library::expand(paths);
        if expanded.is_empty() {
            if !paths.is_empty() {
                self.notify(Notice::Error("No playable audio files in what was dropped.".into()));
            }
            return;
        }
        let mut first = None;
        let mut added = 0;
        for path in expanded {
            let index = match self.index_of(&path) {
                Some(index) => index,
                None => {
                    self.info_reader.request(path.clone());
                    self.tracks.push(Track::new(path));
                    added += 1;
                    self.tracks.len() - 1
                }
            };
            first.get_or_insert(index);
        }
        self.playlist_version += 1;
        if added > 1 {
            self.notify(Notice::Info(format!("Added {added} songs.")));
        }
        if let Some(first) = first {
            if play || !self.player.is_loaded() {
                self.play_index(first);
            }
        }
    }

    fn index_of(&self, path: &Path) -> Option<usize> {
        self.tracks.iter().position(|t| t.path == path)
    }

    /// Starts track `index`; skips ahead past files that won't play.
    fn play_index(&mut self, index: usize) {
        let mut index = index;
        for _ in 0..self.tracks.len() {
            let path = self.tracks[index].path.clone();
            match self.player.load(&path) {
                Ok(()) => {
                    self.tracks[index].error = None;
                    if let (Some(duration), Some(info)) = (self.player.duration, self.tracks[index].info.as_mut()) {
                        info.duration.get_or_insert(duration);
                    }
                    self.current = Some(index);
                    self.selected = Some(index);
                    self.scroll_to_current = true;
                    self.seek_preview = None;
                    self.started_playing();
                    return;
                }
                Err(err) => {
                    self.tracks[index].error = Some(err);
                    index = (index + 1) % self.tracks.len();
                    if index == 0 && self.settings.repeat == Repeat::Off {
                        break;
                    }
                }
            }
        }
        self.player.stop();
        self.current = None;
        self.notify(Notice::Error("That file couldn't be played.".into()));
    }

    fn seek_to(&mut self, to: Duration) {
        self.player.seek(to);
        if let Some(keys) = &mut self.media_keys {
            keys.invalidate();
        }
    }

    fn handle_media_key(&mut self, ctx: &egui::Context, event: souvlaki::MediaControlEvent) {
        use souvlaki::MediaControlEvent as E;
        if let (Some(peers), Some(other)) = (&self.peers, &self.newest_other) {
            if peers.forward(other, &event) {
                return;
            }
            if !peers.is_present(other) {
                self.newest_other = None;
            }
        }
        if let Some((forward, by)) = media::seek_step(&event) {
            let position = self.player.position();
            self.seek_to(if forward { position + by } else { position.saturating_sub(by) });
            return;
        }
        match event {
            E::Toggle => self.toggle_play(),
            E::Play if !self.player.is_playing() => self.toggle_play(),
            E::Pause => self.player.pause(),
            E::Next if !self.tracks.is_empty() => self.next(),
            E::Previous if !self.tracks.is_empty() => self.previous(),
            E::Stop => {
                self.player.pause();
                self.seek_to(Duration::ZERO);
            }
            E::SetPosition(position) => self.seek_to(position.0),
            E::SetVolume(volume) => {
                self.player.set_volume(volume as f32);
                self.settings.volume = self.player.volume();
                self.settings.save();
            }
            E::OpenUri(uri) => {
                if let Some(path) = library::path_from_arg(std::ffi::OsStr::new(&uri)) {
                    self.open_files(&[path], true);
                }
            }
            E::Raise => ctx.send_viewport_cmd(egui::ViewportCommand::Focus),
            E::Quit => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
            _ => {}
        }
    }

    fn sync_media_keys(&mut self) {
        let Some(keys) = &mut self.media_keys else { return };
        let track = self.current.and_then(|i| self.tracks.get(i));
        let info = track.and_then(|t| t.info.as_ref());
        let now = NowPlaying {
            path: track.map(|t| t.path.clone()),
            title: track.map(Track::title).unwrap_or_default(),
            artist: info.and_then(|i| i.artist.clone()),
            album: info.and_then(|i| i.album.clone()),
            duration: track.and_then(Track::duration).or(self.player.duration),
            playing: self.player.is_playing(),
            loaded: self.player.is_loaded(),
        };
        keys.sync(now, self.player.position());
    }

    fn started_playing(&mut self) {
        if let Some(instance) = &self.instance {
            instance.pause_others();
        }
        self.newest_other = None;
        if let Some(peers) = &self.peers {
            let tx = self.message_tx.clone();
            peers.pause_others(move || {
                let _ = tx.send(Message::Reannounce);
            });
        }
    }

    fn toggle_play(&mut self) {
        if self.player.is_loaded() {
            if self.player.is_playing() {
                self.player.pause();
            } else {
                self.player.resume();
                self.started_playing();
            }
        } else if let Some(index) = self.current.or(self.selected).or((!self.tracks.is_empty()).then_some(0)) {
            self.play_index(index);
        }
    }

    fn next(&mut self) {
        let Some(current) = self.current else {
            if !self.tracks.is_empty() {
                self.play_index(0);
            }
            return;
        };
        if current + 1 < self.tracks.len() {
            self.play_index(current + 1);
        } else if self.settings.repeat != Repeat::Off {
            self.play_index(0);
        }
    }

    fn previous(&mut self) {
        if self.player.position() > Duration::from_secs(3) {
            self.seek_to(Duration::ZERO);
            return;
        }
        match self.current {
            Some(0) if self.settings.repeat != Repeat::Off => self.play_index(self.tracks.len() - 1),
            Some(0) => self.seek_to(Duration::ZERO),
            Some(current) => self.play_index(current - 1),
            None if !self.tracks.is_empty() => self.play_index(0),
            None => {}
        }
    }

    fn on_song_finished(&mut self) {
        let Some(current) = self.current else { return };
        match self.settings.repeat {
            Repeat::One => self.play_index(current),
            _ if current + 1 < self.tracks.len() => self.play_index(current + 1),
            Repeat::All => self.play_index(0),
            Repeat::Off => {
                // End of the playlist: stay on the last song, ready to replay.
                self.player.stop();
            }
        }
    }

    fn remove(&mut self, index: usize) {
        if index >= self.tracks.len() {
            return;
        }
        self.tracks.remove(index);
        self.playlist_version += 1;
        self.selected = None;
        match self.current {
            Some(current) if current == index => {
                let was_playing = self.player.is_playing();
                self.player.stop();
                self.current = None;
                if was_playing && index < self.tracks.len() {
                    self.play_index(index);
                }
            }
            Some(current) if current > index => self.current = Some(current - 1),
            _ => {}
        }
    }

    fn clear(&mut self) {
        self.player.stop();
        self.tracks.clear();
        self.current = None;
        self.selected = None;
        self.playlist_version += 1;
    }

    /// The song the folder button works from: the playing one, else the selected one.
    fn anchor(&self) -> Option<usize> {
        self.current.or(self.selected).or((self.tracks.len() == 1).then_some(0))
    }

    fn refresh_folder_scan(&mut self) {
        let Some(anchor) = self.anchor() else {
            self.folder_scan = None;
            return;
        };
        let Some(folder) = self.tracks[anchor].path.parent().map(Path::to_path_buf) else { return };
        let stale = self
            .folder_scan
            .as_ref()
            .is_none_or(|scan| scan.folder != folder || scan.scanned_for != self.playlist_version);
        if !stale {
            return;
        }
        let files = if self.folder_scan.as_ref().is_some_and(|scan| scan.folder == folder) {
            self.folder_scan.take().map(|scan| scan.files).unwrap_or_default()
        } else {
            library::audio_files_in(&folder)
        };
        let known: HashSet<&Path> = self.tracks.iter().map(|t| t.path.as_path()).collect();
        let missing = files.iter().filter(|f| !known.contains(f.as_path())).count();
        self.folder_scan = Some(FolderScan { folder, files, missing, scanned_for: self.playlist_version });
    }

    /// Brings in the rest of the folder of track `anchor`, keeping the folder's
    /// order around that song so playback carries on with the next file.
    fn add_folder(&mut self, anchor: usize) {
        let anchor_path = self.tracks[anchor].path.clone();
        let Some(folder) = anchor_path.parent() else { return };
        let files = library::audio_files_in(folder);
        let known: HashSet<PathBuf> = self.tracks.iter().map(|t| t.path.clone()).collect();
        let split = files.iter().position(|f| *f == anchor_path).unwrap_or(0);
        let before: Vec<PathBuf> = files[..split].iter().filter(|f| !known.contains(*f)).cloned().collect();
        let after: Vec<PathBuf> = files[split..].iter().filter(|f| !known.contains(*f)).cloned().collect();
        let (n_before, n_after) = (before.len(), after.len());

        let new_tracks = before.into_iter().chain(std::iter::once(PathBuf::new())).chain(after);
        let spliced: Vec<Track> = new_tracks
            .map(|path| {
                if path.as_os_str().is_empty() {
                    std::mem::replace(&mut self.tracks[anchor], Track::new(PathBuf::new()))
                } else {
                    self.info_reader.request(path.clone());
                    Track::new(path)
                }
            })
            .collect();
        self.tracks.splice(anchor..=anchor, spliced);

        let moved = |i: usize| match i.cmp(&anchor) {
            std::cmp::Ordering::Less => i,
            std::cmp::Ordering::Equal => i + n_before,
            std::cmp::Ordering::Greater => i + n_before + n_after,
        };
        self.current = self.current.map(moved);
        self.selected = self.selected.map(moved);
        self.playlist_version += 1;
        self.folder_scan = None;
        self.scroll_to_current = true;
        let folder_name = folder.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        self.notify(Notice::Info(format!("Added {} songs from \u{201c}{folder_name}\u{201d}.", n_before + n_after)));
    }

    fn pick_files(&self, ctx: &egui::Context) {
        let tx = self.message_tx.clone();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let picked = rfd::FileDialog::new()
                .set_title("Add songs")
                .add_filter("Audio", library::AUDIO_EXTENSIONS)
                .pick_files();
            if let Some(paths) = picked {
                let _ = tx.send(Message::Add(paths));
                ctx.request_repaint();
            }
        });
    }

    fn handle_messages(&mut self, ctx: &egui::Context) {
        while let Ok(message) = self.messages.try_recv() {
            match message {
                Message::Open(paths) => {
                    self.open_files(&paths, true);
                    ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
                }
                Message::Add(paths) => self.open_files(&paths, false),
                Message::Pause => self.player.pause(),
                Message::Media(event) => self.handle_media_key(ctx, event),
                Message::OtherPlayerStarted(name) => {
                    self.player.pause();
                    self.newest_other = Some(name);
                }
                Message::Reannounce => {
                    if let Some(keys) = &mut self.media_keys {
                        keys.invalidate();
                    }
                }
            }
        }
        while let Ok((path, info)) = self.info_reader.results.try_recv() {
            for track in self.tracks.iter_mut().filter(|t| t.path == path) {
                let mut info = info.clone();
                if info.duration.is_none() {
                    info.duration = track.info.as_ref().and_then(|i| i.duration);
                }
                track.info = Some(info);
            }
        }
    }

    fn handle_input(&mut self, ctx: &egui::Context) {
        let dropped: Vec<PathBuf> =
            ctx.input(|i| i.raw.dropped_files.iter().filter_map(|f| f.path.clone()).collect());
        if !dropped.is_empty() {
            self.open_files(&dropped, false);
        }

        if ctx.memory(|m| m.focused().is_some()) {
            return;
        }
        let (space, left, right, delete, open, enter) = ctx.input_mut(|i| {
            (
                i.consume_key(egui::Modifiers::NONE, Key::Space),
                i.consume_key(egui::Modifiers::NONE, Key::ArrowLeft),
                i.consume_key(egui::Modifiers::NONE, Key::ArrowRight),
                i.consume_key(egui::Modifiers::NONE, Key::Delete),
                i.consume_key(egui::Modifiers::COMMAND, Key::O),
                i.consume_key(egui::Modifiers::NONE, Key::Enter),
            )
        });
        if space {
            self.toggle_play();
        }
        let step = Duration::from_secs(5);
        if left {
            self.seek_to(self.player.position().saturating_sub(step));
        }
        if right {
            self.seek_to(self.player.position() + step);
        }
        if delete {
            if let Some(selected) = self.selected {
                self.remove(selected);
            }
        }
        if enter {
            if let Some(selected) = self.selected {
                self.play_index(selected);
            }
        }
        if open {
            self.pick_files(ctx);
        }
    }

    fn now_playing(&mut self, ui: &mut egui::Ui) {
        let track = self.current.or(self.selected).and_then(|i| self.tracks.get(i));
        let title = track.map(Track::title).unwrap_or_else(|| "Nothing playing".into());
        let subtitle = track.map(Track::subtitle).unwrap_or_else(|| "Drop songs below to start".into());
        let seed = track.map(|t| hash_path(t.path.parent().unwrap_or(&t.path)));
        let duration = track.and_then(Track::duration).or(self.player.duration);
        let loaded = self.player.is_loaded();

        ui.horizontal(|ui| {
            widgets::cover(ui, 76.0, seed);
            ui.add_space(6.0);
            ui.vertical(|ui| {
                ui.add_space(8.0);
                ui.add(egui::Label::new(RichText::new(title).size(19.0).strong().color(widgets::TEXT)).truncate());
                ui.add_space(2.0);
                ui.add(egui::Label::new(RichText::new(subtitle).size(13.5).color(widgets::TEXT_DIM)).truncate());
            });
        });
        ui.add_space(10.0);

        let position = self.player.position();
        let fraction = match duration {
            Some(d) if !d.is_zero() => position.as_secs_f32() / d.as_secs_f32(),
            _ => 0.0,
        };
        ui.horizontal(|ui| {
            let time_width = 44.0;
            let shown_position = match (self.seek_preview, duration) {
                (Some(f), Some(d)) => d.mul_f32(f),
                _ => position,
            };
            ui.allocate_ui_with_layout(vec2(time_width, 18.0), Layout::right_to_left(Align::Center), |ui| {
                ui.label(RichText::new(widgets::format_time(shown_position)).size(12.0).color(widgets::TEXT_DIM));
            });
            let bar_width = ui.available_width() - time_width - ui.spacing().item_spacing.x;
            let can_seek = loaded && duration.is_some();
            let (response, picked) =
                widgets::slider_bar(ui, self.seek_preview.unwrap_or(fraction), bar_width, can_seek);
            if let Some(picked) = picked {
                self.seek_preview = Some(picked);
            }
            if response.drag_stopped() || response.clicked() {
                if let (Some(f), Some(d)) = (self.seek_preview.take(), duration) {
                    self.seek_to(d.mul_f32(f));
                }
            }
            let total = duration.map(widgets::format_time).unwrap_or_else(|| "--:--".into());
            ui.label(RichText::new(total).size(12.0).color(widgets::TEXT_DIM));
        });
        ui.add_space(8.0);

        let has_tracks = !self.tracks.is_empty();
        let row_height = 56.0;
        let full = ui.available_width();
        let (row_rect, _) = ui.allocate_exact_size(vec2(full, row_height), Sense::hover());

        // Transport buttons centred; repeat on the left, volume on the right.
        let mut centre = ui.new_child(
            UiBuilder::new()
                .max_rect(egui::Rect::from_center_size(row_rect.center(), vec2(170.0, row_height)))
                .layout(Layout::left_to_right(Align::Center)),
        );
        centre.spacing_mut().item_spacing.x = 12.0;
        if widgets::transport_button(&mut centre, Glyph::Prev, 40.0, false, has_tracks)
            .on_hover_text("Previous")
            .clicked()
            && has_tracks
        {
            self.previous();
        }
        let glyph = if self.player.is_playing() { Glyph::Pause } else { Glyph::Play };
        if widgets::transport_button(&mut centre, glyph, 56.0, true, has_tracks).clicked() && has_tracks {
            self.toggle_play();
        }
        if widgets::transport_button(&mut centre, Glyph::Next, 40.0, false, has_tracks)
            .on_hover_text("Next")
            .clicked()
            && has_tracks
        {
            self.next();
        }

        let mut left = ui.new_child(
            UiBuilder::new()
                .max_rect(egui::Rect::from_min_size(row_rect.min, vec2(100.0, row_height)))
                .layout(Layout::left_to_right(Align::Center)),
        );
        let repeat_on = self.settings.repeat != Repeat::Off;
        let repeat_text = match self.settings.repeat {
            Repeat::Off => "Repeat",
            Repeat::All => "Repeat all",
            Repeat::One => "Repeat one",
        };
        let repeat = left
            .add(chip(repeat_text, repeat_on))
            .on_hover_text(format!("{} (click to change)", self.settings.repeat.label()));
        if repeat.clicked() {
            self.settings.repeat = self.settings.repeat.next();
            self.settings.save();
        }

        let mut right = ui.new_child(
            UiBuilder::new()
                .max_rect(egui::Rect::from_min_max(pos2(row_rect.right() - 100.0, row_rect.top()), row_rect.max))
                .layout(Layout::right_to_left(Align::Center)),
        );
        let (volume_response, picked) = widgets::slider_bar(&mut right, self.player.volume(), 72.0, true);
        if let Some(volume) = picked {
            self.player.set_volume(volume);
            self.settings.volume = volume;
        }
        if volume_response.drag_stopped() || volume_response.clicked() {
            self.settings.save();
        }
        volume_response.on_hover_text(format!("Volume {:.0}%", self.player.volume() * 100.0));
        paint_speaker(&right, self.player.volume());
    }

    fn folder_button(&mut self, ui: &mut egui::Ui) {
        self.refresh_folder_scan();
        let (enabled, label, hint) = match &self.folder_scan {
            None => (false, "Add whole folder".to_owned(), "Play a song first, then bring in the rest of its folder.".to_owned()),
            Some(scan) => {
                let folder = scan.folder.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                if scan.missing == 0 {
                    (false, "Whole folder is in the playlist".to_owned(), format!("Everything from \u{201c}{folder}\u{201d} is already here."))
                } else {
                    let songs = if scan.missing == 1 { "song" } else { "songs" };
                    (
                        true,
                        format!("Add whole folder  \u{00b7}  {} more {songs}", scan.missing),
                        format!("Adds the other songs from \u{201c}{folder}\u{201d} around the playing one."),
                    )
                }
            }
        };
        let width = ui.available_width();
        let (rect, response) = ui.allocate_exact_size(vec2(width, 40.0), if enabled { Sense::click() } else { Sense::hover() });
        let hovered = enabled && response.hovered();
        let (fill, stroke, text) = match (enabled, hovered) {
            (true, true) => (widgets::ACCENT, Stroke::NONE, widgets::ON_ACCENT),
            (true, false) => (widgets::CARD, Stroke::new(1.5, widgets::ACCENT), widgets::ACCENT),
            (false, _) => (widgets::PANEL, Stroke::new(1.0, widgets::CARD), widgets::TEXT_DIM),
        };
        let painter = ui.painter();
        painter.rect(rect, CornerRadius::same(10), fill, stroke, StrokeKind::Inside);
        let galley = painter.layout_no_wrap(label, FontId::proportional(14.5), text);
        let icon_w = 18.0;
        let total = icon_w + 8.0 + galley.size().x;
        let start = rect.center().x - total / 2.0;
        paint_folder(painter, egui::Rect::from_center_size(pos2(start + icon_w / 2.0, rect.center().y), vec2(icon_w, 14.0)), text);
        painter.galley(pos2(start + icon_w + 8.0, rect.center().y - galley.size().y / 2.0), galley, text);
        let response = response.on_hover_text(hint);
        if enabled && response.clicked() {
            if let Some(anchor) = self.anchor() {
                self.add_folder(anchor);
            }
        }
        if enabled {
            response.on_hover_cursor(egui::CursorIcon::PointingHand);
        }
    }

    fn playlist_header(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label(RichText::new("Playlist").size(15.0).strong().color(widgets::TEXT));
            if !self.tracks.is_empty() {
                let known: Duration = self.tracks.iter().filter_map(Track::duration).sum();
                let count = self.tracks.len();
                let songs = if count == 1 { "song" } else { "songs" };
                let mut summary = format!("{count} {songs}");
                if !known.is_zero() {
                    summary.push_str(&format!(" \u{00b7} {}", widgets::format_time(known)));
                }
                ui.label(RichText::new(summary).size(12.5).color(widgets::TEXT_DIM));
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                self.menu(ui);
                if !self.tracks.is_empty() && ui.add(small_button("Clear")).on_hover_text("Empty the playlist").clicked() {
                    self.clear();
                }
                if ui.add(small_button("Add files\u{2026}")).on_hover_text("Ctrl+O").clicked() {
                    self.pick_files(ui.ctx());
                }
            });
        });
    }

    fn menu(&mut self, ui: &mut egui::Ui) {
        let response = ui.add(small_button("     ")).on_hover_text("More");
        for dx in [-5.0, 0.0, 5.0] {
            ui.painter().circle_filled(response.rect.center() + vec2(dx, 0.0), 1.8, widgets::TEXT);
        }
        egui::Popup::menu(&response).show(|ui| {
            ui.set_min_width(230.0);
            use crate::desktop::InstallState;
            let state = *self.install_state.get_or_insert_with(crate::desktop::install_state);
            let label = match state {
                InstallState::NotInstalled => Some("Install, and open songs from Files here"),
                InstallState::Outdated => Some("Update the installed player"),
                InstallState::Installed => Some("Installed \u{2713}"),
                InstallState::Packaged => None,
            };
            if let Some(label) = label {
                if ui.add_enabled(state != InstallState::Installed, egui::Button::new(label)).clicked() {
                    match crate::desktop::make_default_player() {
                        Ok(message) => {
                            self.install_state = Some(InstallState::Installed);
                            self.notify(Notice::Info(message));
                        }
                        Err(err) => self.notify(Notice::Error(err)),
                    }
                    ui.close();
                }
                ui.separator();
            }
            ui.label(RichText::new("Space  play / pause\n\u{2190} \u{2192}  skip 5 seconds\nEnter  play selected\nDelete  remove selected").size(12.0).color(widgets::TEXT_DIM));
        });
    }

    fn playlist(&mut self, ui: &mut egui::Ui) {
        if self.tracks.is_empty() {
            self.empty_state(ui);
            return;
        }
        let row_height = 44.0;
        let mut play = None;
        let mut remove = None;
        let mut folder_of = None;
        let scroll_target = self.scroll_to_current.then_some(self.current).flatten();
        self.scroll_to_current = false;
        let animate = self.player.is_playing();

        egui::ScrollArea::vertical().auto_shrink([false, false]).show_rows(ui, row_height, self.tracks.len(), |ui, range| {
            for index in range {
                let track = &self.tracks[index];
                let width = ui.available_width();
                let (rect, response) = ui.allocate_exact_size(vec2(width, row_height), Sense::click());
                if scroll_target == Some(index) {
                    response.scroll_to_me(Some(Align::Center));
                }
                let is_current = self.current == Some(index);
                let is_selected = self.selected == Some(index);
                let painter = ui.painter_at(rect);
                let bg = if is_selected {
                    widgets::ROW_SELECTED
                } else if response.hovered() {
                    widgets::ROW_HOVER
                } else {
                    Color32::TRANSPARENT
                };
                painter.rect_filled(rect.shrink2(vec2(0.0, 1.0)), CornerRadius::same(8), bg);

                let number_center = pos2(rect.left() + 20.0, rect.center().y);
                if is_current {
                    widgets::playing_bars(ui, number_center, animate);
                } else {
                    painter.text(number_center, Align2::CENTER_CENTER, format!("{}", index + 1), FontId::proportional(12.0), widgets::TEXT_DIM);
                }

                let title_color = if track.error.is_some() {
                    widgets::ERROR
                } else if is_current {
                    widgets::ACCENT
                } else {
                    widgets::TEXT
                };
                let duration = track.duration().map(widgets::format_time).unwrap_or_default();
                let right_reserved = if response.hovered() { 72.0 } else { 56.0 };
                let text_left = rect.left() + 42.0;
                let text_width = (rect.right() - right_reserved - text_left).max(20.0);
                let title = ui.fonts_mut(|f| {
                    f.layout_job(truncated_job(&track.title(), 14.0, title_color, text_width))
                });
                let subtitle_text = track.error.clone().unwrap_or_else(|| track.subtitle());
                let subtitle = ui.fonts_mut(|f| {
                    f.layout_job(truncated_job(&subtitle_text, 11.5, widgets::TEXT_DIM, text_width))
                });
                painter.galley(pos2(text_left, rect.top() + 5.0), title, title_color);
                painter.galley(pos2(text_left, rect.top() + 24.0), subtitle, widgets::TEXT_DIM);

                if response.hovered() {
                    let close = egui::Rect::from_center_size(pos2(rect.right() - 18.0, rect.center().y), vec2(24.0, 24.0));
                    let close_response = ui.interact(close, ui.id().with(("remove", index)), Sense::click());
                    let color = if close_response.hovered() { widgets::TEXT } else { widgets::TEXT_DIM };
                    if close_response.hovered() {
                        painter.circle_filled(close.center(), 11.0, widgets::CARD);
                    }
                    let c = close.center();
                    let k = 4.5;
                    painter.line_segment([pos2(c.x - k, c.y - k), pos2(c.x + k, c.y + k)], Stroke::new(1.6, color));
                    painter.line_segment([pos2(c.x - k, c.y + k), pos2(c.x + k, c.y - k)], Stroke::new(1.6, color));
                    if close_response.on_hover_text("Remove from playlist").clicked() {
                        remove = Some(index);
                    }
                    painter.text(pos2(rect.right() - 38.0, rect.center().y), Align2::RIGHT_CENTER, duration, FontId::proportional(12.0), widgets::TEXT_DIM);
                } else {
                    painter.text(pos2(rect.right() - 14.0, rect.center().y), Align2::RIGHT_CENTER, duration, FontId::proportional(12.0), widgets::TEXT_DIM);
                }

                if response.double_clicked() {
                    play = Some(index);
                } else if response.clicked() {
                    self.selected = Some(index);
                }
                let path = track.path.clone();
                response.context_menu(|ui| {
                    if ui.button("Play").clicked() {
                        play = Some(index);
                        ui.close();
                    }
                    if ui.button("Add whole folder of this song").clicked() {
                        folder_of = Some(index);
                        ui.close();
                    }
                    if ui.button("Show in folder").clicked() {
                        if let Some(parent) = path.parent() {
                            open_in_file_manager(parent);
                        }
                        ui.close();
                    }
                    ui.separator();
                    if ui.button("Remove from playlist").clicked() {
                        remove = Some(index);
                        ui.close();
                    }
                });
            }
        });

        if let Some(index) = play {
            self.play_index(index);
        }
        if let Some(index) = folder_of {
            self.add_folder(index);
        }
        if let Some(index) = remove {
            self.remove(index);
        }
    }

    fn empty_state(&mut self, ui: &mut egui::Ui) {
        let rect = ui.available_rect_before_wrap().shrink(4.0);
        let response = ui.allocate_rect(rect, Sense::click());
        let painter = ui.painter();
        let hovered = response.hovered();
        dashed_rounded_rect(painter, rect, if hovered { widgets::ACCENT } else { widgets::TRACK });

        let c = rect.center();
        painter.circle_filled(pos2(c.x, c.y - 46.0), 30.0, widgets::CARD);
        paint_arrow_down(painter, pos2(c.x, c.y - 46.0), widgets::ACCENT);
        painter.text(pos2(c.x, c.y + 4.0), Align2::CENTER_CENTER, "Drop songs or folders here", FontId::proportional(17.0), widgets::TEXT);
        painter.text(
            pos2(c.x, c.y + 30.0),
            Align2::CENTER_CENTER,
            "or double-click a song in your file manager",
            FontId::proportional(13.0),
            widgets::TEXT_DIM,
        );
        painter.text(pos2(c.x, c.y + 54.0), Align2::CENTER_CENTER, "Click to browse", FontId::proportional(12.5), widgets::ACCENT);
        if response.on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
            self.pick_files(ui.ctx());
        }
    }

    fn drop_overlay(&self, ctx: &egui::Context) {
        let hovering = ctx.input(|i| !i.raw.hovered_files.is_empty());
        if !hovering {
            return;
        }
        let screen = ctx.content_rect();
        let painter = ctx.layer_painter(egui::LayerId::new(egui::Order::Foreground, egui::Id::new("drop")));
        painter.rect_filled(screen, CornerRadius::ZERO, Color32::from_black_alpha(190));
        let inner = screen.shrink(18.0);
        dashed_rounded_rect(&painter, inner, widgets::ACCENT);
        paint_arrow_down(&painter, pos2(inner.center().x, inner.center().y - 30.0), widgets::ACCENT);
        painter.text(inner.center() + vec2(0.0, 18.0), Align2::CENTER_CENTER, "Drop to add to the playlist", FontId::proportional(20.0), widgets::TEXT);
    }

    fn notice_bar(&mut self, ctx: &egui::Context) {
        let Some((notice, since)) = &self.notice else { return };
        let age = since.elapsed().as_secs_f32();
        if age > 4.0 {
            self.notice = None;
            return;
        }
        let (text, color) = match notice {
            Notice::Info(text) => (text.clone(), widgets::TEXT),
            Notice::Error(text) => (text.clone(), widgets::ERROR),
        };
        egui::Area::new(egui::Id::new("notice"))
            .anchor(Align2::CENTER_BOTTOM, vec2(0.0, -16.0))
            .order(egui::Order::Foreground)
            .interactable(false)
            .show(ctx, |ui| {
                egui::Frame::new()
                    .fill(widgets::CARD)
                    .stroke(Stroke::new(1.0, widgets::TRACK))
                    .corner_radius(CornerRadius::same(10))
                    .inner_margin(Margin::symmetric(14, 8))
                    .show(ui, |ui| {
                        ui.label(RichText::new(text).size(13.0).color(color));
                    });
            });
        ctx.request_repaint_after(Duration::from_millis(250));
    }

    fn take_screenshot(&mut self, ctx: &egui::Context) {
        let Some(shot) = &mut self.screenshot else { return };
        let image = ctx.input(|i| {
            i.raw.events.iter().find_map(|e| match e {
                egui::Event::Screenshot { image, .. } => Some(image.clone()),
                _ => None,
            })
        });
        if let Some(image) = image {
            let [w, h] = image.size;
            let bytes: Vec<u8> = image.pixels.iter().flat_map(|p| p.to_array()).collect();
            match image::RgbaImage::from_raw(w as u32, h as u32, bytes) {
                Some(buffer) => {
                    if let Err(err) = buffer.save(&shot.path) {
                        eprintln!("easy-player: could not save screenshot: {err}");
                    }
                }
                None => eprintln!("easy-player: screenshot had an unexpected size"),
            }
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            return;
        }
        shot.frames += 1;
        if shot.frames >= 120 && !shot.requested {
            shot.requested = true;
            ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::UserData::default()));
        }
        ctx.request_repaint();
    }
}

impl eframe::App for EasyPlayer {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.handle_messages(ctx);
        self.handle_input(ctx);
        if self.player.finished() {
            self.on_song_finished();
        }

        egui::TopBottomPanel::top("now_playing")
            .frame(egui::Frame::new().fill(widgets::PANEL).inner_margin(Margin { left: 18, right: 18, top: 18, bottom: 14 }))
            .show_separator_line(false)
            .show(ctx, |ui| {
                egui::Frame::new()
                    .fill(widgets::CARD)
                    .corner_radius(CornerRadius::same(16))
                    .inner_margin(Margin::same(14))
                    .show(ui, |ui| self.now_playing(ui));
                ui.add_space(12.0);
                self.folder_button(ui);
            });

        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(widgets::BG).inner_margin(Margin { left: 12, right: 12, top: 12, bottom: 8 }))
            .show(ctx, |ui| {
                ui.add_space(2.0);
                ui.horizontal(|ui| {
                    ui.add_space(6.0);
                    self.playlist_header(ui);
                });
                ui.add_space(6.0);
                self.playlist(ui);
            });

        self.drop_overlay(ctx);
        self.notice_bar(ctx);
        self.busy.store(!self.tracks.is_empty(), Ordering::SeqCst);
        self.sync_media_keys();

        if self.player.is_playing() {
            ctx.request_repaint_after(Duration::from_millis(60));
        } else if self.player.is_loaded() {
            // Paused: still notice a "pause" turning into "finished" etc.
            ctx.request_repaint_after(Duration::from_millis(500));
        }
        self.take_screenshot(ctx);
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.player.stop();
        self.settings.save();
        self.instance = None;
        self.media_keys = None;
    }
}

fn apply_style(ctx: &egui::Context) {
    ctx.set_theme(egui::Theme::Dark);
    ctx.all_styles_mut(|style| {
        let v = &mut style.visuals;
        *v = egui::Visuals::dark();
        v.panel_fill = widgets::BG;
        v.window_fill = widgets::CARD;
        v.window_stroke = Stroke::new(1.0, widgets::TRACK);
        v.window_corner_radius = CornerRadius::same(10);
        v.menu_corner_radius = CornerRadius::same(10);
        v.extreme_bg_color = widgets::BG;
        v.selection.bg_fill = widgets::ACCENT_DEEP;
        v.selection.stroke = Stroke::new(1.0, widgets::ON_ACCENT);
        v.widgets.inactive.weak_bg_fill = widgets::CARD;
        v.widgets.inactive.bg_fill = widgets::CARD;
        v.widgets.hovered.weak_bg_fill = widgets::ROW_SELECTED;
        v.widgets.hovered.bg_fill = widgets::ROW_SELECTED;
        v.widgets.active.weak_bg_fill = widgets::TRACK;
        v.widgets.noninteractive.fg_stroke = Stroke::new(1.0, widgets::TEXT);
        v.widgets.inactive.fg_stroke = Stroke::new(1.0, widgets::TEXT);
        v.widgets.hovered.fg_stroke = Stroke::new(1.0, widgets::TEXT);
        for w in [&mut v.widgets.inactive, &mut v.widgets.hovered, &mut v.widgets.active] {
            w.corner_radius = CornerRadius::same(8);
            w.bg_stroke = Stroke::NONE;
        }
        style.spacing.button_padding = vec2(10.0, 5.0);
        style.spacing.menu_margin = Margin::same(8);
        style.interaction.selectable_labels = false;
    });
}

fn small_button(text: &str) -> egui::Button<'static> {
    egui::Button::new(RichText::new(text.to_owned()).size(12.5).color(widgets::TEXT)).fill(widgets::CARD).corner_radius(8)
}

fn chip(text: &str, on: bool) -> egui::Button<'static> {
    let color = if on { widgets::ACCENT } else { widgets::TEXT_DIM };
    egui::Button::new(RichText::new(text.to_owned()).size(12.0).color(color))
        .fill(if on { Color32::from_rgba_unmultiplied(0xff, 0xae, 0x52, 28) } else { Color32::TRANSPARENT })
        .stroke(Stroke::new(1.0, if on { widgets::ACCENT } else { widgets::TRACK }))
        .corner_radius(12)
}

fn truncated_job(text: &str, size: f32, color: Color32, width: f32) -> egui::text::LayoutJob {
    let mut job = egui::text::LayoutJob::single_section(text.to_owned(), egui::TextFormat::simple(FontId::proportional(size), color));
    job.wrap = egui::text::TextWrapping::truncate_at_width(width);
    job
}

fn hash_path(path: &Path) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    path.hash(&mut hasher);
    hasher.finish()
}

fn dashed_rounded_rect(painter: &egui::Painter, rect: egui::Rect, color: Color32) {
    let stroke = Stroke::new(1.5, color);
    let r = 16.0;
    let (l, t, rt, b) = (rect.left(), rect.top(), rect.right(), rect.bottom());
    let mut path = Vec::new();
    let arc = |path: &mut Vec<egui::Pos2>, cx: f32, cy: f32, start: f32| {
        for k in 0..=8 {
            let a = start + k as f32 / 8.0 * std::f32::consts::FRAC_PI_2;
            path.push(pos2(cx + r * a.cos(), cy + r * a.sin()));
        }
    };
    use std::f32::consts::PI;
    arc(&mut path, l + r, t + r, PI);
    arc(&mut path, rt - r, t + r, 1.5 * PI);
    arc(&mut path, rt - r, b - r, 0.0);
    arc(&mut path, l + r, b - r, 0.5 * PI);
    path.push(path[0]);
    painter.extend(egui::Shape::dashed_line(&path, stroke, 8.0, 6.0));
}

fn paint_arrow_down(painter: &egui::Painter, c: egui::Pos2, color: Color32) {
    let stroke = Stroke::new(3.0, color);
    painter.line_segment([pos2(c.x, c.y - 13.0), pos2(c.x, c.y + 9.0)], stroke);
    painter.line_segment([pos2(c.x - 9.0, c.y), pos2(c.x, c.y + 10.0)], stroke);
    painter.line_segment([pos2(c.x + 9.0, c.y), pos2(c.x, c.y + 10.0)], stroke);
    painter.line_segment([pos2(c.x - 14.0, c.y + 16.0), pos2(c.x + 14.0, c.y + 16.0)], stroke);
}

fn paint_folder(painter: &egui::Painter, rect: egui::Rect, color: Color32) {
    let stroke = Stroke::new(1.6, color);
    let (l, t, r, b) = (rect.left(), rect.top(), rect.right(), rect.bottom());
    let tab = vec![
        pos2(l, b),
        pos2(l, t),
        pos2(l + rect.width() * 0.38, t),
        pos2(l + rect.width() * 0.5, t + 3.0),
        pos2(r, t + 3.0),
        pos2(r, b),
        pos2(l, b),
    ];
    painter.add(egui::Shape::line(tab, stroke));
    painter.line_segment([pos2(l, t + 6.0), pos2(r, t + 6.0)], stroke);
}

/// Speaker glyph to the left of the volume bar (the right-to-left child's cursor).
fn paint_speaker(ui: &egui::Ui, volume: f32) {
    let rect = ui.min_rect();
    let c = pos2(rect.left() - 14.0, rect.center().y);
    let painter = ui.painter();
    painter.add(egui::Shape::convex_polygon(
        vec![
            pos2(c.x - 7.0, c.y - 3.0),
            pos2(c.x - 3.0, c.y - 3.0),
            pos2(c.x + 2.0, c.y - 7.0),
            pos2(c.x + 2.0, c.y + 7.0),
            pos2(c.x - 3.0, c.y + 3.0),
            pos2(c.x - 7.0, c.y + 3.0),
        ],
        widgets::TEXT_DIM,
        Stroke::NONE,
    ));
    let waves = if volume <= 0.001 { 0 } else if volume < 0.5 { 1 } else { 2 };
    for k in 0..waves {
        let radius = 5.0 + k as f32 * 4.0;
        let points: Vec<_> = (0..=8)
            .map(|i| {
                let a = -0.8 + 1.6 * i as f32 / 8.0;
                pos2(c.x + 2.0 + radius * a.cos(), c.y + radius * a.sin())
            })
            .collect();
        painter.add(egui::Shape::line(points, Stroke::new(1.4, widgets::TEXT_DIM)));
    }
}

fn open_in_file_manager(folder: &Path) {
    #[cfg(target_os = "linux")]
    let program = "xdg-open";
    #[cfg(target_os = "macos")]
    let program = "open";
    #[cfg(windows)]
    let program = "explorer";
    let _ = std::process::Command::new(program).arg(folder).spawn();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn album() -> Option<PathBuf> {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("test-audio/Sample Album");
        dir.is_dir().then_some(dir)
    }

    fn names(app: &EasyPlayer) -> Vec<String> {
        app.tracks.iter().map(|t| library::file_stem(&t.path)).collect()
    }

    #[test]
    fn folder_button_keeps_folder_order_around_the_playing_song() {
        let Some(album) = album() else { return };
        let single = album.parent().unwrap().join("single.wav");
        let ctx = egui::Context::default();
        let mut app = EasyPlayer::create(&ctx, vec![single, album.join("05 Tone 440.flac")], None, false);
        app.play_index(1);
        assert_eq!(app.current, Some(1));

        app.add_folder(app.anchor().unwrap());
        let names = names(&app);
        assert_eq!(names.len(), 13);
        assert_eq!(names[0], "single");
        assert_eq!(names[1], "01 Tone 220");
        assert_eq!(names[5], "05 Tone 440");
        assert_eq!(names[12], "12 Tone 1175");
        assert_eq!(app.current, Some(5), "the playing song must stay current");

        app.next();
        assert_eq!(app.current, Some(6));
        app.refresh_folder_scan();
        assert_eq!(app.folder_scan.as_ref().map(|s| s.missing), Some(0));
    }

    #[test]
    fn dropping_a_folder_adds_its_songs_once() {
        let Some(album) = album() else { return };
        let ctx = egui::Context::default();
        let mut app = EasyPlayer::create(&ctx, vec![album.clone()], None, false);
        app.open_files(&[album.join("03 Tone 330.mp3")], false);
        assert_eq!(app.tracks.len(), 12);
    }

    /// Plays through the speakers at zero volume: `cargo test -- --ignored`.
    #[test]
    #[ignore]
    fn real_output_plays_and_finishes() {
        let Some(album) = album() else { return };
        let mut player = Player::new(true, 0.0);
        player.set_volume(0.0);
        assert!(player.output_error.is_none(), "{:?}", player.output_error);
        player.load(&album.parent().unwrap().join("single.wav")).unwrap();
        assert!(player.is_playing());
        std::thread::sleep(Duration::from_millis(1200));
        let position = player.position();
        assert!(position > Duration::from_millis(800), "{position:?}");
        player.seek(Duration::from_millis(2500));
        std::thread::sleep(Duration::from_millis(1500));
        assert!(player.finished());
    }
}
