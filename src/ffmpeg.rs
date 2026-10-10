//! Locating and launching the ffmpeg / ffprobe binaries.

use std::path::{Path, PathBuf};
use std::process::Command;

#[cfg(windows)]
use std::os::windows::process::CommandExt;

/// Keep ffmpeg's console window from flashing on screen.
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Where we look for a tool, in order: next to our own exe, the usual Windows
/// install spots, then PATH.
pub fn find_tool(name: &str) -> Option<PathBuf> {
    let file = format!("{name}.exe");

    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let p = dir.join(&file);
            if p.is_file() {
                return Some(p);
            }
            let p = dir.join("ffmpeg").join("bin").join(&file);
            if p.is_file() {
                return Some(p);
            }
        }
    }

    for dir in [
        r"C:\ffmpeg\bin",
        r"C:\Program Files\ffmpeg\bin",
        r"C:\Program Files (x86)\ffmpeg\bin",
    ] {
        let p = Path::new(dir).join(&file);
        if p.is_file() {
            return Some(p);
        }
    }

    if let Ok(path) = std::env::var("PATH") {
        for dir in std::env::split_paths(&path) {
            let p = dir.join(&file);
            if p.is_file() {
                return Some(p);
            }
        }
    }

    None
}

/// A `Command` for `exe` that won't pop a console window.
pub fn command(exe: &Path) -> Command {
    let mut cmd = Command::new(exe);
    #[cfg(windows)]
    cmd.creation_flags(CREATE_NO_WINDOW);
    cmd
}

/// Both tools we need, resolved once at startup.
#[derive(Clone)]
pub struct Tools {
    pub ffmpeg: PathBuf,
    pub ffprobe: PathBuf,
}

impl Tools {
    pub fn locate() -> Result<Self, String> {
        let ffmpeg = find_tool("ffmpeg").ok_or_else(|| {
            r"ffmpeg.exe not found (looked next to this app, in C:\ffmpeg\bin, and on PATH)"
                .to_owned()
        })?;
        let ffprobe = find_tool("ffprobe").ok_or_else(|| {
            r"ffprobe.exe not found (looked next to this app, in C:\ffmpeg\bin, and on PATH)"
                .to_owned()
        })?;
        Ok(Self { ffmpeg, ffprobe })
    }
}
