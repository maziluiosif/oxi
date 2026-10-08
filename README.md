<p align="center">
  <img src="assets/app-icon.png" alt="" width="96" />
</p>

<h1 align="center">oxi</h1>

<p align="center">
  <b>The native coding agent for any model.</b><br />
  Run open models on your own machine, or bring the Claude Code, Codex or Cursor subscription you already pay for.<br />
  One Rust binary. No Electron, no account, around 110 MB of RAM.
</p>

<p align="center">
  <a href="#install"><b>Install</b></a> ·
  <a href="https://maziluiosif.github.io/oxi/">Website</a> ·
  <a href="#what-you-get">Features</a> ·
  <a href="https://github.com/maziluiosif/oxi/releases/latest">Latest release</a>
</p>

<p align="center">
  <a href="https://github.com/maziluiosif/oxi/releases/latest"><img src="https://img.shields.io/github/v/release/maziluiosif/oxi?color=e26a2c&label=release" alt="Latest release" /></a>
  <img src="https://img.shields.io/badge/macOS%20%C2%B7%20Linux%20%C2%B7%20Windows-555" alt="macOS, Linux and Windows" />
  <a href="https://github.com/maziluiosif/oxi/actions/workflows/ci.yml"><img src="https://github.com/maziluiosif/oxi/actions/workflows/ci.yml/badge.svg" alt="CI" /></a>
  <a href="LICENSE"><img src="https://img.shields.io/badge/license-MIT-yellow.svg" alt="MIT license" /></a>
</p>

<p align="center">
  <a href="https://maziluiosif.github.io/oxi/#demo"><img src="assets/demo/demo.gif" alt="A one-minute tour of oxi: local models, providers, the agent fixing a failing test, reviewing the diff, committing, plan mode and themes" /></a>
  <br />
  <sub>One-minute tour, recorded from the real app. <a href="https://maziluiosif.github.io/oxi/#demo">Watch it in HD</a>.</sub>
</p>

## Install

**macOS (Apple Silicon) and Linux (x86_64):**

```bash
curl -fsSL https://maziluiosif.github.io/oxi/install.sh | sh
```

**Windows (x86_64), in PowerShell:**

```powershell
irm https://maziluiosif.github.io/oxi/install.ps1 | iex
```

Then run `oxi` inside a project folder, or open it from your apps menu. To update, run `oxi update` or the same command again.

<details>
<summary>Homebrew, manual download, install options</summary>

### Homebrew (macOS, Linux)

```bash
brew tap maziluiosif/tap
brew install --cask oxi
xattr -cr /Applications/oxi.app   # macOS only, once after installing
```

### Manual download

Get the archive for your platform from [Releases](https://github.com/maziluiosif/oxi/releases) and extract it. On macOS, move `oxi.app` to `/Applications` and run `xattr -cr /Applications/oxi.app` once, because the app is not notarized by Apple. On Windows, click **More info → Run anyway** the first time.

### Install options

| Variable | Effect |
|---|---|
| `OXI_VERSION` | Install a specific release, e.g. `v1.8.0` |
| `OXI_BIN_DIR` | macOS/Linux: where the `oxi` command goes (default `~/.local/bin`) |
| `OXI_APP_DIR` | macOS: where `oxi.app` goes (default `/Applications`) |
| `OXI_INSTALL_DIR` | Windows: install folder (default `%LOCALAPPDATA%\Programs\oxi`) |

</details>

<details>
<summary>Build from source</summary>

You need **Rust 1.92+** ([rustup](https://rustup.rs)) and C/C++ build tools.

```bash
git clone https://github.com/maziluiosif/oxi
cd oxi
cargo run --release
```

#### Windows prerequisites

```powershell
winget install --id LLVM.LLVM -e
winget install --id Kitware.CMake -e
```

Also install Visual Studio Build Tools with the **Desktop development with C++** workload. If the build can't find `libclang`, set:

```powershell
[Environment]::SetEnvironmentVariable("LIBCLANG_PATH", "C:\Program Files\LLVM\bin", "User")
```

</details>

## Your first five minutes

1. **Open a project.** Run `oxi` in its folder, or use **Open folder…** in the sidebar.
2. **Pick a model** in Settings → Models & providers:
   - **Free and private:** choose **Local HF**, click **Install runtime**, download a model (the usual choice is marked) and click **Run**.
   - **Already have Claude Code, Codex or Cursor?** Choose it under External agents and click **Make active**. It uses your existing login.
   - **Have an API key?** Paste it under OpenAI, OpenRouter, Anthropic-compatible and others.
3. **Ask for something real:** *"Run the tests and fix the failing one."* For bigger changes, type `/plan` first.

## What you get

<table>
  <tr>
    <td width="50%" valign="top">
      <img src="assets/screenshots/stage-local-models.png" alt="Local HF setup with the runtime installed and a Qwen model running" />
      <b>Local models, set up for you.</b> Pick a GGUF from HuggingFace and oxi installs <code>llama-server</code>, downloads it and runs it, here or on a GPU box over SSH. LM Studio and Ollama work too.
    </td>
    <td width="50%" valign="top">
      <img src="assets/screenshots/stage-router.png" alt="Router settings" />
      <b>Any provider, or all of them.</b> Claude Code, Codex and Cursor over ACP, ChatGPT sign-in, or any API. The Router picks one per task and switches when a quota runs out.
    </td>
  </tr>
  <tr>
    <td width="50%" valign="top">
      <img src="assets/screenshots/stage-agent.png" alt="The agent running tests, reading a file and editing it" />
      <b>An agent that shows its work.</b> It reads, searches and edits code, runs commands, searches the web and calls your MCP servers. Edits and commands can ask for approval first.
    </td>
    <td width="50%" valign="top">
      <img src="assets/screenshots/stage-review.png" alt="Side-by-side diff with Stage and Discard buttons on a change" />
      <b>Review in a real diff editor.</b> Side by side or inline, editable, with Stage and Discard on every block. Go to definition, find and replace, minimap and tabs.
    </td>
  </tr>
  <tr>
    <td width="50%" valign="top">
      <img src="assets/screenshots/stage-git.png" alt="Git panel with a new commit in the history" />
      <b>Git built in.</b> Stage, commit, branch, compare, pull and push, with AI-written commit messages. A terminal rooted in your workspace sits one click away.
    </td>
    <td width="50%" valign="top">
      <img src="assets/screenshots/stage-plan.png" alt="A plan with the Implement plan button" />
      <b>Plan first, then build.</b> <code>/plan</code> investigates without changing a file, with a live checklist. One click implements it.
    </td>
  </tr>
</table>

Also: chats saved per workspace, local Whisper voice dictation, web search without an API key (DuckDuckGo, Bing or your SearXNG), custom system prompts, `AGENTS.md` support, and five themes (Dark, Light, Midnight, Sublime, Sublime 4).

## Models and providers

Pick a provider in Settings or from the model picker in the composer.

| Provider | |
|---|---|
| **Local HF** | oxi downloads a GGUF and runs `llama-server` for you |
| **Remote HF** | Same, on another machine over SSH |
| **LM Studio, Ollama, llama.cpp** | Your own local or LAN server, optionally over SSH |
| **Claude Code, Cursor, Codex** | Uses your existing subscription through ACP |
| **GPT Codex** | Sign in with ChatGPT, or use an OpenAI key |
| **OpenAI, Azure, OpenRouter, OpenCode Go, Anthropic-compatible** | Hosted APIs with your key |
| **Router** | Picks one of your providers per task, and switches if one runs out of quota |

With Router you can also just say it in the chat: *"use Codex for this"* or *"from now on use Claude Code"*.

<details>
<summary>Default URLs and environment variables</summary>

| Provider | Default base URL | Key from environment |
|---|---|---|
| OpenAI | `https://api.openai.com/v1` | `OPENAI_API_KEY` |
| Azure OpenAI | `https://YOUR_RESOURCE.openai.azure.com/openai/deployments/YOUR_DEPLOYMENT` | `AZURE_OPENAI_API_KEY` |
| OpenRouter | `https://openrouter.ai/api/v1` | `OPENROUTER_API_KEY` |
| GPT Codex | OAuth, or `https://api.openai.com/v1` with a key | `OPENAI_API_KEY` |
| OpenCode Go | `https://opencode.ai/zen/go` | `OPENCODE_GO_API_KEY`, `OPENCODE_API_KEY` |
| Custom Anthropic | `http://localhost:8000` | `CUSTOM_ANTHROPIC_API_KEY`, `ANTHROPIC_API_KEY` |
| LM Studio | `http://localhost:1234/v1` | `LMSTUDIO_API_KEY` (optional) |
| llama.cpp | `http://localhost:8080/v1` | `LLAMA_API_KEY` (optional) |
| Ollama | `http://localhost:11434/v1` | `OLLAMA_API_KEY` (optional) |
| Local HF / Remote HF | `http://127.0.0.1:18080/v1` | none |

ChatGPT sign-in needs port `1455` free for the login callback. The Router's task classifier uses OpenRouter if a key is set, and falls back to local rules otherwise; you can turn it off in Settings → Providers → Router.

</details>

## Your data

Everything stays on your machine. Settings live in your config folder (e.g. `~/.config/oxi/settings.json` on Linux), next to the diagnostics log `oxi.log` and, if oxi ever crashes, `crash.log` (Settings → About opens them). API keys, sign-in tokens and SSH passwords go to the system keychain (macOS Keychain, Windows Credential Manager, Secret Service on Linux), never into the settings file.

If a workspace has an `AGENTS.md` at its root, oxi adds it to the agent's instructions.

## Good to know

- Approval prompts are the real safety boundary. Shell commands and MCP tools are not sandboxed.
- Only point SSH tunnels at machines you control.
- Image input depends on whether your model supports images.

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md) for building, tests, code layout and PR conventions.
