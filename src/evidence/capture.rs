//! Making captures: a page in headless Chrome, an app window or the screen (image or a short
//! recording), and turning recordings into an animated GIF the chat can play inline.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use super::ItemKind;
use super::exec::{CmdResult, Execution};

/// Longest recording oxi makes (and the longest stretch a GIF preview keeps).
pub const MAX_VIDEO_SECS: u64 = 30;

pub struct Captured {
    pub file: PathBuf,
    pub kind: ItemKind,
    pub preview: Option<PathBuf>,
    pub source: String,
}

pub fn kind_of(path: &Path) -> Option<ItemKind> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    match ext.as_str() {
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" => Some(ItemKind::Image),
        "mp4" | "m4v" | "mov" | "webm" | "mkv" => Some(ItemKind::Video),
        _ => None,
    }
}

fn quiet(command: &mut Command) -> &mut Command {
    // No console window for the browser/ffmpeg capture helpers on a Windows GUI process.
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000);
    }
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
}

fn finish(
    c: Command,
    what: &str,
    timeout: Duration,
    execution: &Execution,
) -> Result<CmdResult, String> {
    let result = execution.run_child(c, timeout)?;
    if result.timed_out {
        return Err(format!("{what} timed out."));
    }
    Ok(result)
}

fn require_file(path: &Path, what: &str, result: &CmdResult) -> Result<(), String> {
    if std::fs::metadata(path).is_ok_and(|m| m.len() > 0) {
        return Ok(());
    }
    let detail = result.output.trim();
    Err(if detail.is_empty() {
        format!("{what} produced no file.")
    } else {
        format!("{what} produced no file: {detail}")
    })
}

// ── Web page ────────────────────────────────────────────────────────────────────────────────

/// A Chromium-family browser to run headless.
pub fn find_browser() -> Option<PathBuf> {
    #[cfg(target_os = "macos")]
    let candidates: Vec<PathBuf> = [
        "Google Chrome",
        "Chromium",
        "Brave Browser",
        "Microsoft Edge",
        "Google Chrome Canary",
    ]
    .iter()
    .flat_map(|app| {
        let bin = format!("{app}.app/Contents/MacOS/{app}");
        let mut paths = vec![PathBuf::from("/Applications").join(&bin)];
        if let Some(home) = crate::app_dirs::home_dir() {
            paths.push(home.join("Applications").join(&bin));
        }
        paths
    })
    .collect();
    #[cfg(windows)]
    let candidates: Vec<PathBuf> = ["PROGRAMFILES", "PROGRAMFILES(X86)", "LOCALAPPDATA"]
        .iter()
        .filter_map(|var| std::env::var_os(var).map(PathBuf::from))
        .flat_map(|root| {
            [
                root.join("Google/Chrome/Application/chrome.exe"),
                root.join("Microsoft/Edge/Application/msedge.exe"),
                root.join("BraveSoftware/Brave-Browser/Application/brave.exe"),
            ]
        })
        .collect();
    #[cfg(all(unix, not(target_os = "macos")))]
    let candidates: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|path| {
            std::env::split_paths(&path)
                .flat_map(|dir| {
                    [
                        "google-chrome",
                        "google-chrome-stable",
                        "chromium",
                        "chromium-browser",
                        "brave-browser",
                        "microsoft-edge",
                    ]
                    .map(|name| dir.join(name))
                })
                .collect()
        })
        .unwrap_or_default();
    candidates.into_iter().find(|path| path.is_file())
}

/// Screenshot `url` at a `width`×`height` viewport (rendered at 2× for sharpness).
#[cfg(test)]
pub fn url(
    url: &str,
    out: &Path,
    width: u32,
    height: u32,
    wait_ms: u64,
) -> Result<Captured, String> {
    url_with_execution(url, out, width, height, wait_ms, &Execution::default())
}

pub fn url_with_execution(
    url: &str,
    out: &Path,
    width: u32,
    height: u32,
    wait_ms: u64,
    execution: &Execution,
) -> Result<Captured, String> {
    let browser = find_browser().ok_or(
        "No Chrome, Chromium, Edge or Brave found for headless capture. Capture the page another way (a window capture of a visible browser, or a command that writes a screenshot) instead.",
    )?;
    let profile = out.with_extension("chrome-profile");
    let mut c = Command::new(&browser);
    c.args([
        "--headless=new",
        "--disable-gpu",
        "--hide-scrollbars",
        "--no-first-run",
        "--no-default-browser-check",
        "--force-device-scale-factor=2",
    ])
    .arg(format!("--user-data-dir={}", profile.display()))
    .arg(format!("--window-size={width},{height}"))
    .arg(format!("--virtual-time-budget={}", wait_ms.max(500)))
    .arg(format!("--screenshot={}", out.display()))
    .arg(url);
    quiet(&mut c);
    let result = finish(
        c,
        "The headless browser",
        Duration::from_secs(90),
        execution,
    );
    let _ = std::fs::remove_dir_all(&profile);
    let result = result?;
    require_file(out, "The headless browser", &result)?;
    Ok(Captured {
        file: out.to_path_buf(),
        kind: ItemKind::Image,
        preview: None,
        source: format!(
            "{} headless · {url}",
            browser
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| "Chrome".into())
        ),
    })
}

// ── Window and screen ───────────────────────────────────────────────────────────────────────

/// What to capture on screen.
pub enum ScreenTarget<'a> {
    /// The largest on-screen window whose app name or title contains this (case-insensitive).
    Window(&'a str),
    Screen,
}

/// Capture a window or the screen: an image, or a recording of `seconds` while `during` (a
/// shell command run in `cwd`) drives the app.
pub fn screen_with_execution(
    target: ScreenTarget<'_>,
    out_stem: &Path,
    seconds: u64,
    during: Option<&str>,
    cwd: &Path,
    execution: &Execution,
) -> Result<Captured, String> {
    platform::screen(
        target,
        out_stem,
        seconds.min(MAX_VIDEO_SECS),
        during,
        cwd,
        execution,
    )
}

/// Start `during` (if any) shortly after a recording began, so its first action is recorded.
#[cfg(unix)]
fn spawn_during(
    during: Option<&str>,
    cwd: &Path,
    execution: &Execution,
) -> Option<std::thread::JoinHandle<()>> {
    let command = during?.to_string();
    let cwd = cwd.to_path_buf();
    let execution = execution.clone();
    Some(std::thread::spawn(move || {
        if execution.wait(Duration::from_millis(700)).is_ok() {
            let _ = execution.run(&command, &cwd, Duration::from_secs(MAX_VIDEO_SECS + 30));
        }
    }))
}

#[cfg(target_os = "macos")]
mod platform {
    use super::*;

    /// Smaller than this, a window is a Stage Manager or Mission Control thumbnail.
    const MIN_W: f64 = 300.0;
    const MIN_H: f64 = 200.0;

    /// The window to capture, brought to the front first when it is only shown as a thumbnail.
    fn front_window(query: &str, execution: &Execution) -> Result<mac::Window, String> {
        let missing = || {
            let names = mac::window_owners();
            format!(
                "No on-screen window matches \"{query}\". Start the app first and leave its window visible. Windows on screen: {}.",
                if names.is_empty() {
                    "none".to_string()
                } else {
                    names.join(", ")
                }
            )
        };
        let window = mac::find_window(query).ok_or_else(missing)?;
        if window.width >= MIN_W && window.height >= MIN_H {
            return Ok(window);
        }
        // Stage Manager shrinks the windows of apps in the background into its strip.
        let script = format!(
            "tell application \"System Events\" to set frontmost of (first process whose unix id is {}) to true",
            window.pid
        );
        let mut activate = Command::new("osascript");
        activate.args(["-e", &script]);
        quiet(&mut activate);
        let _ = execution.run_child(activate, Duration::from_secs(10));
        execution.wait(Duration::from_millis(900))?;
        let window = mac::find_window(query).ok_or_else(missing)?;
        if window.width >= MIN_W && window.height >= MIN_H {
            return Ok(window);
        }
        Err(format!(
            "The {} window is only shown as a {}×{} thumbnail (Stage Manager, or minimized) and oxi could not bring it to the front. Ask the user to bring it forward, or capture another way.",
            window.owner, window.width as i64, window.height as i64
        ))
    }

    pub fn screen(
        target: ScreenTarget<'_>,
        out_stem: &Path,
        seconds: u64,
        during: Option<&str>,
        cwd: &Path,
        execution: &Execution,
    ) -> Result<Captured, String> {
        if !mac::screen_capture_allowed() {
            mac::request_screen_capture();
            return Err("oxi is not allowed to record the screen. Ask the user to allow oxi in System Settings > Privacy & Security > Screen & System Audio Recording (then restart oxi), or capture another way (url, or a command that writes the image).".into());
        }
        let window = match target {
            ScreenTarget::Window(query) => Some(front_window(query, execution)?),
            ScreenTarget::Screen => None,
        };
        let source = match &window {
            Some(w) => format!("Window of {}", w.owner),
            None => "Screen".to_string(),
        };
        let mut c = Command::new("/usr/sbin/screencapture");
        c.arg("-x");
        if seconds == 0 {
            let out = out_stem.with_extension("png");
            match &window {
                Some(w) => c.arg("-o").arg(format!("-l{}", w.id)),
                None => c.arg("-m"),
            };
            c.arg(&out);
            quiet(&mut c);
            let result = finish(c, "screencapture", Duration::from_secs(30), execution)?;
            require_file(&out, "screencapture", &result)?;
            return Ok(Captured {
                file: out,
                kind: ItemKind::Image,
                preview: None,
                source,
            });
        }
        let out = out_stem.with_extension("mov");
        c.args(["-v", "-k"]).arg(format!("-V{seconds}"));
        match &window {
            Some(w) => c.arg(format!(
                "-R{},{},{},{}",
                w.x as i64, w.y as i64, w.width as i64, w.height as i64
            )),
            None => c.arg("-m"),
        };
        c.arg(&out);
        quiet(&mut c);
        let driver = spawn_during(during, cwd, execution);
        let result = finish(
            c,
            "screencapture",
            Duration::from_secs(seconds + 30),
            execution,
        );
        if let Some(driver) = driver {
            let _ = driver.join();
        }
        let result = result?;
        require_file(&out, "screencapture", &result)?;
        Ok(Captured {
            preview: video_preview_with_execution(&out, execution),
            file: out,
            kind: ItemKind::Video,
            source: format!("{source} · {seconds}s recording"),
        })
    }
}

#[cfg(all(unix, not(target_os = "macos")))]
mod platform {
    use super::*;

    fn on_path(name: &str) -> bool {
        std::env::var_os("PATH")
            .is_some_and(|path| std::env::split_paths(&path).any(|d| d.join(name).is_file()))
    }

    pub fn screen(
        target: ScreenTarget<'_>,
        out_stem: &Path,
        seconds: u64,
        during: Option<&str>,
        cwd: &Path,
        execution: &Execution,
    ) -> Result<Captured, String> {
        if let ScreenTarget::Window(query) = target {
            return Err(format!(
                "Cannot capture window \"{query}\": window capture is only available on macOS for now. Use screen, url, or a command that writes the image."
            ));
        }
        let wayland = std::env::var_os("WAYLAND_DISPLAY").is_some();
        if seconds == 0 {
            let out = out_stem.with_extension("png");
            let mut c = if wayland && on_path("grim") {
                let mut c = Command::new("grim");
                c.arg(&out);
                c
            } else if on_path("import") {
                let mut c = Command::new("import");
                c.args(["-window", "root"]).arg(&out);
                c
            } else {
                return Err("No screenshot tool found (install grim on Wayland or ImageMagick's import on X11).".into());
            };
            quiet(&mut c);
            let result = finish(c, "The screenshot tool", Duration::from_secs(30), execution)?;
            require_file(&out, "The screenshot tool", &result)?;
            return Ok(Captured {
                file: out,
                kind: ItemKind::Image,
                preview: None,
                source: "Screen".into(),
            });
        }
        let display = std::env::var("DISPLAY")
            .ok()
            .filter(|_| !wayland && on_path("ffmpeg"));
        let Some(display) = display else {
            return Err("Screen recording needs ffmpeg on an X11 session.".into());
        };
        let out = out_stem.with_extension("mp4");
        let mut c = Command::new("ffmpeg");
        c.args(["-y", "-loglevel", "error", "-f", "x11grab", "-t"])
            .arg(seconds.to_string())
            .args(["-i", &display, "-pix_fmt", "yuv420p"])
            .arg(&out);
        quiet(&mut c);
        let driver = spawn_during(during, cwd, execution);
        let result = finish(c, "ffmpeg", Duration::from_secs(seconds + 30), execution);
        if let Some(driver) = driver {
            let _ = driver.join();
        }
        let result = result?;
        require_file(&out, "ffmpeg", &result)?;
        Ok(Captured {
            preview: video_preview_with_execution(&out, execution),
            file: out,
            kind: ItemKind::Video,
            source: format!("Screen · {seconds}s recording"),
        })
    }
}

#[cfg(windows)]
mod platform {
    use super::*;

    pub fn screen(
        target: ScreenTarget<'_>,
        _out_stem: &Path,
        _seconds: u64,
        _during: Option<&str>,
        _cwd: &Path,
        execution: &Execution,
    ) -> Result<Captured, String> {
        execution.check_cancelled()?;
        let target = match target {
            ScreenTarget::Window(query) => format!("Window \"{query}\""),
            ScreenTarget::Screen => "Screen".into(),
        };
        Err(format!(
            "{target} capture is not available on Windows yet. Use url, or a command that writes the image or video."
        ))
    }
}

// ── Command output ──────────────────────────────────────────────────────────────────────────

/// Run `command`, then take the image or video it wrote to `output` (relative to `cwd`).
#[cfg(all(test, unix))]
pub fn command_output(
    command: &str,
    output: &str,
    cwd: &Path,
    timeout: Duration,
    dest_stem: &Path,
) -> Result<(Captured, CmdResult), String> {
    command_output_with_execution(
        command,
        output,
        cwd,
        timeout,
        dest_stem,
        &Execution::default(),
    )
}

pub fn command_output_with_execution(
    command: &str,
    output: &str,
    cwd: &Path,
    timeout: Duration,
    dest_stem: &Path,
    execution: &Execution,
) -> Result<(Captured, CmdResult), String> {
    let source = cwd.join(output);
    let kind = kind_of(&source).ok_or_else(|| {
        format!("`output` must be an image or video file (png, jpg, gif, webp, mp4, mov, webm), not {output}.")
    })?;
    let before = std::fs::metadata(&source).and_then(|m| m.modified()).ok();
    let result = execution.run(command, cwd, timeout)?;
    let written = std::fs::metadata(&source)
        .and_then(|m| m.modified())
        .ok()
        .is_some_and(|modified| before.is_none_or(|b| modified > b));
    if !written {
        return Err(format!(
            "The command finished (exit code {}) without writing {output}.\n{}",
            result
                .exit_code
                .map_or_else(|| "none".to_string(), |c| c.to_string()),
            result.output.trim()
        ));
    }
    let file = copy_into(&source, dest_stem)?;
    let preview = (kind == ItemKind::Video)
        .then(|| video_preview_with_execution(&file, execution))
        .flatten();
    Ok((
        Captured {
            file,
            kind,
            preview,
            source: format!("`{command}` → {output}"),
        },
        result,
    ))
}

/// Copy `source` next to `dest_stem`, keeping its extension.
pub fn copy_into(source: &Path, dest_stem: &Path) -> Result<PathBuf, String> {
    let ext = source
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    let dest = dest_stem.with_extension(ext);
    std::fs::copy(source, &dest)
        .map_err(|e| format!("Could not copy {}: {e}", source.display()))?;
    Ok(dest)
}

// ── Video preview ───────────────────────────────────────────────────────────────────────────

fn ffmpeg() -> Option<PathBuf> {
    let mut dirs: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|path| std::env::split_paths(&path).collect())
        .unwrap_or_default();
    // An app started from the Dock gets a short PATH.
    dirs.extend(["/opt/homebrew/bin", "/usr/local/bin"].map(PathBuf::from));
    let name = if cfg!(windows) {
        "ffmpeg.exe"
    } else {
        "ffmpeg"
    };
    dirs.into_iter()
        .map(|dir| dir.join(name))
        .find(|path| path.is_file())
}

/// An animated GIF of the recording for the chat (ffmpeg), else a poster frame (Quick Look).
pub fn video_preview_with_execution(video: &Path, execution: &Execution) -> Option<PathBuf> {
    if let Some(ffmpeg) = ffmpeg() {
        let gif = video.with_extension("preview.gif");
        let mut c = Command::new(ffmpeg);
        c.args(["-y", "-loglevel", "error", "-t"])
            .arg(MAX_VIDEO_SECS.to_string())
            .arg("-i")
            .arg(video)
            .args([
                "-vf",
                "fps=12,scale='min(1100,iw)':-2:flags=lanczos,split[a][b];[a]palettegen=stats_mode=diff[p];[b][p]paletteuse=dither=bayer:bayer_scale=4",
                "-loop",
                "0",
            ])
            .arg(&gif);
        quiet(&mut c);
        if execution
            .run_child(c, Duration::from_secs(120))
            .is_ok_and(|r| r.passed())
            && gif.is_file()
        {
            return Some(gif);
        }
    }
    #[cfg(target_os = "macos")]
    {
        let dir = video.parent()?;
        let mut c = Command::new("qlmanage");
        c.args(["-t", "-s", "1100", "-o"]).arg(dir).arg(video);
        quiet(&mut c);
        let _ = execution.run_child(c, Duration::from_secs(30));
        let poster = dir.join(format!("{}.png", video.file_name()?.to_string_lossy()));
        if poster.is_file() {
            return Some(poster);
        }
    }
    None
}

// ── macOS window list ───────────────────────────────────────────────────────────────────────

#[cfg(target_os = "macos")]
mod mac {
    use std::ffi::{CStr, CString, c_char, c_void};

    type CFRef = *const c_void;

    #[repr(C)]
    #[derive(Default)]
    struct CGRect {
        x: f64,
        y: f64,
        width: f64,
        height: f64,
    }

    #[link(name = "CoreGraphics", kind = "framework")]
    unsafe extern "C" {
        fn CGWindowListCopyWindowInfo(option: u32, relative_to: u32) -> CFRef;
        fn CGRectMakeWithDictionaryRepresentation(dict: CFRef, rect: *mut CGRect) -> bool;
        fn CGPreflightScreenCaptureAccess() -> bool;
        fn CGRequestScreenCaptureAccess() -> bool;
    }

    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        fn CFArrayGetCount(array: CFRef) -> isize;
        fn CFArrayGetValueAtIndex(array: CFRef, index: isize) -> CFRef;
        fn CFDictionaryGetValue(dict: CFRef, key: CFRef) -> CFRef;
        fn CFStringCreateWithCString(alloc: CFRef, s: *const c_char, encoding: u32) -> CFRef;
        fn CFStringGetCString(s: CFRef, buf: *mut c_char, size: isize, encoding: u32) -> bool;
        fn CFNumberGetValue(number: CFRef, kind: isize, out: *mut c_void) -> bool;
        fn CFRelease(object: CFRef);
    }

    const UTF8: u32 = 0x0800_0100;
    const ON_SCREEN_ONLY: u32 = 1;
    const EXCLUDE_DESKTOP: u32 = 16;
    const SINT64: isize = 4;

    pub struct Window {
        pub id: i64,
        pub pid: i64,
        pub owner: String,
        pub x: f64,
        pub y: f64,
        pub width: f64,
        pub height: f64,
    }

    struct Key(CFRef);

    impl Key {
        fn new(name: &str) -> Self {
            let name = CString::new(name).expect("no NUL");
            // SAFETY: a valid C string; the CFString is released in Drop.
            Self(unsafe { CFStringCreateWithCString(std::ptr::null(), name.as_ptr(), UTF8) })
        }
    }

    impl Drop for Key {
        fn drop(&mut self) {
            if !self.0.is_null() {
                // SAFETY: created by CFStringCreateWithCString above and released once.
                unsafe { CFRelease(self.0) }
            }
        }
    }

    unsafe fn string(dict: CFRef, key: &Key) -> String {
        // SAFETY (caller): `dict` is a live CFDictionary from the window list.
        let value = unsafe { CFDictionaryGetValue(dict, key.0) };
        if value.is_null() {
            return String::new();
        }
        let mut buf = [0 as c_char; 512];
        // SAFETY: the window list stores CFStrings under the name keys; buf is large enough or
        // the call fails and the name stays empty.
        if unsafe { CFStringGetCString(value, buf.as_mut_ptr(), buf.len() as isize, UTF8) } {
            // SAFETY: CFStringGetCString NUL-terminated the buffer.
            unsafe { CStr::from_ptr(buf.as_ptr()) }
                .to_string_lossy()
                .into_owned()
        } else {
            String::new()
        }
    }

    unsafe fn number(dict: CFRef, key: &Key) -> i64 {
        // SAFETY (caller): `dict` is a live CFDictionary from the window list.
        let value = unsafe { CFDictionaryGetValue(dict, key.0) };
        let mut out: i64 = 0;
        if !value.is_null() {
            // SAFETY: the number keys hold CFNumbers; out is an i64.
            unsafe { CFNumberGetValue(value, SINT64, (&mut out as *mut i64).cast()) };
        }
        out
    }

    /// On-screen, normal-layer windows: (window, title).
    fn windows() -> Vec<(Window, String)> {
        let keys = [
            "kCGWindowNumber",
            "kCGWindowOwnerName",
            "kCGWindowName",
            "kCGWindowLayer",
            "kCGWindowBounds",
            "kCGWindowOwnerPID",
        ]
        .map(Key::new);
        // SAFETY: the returned CFArray is owned here and released below; every dictionary and
        // value read from it stays valid until then.
        unsafe {
            let list = CGWindowListCopyWindowInfo(ON_SCREEN_ONLY | EXCLUDE_DESKTOP, 0);
            if list.is_null() {
                return Vec::new();
            }
            let mut out = Vec::new();
            for i in 0..CFArrayGetCount(list) {
                let dict = CFArrayGetValueAtIndex(list, i);
                if number(dict, &keys[3]) != 0 {
                    continue;
                }
                let mut rect = CGRect::default();
                let bounds = CFDictionaryGetValue(dict, keys[4].0);
                if bounds.is_null() || !CGRectMakeWithDictionaryRepresentation(bounds, &mut rect) {
                    continue;
                }
                if rect.width < 40.0 || rect.height < 40.0 {
                    continue;
                }
                out.push((
                    Window {
                        id: number(dict, &keys[0]),
                        pid: number(dict, &keys[5]),
                        owner: string(dict, &keys[1]),
                        x: rect.x,
                        y: rect.y,
                        width: rect.width,
                        height: rect.height,
                    },
                    string(dict, &keys[2]),
                ));
            }
            CFRelease(list);
            out
        }
    }

    /// The largest on-screen window whose app name or title contains `query`.
    pub fn find_window(query: &str) -> Option<Window> {
        let query = query.trim().to_lowercase();
        windows()
            .into_iter()
            .filter(|(w, title)| {
                w.owner.to_lowercase().contains(&query) || title.to_lowercase().contains(&query)
            })
            .map(|(w, _)| w)
            .max_by(|a, b| (a.width * a.height).total_cmp(&(b.width * b.height)))
    }

    pub fn window_owners() -> Vec<String> {
        let mut names: Vec<String> = windows().into_iter().map(|(w, _)| w.owner).collect();
        names.sort();
        names.dedup();
        names
    }

    pub fn screen_capture_allowed() -> bool {
        // SAFETY: no arguments; available since macOS 10.15.
        unsafe { CGPreflightScreenCaptureAccess() }
    }

    pub fn request_screen_capture() {
        // SAFETY: as above; shows the system prompt once per app.
        unsafe {
            CGRequestScreenCaptureAccess();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_capture_files() {
        assert_eq!(kind_of(Path::new("a/shot.PNG")), Some(ItemKind::Image));
        assert_eq!(kind_of(Path::new("rec.webm")), Some(ItemKind::Video));
        assert_eq!(kind_of(Path::new("notes.txt")), None);
    }

    #[cfg(unix)]
    #[test]
    fn takes_the_file_a_command_writes() {
        let dir = super::super::tests::tempfile_dir("command-output");
        let dest = dir.join("e1");
        let (captured, result) = command_output(
            "printf png > out.png",
            "out.png",
            &dir,
            Duration::from_secs(10),
            &dest,
        )
        .unwrap();
        assert!(result.passed());
        assert_eq!(captured.file, dir.join("e1.png"));
        assert_eq!(std::fs::read(&captured.file).unwrap(), b"png");
        // Not written by this run: refused.
        let stale = command_output("true", "out.png", &dir, Duration::from_secs(10), &dest);
        assert!(stale.is_err());
    }
}
