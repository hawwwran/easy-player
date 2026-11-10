//! Sound output: one song at a time through rodio.

use std::fs::File;
use std::io::BufReader;
use std::path::Path;
use std::time::Duration;

use rodio::{Decoder, OutputStream, OutputStreamBuilder, Sink, Source};

pub struct Player {
    stream: Option<OutputStream>,
    sink: Option<Sink>,
    volume: f32,
    /// Length reported by the decoder; tags sometimes lack it.
    pub duration: Option<Duration>,
    pub output_error: Option<String>,
}

impl Player {
    /// `with_output: false` keeps the window silent (screenshot runs).
    pub fn new(with_output: bool, volume: f32) -> Self {
        let (stream, output_error) = if with_output {
            match OutputStreamBuilder::open_default_stream() {
                Ok(mut stream) => {
                    stream.log_on_drop(false);
                    (Some(stream), None)
                }
                Err(err) => (None, Some(format!("No sound output: {err}"))),
            }
        } else {
            (None, None)
        };
        Self { stream, sink: None, volume, duration: None, output_error }
    }

    pub fn load(&mut self, path: &Path) -> Result<(), String> {
        self.stop();
        let file = File::open(path).map_err(|err| format!("Can't open file: {err}"))?;
        let byte_len = file.metadata().map(|m| m.len()).unwrap_or(0);
        let mut builder = Decoder::builder()
            .with_data(BufReader::new(file))
            .with_byte_len(byte_len)
            .with_seekable(true);
        if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
            builder = builder.with_hint(ext);
        }
        let decoder = builder.build().map_err(|err| format!("Can't play this file: {err}"))?;
        self.duration = decoder.total_duration();
        if let Some(stream) = &self.stream {
            let sink = Sink::connect_new(stream.mixer());
            sink.set_volume(self.volume * self.volume);
            sink.append(decoder);
            self.sink = Some(sink);
        }
        Ok(())
    }

    pub fn stop(&mut self) {
        if let Some(sink) = self.sink.take() {
            sink.stop();
        }
        self.duration = None;
    }

    pub fn is_loaded(&self) -> bool {
        self.sink.is_some()
    }

    pub fn is_playing(&self) -> bool {
        self.sink.as_ref().is_some_and(|s| !s.is_paused() && !s.empty())
    }

    /// The song ran to its end on its own.
    pub fn finished(&self) -> bool {
        self.sink.as_ref().is_some_and(Sink::empty)
    }

    pub fn pause(&self) {
        if let Some(sink) = &self.sink {
            sink.pause();
        }
    }

    pub fn resume(&self) {
        if let Some(sink) = &self.sink {
            sink.play();
        }
    }

    pub fn position(&self) -> Duration {
        self.sink.as_ref().map(Sink::get_pos).unwrap_or_default()
    }

    pub fn seek(&self, to: Duration) {
        if let Some(sink) = &self.sink {
            if let Err(err) = sink.try_seek(to) {
                eprintln!("easy-player: seek failed: {err}");
            }
        }
    }

    pub fn volume(&self) -> f32 {
        self.volume
    }

    pub fn set_volume(&mut self, volume: f32) {
        self.volume = volume.clamp(0.0, 1.0);
        if let Some(sink) = &self.sink {
            // Squared so the slider's lower half isn't all "loud".
            sink.set_volume(self.volume * self.volume);
        }
    }
}
