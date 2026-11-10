//! Finding audio files on disk and reading their tags and length.

use std::cmp::Ordering;
use std::ffi::OsStr;
use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::Duration;

use symphonia::core::formats::FormatOptions;
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::{MetadataOptions, MetadataRevision, StandardTagKey};
use symphonia::core::probe::Hint;

/// Extensions the bundled decoders can play.
pub const AUDIO_EXTENSIONS: &[&str] = &[
    "mp3", "flac", "ogg", "oga", "wav", "wave", "m4a", "m4b", "aac", "aif", "aiff", "aifc", "caf",
    "mka",
];

pub fn is_audio(path: &Path) -> bool {
    path.extension()
        .and_then(OsStr::to_str)
        .is_some_and(|ext| AUDIO_EXTENSIONS.contains(&ext.to_ascii_lowercase().as_str()))
}

/// Command-line arguments come as plain paths (`%F`) or `file://` URIs (`%U`).
pub fn path_from_arg(arg: &OsStr) -> Option<PathBuf> {
    let text = arg.to_string_lossy();
    let path = if let Some(rest) = text.strip_prefix("file://") {
        let rest = rest.strip_prefix("localhost").unwrap_or(rest);
        PathBuf::from(percent_decode(rest)?)
    } else if text.starts_with("--") {
        return None;
    } else {
        PathBuf::from(arg)
    };
    Some(std::path::absolute(&path).unwrap_or(path))
}

fn percent_decode(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).ok()?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

/// Audio files directly inside `dir`, in the order a person would number them.
pub fn audio_files_in(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut files: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.is_file() && is_audio(path))
        .collect();
    sort_naturally(&mut files);
    files
}

/// Expands dropped folders (recursively) into their audio files; keeps files as given.
pub fn expand(paths: &[PathBuf]) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for path in paths {
        if path.is_dir() {
            out.extend(audio_files_in(path));
            let mut subdirs: Vec<PathBuf> = std::fs::read_dir(path)
                .into_iter()
                .flatten()
                .filter_map(Result::ok)
                .map(|entry| entry.path())
                .filter(|p| p.is_dir())
                .collect();
            sort_naturally(&mut subdirs);
            out.extend(expand(&subdirs));
        } else if is_audio(path) {
            out.push(path.clone());
        }
    }
    out
}

fn sort_naturally(paths: &mut [PathBuf]) {
    paths.sort_by(|a, b| {
        let a = a.file_name().map(|n| n.to_string_lossy().to_lowercase()).unwrap_or_default();
        let b = b.file_name().map(|n| n.to_string_lossy().to_lowercase()).unwrap_or_default();
        natural_cmp(&a, &b)
    });
}

/// "track 2" sorts before "track 10".
fn natural_cmp(a: &str, b: &str) -> Ordering {
    let (mut a, mut b) = (a, b);
    loop {
        match (a.chars().next(), b.chars().next()) {
            (None, None) => return Ordering::Equal,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(ca), Some(cb)) if ca.is_ascii_digit() && cb.is_ascii_digit() => {
                let (na, ra) = split_digits(a);
                let (nb, rb) = split_digits(b);
                let ta = na.trim_start_matches('0');
                let tb = nb.trim_start_matches('0');
                let order = ta.len().cmp(&tb.len()).then_with(|| ta.cmp(tb));
                if order != Ordering::Equal {
                    return order;
                }
                (a, b) = (ra, rb);
            }
            (Some(ca), Some(cb)) => {
                if ca != cb {
                    return ca.cmp(&cb);
                }
                (a, b) = (&a[ca.len_utf8()..], &b[cb.len_utf8()..]);
            }
        }
    }
}

fn split_digits(s: &str) -> (&str, &str) {
    let end = s.find(|c: char| !c.is_ascii_digit()).unwrap_or(s.len());
    s.split_at(end)
}

pub fn file_stem(path: &Path) -> String {
    path.file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string_lossy().into_owned())
}

pub fn folder_name(path: &Path) -> String {
    path.parent()
        .and_then(Path::file_name)
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default()
}

#[derive(Clone, Debug, Default)]
pub struct TrackInfo {
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub duration: Option<Duration>,
}

pub fn read_info(path: &Path) -> TrackInfo {
    let mut info = TrackInfo::default();
    let Ok(file) = File::open(path) else { return info };
    let source = MediaSourceStream::new(Box::new(file), Default::default());
    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(OsStr::to_str) {
        hint.with_extension(ext);
    }
    let Ok(mut probed) = symphonia::default::get_probe().format(
        &hint,
        source,
        &FormatOptions::default(),
        &MetadataOptions::default(),
    ) else {
        return info;
    };

    // ID3 tags arrive through the probe, Vorbis/MP4 tags through the container.
    if let Some(revision) = probed.metadata.get().as_ref().and_then(|m| m.current().cloned()) {
        apply_tags(&mut info, &revision);
    }
    if let Some(revision) = probed.format.metadata().current() {
        apply_tags(&mut info, revision);
    }

    if let Some(track) = probed.format.default_track() {
        let params = &track.codec_params;
        if let (Some(frames), Some(time_base)) = (params.n_frames, params.time_base) {
            let time = time_base.calc_time(frames);
            info.duration = Some(Duration::from_secs(time.seconds) + Duration::from_secs_f64(time.frac));
        } else if let (Some(frames), Some(rate)) = (params.n_frames, params.sample_rate) {
            info.duration = Some(Duration::from_secs_f64(frames as f64 / rate as f64));
        }
    }
    info
}

fn apply_tags(info: &mut TrackInfo, revision: &MetadataRevision) {
    for tag in revision.tags() {
        let value = tag.value.to_string().trim().to_owned();
        if value.is_empty() {
            continue;
        }
        let slot = match tag.std_key {
            Some(StandardTagKey::TrackTitle) => &mut info.title,
            Some(StandardTagKey::Artist) => &mut info.artist,
            Some(StandardTagKey::AlbumArtist) if info.artist.is_none() => &mut info.artist,
            Some(StandardTagKey::Album) => &mut info.album,
            _ => continue,
        };
        if slot.is_none() {
            *slot = Some(value);
        }
    }
}

/// Reads tags on a background thread so big folders don't freeze the window.
pub struct InfoReader {
    jobs: Sender<PathBuf>,
    pub results: Receiver<(PathBuf, TrackInfo)>,
}

impl InfoReader {
    pub fn new(ctx: eframe::egui::Context) -> Self {
        let (jobs, job_rx) = channel::<PathBuf>();
        let (result_tx, results) = channel();
        std::thread::spawn(move || {
            for path in job_rx {
                let info = read_info(&path);
                if result_tx.send((path, info)).is_err() {
                    break;
                }
                ctx.request_repaint();
            }
        });
        Self { jobs, results }
    }

    pub fn request(&self, path: PathBuf) {
        let _ = self.jobs.send(path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn natural_order() {
        let mut names = vec!["10 b.mp3", "2 a.mp3", "1 c.mp3", "Track 02.mp3", "track 1.mp3"];
        names.sort_by(|a, b| natural_cmp(&a.to_lowercase(), &b.to_lowercase()));
        assert_eq!(names, ["1 c.mp3", "2 a.mp3", "10 b.mp3", "track 1.mp3", "Track 02.mp3"]);
    }

    #[test]
    fn file_uri_arguments() {
        let path = path_from_arg(OsStr::new("file:///music/My%20Song.mp3")).unwrap();
        assert_eq!(path, PathBuf::from("/music/My Song.mp3"));
    }

    #[test]
    fn reads_tags_and_length_of_test_tones() {
        let album = Path::new(env!("CARGO_MANIFEST_DIR")).join("test-audio/Sample Album");
        let files = audio_files_in(&album);
        if files.is_empty() {
            eprintln!("no test audio; run tools/make_test_audio.py");
            return;
        }
        assert_eq!(files.len(), 12);
        assert!(files[0].ends_with("01 Tone 220.ogg"));
        for file in &files {
            let info = read_info(file);
            assert_eq!(info.artist.as_deref(), Some("Test Signals"), "{file:?}");
            assert!(info.title.as_deref().is_some_and(|t| t.starts_with("Tone")), "{file:?}");
            println!("{:?}: {:?}", file.file_name().unwrap(), info.duration);
        }
    }
}
