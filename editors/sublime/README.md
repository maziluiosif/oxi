# oxi for Sublime Text

oxi's chat inside a Sublime Text window: the chat list in a narrow column on the left, the
transcript on its right and the composer under the transcript. With files open, the oxi
columns go to the right of them. The agents, approvals and chat
history are oxi's own. The package starts `oxi headless` and talks to it over stdio, so a
chat started in Sublime also appears in the oxi app for the same folder, and the other way
round.

`oxi headless` connects to one oxi daemon that every Sublime window and every other editor
shares (it starts the daemon when none runs). A chat open in two places streams in both, an
approval can be answered from either, and a turn keeps running when Sublime closes; reopen
the chat to see where it got. The daemon exits after 15 minutes with no editor connected and
nothing running. Chats the oxi app changes are read again as soon as their file changes.
*Oxi: Restart oxi* stops the daemon (cancelling running turns everywhere); the next message
starts it again, on the current oxi binary.

## Install

The oxi installer adds the package when it finds Sublime Text:

```sh
# macOS, Linux
curl -fsSL https://maziluiosif.github.io/oxi/install.sh | sh
```

```powershell
# Windows
irm https://maziluiosif.github.io/oxi/install.ps1 | iex
```

It puts `Oxi.sublime-package` in Sublime's `Installed Packages` folder; running it again
updates oxi and the package. Each release also has the package as
`oxi-sublime.sublime-package`; to install it by hand, save it there as `Oxi.sublime-package`.

The package finds oxi on `PATH`, in `~/.local/bin` or in the usual app folder. Elsewhere, set
its path in *Preferences → Package Settings → Oxi*:

```json
{ "oxi_path": "/path/to/oxi" }
```

### From a checkout

To work on the package, link the folder into Sublime's `Packages` folder (*Preferences →
Browse Packages…*) instead; the installer leaves a linked copy alone:

```sh
# macOS
ln -s "$PWD/editors/sublime/Oxi" ~/Library/Application\ Support/Sublime\ Text/Packages/Oxi
# Linux
ln -s "$PWD/editors/sublime/Oxi" ~/.config/sublime-text/Packages/Oxi
```

## Use

- **Ctrl+Alt+O** (or *Oxi: Toggle Chat Panel*) opens the oxi columns for the window's first
  folder, moves to the composer when they are open, and closes them from the composer. The
  minimap is hidden while they are open (`"hide_minimap": false` keeps it).
- **Enter** sends, **Shift+Enter** adds a line, **Esc** stops a running turn.
- `@path` mentions attach files or folders (completion appears after `@`), `/plan` toggles
  plan mode, `/new` starts a chat.
- The line above the composer picks the agent or provider, the model, the reasoning effort and
  plan mode.
- **Cmd+V** / **Ctrl+V** in the composer with a screenshot or image in the clipboard attaches it;
  *Oxi: Attach Image…* picks image files. Attached images sit above the toolbar: click one to
  see it, ✕ to drop it. Images in sent messages open from the links after the message.
- **Ctrl+Alt+L** (or *Add to oxi Chat* in the context menu) puts the selection, with its file
  and lines, into the composer.
- Click a chat in the list to open it; **Delete** / **Backspace** (or *Delete Chat…* in its
  context menu) deletes the chat under the caret or the pointer, after asking. **☰ Chats** and **＋ New chat** at the top of the
  transcript bring the list back or start a chat. Errors show as a red card at the end of
  the transcript. Double-click (or Enter on) a tool or thinking line in
  the transcript to open its full input, output and diff. `diff` and `open` links sit next to
  tools that changed or point at files.
- When a tool needs approval a card with Allow / Allow for this turn / Always allow / Deny
  appears at the end of the transcript.

- The transcript's Markdown is styled like a preview: headings, bold, italics and code are
  styled and the markers (`#`, `**`, backticks, fences, table pipes) are faded. The rules go
  into `Packages/User/oxi-transcript-colors/`, named after your color scheme, and only match
  oxi transcripts; `"markdown_preview": false` removes them.

Everything is also in the command palette under `Oxi:`.

## Protocol

`oxi headless` reads one JSON request per line on stdin (`{"id", "method", "params"}`) and
writes one JSON object per line on stdout: responses (`{"id", "result" | "error"}`) and
notifications (`{"method", "params"}`). See `src/headless/mod.rs` for the methods; other
editors can use the same server. Responses go to the editor that asked; notifications go to
every connected editor, so filter them by `chat`. `shutdown` disconnects the caller and
`daemon.stop` stops the daemon.

`oxi headless --status` shows the running daemon, `--stop` stops it, and `--stdio` serves a
single editor in its own process, without the daemon.
