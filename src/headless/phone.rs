//! Phones on the tailnet reach the daemon over TCP (the oxi Android app in
//! `frontends/android`).
//!
//! `oxi server phone` (or Settings → Server in the app) writes `phone.json` (a port and a
//! random token) to the config folder; a daemon that finds it listens on that port. A
//! connection counts only when it comes from this machine or a Tailscale address
//! (100.64.0.0/10, fd7a:115c:a1e0::/48), whose traffic WireGuard encrypts, and when its
//! `daemon.hello` carries the token. With `lan` set, private LAN addresses get in too; that
//! traffic is not encrypted. After the hello a phone is a client like any editor: the same
//! JSON lines protocol, replies to it, notifications to all.

use std::net::{IpAddr, TcpListener};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicU64;
use std::sync::mpsc::Sender;

use serde::{Deserialize, Serialize};

use super::Input;

pub const DEFAULT_PORT: u16 = 47811;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct PhoneConfig {
    pub port: u16,
    pub token: String,
    /// Also accept phones on the local network, not only on the tailnet.
    #[serde(default)]
    pub lan: bool,
}

fn config_path() -> PathBuf {
    crate::app_dirs::config_dir().join("phone.json")
}

pub fn load() -> Option<PhoneConfig> {
    let text = std::fs::read_to_string(config_path()).ok()?;
    serde_json::from_str(&text).ok()
}

/// The phone settings, created on first use; `reset` makes a new token, so paired phones
/// have to pair again. The flag says whether the file changed.
pub fn ensure(reset: bool) -> Result<(PhoneConfig, bool), String> {
    if !reset && let Some(config) = load() {
        return Ok((config, false));
    }
    let previous = load();
    let config = PhoneConfig {
        port: previous.as_ref().map_or(DEFAULT_PORT, |c| c.port),
        token: format!("{:032x}", rand::random::<u128>()),
        lan: previous.is_some_and(|c| c.lan),
    };
    save(&config)?;
    Ok((config, true))
}

pub fn save(config: &PhoneConfig) -> Result<(), String> {
    let path = config_path();
    let body = serde_json::to_string_pretty(config).map_err(|e| e.to_string())?;
    // Owner-only from the first byte: the file holds the pairing token.
    crate::fsutil::write_atomic_private(&path, body.as_bytes())
        .map_err(|e| format!("{}: {e}", path.display()))
}

/// Remove the phone settings: the daemon stops listening once restarted.
pub fn disable() -> Result<(), String> {
    match std::fs::remove_file(config_path()) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e.to_string()),
    }
}

/// Compare without stopping at the first differing byte.
pub(super) fn token_matches(given: Option<&str>, token: &str) -> bool {
    let Some(given) = given else {
        return false;
    };
    given.len() == token.len()
        && given
            .bytes()
            .zip(token.bytes())
            .fold(0u8, |acc, (a, b)| acc | (a ^ b))
            == 0
}

/// This machine or a Tailscale peer; with `lan`, also a private local network address.
fn allowed_peer(ip: IpAddr, lan: bool) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            let [a, b, ..] = v4.octets();
            v4.is_loopback()
                || (a == 100 && (64..128).contains(&b))
                || (lan && (v4.is_private() || v4.is_link_local()))
        }
        IpAddr::V6(v6) => {
            if let Some(v4) = v6.to_ipv4_mapped() {
                return allowed_peer(IpAddr::V4(v4), lan);
            }
            let s = v6.segments();
            v6.is_loopback()
                || (s[0] == 0xfd7a && s[1] == 0x115c && s[2] == 0xa1e0)
                || (lan && ((s[0] & 0xfe00) == 0xfc00 || (s[0] & 0xffc0) == 0xfe80))
        }
    }
}

/// This machine's Tailscale IPv4 address, when Tailscale runs.
pub fn tailscale_ip() -> Option<String> {
    let candidates = [
        "tailscale",
        "/Applications/Tailscale.app/Contents/MacOS/Tailscale",
        "/usr/local/bin/tailscale",
        "/opt/homebrew/bin/tailscale",
        r"C:\Program Files\Tailscale\tailscale.exe",
    ];
    candidates.iter().find_map(|bin| {
        let mut command = std::process::Command::new(bin);
        command.args(["ip", "-4"]);
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
        }
        let out = command.output().ok()?;
        if !out.status.success() {
            return None;
        }
        // Only an address counts: some builds print errors and still exit 0.
        String::from_utf8(out.stdout)
            .ok()?
            .lines()
            .find_map(tailnet_ipv4)
    })
}

/// `line` as a Tailscale IPv4 address (100.64.0.0/10).
fn tailnet_ipv4(line: &str) -> Option<String> {
    let ip: std::net::Ipv4Addr = line.trim().parse().ok()?;
    let [a, b, ..] = ip.octets();
    (a == 100 && (64..128).contains(&b)).then(|| ip.to_string())
}

/// The address this machine uses on its local network (no packet is sent).
pub fn lan_ip() -> Option<String> {
    let socket = std::net::UdpSocket::bind(("0.0.0.0", 0)).ok()?;
    socket.connect(("192.168.0.1", 9)).ok()?;
    let ip = socket.local_addr().ok()?.ip();
    (!ip.is_loopback() && !ip.is_unspecified()).then(|| ip.to_string())
}

/// Where a phone should connect: the Tailscale address, else (with `lan`) the LAN one.
pub fn pair_host(config: &PhoneConfig) -> Option<String> {
    tailscale_ip().or_else(|| config.lan.then(lan_ip).flatten())
}

/// `oxi://pair?...`, what the app's QR scanner and deep link read.
pub fn pair_link(config: &PhoneConfig, host: &str, name: &str) -> String {
    fn enc(s: &str) -> String {
        s.bytes()
            .map(|b| match b {
                b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                    (b as char).to_string()
                }
                _ => format!("%{b:02X}"),
            })
            .collect()
    }
    format!(
        "oxi://pair?host={}&port={}&token={}&name={}",
        enc(host),
        config.port,
        enc(&config.token),
        enc(name)
    )
}

/// This machine's name, as phones list it.
pub fn machine_name() -> String {
    super::remote_app::host_name()
}

/// Accept phones on a thread when `phone.json` exists. Failing to bind only logs: editors on
/// this machine keep working.
pub(super) fn listen(tx: Sender<Input>, next_client: Arc<AtomicU64>) {
    let Some(config) = load() else {
        return;
    };
    let listener = match TcpListener::bind(("0.0.0.0", config.port)) {
        Ok(listener) => listener,
        Err(e) => {
            log::warn!("headless: phones: port {}: {e}", config.port);
            return;
        }
    };
    log::info!("headless: phones can connect on port {}", config.port);
    let token: Arc<str> = config.token.into();
    let lan = config.lan;
    let spawned = std::thread::Builder::new()
        .name("oxi-headless-phones".into())
        .spawn(move || {
            for stream in listener.incoming() {
                let stream = match stream {
                    Ok(stream) => stream,
                    Err(e) => {
                        log::warn!("headless: phones: accept: {e}");
                        continue;
                    }
                };
                match stream.peer_addr() {
                    Ok(peer) if allowed_peer(peer.ip(), lan) => {}
                    Ok(peer) => {
                        log::warn!("headless: phones: refused {peer} (not on the tailnet)");
                        continue;
                    }
                    Err(_) => continue,
                }
                let _ = stream.set_nodelay(true);
                super::daemon::accept(stream, Some(token.clone()), &tx, &next_client);
            }
        });
    if let Err(e) = spawned {
        log::warn!("headless: phones: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_local_and_tailnet_peers_get_in() {
        let ok = [
            "127.0.0.1",
            "100.99.167.120",
            "100.64.0.1",
            "100.127.255.255",
            "::1",
        ];
        let refused = [
            "192.168.1.20",
            "100.128.0.1",
            "100.63.0.1",
            "10.0.0.2",
            "8.8.8.8",
        ];
        for ip in ok {
            assert!(allowed_peer(ip.parse().unwrap(), false), "{ip}");
        }
        for ip in refused {
            assert!(!allowed_peer(ip.parse().unwrap(), false), "{ip}");
        }
        assert!(allowed_peer("fd7a:115c:a1e0::1".parse().unwrap(), false));
        assert!(allowed_peer("::ffff:100.99.1.2".parse().unwrap(), false));
        assert!(!allowed_peer("::ffff:192.168.1.2".parse().unwrap(), false));
    }

    #[test]
    fn only_tailnet_addresses_count_as_the_tailscale_ip() {
        assert_eq!(
            tailnet_ipv4("100.121.160.28\n").as_deref(),
            Some("100.121.160.28")
        );
        assert_eq!(tailnet_ipv4("192.168.1.2"), None);
        assert_eq!(tailnet_ipv4("The Tailscale GUI failed to start"), None);
    }

    #[test]
    fn lan_mode_adds_private_networks_only() {
        for ip in [
            "192.168.1.20",
            "10.0.0.2",
            "172.16.4.1",
            "fe80::1",
            "fd12::1",
        ] {
            assert!(allowed_peer(ip.parse().unwrap(), true), "{ip}");
            assert!(!allowed_peer(ip.parse().unwrap(), false), "{ip}");
        }
        for ip in ["8.8.8.8", "2001:db8::1"] {
            assert!(!allowed_peer(ip.parse().unwrap(), true), "{ip}");
        }
    }

    #[test]
    fn pair_links_escape_their_values() {
        let config = PhoneConfig {
            port: 47811,
            token: "ab12".into(),
            lan: false,
        };
        assert_eq!(
            pair_link(&config, "100.1.2.3", "Manu's Mac"),
            "oxi://pair?host=100.1.2.3&port=47811&token=ab12&name=Manu%27s%20Mac"
        );
    }

    #[test]
    fn tokens_must_match_exactly() {
        assert!(token_matches(Some("abc"), "abc"));
        assert!(!token_matches(Some("abd"), "abc"));
        assert!(!token_matches(Some("ab"), "abc"));
        assert!(!token_matches(None, "abc"));
    }
}
