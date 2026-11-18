//! Locating and running the `ffmpeg` / `ffprobe` executables.

use crate::{Error, Result};
use serde::Serialize;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex, OnceLock};

/// Resolves a tool: `$MEDIAANON_FFMPEG` / `$MEDIAANON_FFPROBE`, then next to
/// our own executable (bundled builds), then `PATH`, then well-known install
/// locations.
///
/// The last step matters for desktop apps: a macOS `.app` started from the
/// Finder gets a minimal `PATH` without Homebrew, and Windows apps started
/// from the Start menu may miss directories added for the shell only.
fn locate(name: &str, env: &str) -> PathBuf {
    if let Some(p) = std::env::var_os(env).filter(|p| !p.is_empty()) {
        return PathBuf::from(p);
    }
    let file = format!("{name}{}", std::env::consts::EXE_SUFFIX);
    let beside_exe = std::env::current_exe().ok().and_then(|e| e.parent().map(|d| d.join(&file)));
    let on_path = std::env::var_os("PATH").into_iter().flat_map(|p| std::env::split_paths(&p).collect::<Vec<_>>());
    beside_exe
        .into_iter()
        .chain(on_path.map(|d| d.join(&file)))
        .chain(well_known_dirs().into_iter().map(|d| d.join(&file)))
        .find(|p| p.is_file())
        .map(|p| if cfg!(windows) { scoop_shim_target(&p).unwrap_or(p) } else { p })
        .unwrap_or_else(|| PathBuf::from(file))
}

/// The executable a Scoop shim starts, read from the `<name>.shim` file next
/// to it (`path = "C:\...\ffprobe.exe"`).
///
/// Scoop puts small launcher executables in `scoop\shims`. Running FFmpeg
/// through them adds a process in between, and they can fail where the real
/// executable works ("Shim: Could not create process with command ..."), so
/// the real one is run directly.
fn scoop_shim_target(exe: &Path) -> Option<PathBuf> {
    let text = std::fs::read_to_string(exe.with_extension("shim")).ok()?;
    let target = text.trim_start_matches('\u{feff}').lines().find_map(|line| {
        let (key, value) = line.split_once('=')?;
        (key.trim() == "path").then(|| value.trim().trim_matches('"').trim())
    })?;
    let target = PathBuf::from(target);
    target.is_file().then_some(target)
}

/// Usual FFmpeg install directories per platform.
fn well_known_dirs() -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = Vec::new();
    if cfg!(target_os = "macos") {
        // Homebrew (Apple silicon, Intel) and MacPorts.
        dirs.extend(["/opt/homebrew/bin", "/usr/local/bin", "/opt/local/bin"].map(PathBuf::from));
    } else if cfg!(windows) {
        let var = |k: &str| std::env::var_os(k).map(PathBuf::from);
        if let Some(local) = var("LOCALAPPDATA") {
            dirs.push(local.join("Microsoft").join("WinGet").join("Links"));
        }
        if let Some(pf) = var("ProgramFiles") {
            dirs.push(pf.join("ffmpeg").join("bin"));
        }
        if let Some(choco) = var("ChocolateyInstall") {
            dirs.push(choco.join("bin"));
        }
        // Scoop: the real executables first (`$SCOOP` is a custom install
        // root), then the shims (followed by `scoop_shim_target`).
        let scoop = var("SCOOP").or_else(|| var("USERPROFILE").map(|h| h.join("scoop")));
        if let Some(scoop) = &scoop {
            dirs.push(scoop.join("apps").join("ffmpeg").join("current").join("bin"));
        }
        if let Some(global) = var("SCOOP_GLOBAL").or_else(|| var("ProgramData").map(|d| d.join("scoop"))) {
            dirs.push(global.join("apps").join("ffmpeg").join("current").join("bin"));
        }
        if let Some(scoop) = scoop {
            dirs.push(scoop.join("shims"));
        }
        dirs.push(PathBuf::from(r"C:\ffmpeg\bin"));
    } else {
        dirs.extend(["/usr/local/bin", "/usr/bin", "/snap/bin"].map(PathBuf::from));
    }
    dirs
}

pub fn ffmpeg_path() -> &'static Path {
    static P: OnceLock<PathBuf> = OnceLock::new();
    P.get_or_init(|| locate("ffmpeg", "MEDIAANON_FFMPEG"))
}

pub fn ffprobe_path() -> &'static Path {
    static P: OnceLock<PathBuf> = OnceLock::new();
    P.get_or_init(|| locate("ffprobe", "MEDIAANON_FFPROBE"))
}

pub(crate) fn command(tool: &Path) -> Command {
    let mut c = Command::new(tool);
    c.stdin(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        c.creation_flags(CREATE_NO_WINDOW);
    }
    c
}

pub(crate) fn not_found(tool: &Path, e: std::io::Error) -> Error {
    if e.kind() == std::io::ErrorKind::NotFound {
        Error::Ffmpeg(format!(
            "`{}` not found: install FFmpeg (5.1 or newer) and make sure it is on PATH, \
             or set MEDIAANON_FFMPEG / MEDIAANON_FFPROBE",
            tool.display()
        ))
    } else {
        Error::Ffmpeg(format!("cannot run `{}`: {e}", tool.display()))
    }
}

/// Runs a command to completion and returns its stdout, or an error carrying
/// its stderr.
pub(crate) fn run(mut cmd: Command) -> Result<Vec<u8>> {
    let tool = PathBuf::from(cmd.get_program());
    tracing::debug!(command = ?cmd, "running");
    let out = cmd.stdout(Stdio::piped()).stderr(Stdio::piped()).output().map_err(|e| not_found(&tool, e))?;
    if !out.status.success() {
        return Err(Error::Ffmpeg(format!(
            "`{}` failed ({}): {}",
            tool.display(),
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    Ok(out.stdout)
}

/// A spawned process whose stderr is collected in the background so that
/// it can never fill its pipe and block, and is available for error messages.
pub(crate) struct Process {
    pub child: Child,
    stderr: Arc<Mutex<String>>,
    reader: Option<std::thread::JoinHandle<()>>,
    name: &'static str,
}

impl Process {
    pub fn spawn(mut cmd: Command, name: &'static str) -> Result<Self> {
        let tool = PathBuf::from(cmd.get_program());
        tracing::debug!(command = ?cmd, "spawning {name}");
        let mut child = cmd.stderr(Stdio::piped()).spawn().map_err(|e| not_found(&tool, e))?;
        let stderr = Arc::new(Mutex::new(String::new()));
        let mut pipe = child.stderr.take().expect("stderr is piped");
        let sink = Arc::clone(&stderr);
        let reader = std::thread::spawn(move || {
            let mut buf = [0u8; 4096];
            while let Ok(n) = pipe.read(&mut buf) {
                if n == 0 {
                    break;
                }
                let mut s = sink.lock().unwrap_or_else(|e| e.into_inner());
                s.push_str(&String::from_utf8_lossy(&buf[..n]));
                // Keep only the tail: that is where FFmpeg puts the actual error.
                if s.len() > 16 * 1024 {
                    let cut = s.len() - 8 * 1024;
                    let cut = (cut..s.len()).find(|&i| s.is_char_boundary(i)).unwrap_or(s.len());
                    s.drain(..cut);
                }
            }
        });
        Ok(Self { child, stderr, reader: Some(reader), name })
    }

    /// Waits for exit; returns an error with the stderr tail on failure.
    pub fn finish(mut self) -> Result<()> {
        let status = self.child.wait().map_err(|e| Error::Ffmpeg(e.to_string()))?;
        if let Some(r) = self.reader.take() {
            let _ = r.join();
        }
        if status.success() {
            Ok(())
        } else {
            Err(self.error(&status.to_string()))
        }
    }

    pub fn error(&self, what: &str) -> Error {
        let log = self.stderr.lock().unwrap_or_else(|e| e.into_inner());
        Error::Ffmpeg(format!("{} {what}: {}", self.name, log.trim()))
    }

    pub fn kill(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// What the installed FFmpeg supports.
#[derive(Debug, Clone, Default, Serialize)]
pub struct Capabilities {
    pub version: String,
    /// Hardware decoding back-ends (`ffmpeg -hwaccels`).
    pub hwaccels: Vec<String>,
    /// Video encoders (`ffmpeg -encoders`).
    pub encoders: Vec<String>,
}

impl Capabilities {
    pub fn has_encoder(&self, name: &str) -> bool {
        self.encoders.iter().any(|e| e == name)
    }
}

/// Queries (once) and caches the FFmpeg capabilities.
pub fn capabilities() -> Result<&'static Capabilities> {
    static CAPS: OnceLock<std::result::Result<Capabilities, String>> = OnceLock::new();
    CAPS.get_or_init(|| query_capabilities().map_err(|e| e.to_string())).as_ref().map_err(|e| Error::Ffmpeg(e.clone()))
}

fn query_capabilities() -> Result<Capabilities> {
    let text = |args: &[&str]| -> Result<String> {
        let mut c = command(ffmpeg_path());
        c.args(["-hide_banner"]).args(args);
        Ok(String::from_utf8_lossy(&run(c)?).into_owned())
    };
    let version = text(&["-version"])?
        .lines()
        .next()
        .and_then(|l| l.strip_prefix("ffmpeg version "))
        .map(|l| l.split_whitespace().next().unwrap_or_default().to_string())
        .unwrap_or_default();
    let hwaccels = text(&["-hwaccels"])?
        .lines()
        .skip_while(|l| !l.starts_with("Hardware acceleration methods"))
        .skip(1)
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(String::from)
        .collect();
    // Lines look like " V....D libx264   libx264 H.264 / AVC ..."
    let encoders = text(&["-encoders"])?
        .lines()
        .filter_map(|l| {
            let mut it = l.split_whitespace();
            let flags = it.next()?;
            let name = it.next()?;
            (flags.len() == 6 && flags.starts_with('V') && name != "=").then(|| name.to_string())
        })
        .collect();
    Ok(Capabilities { version, hwaccels, encoders })
}

#[cfg(test)]
mod tests {
    use super::scoop_shim_target;

    #[test]
    fn scoop_shims_are_followed_to_the_real_executable() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("apps").join("ffprobe.exe");
        std::fs::create_dir_all(real.parent().unwrap()).unwrap();
        std::fs::write(&real, b"").unwrap();
        let shim = dir.path().join("ffprobe.exe");
        std::fs::write(&shim, b"").unwrap();

        // No .shim file: not a Scoop shim.
        assert_eq!(scoop_shim_target(&shim), None);
        // Scoop's format, with a UTF-8 BOM and CRLF line endings.
        let text = format!("\u{feff}path = \"{}\"\r\nargs = -hide_banner\r\n", real.display());
        std::fs::write(dir.path().join("ffprobe.shim"), text).unwrap();
        assert_eq!(scoop_shim_target(&shim), Some(real.clone()));
        // A shim pointing to a missing file is ignored.
        std::fs::remove_file(&real).unwrap();
        assert_eq!(scoop_shim_target(&shim), None);
    }
}
