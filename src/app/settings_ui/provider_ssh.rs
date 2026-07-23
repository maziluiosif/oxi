//! Remote compute target, SSH credentials, and connection testing.

use eframe::egui::{self, RichText, Ui};

use crate::app::task_runner::spawn_async_task;
use crate::settings::{ComputeLocation, LlmProviderKind, SshConfig};
use crate::theme::*;
use crate::ui::chrome::{
    alert_banner, card_frame, field_label_first, ghost_button, pill_tab, settings_card_header,
    settings_password_field, settings_text_field_width,
};

use super::super::{OxiApp, SshTestMsg};

impl OxiApp {
    /// "Local" vs "Remote (SSH)" compute target, shown only for self-hosted runtimes
    /// (LM Studio / Ollama / Local HF) where running on another host over SSH is meaningful.
    pub(super) fn render_compute_target_section(&mut self, ui: &mut Ui, kind: LlmProviderKind) {
        ui.add_space(12.0);
        card_frame().show(ui, |ui| {
            settings_card_header(
                ui,
                "Compute target",
                Some("Where the model runtime listens: this machine, or another host via SSH tunnel."),
            );
            let is_remote = matches!(
                self.conv.settings.provider(kind).location,
                ComputeLocation::RemoteSsh(_)
            );
            if kind != LlmProviderKind::RemoteHf {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 6.0;
                    if pill_tab(ui, "Local", !is_remote) && is_remote {
                        self.conv.settings.provider_mut(kind).location = ComputeLocation::Local;
                    }
                    if pill_tab(ui, "Remote (SSH)", is_remote) && !is_remote {
                        self.conv.settings.provider_mut(kind).location =
                            ComputeLocation::RemoteSsh(SshConfig {
                                remote_runtime_port: kind.default_remote_runtime_port(),
                                ..SshConfig::default()
                            });
                    }
                });
            }

            if let ComputeLocation::RemoteSsh(cfg) =
                &mut self.conv.settings.provider_mut(kind).location
            {
                ui.add_space(8.0);
                ui.label(
                    RichText::new(
                        if kind == LlmProviderKind::RemoteHf {
                            "Runs the oxi-managed HF model on another host over SSH. oxi can install llama-server, download GGUF files, start/stop the runtime, and tunnel chat to it."
                        } else {
                            "Runs the model on another host (e.g. a machine on your LAN) over SSH. The runtime must listen on 127.0.0.1 there; oxi forwards a local port to it."
                        },
                    )
                    .size(FS_TINY)
                    .color(c_text_faint()),
                );
                ui.add_space(6.0);

                ui.horizontal(|ui| {
                    ui.vertical(|ui| {
                        field_label_first(ui, "SSH host");
                        settings_text_field_width(
                            ui,
                            &mut cfg.host,
                            "192.168.1.10 or myhost.local",
                            220.0,
                        );
                    });
                    ui.add_space(8.0);
                    ui.vertical(|ui| {
                        field_label_first(ui, "SSH port");
                        let mut port_str = cfg.port.to_string();
                        if settings_text_field_width(ui, &mut port_str, "22", 80.0).changed()
                            && let Ok(p) = port_str.trim().parse::<u16>()
                        {
                            cfg.port = p;
                        }
                    });
                });
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    ui.vertical(|ui| {
                        field_label_first(ui, "SSH user");
                        settings_text_field_width(ui, &mut cfg.user, "e.g. ioan", 220.0);
                    });
                    if kind != LlmProviderKind::RemoteHf {
                        ui.add_space(8.0);
                        ui.vertical(|ui| {
                            field_label_first(ui, "Remote runtime port");
                            let mut rport_str = cfg.remote_runtime_port.to_string();
                            if settings_text_field_width(ui, &mut rport_str, "11434", 80.0)
                                .changed()
                                && let Ok(p) = rport_str.trim().parse::<u16>()
                            {
                                cfg.remote_runtime_port = p;
                            }
                        });
                    }
                });
            }
        });

        if !matches!(
            self.conv.settings.provider(kind).location,
            ComputeLocation::RemoteSsh(_)
        ) {
            return;
        }

        // Lazily load the saved password (if any) into the in-memory draft on first touch.
        self.conv
            .ssh_password_drafts
            .entry(kind)
            .or_insert_with(|| {
                let creds = crate::compute::load_ssh_credentials();
                creds
                    .get(kind.slug())
                    // Remote HF used the Local HF credential key before the providers
                    // were split. Keep that setup working without asking for the password again.
                    .or_else(|| {
                        (kind == LlmProviderKind::RemoteHf)
                            .then(|| creds.get(LlmProviderKind::LocalHf.slug()))
                            .flatten()
                    })
                    .unwrap_or_default()
                    .to_string()
            });

        ui.add_space(12.0);
        card_frame().show(ui, |ui| {
            settings_card_header(
                ui,
                "SSH credentials",
                Some("Password is stored in the OS keychain, never in settings.json."),
            );
            field_label_first(ui, "SSH password");
            let changed = self
                .conv
                .ssh_password_drafts
                .get_mut(&kind)
                .is_some_and(|password| {
                    settings_password_field(ui, password, "SSH password").changed()
                });
            if changed {
                let pw = self
                    .conv
                    .ssh_password_drafts
                    .get(&kind)
                    .cloned()
                    .unwrap_or_default();
                let mut creds = crate::compute::load_ssh_credentials();
                creds.set(kind.slug(), pw);
                if let Err(e) = crate::compute::save_ssh_credentials(&creds) {
                    self.run_state_mut(self.active_session_key()).stream_error =
                        Some(format!("Save SSH password: {e}"));
                }
            }

            ui.add_space(8.0);
            // Clone the status out first so rendering it doesn't hold an immutable borrow of
            // `self.conv` while the buttons need `&mut self`.
            let status = self.conv.ssh_test.get(&kind).cloned();
            let pinned = self
                .conv
                .settings
                .provider(kind)
                .ssh_config()
                .and_then(|c| c.pinned_host_key.clone());
            let mut rerun_test = false;
            let mut accept_key: Option<String> = None;
            ui.horizontal(|ui| {
                if ghost_button(ui, "Test connection", false).clicked() {
                    rerun_test = true;
                }
                ui.add_space(8.0);
                if let Some(status) = &status {
                    if status.loading {
                        ui.label(
                            RichText::new("Connecting…")
                                .size(FS_TINY)
                                .color(c_text_muted()),
                        );
                    } else if let Some(Ok(port)) = &status.result {
                        ui.label(
                            RichText::new(format!("Connected (local tunnel port {port})"))
                                .size(FS_TINY)
                                .color(c_accent()),
                        );
                    }
                }
            });
            if let Some(status) = &status
                && let Some(Err(err)) = &status.result
            {
                ui.add_space(6.0);
                match err {
                    crate::compute::TunnelError::HostKeyMismatch { pinned, observed } => {
                        alert_banner(
                            ui,
                            &format!(
                                "Host key changed! Pinned {pinned}, server now presents \
                                 {observed}. Accept only if you know the host was rebuilt.",
                            ),
                            true,
                        );
                        ui.add_space(6.0);
                        if ghost_button(ui, "Accept new key", false).clicked() {
                            accept_key = Some(observed.clone());
                        }
                    }
                    e => alert_banner(ui, &e.to_string(), true),
                }
            }
            if let Some(fp) = &pinned {
                let short = fp.get(..23).unwrap_or(fp.as_str());
                ui.label(
                    RichText::new(format!("Host key pinned: {short}…"))
                        .size(FS_TINY)
                        .color(c_text_faint()),
                );
            }
            if let Some(fp) = accept_key {
                if let ComputeLocation::RemoteSsh(cfg) =
                    &mut self.conv.settings.provider_mut(kind).location
                {
                    cfg.pinned_host_key = Some(fp);
                }
                if let Err(e) = self.conv.settings.save() {
                    self.run_state_mut(self.active_session_key()).stream_error =
                        Some(format!("Save settings: {e}"));
                }
                rerun_test = true;
            }
            if rerun_test {
                self.spawn_ssh_test(ui.ctx(), kind);
            }
        });
    }

    /// Kick off a background SSH "Test connection" check for `kind`'s `RemoteSsh` config,
    /// if one isn't already in flight. Results arrive on `conv.ssh_test_rx` and are
    /// drained each frame.
    fn spawn_ssh_test(&mut self, ctx: &egui::Context, kind: LlmProviderKind) {
        let Some(cfg) = self.conv.settings.provider(kind).ssh_config().cloned() else {
            return;
        };
        let password = self
            .conv
            .ssh_password_drafts
            .get(&kind)
            .cloned()
            .unwrap_or_default();

        let entry = self.conv.ssh_test.entry(kind).or_default();
        if entry.loading {
            return;
        }
        entry.loading = true;
        entry.result = None;

        let (tx, rx) = std::sync::mpsc::channel::<SshTestMsg>();
        self.conv.ssh_test_rx = Some(rx);
        let ctx = ctx.clone();
        let tunnels = self.tunnels.clone();
        let err_tx = tx.clone();
        let err_ctx = ctx.clone();
        spawn_async_task(
            move |err| {
                let _ = err_tx.send(SshTestMsg {
                    provider: kind,
                    result: Err(crate::compute::TunnelError::Other(err)),
                });
                err_ctx.request_repaint();
            },
            move |rt| {
                let r = rt
                    .block_on(tunnels.ensure_tunnel(kind.slug(), &cfg, &password))
                    .map(|ok| ok.local_port);
                let _ = tx.send(SshTestMsg {
                    provider: kind,
                    result: r,
                });
                ctx.request_repaint();
            },
        );
    }

    /// Pin host keys observed on successful SSH connects (trust-on-first-use). Drains the
    /// tunnel manager's observed-fingerprint map each frame; for any provider whose
    /// `SshConfig` has no pinned key yet, records the observed fingerprint and saves
    /// settings. Already-pinned providers are left untouched — a mismatch never reaches a
    /// successful connect, so an attacker key can't silently overwrite an existing pin.
    pub(crate) fn pin_observed_host_keys(&mut self) {
        let observed = self.tunnels.take_observed_host_keys();
        if observed.is_empty() {
            return;
        }
        let mut changed = false;
        for (slug, fp) in observed {
            let Some(kind) = LlmProviderKind::ALL.into_iter().find(|k| k.slug() == slug) else {
                continue;
            };
            if let ComputeLocation::RemoteSsh(cfg) =
                &mut self.conv.settings.provider_mut(kind).location
                && cfg.pinned_host_key.is_none()
            {
                cfg.pinned_host_key = Some(fp);
                changed = true;
            }
        }
        if changed && let Err(e) = self.conv.settings.save() {
            self.run_state_mut(self.active_session_key()).stream_error =
                Some(format!("Save settings: {e}"));
        }
    }

    /// Drain background SSH "Test connection" results into `conv.ssh_test`. Mirrors
    /// [`Self::drain_models`].
    pub(crate) fn drain_ssh_test(&mut self, ctx: &egui::Context) {
        let Some(rx) = self.conv.ssh_test_rx.take() else {
            return;
        };
        let mut repainted = false;
        loop {
            match rx.try_recv() {
                Ok(msg) => {
                    // A working Remote HF connection means credentials are good now —
                    // drop the cached (possibly failed) remote model listing so the
                    // panel refetches it.
                    if msg.provider == LlmProviderKind::RemoteHf && msg.result.is_ok() {
                        self.conv.local_models.remote_list_for = None;
                    }
                    let entry = self.conv.ssh_test.entry(msg.provider).or_default();
                    entry.loading = false;
                    entry.result = Some(msg.result);
                    repainted = true;
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {
                    self.conv.ssh_test_rx = Some(rx);
                    break;
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => break,
            }
        }
        if repainted {
            ctx.request_repaint();
        }
    }
}
