//! Volume and repeat mode survive restarts in a small text file.

use std::path::PathBuf;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Repeat {
    Off,
    All,
    One,
}

impl Repeat {
    pub fn next(self) -> Self {
        match self {
            Repeat::Off => Repeat::All,
            Repeat::All => Repeat::One,
            Repeat::One => Repeat::Off,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Repeat::Off => "Repeat off",
            Repeat::All => "Repeat all",
            Repeat::One => "Repeat one",
        }
    }
}

pub struct Settings {
    pub volume: f32,
    pub repeat: Repeat,
}

impl Default for Settings {
    fn default() -> Self {
        Self { volume: 0.8, repeat: Repeat::All }
    }
}

fn file() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(base.join("easy-player").join("settings.txt"))
}

impl Settings {
    pub fn load() -> Self {
        let mut settings = Self::default();
        let Some(text) = file().and_then(|f| std::fs::read_to_string(f).ok()) else {
            return settings;
        };
        for line in text.lines() {
            match line.split_once('=') {
                Some(("volume", v)) => {
                    if let Ok(v) = v.trim().parse::<f32>() {
                        settings.volume = v.clamp(0.0, 1.0);
                    }
                }
                Some(("repeat", v)) => {
                    settings.repeat = match v.trim() {
                        "off" => Repeat::Off,
                        "one" => Repeat::One,
                        _ => Repeat::All,
                    }
                }
                _ => {}
            }
        }
        settings
    }

    pub fn save(&self) {
        let Some(path) = file() else { return };
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let repeat = match self.repeat {
            Repeat::Off => "off",
            Repeat::All => "all",
            Repeat::One => "one",
        };
        let _ = std::fs::write(path, format!("volume={:.3}\nrepeat={repeat}\n", self.volume));
    }
}
