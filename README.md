# oxi

Desktop chat UI (Rust + **egui**) with a **local agent loop**: HTTP streaming to OpenAI-compatible APIs and built-in tools (`read`, `write`, `edit`, `bash`, `grep`, `find`, `ls`). **No Node, no `pi` binary, and no JSONL RPC** — a single Rust binary.

In this monorepo the crate lives under the directory [`pi-rust-chat/`](.) (historical folder name); the built binary is **`oxi`**.

## Requirements

- Rust toolchain (2021 edition)

## Configuration

Settings are stored at `~/.config/oxi/settings.json` (macOS/Linux) or the platform config dir from the `dirs` crate. You can set **provider**, **model id**, optional **base URL override**, **system prompt**, and which **tools** are enabled.

If you previously used the app under `~/.config/pi-rust-chat/`, copy `settings.json` and `oauth.json` into `~/.config/oxi/` (or rename the folder).

### OAuth (recommended for Codex and Copilot)

Open **Settings** (model chip in the composer) and use **OAuth sign-in**:

- **GitHub Copilot** — GitHub device flow (browser + user code). Tokens are saved to `~/.config/oxi/oauth.json`. Optional **Enterprise hostname** (blank = `github.com`).
- **ChatGPT / Codex** — OpenAI OAuth (PKCE) with redirect to `http://localhost:1455/auth/callback`. Requires port **1455** free. Tokens are stored in the same `oauth.json`.

If OAuth is configured for a provider, it takes precedence over environment variables for that provider.

### Environment variables (API keys, fallback)

| Provider | Variable |
|----------|----------|
| OpenAI | `OPENAI_API_KEY` |
| OpenRouter | `OPENROUTER_API_KEY` |
| GPT Codex (no OAuth) | `OPENAI_API_KEY` — Chat Completions on `api.openai.com` |
| GitHub Copilot (no OAuth) | `COPILOT_GITHUB_TOKEN`, or `GH_TOKEN`, or `GITHUB_TOKEN` |

Optional OpenRouter headers (see [OpenRouter docs](https://openrouter.ai/docs)):

- `OPENROUTER_HTTP_REFERER` — sent as `HTTP-Referer`
- `OPENROUTER_TITLE` — sent as `X-Title`

Default base URLs: OpenAI / Codex (API key mode) `https://api.openai.com/v1`; Codex (OAuth) uses `https://chatgpt.com/backend-api` (Responses API); OpenRouter `https://openrouter.ai/api/v1`; Copilot API host is taken from the Copilot token when using OAuth, or `https://api.individual.githubcopilot.com` when using a PAT. Override with the **base URL** field where applicable.

**Codex + ChatGPT account:** not every model id is allowed (the API may return `detail` explaining unsupported models). If you see that error, change **model id** in settings to one that your plan exposes for Codex (see ChatGPT / OpenAI model picker for Codex).

## Run

```bash
cd pi-rust-chat
cargo run --release
# binary: target/release/oxi
```

Use **Set cwd** in the sidebar (or launch from your repo) so the workspace root matches the project you want the agent to use.

## Standalone behavior

- The app runs a **local agent loop** directly inside the Rust process.
- Chats can still be persisted locally as JSONL session files under the app config directory unless session loading is disabled for a workspace path.
- Image attachments in the composer are not sent to the model in this build (a notice is shown if you try).
- OAuth tokens are currently stored in plain JSON at `~/.config/oxi/oauth.json`, so prefer OS-level disk encryption on shared machines.

## Safety notes

- `bash` is powerful and can modify the workspace; disable it in Settings if you want a read-only review workflow.
- `bash` runs with a bounded timeout and basic risky-command rejection, but it still executes shell commands inside the selected workspace.
- File-search tools skip common heavy directories such as `.git`, `target`, and `node_modules`.
