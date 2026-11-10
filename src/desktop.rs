//! Installs the player for the current user: copies the program to
//! ~/.local/bin, adds a launcher entry and icon (so it shows in the app menu)
//! and registers it as the default for the common audio types, so a
//! double-click in the file manager opens songs here.

use crate::APP_ID;

pub const MIME_TYPES: &[&str] = &[
    "audio/mpeg",
    "audio/mp3",
    "audio/x-mp3",
    "audio/flac",
    "audio/x-flac",
    "audio/ogg",
    "audio/x-vorbis+ogg",
    "audio/vorbis",
    "audio/wav",
    "audio/x-wav",
    "audio/vnd.wave",
    "audio/mp4",
    "audio/x-m4a",
    "audio/m4a",
    "audio/aac",
    "audio/x-aac",
    "audio/x-aiff",
    "audio/aiff",
    "audio/x-matroska",
];

#[derive(Clone, Copy, PartialEq)]
pub enum InstallState {
    NotInstalled,
    Installed,
    /// Installed, but this running build is newer than the installed copy.
    Outdated,
    /// Running from a Flatpak, which brings its own launcher and file types.
    Packaged,
}

#[cfg(target_os = "linux")]
pub fn install_state() -> InstallState {
    if crate::in_flatpak() {
        return InstallState::Packaged;
    }
    if !is_default_player() {
        return InstallState::NotInstalled;
    }
    let installed = std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".local/bin/easy-player"));
    let Some(installed) = installed else { return InstallState::Installed };
    let running = std::env::current_exe().ok();
    if running.as_deref() == Some(installed.as_path()) {
        return InstallState::Installed;
    }
    let modified = |p: &std::path::Path| std::fs::metadata(p).and_then(|m| m.modified()).ok();
    match (running.as_deref().and_then(modified), modified(&installed)) {
        (Some(run), Some(inst)) if run <= inst => InstallState::Installed,
        _ => InstallState::Outdated,
    }
}

#[cfg(not(target_os = "linux"))]
pub fn install_state() -> InstallState {
    InstallState::NotInstalled
}

#[cfg(target_os = "linux")]
fn is_default_player() -> bool {
    std::process::Command::new("xdg-mime")
        .args(["query", "default", "audio/mpeg"])
        .output()
        .is_ok_and(|out| String::from_utf8_lossy(&out.stdout).trim() == format!("{APP_ID}.desktop"))
}

#[cfg(not(target_os = "linux"))]
pub fn make_default_player() -> Result<String, String> {
    Err("Setting the default player is only supported on Linux for now.".into())
}

#[cfg(target_os = "linux")]
pub fn make_default_player() -> Result<String, String> {
    use std::path::PathBuf;
    use std::process::Command;

    let home = std::env::var_os("HOME").map(PathBuf::from).ok_or("HOME is not set")?;
    let data = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".local/share"));
    let exe = install_program(&home)?;

    let apps = data.join("applications");
    let icons = data.join("icons/hicolor/scalable/apps");
    std::fs::create_dir_all(&apps).map_err(|err| err.to_string())?;
    std::fs::create_dir_all(&icons).map_err(|err| err.to_string())?;
    std::fs::write(icons.join(format!("{APP_ID}.svg")), crate::icon::SVG).map_err(|err| err.to_string())?;

    let exec = exe.to_string_lossy().replace('\\', "\\\\").replace('"', "\\\"");
    let entry = format!(
        "[Desktop Entry]\n\
         Type=Application\n\
         Name={name}\n\
         GenericName=Audio Player\n\
         Comment=Play songs and whole folders\n\
         Exec=\"{exec}\" %F\n\
         Icon={APP_ID}\n\
         Terminal=false\n\
         Categories=AudioVideo;Audio;Player;\n\
         MimeType={mimes};\n\
         StartupWMClass={APP_ID}\n",
        name = crate::APP_NAME,
        mimes = MIME_TYPES.join(";"),
    );
    let desktop_file = format!("{APP_ID}.desktop");
    std::fs::write(apps.join(&desktop_file), entry).map_err(|err| err.to_string())?;

    let _ = Command::new("update-desktop-database").arg(&apps).status();
    let status = Command::new("xdg-mime")
        .arg("default")
        .arg(&desktop_file)
        .args(MIME_TYPES)
        .status()
        .map_err(|err| format!("Couldn't run xdg-mime: {err}"))?;
    if !status.success() {
        return Err("xdg-mime refused the change".into());
    }
    Ok("Installed. Double-clicking a song now opens it here.".into())
}

/// Copies the running program to ~/.local/bin, so the launcher keeps working
/// after the build folder changes or goes away. Returns the installed path.
#[cfg(target_os = "linux")]
fn install_program(home: &std::path::Path) -> Result<std::path::PathBuf, String> {
    let running = std::env::current_exe().map_err(|err| format!("Can't find the player program: {err}"))?;
    let bin = home.join(".local/bin");
    let target = bin.join("easy-player");
    if running == target {
        return Ok(target);
    }
    std::fs::create_dir_all(&bin).map_err(|err| err.to_string())?;
    // Copy then rename: replaces an installed copy even while it is running.
    let partial = bin.join(".easy-player.partial");
    std::fs::copy(&running, &partial).map_err(|err| format!("Couldn't copy the player: {err}"))?;
    std::fs::rename(&partial, &target).map_err(|err| format!("Couldn't install the player: {err}"))?;
    Ok(target)
}
