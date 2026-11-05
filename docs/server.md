# oxi server: use your agents from your phone or another computer

`oxi server` keeps oxi's agents running on one machine (a desktop, a Mac mini, a
Linux box) so other devices can use them:

- **Phones** — the oxi Android app connects over [Tailscale](https://tailscale.com).
- **Other computers** — the desktop app opens folders on the server over SSH
  (see [Remote workspaces over SSH](remote-ssh.md)).

The server owns the project files, agent processes and chats. Clients show the
conversation, approvals and tool output, and turns keep running when a client
disconnects.

## 1. Install oxi on the server

Use the regular installer (`curl -fsSL https://maziluiosif.github.io/oxi/install.sh | sh`
on macOS and Linux, `irm https://maziluiosif.github.io/oxi/install.ps1 | iex` on
Windows), then sign in to the agents you want to use there (Claude Code, Codex, …)
and add any API keys in oxi's settings.

## 2. Start the server

**From the app:** open **Settings → Server & phone** and turn on
**Start at login and keep running**.

**From a terminal** (for example over SSH):

```sh
oxi server install
oxi server            # status
```

| Platform | What `install` sets up |
|---|---|
| macOS | LaunchAgent `dev.oxi.server` (runs in your login session) |
| Linux | systemd user service `oxi-server.service`, with lingering when allowed |
| Windows | starts at sign-in (HKCU `Run` entry) |

On macOS the server copies oxi's keys out of the keychain once (allow it if macOS
asks), so updates never stop it at a keychain prompt. Keys you change later in the
desktop app on the same machine reach it too.

`oxi server uninstall` removes the service; settings and chats stay.

## 3. Pair a phone

1. Install [Tailscale](https://tailscale.com/download) on the server and on the
   phone, signed in to the same account.
2. Install the oxi Android app: `oxi-android.apk` from the
   [latest release](https://github.com/maziluiosif/oxi/releases/latest). It updates
   itself from then on.
3. On the server, turn on **Allow phones to connect** in **Settings → Server &
   phone**, or run `oxi server phone`. Both show a QR code.
4. In the app, tap **Scan QR code** (or **Add server** in the server menu) and scan it.

Add as many servers as you like; switch between them from the name at the top of
the app.

### How phones connect

The server listens on port 47811 and accepts a phone only when:

- it comes from a Tailscale address (WireGuard encrypts that traffic), and
- it presents the pairing token from the QR code.

`oxi server phone --reset` (or **New token**) makes a new token: paired phones have
to scan again. `oxi server phone --off` stops accepting phones.

If a phone can't use Tailscale, `oxi server phone --lan` (or **Also allow phones on
my local network**) accepts private LAN addresses too. That traffic is not
encrypted, so only use it on a network you trust.

### Attachments

The **+** in the composer attaches photos and files. Photos go to the model as images
(scaled down to 2048 px). Other files are saved on the server in the workspace, under
`.oxi/attachments/` (kept out of Git by its own `.gitignore`), and the agent is told
where. For PDF, Word, Excel, PowerPoint and OpenDocument files the server also puts
their text in the prompt, up to 100,000 characters, so models without file tools can
read them. Scanned PDFs have no text to extract. Files can be up to 30 MB.

## Commands

```text
oxi server                 status and the pair link
oxi server install         start at login and keep running
oxi server uninstall       remove the service
oxi server restart         restart (stops running turns)
oxi server phone           allow phones, print the QR code
oxi server phone --reset   new pairing token
oxi server phone --lan     also accept the local network
oxi server phone --no-lan  Tailscale only
oxi server phone --off     stop accepting phones
```

## Troubleshooting

- **The app says it can't reach the server:** check that Tailscale is connected on
  both devices (`tailscale status`) and that `oxi server` shows the server running.
- **"No Tailscale address found":** start Tailscale on the server, or use `--lan`.
- **Linux: the server stops after logout:** run `sudo loginctl enable-linger $USER`.
- Logs are in `oxi.log` (and `server.log` on macOS) in oxi's config folder.
