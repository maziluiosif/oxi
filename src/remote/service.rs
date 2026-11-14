//! `oxi server`: keep the headless daemon running on this machine for the apps that connect
//! to it: the desktop app over SSH (`ssh://host/path` workspaces) and phones over Tailscale.
//!
//! `install` registers a per-user service that starts the daemon at login and restarts it
//! when it stops (`daemon.stop` is how a bridge replaces a daemon running an older build):
//!
//! - macOS: the LaunchAgent `dev.oxi.server`. The daemon has to run inside the user's login
//!   session: started from an SSH login it can't open the keychain, where oxi keeps API keys
//!   and agents such as Claude Code keep their sign-in.
//! - Linux: the systemd user unit `oxi-server.service` (with lingering, it also runs without
//!   a login).
//! - Windows: a `Run` entry under HKCU, which starts it at sign-in; a marker file in the
//!   config folder says it is installed.
//!
//! Bridges then ask the service for the daemon instead of starting one themselves. A managed
//! daemon keeps secrets in `server-secrets.json` (see `crate::secrets`), so a new build never
//! waits at a keychain prompt. `install` also links `~/.local/bin/oxi-server` to this binary;
//! the desktop app's SSH command runs that first.
//!
//! `phone` turns on phone access (see `headless::phone`). The desktop app drives the same
//! functions from Settings → Server.

use std::path::PathBuf;

use crate::headless::phone::{self, PhoneConfig};

pub const SUBCOMMAND: &str = "server";
#[cfg(target_os = "macos")]
pub const LABEL: &str = "dev.oxi.server";
#[cfg(all(unix, not(target_os = "macos")))]
const UNIT: &str = "oxi-server.service";
#[cfg(windows)]
const RUN_KEY: &str = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run";
#[cfg(windows)]
const RUN_VALUE: &str = "oxi server";

const USAGE: &str = "\
usage: oxi server [command]

  status              what runs here and how phones can connect (the default)
  install             start the oxi server at login and keep it running
  uninstall           remove that service (settings and chats stay)
  restart             restart the server (stops running turns)
  phone               let phones connect, and show the pairing QR code
  phone --reset       new pairing token: paired phones have to pair again
  phone --lan         also accept phones on the local network (unencrypted)
  phone --no-lan      Tailscale only again
  phone --off         stop accepting phones";

pub fn run() -> i32 {
    #[cfg(windows)]
    {
        attach_console();
        keep_std_handles_from_children();
    }
    crate::logging::init();
    let args: Vec<String> = std::env::args().skip(2).collect();
    let flag = args.get(1).map(String::as_str);
    match args.first().map(String::as_str) {
        Some("install") => report(install()),
        Some("uninstall") => report(uninstall()),
        Some("restart") => report(restart().map(|()| "oxi server restarted.".into())),
        Some("status") | None => {
            println!("{}", status().describe());
            0
        }
        Some("phone") | Some("pair") => report(phone_command(flag)),
        Some("help" | "--help" | "-h") => {
            println!("{USAGE}");
            0
        }
        Some(other) => {
            eprintln!("oxi server: unknown command {other}\n\n{USAGE}");
            2
        }
    }
}

/// oxi is a GUI program on Windows; show `oxi server` output in the terminal it ran from.
#[cfg(windows)]
fn attach_console() {
    use windows_sys::Win32::System::Console::{ATTACH_PARENT_PROCESS, AttachConsole};
    // SAFETY: plain Win32 call without pointers.
    unsafe {
        AttachConsole(ATTACH_PARENT_PROCESS);
    }
}

/// A daemon this command starts would inherit its stdin/stdout/stderr (Windows passes every
/// inheritable handle on) and hold the caller's pipe open: a script piping `oxi server` or an
/// SSH session would never see it end. This process exits soon, so it can give them up.
#[cfg(windows)]
fn keep_std_handles_from_children() {
    use windows_sys::Win32::Foundation::{HANDLE_FLAG_INHERIT, SetHandleInformation};
    use windows_sys::Win32::System::Console::{
        GetStdHandle, STD_ERROR_HANDLE, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE,
    };
    for id in [STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, STD_ERROR_HANDLE] {
        // SAFETY: plain Win32 calls on this process's own standard handles.
        unsafe {
            let handle = GetStdHandle(id);
            if !handle.is_null() {
                SetHandleInformation(handle, HANDLE_FLAG_INHERIT, 0);
            }
        }
    }
}

fn report(result: Result<String, String>) -> i32 {
    match result {
        Ok(message) => {
            println!("{message}");
            0
        }
        Err(e) => {
            eprintln!("oxi server: {e}");
            1
        }
    }
}

// ---- status -----------------------------------------------------------------------------

/// What runs on this machine, for `oxi server status` and Settings → Server.
#[derive(Clone, Debug)]
pub struct Status {
    /// The login service is installed.
    pub service: bool,
    /// The daemon's `daemon.hello` answer, when one runs.
    pub daemon: Option<serde_json::Value>,
    pub phone: Option<PhoneConfig>,
    /// Where phones connect, when phone access is on and an address was found.
    pub pair_host: Option<String>,
    pub name: String,
}

impl Status {
    pub fn running(&self) -> bool {
        self.daemon.is_some()
    }

    /// Phones currently connected count as clients, like editors.
    pub fn clients(&self) -> u64 {
        self.daemon
            .as_ref()
            .and_then(|d| d["clients"].as_u64())
            .unwrap_or(0)
    }

    pub fn pair_link(&self) -> Option<String> {
        Some(phone::pair_link(
            self.phone.as_ref()?,
            self.pair_host.as_deref()?,
            &self.name,
        ))
    }

    pub fn describe(&self) -> String {
        let mut out = String::new();
        out += &format!(
            "Service:  {}\n",
            if self.service {
                "installed, starts at login"
            } else {
                "not installed (run `oxi server install`)"
            }
        );
        out += &match &self.daemon {
            Some(d) => format!(
                "Server:   running, oxi {} (pid {}), {} client(s)\n",
                d["version"].as_str().unwrap_or("?"),
                d["pid"],
                d["clients"]
            ),
            None => "Server:   not running\n".into(),
        };
        out += &match &self.phone {
            None => "Phones:   off (run `oxi server phone`)\n".into(),
            Some(config) => format!(
                "Phones:   on, port {}{}\n",
                config.port,
                if config.lan {
                    ", Tailscale and local network"
                } else {
                    ", Tailscale only"
                }
            ),
        };
        if self.phone.is_some() {
            match self.pair_link() {
                Some(link) => out += &format!("Pair:     {link}\n"),
                None => {
                    out += "Pair:     no Tailscale address found. Start Tailscale here, or run \
                            `oxi server phone --lan` to pair over the local network.\n"
                }
            }
        }
        out.trim_end().to_string()
    }
}

pub fn status() -> Status {
    let phone = phone::load();
    Status {
        service: managed(),
        daemon: crate::headless::daemon_hello(),
        pair_host: phone.as_ref().and_then(phone::pair_host),
        phone,
        name: phone::machine_name(),
    }
}

// ---- phones -----------------------------------------------------------------------------

fn phone_command(flag: Option<&str>) -> Result<String, String> {
    match flag {
        Some("--off") => {
            disable_phones()?;
            return Ok("Phone access is off.".into());
        }
        Some("--lan") => set_phone_lan(true)?,
        Some("--no-lan") => set_phone_lan(false)?,
        Some("--reset") => {
            enable_phones(true)?;
        }
        None => {
            enable_phones(false)?;
        }
        Some(other) => return Err(format!("unknown option {other}\n\n{USAGE}")),
    }
    let status = status();
    let mut out = String::new();
    if let Some(link) = status.pair_link() {
        out += &qr_text(&link);
        out += "\nScan this with the oxi app on your phone (Add server → Scan QR code),\n\
                or open the link on the phone:\n\n  ";
        out += &link;
        out += "\n\n";
    }
    out += &status.describe();
    if !status.service {
        out += "\n\nThe server stops 15 minutes after the last client leaves. \
                Run `oxi server install` to keep it running.";
    }
    Ok(out)
}

/// Turn on phone access (a new token with `reset`) and make the daemon listen.
pub fn enable_phones(reset: bool) -> Result<PhoneConfig, String> {
    let (config, changed) = phone::ensure(reset)?;
    if changed || !daemon_listens_for_phones() {
        restart()?;
    }
    Ok(config)
}

pub fn disable_phones() -> Result<(), String> {
    if phone::load().is_some() {
        phone::disable()?;
        if crate::headless::daemon_hello().is_some() {
            restart()?;
        }
    }
    Ok(())
}

pub fn set_phone_lan(lan: bool) -> Result<(), String> {
    let (mut config, _) = phone::ensure(false)?;
    if config.lan != lan {
        config.lan = lan;
        phone::save(&config)?;
    }
    restart()
}

fn daemon_listens_for_phones() -> bool {
    phone::load().is_some_and(|config| {
        std::net::TcpStream::connect_timeout(
            &std::net::SocketAddr::from(([127, 0, 0, 1], config.port)),
            std::time::Duration::from_millis(300),
        )
        .is_ok()
    })
}

/// The link as a QR code drawn with block characters, two modules per character row.
pub fn qr_text(link: &str) -> String {
    let Some((width, modules)) = qr_modules(link) else {
        return String::new();
    };
    let quiet = 2;
    let size = width + 2 * quiet;
    let dark = |x: usize, y: usize| {
        x >= quiet
            && y >= quiet
            && x < width + quiet
            && y < width + quiet
            && modules[(y - quiet) * width + (x - quiet)]
    };
    let mut out = String::new();
    for y in (0..size).step_by(2) {
        for x in 0..size {
            // Light modules print as blocks: the code reads on dark and light terminals.
            out.push(match (!dark(x, y), y + 1 < size && !dark(x, y + 1)) {
                (true, true) => '█',
                (true, false) => '▀',
                (false, true) => '▄',
                (false, false) => ' ',
            });
        }
        out.push('\n');
    }
    out
}

/// The QR code's width and its modules, row by row (`true` = dark).
pub fn qr_modules(text: &str) -> Option<(usize, Vec<bool>)> {
    let code = qrcode::QrCode::with_error_correction_level(text, qrcode::EcLevel::M).ok()?;
    let width = code.width();
    let modules = code
        .to_colors()
        .into_iter()
        .map(|c| c == qrcode::Color::Dark)
        .collect();
    Some((width, modules))
}

// ---- service, shared --------------------------------------------------------------------

/// The link the app's SSH command tries first.
fn server_link() -> Option<PathBuf> {
    Some(dirs::home_dir()?.join(".local/bin/oxi-server"))
}

fn link_this_binary() -> Result<PathBuf, String> {
    let exe = std::env::current_exe()
        .and_then(|p| p.canonicalize())
        .map_err(|e| format!("this binary: {e}"))?;
    let link = server_link().ok_or("no home folder")?;
    if let Some(parent) = link.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
    }
    let _ = std::fs::remove_file(&link);
    #[cfg(unix)]
    std::os::unix::fs::symlink(&exe, &link).map_err(|e| format!("{}: {e}", link.display()))?;
    #[cfg(not(unix))]
    {
        let link = link.with_extension("exe");
        let _ = std::fs::remove_file(&link);
        std::fs::copy(&exe, &link).map_err(|e| format!("{}: {e}", link.display()))?;
    }
    Ok(exe)
}

fn remove_server_link() {
    if let Some(link) = server_link() {
        let _ = std::fs::remove_file(&link);
        let _ = std::fs::remove_file(link.with_extension("exe"));
    }
}

fn run_tool(program: &str, args: &[&str]) -> Result<String, String> {
    let mut command = std::process::Command::new(program);
    command.args(args);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    let out = command.output().map_err(|e| format!("{program}: {e}"))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        Err(format!(
            "{program} {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        ))
    }
}

/// Restart the daemon so it picks up a changed setting (phone access) or a new binary. Turns
/// it runs are stopped. Starts it when none runs, and waits until it answers.
pub fn restart() -> Result<(), String> {
    restart_now()?;
    let started = std::time::Instant::now();
    while started.elapsed() < std::time::Duration::from_secs(10) {
        if crate::headless::daemon_hello().is_some() {
            return Ok(());
        }
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
    Err("the server did not start; see oxi.log in oxi's config folder".into())
}

fn restart_now() -> Result<(), String> {
    #[cfg(target_os = "macos")]
    if managed() {
        let target = format!("gui/{}/{LABEL}", uid());
        return run_tool("launchctl", &["kickstart", "-k", &target]).map(drop);
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    if managed() {
        return run_tool("systemctl", &["--user", "restart", UNIT]).map(drop);
    }
    crate::headless::stop_running_daemon();
    crate::headless::start_daemon(managed()).map_err(|e| format!("start the server: {e}"))
}

// ---- macOS: LaunchAgent -----------------------------------------------------------------

#[cfg(target_os = "macos")]
fn plist_path() -> Option<PathBuf> {
    Some(dirs::home_dir()?.join(format!("Library/LaunchAgents/{LABEL}.plist")))
}

#[cfg(target_os = "macos")]
fn uid() -> u32 {
    unsafe { libc::getuid() }
}

/// Whether the service is installed, so a bridge should ask it for the daemon.
pub fn managed() -> bool {
    #[cfg(target_os = "macos")]
    {
        plist_path().is_some_and(|p| p.exists())
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        unit_path().is_some_and(|p| p.exists())
    }
    #[cfg(windows)]
    {
        windows_marker().exists()
    }
}

/// Ask the service to start the daemon now (it may have exited after `daemon.stop`).
pub fn kickstart() -> bool {
    #[cfg(target_os = "macos")]
    {
        let target = format!("gui/{}/{LABEL}", uid());
        run_tool("launchctl", &["kickstart", &target]).is_ok()
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        run_tool("systemctl", &["--user", "start", UNIT]).is_ok()
    }
    #[cfg(windows)]
    {
        crate::headless::start_daemon(true).is_ok()
    }
}

#[cfg(target_os = "macos")]
pub fn install() -> Result<String, String> {
    let exe = link_this_binary()?;
    let plist = plist_path().ok_or("no home folder")?;
    let log = crate::app_dirs::config_dir().join("server.log");
    let escape = |s: &str| {
        s.replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
    };
    let home = dirs::home_dir().unwrap_or_default();
    let body = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>{LABEL}</string>
    <key>ProgramArguments</key>
    <array>
        <string>{exe}</string>
        <string>{headless}</string>
        <string>--daemon</string>
        <string>--keep</string>
    </array>
    <key>EnvironmentVariables</key>
    <dict>
        <key>{secrets_env}</key>
        <string>1</string>
    </dict>
    <key>WorkingDirectory</key>
    <string>{home}</string>
    <key>RunAtLoad</key>
    <true/>
    <key>KeepAlive</key>
    <true/>
    <key>ThrottleInterval</key>
    <integer>5</integer>
    <key>ProcessType</key>
    <string>Interactive</string>
    <key>StandardErrorPath</key>
    <string>{log}</string>
</dict>
</plist>
"#,
        exe = escape(&exe.to_string_lossy()),
        headless = crate::headless::SUBCOMMAND,
        secrets_env = crate::secrets::SERVER_FILE_ENV,
        home = escape(&home.to_string_lossy()),
        log = escape(&log.to_string_lossy()),
    );
    if let Some(parent) = plist.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let target = format!("gui/{}", uid());
    let service = format!("{target}/{LABEL}");
    // Replace a loaded copy, so the new binary path takes effect.
    let _ = run_tool("launchctl", &["bootout", &service]);
    // A daemon started by a bridge (not by launchd) would hold the lock; stop it.
    crate::headless::stop_running_daemon();
    crate::fsutil::write_atomic(&plist, body.as_bytes())
        .map_err(|e| format!("{}: {e}", plist.display()))?;
    run_tool(
        "launchctl",
        &["bootstrap", &target, &plist.to_string_lossy()],
    )?;
    Ok(format!(
        "oxi server installed: {LABEL} runs {} at login.\n\
         On its first start it copies oxi's keys from the keychain into a file only you can \
         read, so updates don't stop it at a keychain prompt: if macOS asks, choose Allow.\n\
         Next: `oxi server phone` to pair a phone.",
        exe.display()
    ))
}

#[cfg(target_os = "macos")]
pub fn uninstall() -> Result<String, String> {
    let service = format!("gui/{}/{LABEL}", uid());
    let _ = run_tool("launchctl", &["bootout", &service]);
    if let Some(plist) = plist_path() {
        let _ = std::fs::remove_file(plist);
    }
    remove_server_link();
    Ok("oxi server removed.".into())
}

// ---- Linux: systemd user unit -----------------------------------------------------------

#[cfg(all(unix, not(target_os = "macos")))]
fn unit_path() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| Some(dirs::home_dir()?.join(".config")))?;
    Some(base.join("systemd/user").join(UNIT))
}

#[cfg(all(unix, not(target_os = "macos")))]
pub fn install() -> Result<String, String> {
    let exe = link_this_binary()?;
    let unit = unit_path().ok_or("no home folder")?;
    // systemd splits ExecStart on spaces unless quoted; `%` starts a specifier.
    let quote = |s: &str| {
        format!(
            "\"{}\"",
            s.replace('\\', "\\\\")
                .replace('"', "\\\"")
                .replace('%', "%%")
        )
    };
    let body = format!(
        "[Unit]\n\
         Description=oxi server (agents for the oxi desktop app and phones)\n\
         After=network-online.target\n\
         \n\
         [Service]\n\
         ExecStart={exe} {headless} --daemon --keep\n\
         Environment={secrets_env}=1\n\
         WorkingDirectory=%h\n\
         Restart=always\n\
         RestartSec=3\n\
         \n\
         [Install]\n\
         WantedBy=default.target\n",
        exe = quote(&exe.to_string_lossy()),
        headless = crate::headless::SUBCOMMAND,
        secrets_env = crate::secrets::SERVER_FILE_ENV,
    );
    if let Some(parent) = unit.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
    }
    crate::headless::stop_running_daemon();
    crate::fsutil::write_atomic(&unit, body.as_bytes())
        .map_err(|e| format!("{}: {e}", unit.display()))?;
    run_tool("systemctl", &["--user", "daemon-reload"])?;
    run_tool("systemctl", &["--user", "enable", "--now", UNIT])?;
    // Without lingering the user's services stop at logout and start only at login.
    let user = std::env::var("USER").unwrap_or_default();
    let linger = if !user.is_empty() && run_tool("loginctl", &["enable-linger", &user]).is_ok() {
        "It also runs while you are logged out."
    } else {
        "It runs while you are logged in; `sudo loginctl enable-linger $USER` keeps it running \
         after logout and after a reboot."
    };
    Ok(format!(
        "oxi server installed: {UNIT} (systemd user service) runs {}.\n{linger}\n\
         Next: `oxi server phone` to pair a phone.",
        exe.display()
    ))
}

#[cfg(all(unix, not(target_os = "macos")))]
pub fn uninstall() -> Result<String, String> {
    let _ = run_tool("systemctl", &["--user", "disable", "--now", UNIT]);
    if let Some(unit) = unit_path() {
        let _ = std::fs::remove_file(unit);
    }
    let _ = run_tool("systemctl", &["--user", "daemon-reload"]);
    remove_server_link();
    Ok("oxi server removed.".into())
}

// ---- Windows: Run key -------------------------------------------------------------------

#[cfg(windows)]
fn windows_marker() -> PathBuf {
    crate::app_dirs::config_dir().join("server-service")
}

#[cfg(windows)]
pub fn install() -> Result<String, String> {
    let exe = link_this_binary()?;
    let command = format!(
        "\"{}\" {} --daemon --keep",
        exe.display(),
        crate::headless::SUBCOMMAND
    );
    run_tool(
        "reg",
        &[
            "add", RUN_KEY, "/v", RUN_VALUE, "/t", "REG_SZ", "/d", &command, "/f",
        ],
    )?;
    let marker = windows_marker();
    if let Some(parent) = marker.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    crate::fsutil::write_atomic(&marker, exe.to_string_lossy().as_bytes())?;
    crate::headless::stop_running_daemon();
    crate::headless::start_daemon(true).map_err(|e| format!("start the server: {e}"))?;
    Ok(format!(
        "oxi server installed: it starts at sign-in and runs {}.\n\
         Next: `oxi server phone` to pair a phone.",
        exe.display()
    ))
}

#[cfg(windows)]
pub fn uninstall() -> Result<String, String> {
    let _ = run_tool("reg", &["delete", RUN_KEY, "/v", RUN_VALUE, "/f"]);
    let _ = std::fs::remove_file(windows_marker());
    crate::headless::stop_running_daemon();
    remove_server_link();
    Ok("oxi server removed.".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_terminal_qr_code_is_square_and_quiet_bordered() {
        let text = qr_text("oxi://pair?host=100.1.2.3&port=47811&token=ab&name=mini");
        let lines: Vec<&str> = text.lines().collect();
        let width = lines[0].chars().count();
        assert!(width > 20);
        assert!(lines.iter().all(|l| l.chars().count() == width));
        // Two characters of light border all round.
        assert!(lines[0].chars().all(|c| c == '█'));
        assert_eq!(lines.len(), width.div_ceil(2));
    }
}
