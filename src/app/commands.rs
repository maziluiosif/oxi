//! The app's commands in one table: the macOS menu bar is built from it, and the command
//! palette (`>` in the Cmd/Ctrl+P picker, or Cmd/Ctrl+Shift+P) lists it on every platform.
//!
//! Most commands are a keyboard shortcut the app already handles. Running one replays the keys
//! into egui's input on the next frame, so the existing handlers decide what it means where focus
//! is — the same as pressing the keys. The rest run an [`AppAction`] directly.

use std::sync::OnceLock;

use eframe::egui::{self, Key, Modifiers};

use super::OxiApp;
use super::state::SettingsTab;

const WEBSITE: &str = "https://maziluiosif.github.io/oxi/";
const RELEASE_NOTES: &str = "https://github.com/maziluiosif/oxi/releases/latest";
const REPORT_ISSUE: &str = "https://github.com/maziluiosif/oxi/issues/new";

const MAC: bool = cfg!(target_os = "macos");

/// Commands that are not a keyboard shortcut of the app.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum AppAction {
    CheckForUpdates,
    KeyboardShortcuts,
    Quit,
    OpenFolder,
    ConnectRemote,
    ExportChat,
    AttachImage,
    ActivityLog,
    GotoLine,
    Website,
    ReleaseNotes,
    ReportIssue,
    ToggleWordWrap,
    ToggleMinimap,
    NewGeneralChat,
    ChatSplit,
    WorktreeSplit,
    FocusSplitPane,
    ToggleSplitMirror,
    CloseSplit,
    CloseAllEditorTabs,
    GotoProject,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Action {
    /// Replay this shortcut as a key press.
    Keys(Modifiers, Key),
    /// Replay Cmd/Ctrl+K, then this key on the next frame (Sublime's chords).
    Chord(Modifiers, Key),
    Copy,
    Cut,
    Paste,
    Run(AppAction),
}

/// Where a command makes sense, which decides whether the palette lists it.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Scope {
    /// Works from anywhere.
    App,
    /// Edits the focused editor; the palette lists it only when a document is open.
    Editor,
    /// Acts on whatever text field has focus (clipboard, undo): menu only, since the palette
    /// itself holds focus while it is open.
    Menu,
}

/// Panels whose menu item shows a check mark while they are open.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Panel {
    Chats,
    Explorer,
    Search,
    SourceControl,
    Terminal,
    ActivityLog,
    WordWrap,
    Minimap,
}

pub(crate) struct Command {
    /// Label in the menu.
    pub label: &'static str,
    /// Label in the palette, `Menu: label` when not set.
    palette: Option<&'static str>,
    pub action: Action,
    /// The keys shown next to the command (and bound to the macOS menu item).
    pub shortcut: Option<(Modifiers, Key)>,
    pub scope: Scope,
    /// Check mark in the macOS menu.
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    pub panel: Option<Panel>,
}

pub(crate) enum Entry {
    Command(Command),
    Separator,
}

pub(crate) struct MenuSpec {
    pub title: &'static str,
    pub entries: Vec<Entry>,
}

/// Command/Ctrl as egui reports it for a real key press on this platform.
fn cmd() -> Modifiers {
    if MAC {
        Modifiers::MAC_CMD.plus(Modifiers::COMMAND)
    } else {
        Modifiers::CTRL.plus(Modifiers::COMMAND)
    }
}

fn cmd_shift() -> Modifiers {
    cmd().plus(Modifiers::SHIFT)
}

fn keys(label: &'static str, modifiers: Modifiers, key: Key) -> Command {
    Command {
        label,
        palette: None,
        action: Action::Keys(modifiers, key),
        shortcut: Some((modifiers, key)),
        scope: Scope::App,
        panel: None,
    }
}

/// A Cmd+K chord: listed in the palette, shown without a menu accelerator.
fn chord(label: &'static str, key: Key) -> Command {
    Command {
        label,
        palette: None,
        action: Action::Chord(cmd(), key),
        shortcut: None,
        scope: Scope::Editor,
        panel: None,
    }
}

fn run(label: &'static str, action: AppAction) -> Command {
    Command {
        label,
        palette: None,
        action: Action::Run(action),
        shortcut: None,
        scope: Scope::App,
        panel: None,
    }
}

impl Command {
    fn palette(mut self, title: &'static str) -> Self {
        self.palette = Some(title);
        self
    }

    fn scope(mut self, scope: Scope) -> Self {
        self.scope = scope;
        self
    }

    fn panel(mut self, panel: Panel) -> Self {
        self.panel = Some(panel);
        self
    }

    fn clipboard(label: &'static str, action: Action, key: Key) -> Self {
        Command {
            label,
            palette: None,
            action,
            shortcut: Some((cmd(), key)),
            scope: Scope::Menu,
            panel: None,
        }
    }

    /// The palette label: `View: Terminal`, or the explicit one.
    pub fn title(&self, menu: &str) -> String {
        match self.palette {
            Some(title) => title.to_owned(),
            None => format!("{menu}: {}", self.label.trim_end_matches('…')),
        }
    }
}

/// Every menu with its commands, in menu order.
pub(crate) fn menus() -> &'static [MenuSpec] {
    static MENUS: OnceLock<Vec<MenuSpec>> = OnceLock::new();
    MENUS.get_or_init(build_menus)
}

fn build_menus() -> Vec<MenuSpec> {
    use AppAction::*;
    use Entry::Separator;
    let c = Entry::Command;
    let ctrl = Modifiers::CTRL;
    let editor = |command: Command| c(command.scope(Scope::Editor));
    // Bindings that differ by platform, matching `handle_global_shortcuts` and the editor.
    let replace = if MAC {
        (cmd().plus(Modifiers::ALT), Key::F)
    } else {
        (cmd(), Key::H)
    };
    // macOS keeps Cmd+` for cycling windows; Ctrl+` (VS Code's binding) toggles the terminal.
    let terminal = if MAC {
        (ctrl, Key::Backtick)
    } else {
        (cmd(), Key::Backtick)
    };
    // Sublime's Distraction Free Mode binding.
    let distraction_free = if MAC {
        (cmd_shift().plus(ctrl), Key::F)
    } else {
        (Modifiers::SHIFT, Key::F11)
    };
    // Sublime's binding on macOS; Ctrl+. is Stop Response elsewhere, so F7 there.
    let modification = if MAC {
        (ctrl, Key::Period)
    } else {
        (Modifiers::NONE, Key::F7)
    };
    let modification_back = if MAC {
        Modifiers::NONE
    } else {
        Modifiers::SHIFT
    };
    let fold = if MAC {
        cmd().plus(Modifiers::ALT)
    } else {
        ctrl.plus(Modifiers::SHIFT)
    };
    let all_occurrences = if MAC {
        (cmd().plus(ctrl), Key::G)
    } else {
        (Modifiers::ALT, Key::F3)
    };
    let caret_line = if MAC {
        ctrl.plus(Modifiers::SHIFT)
    } else {
        ctrl.plus(Modifiers::ALT)
    };
    let jump = if MAC { ctrl } else { Modifiers::ALT };
    let move_line = if MAC { cmd().plus(ctrl) } else { cmd_shift() };
    let bracket = if MAC { ctrl } else { cmd() };
    // Ctrl+G is Find Next outside macOS; Go to Line is `:` in the picker there.
    let goto_line = if MAC {
        keys("Go to Line…", ctrl, Key::G)
    } else {
        run("Go to Line…", GotoLine)
    };

    vec![
        MenuSpec {
            title: "oxi",
            entries: vec![
                c(run("Check for Updates…", CheckForUpdates).palette("oxi: Check for Updates")),
                Separator,
                c(keys("Settings…", cmd(), Key::Comma).palette("Preferences: Open Settings")),
                Separator,
                c(Command {
                    shortcut: Some((cmd(), Key::Q)),
                    scope: Scope::Menu,
                    ..run("Quit oxi", Quit)
                }),
            ],
        },
        MenuSpec {
            title: "File",
            entries: vec![
                c(keys("New Chat", cmd(), Key::N)),
                c(keys("New Scratchpad", cmd_shift(), Key::N)),
                c(run("New General Chat", NewGeneralChat)),
                Separator,
                c(run("Open Folder…", OpenFolder).palette("File: Open Folder as Workspace")),
                c(run("Connect to Remote Host…", ConnectRemote)
                    .palette("Remote: Open a Folder on an SSH Host")),
                Separator,
                editor(keys("Save", cmd(), Key::S)),
                editor(keys("Close Tab", cmd(), Key::W).palette("File: Close Editor Tab")),
                editor(
                    run("Close All Tabs", CloseAllEditorTabs)
                        .palette("File: Close All Editor Tabs"),
                ),
            ],
        },
        MenuSpec {
            title: "Edit",
            entries: vec![
                c(Command::clipboard(
                    "Undo",
                    Action::Keys(cmd(), Key::Z),
                    Key::Z,
                )),
                c(Command {
                    shortcut: Some((cmd_shift(), Key::Z)),
                    ..Command::clipboard("Redo", Action::Keys(cmd_shift(), Key::Z), Key::Z)
                }),
                Separator,
                c(Command::clipboard("Cut", Action::Cut, Key::X)),
                c(Command::clipboard("Copy", Action::Copy, Key::C)),
                c(Command::clipboard("Paste", Action::Paste, Key::V)),
                Separator,
                editor(keys("Find", cmd(), Key::F)),
                editor(keys("Replace", replace.0, replace.1)),
                editor(keys("Find Next", cmd(), Key::G)),
                editor(keys("Find Previous", cmd_shift(), Key::G)),
                c(keys("Find in Files…", cmd_shift(), Key::F)
                    .palette("Search: Find in Files")
                    .panel(Panel::Search)),
                Separator,
                editor(keys("Toggle Line Comment", cmd(), Key::Slash)),
                editor(keys("Indent", cmd(), Key::CloseBracket)),
                editor(keys("Outdent", cmd(), Key::OpenBracket)),
                editor(keys("Join Lines", cmd(), Key::J)),
                editor(keys("Transpose", if MAC { ctrl } else { cmd() }, Key::T)),
                editor(keys(
                    "Sort Lines",
                    Modifiers::NONE,
                    if MAC { Key::F5 } else { Key::F9 },
                )),
                c(chord(
                    if MAC {
                        "Convert Case: Upper Case   ⌘K ⌘U"
                    } else {
                        "Convert Case: Upper Case   Ctrl+K Ctrl+U"
                    },
                    Key::U,
                )
                .palette("Convert Case: Upper Case")),
                c(chord(
                    if MAC {
                        "Convert Case: Lower Case   ⌘K ⌘L"
                    } else {
                        "Convert Case: Lower Case   Ctrl+K Ctrl+L"
                    },
                    Key::L,
                )
                .palette("Convert Case: Lower Case")),
                c(chord(
                    if MAC {
                        "Delete to End of Line   ⌘K ⌘K"
                    } else {
                        "Delete to End of Line   Ctrl+K Ctrl+K"
                    },
                    Key::K,
                )
                .palette("Edit: Delete to End of Line")),
                Separator,
                editor(keys("Fold", fold, Key::OpenBracket).palette("Code Folding: Fold")),
                editor(keys("Unfold", fold, Key::CloseBracket).palette("Code Folding: Unfold")),
                c(chord(
                    if MAC {
                        "Unfold All   ⌘K ⌘0"
                    } else {
                        "Unfold All   Ctrl+K Ctrl+0"
                    },
                    Key::Num0,
                )
                .palette("Code Folding: Unfold All")),
                c(chord(
                    if MAC {
                        "Fold Level 1   ⌘K ⌘1"
                    } else {
                        "Fold Level 1   Ctrl+K Ctrl+1"
                    },
                    Key::Num1,
                )
                .palette("Code Folding: Fold Level 1")),
                c(chord(
                    if MAC {
                        "Fold Level 2   ⌘K ⌘2"
                    } else {
                        "Fold Level 2   Ctrl+K Ctrl+2"
                    },
                    Key::Num2,
                )
                .palette("Code Folding: Fold Level 2")),
                Separator,
                editor(keys("Toggle Bookmark", cmd(), Key::F2).palette("Bookmarks: Toggle")),
                editor(keys("Next Bookmark", Modifiers::NONE, Key::F2).palette("Bookmarks: Next")),
                editor(
                    keys("Previous Bookmark", Modifiers::SHIFT, Key::F2)
                        .palette("Bookmarks: Previous"),
                ),
                editor(
                    keys("Clear Bookmarks", cmd_shift(), Key::F2).palette("Bookmarks: Clear All"),
                ),
            ],
        },
        MenuSpec {
            title: "Selection",
            entries: vec![
                c(Command::clipboard(
                    "Select All",
                    Action::Keys(cmd(), Key::A),
                    Key::A,
                )),
                editor(keys("Expand Selection", cmd_shift(), Key::Space)),
                editor(keys("Quick Add Next", cmd(), Key::D)),
                c(chord(
                    if MAC {
                        "Quick Skip Next   ⌘K ⌘D"
                    } else {
                        "Quick Skip Next   Ctrl+K Ctrl+D"
                    },
                    Key::D,
                )
                .palette("Selection: Quick Skip Next")),
                editor(keys(
                    "Select All Occurrences",
                    all_occurrences.0,
                    all_occurrences.1,
                )),
                editor(keys("Split into Lines", cmd_shift(), Key::L)),
                editor(keys("Add Caret Above", caret_line, Key::ArrowUp)),
                editor(keys("Add Caret Below", caret_line, Key::ArrowDown)),
                Separator,
                editor(keys("Select Line", cmd(), Key::L)),
                editor(keys(
                    "Expand Selection to Brackets",
                    ctrl.plus(Modifiers::SHIFT),
                    Key::M,
                )),
                editor(keys("Expand Selection to Indentation", cmd_shift(), Key::J)),
                Separator,
                editor(keys("Move Line Up", move_line, Key::ArrowUp)),
                editor(keys("Move Line Down", move_line, Key::ArrowDown)),
                editor(keys("Duplicate Line", cmd_shift(), Key::D)),
                editor(keys("Delete Line", cmd_shift(), Key::K)),
                Separator,
                editor(keys(
                    "Add Selection to Chat",
                    cmd().plus(Modifiers::ALT),
                    Key::L,
                )),
            ],
        },
        MenuSpec {
            title: "View",
            entries: vec![
                c(Command {
                    scope: Scope::Menu,
                    ..keys("Command Palette…", cmd_shift(), Key::P)
                }),
                Separator,
                c(keys("Chats", cmd(), Key::B)
                    .palette("View: Toggle Chats")
                    .panel(Panel::Chats)),
                c(keys("Explorer", cmd(), Key::E)
                    .palette("View: Toggle Explorer")
                    .panel(Panel::Explorer)),
                c(keys("Source Control", cmd_shift(), Key::B)
                    .palette("View: Toggle Source Control")
                    .panel(Panel::SourceControl)),
                c(keys("Terminal", terminal.0, terminal.1)
                    .palette("View: Toggle Terminal")
                    .panel(Panel::Terminal)),
                c(run("Activity Log", ActivityLog)
                    .palette("View: Toggle Activity Log")
                    .panel(Panel::ActivityLog)),
                Separator,
                c(keys(
                    "Distraction Free Mode",
                    distraction_free.0,
                    distraction_free.1,
                )
                .palette("View: Toggle Distraction Free Mode")),
                Separator,
                c(run("Word Wrap", ToggleWordWrap)
                    .palette("View: Toggle Word Wrap")
                    .panel(Panel::WordWrap)),
                c(run("Minimap", ToggleMinimap)
                    .palette("View: Toggle Minimap")
                    .panel(Panel::Minimap)),
                c(keys("Larger Font", cmd(), Key::Equals).palette("View: Increase Font Size")),
                c(keys("Smaller Font", cmd(), Key::Minus).palette("View: Decrease Font Size")),
                c(keys("Reset Font Size", cmd(), Key::Num0).palette("View: Reset Font Size")),
            ],
        },
        MenuSpec {
            title: "Go",
            entries: vec![
                c(keys("Go to File…", cmd(), Key::P)),
                editor(keys("Go to Symbol in File…", cmd(), Key::R)),
                c(keys("Go to Symbol in Workspace…", cmd_shift(), Key::R)),
                c(keys("Go to Chat…", cmd_shift(), Key::O)),
                c(run("Go to Project…", GotoProject)),
                editor(goto_line),
                Separator,
                editor(keys("Go to Definition", Modifiers::NONE, Key::F12)),
                editor(keys("Find References", Modifiers::SHIFT, Key::F12)),
                editor(keys("Go to Matching Bracket", bracket, Key::M)),
                editor(keys("Jump Back", jump, Key::Minus)),
                editor(keys(
                    "Jump Forward",
                    jump.plus(Modifiers::SHIFT),
                    Key::Minus,
                )),
                Separator,
                editor(keys("Next Modification", modification.0, modification.1)),
                editor(keys(
                    "Previous Modification",
                    modification.0.plus(modification_back),
                    if MAC { Key::Comma } else { modification.1 },
                )),
            ],
        },
        MenuSpec {
            title: "Chat",
            entries: vec![
                c(run("Attach Image…", AttachImage)),
                Separator,
                c(keys("Next Chat", Modifiers::CTRL, Key::Tab)),
                c(keys(
                    "Previous Chat",
                    Modifiers::CTRL.plus(Modifiers::SHIFT),
                    Key::Tab,
                )),
                Separator,
                c(keys("Stop Response", cmd(), Key::Period)),
                Separator,
                c(run("New Chat Side by Side", ChatSplit)),
                c(run("New Work Tree Chat Side by Side", WorktreeSplit)),
                c(run("Focus Other Split Pane", FocusSplitPane)),
                c(run("Send to Both Split Panes", ToggleSplitMirror)),
                c(run("Close Split View", CloseSplit)),
                Separator,
                c(run("Export as Markdown…", ExportChat)),
            ],
        },
        MenuSpec {
            title: "Help",
            entries: vec![
                c(run("Keyboard Shortcuts", KeyboardShortcuts)),
                Separator,
                c(run("oxi Website", Website)),
                c(run("Release Notes", ReleaseNotes)),
                c(run("Report an Issue…", ReportIssue)),
            ],
        },
    ]
}

/// The commands the palette offers, with their palette titles, in menu order.
pub(crate) fn palette_commands() -> &'static [(String, &'static Command)] {
    static PALETTE: OnceLock<Vec<(String, &'static Command)>> = OnceLock::new();
    PALETTE.get_or_init(|| {
        // The app menu (updates, settings) goes last: the palette is for working commands.
        let (app, rest): (Vec<_>, Vec<_>) = menus().iter().partition(|menu| menu.title == "oxi");
        rest.into_iter()
            .chain(app)
            .flat_map(|menu| {
                menu.entries.iter().filter_map(move |entry| match entry {
                    Entry::Command(command) if command.scope != Scope::Menu => {
                        Some((command.title(menu.title), command))
                    }
                    _ => None,
                })
            })
            .collect()
    })
}

/// `⇧⌘P` on macOS, `Ctrl+Shift+P` elsewhere.
pub(crate) fn shortcut_label(modifiers: Modifiers, key: Key) -> String {
    let mut label = String::new();
    if MAC {
        for (on, glyph) in [
            (modifiers.ctrl, "⌃"),
            (modifiers.alt, "⌥"),
            (modifiers.shift, "⇧"),
            (modifiers.mac_cmd || modifiers.command, "⌘"),
        ] {
            if on {
                label.push_str(glyph);
            }
        }
    } else {
        for (on, word) in [
            (modifiers.ctrl || modifiers.command, "Ctrl+"),
            (modifiers.shift, "Shift+"),
            (modifiers.alt, "Alt+"),
        ] {
            if on {
                label.push_str(word);
            }
        }
    }
    label.push_str(match key {
        Key::Backtick => "`",
        Key::Comma => ",",
        Key::Period => ".",
        Key::Slash => "/",
        Key::OpenBracket => "[",
        Key::CloseBracket => "]",
        Key::Space => "Space",
        Key::Equals => "=",
        Key::Minus => "-",
        Key::Num0 => "0",
        Key::ArrowUp if MAC => "↑",
        Key::ArrowDown if MAC => "↓",
        Key::ArrowUp => "Up",
        Key::ArrowDown => "Down",
        other => other.name(),
    });
    label
}

/// A command picked from the menu or the palette, waiting for its frame.
pub(crate) struct QueuedAction {
    action: Action,
    /// Frames to let pass first, so focus can move back to the editor before its keys land.
    wait: u8,
}

impl OxiApp {
    /// Run `action` as if its keys were pressed; editor commands from the palette first give
    /// focus back to the editor.
    pub(crate) fn queue_action(&mut self, action: Action, refocus_editor: bool) {
        if refocus_editor {
            self.conv.editor.focus_editor_next_frame = true;
        }
        self.queued_actions.push(QueuedAction {
            action,
            wait: if refocus_editor { 2 } else { 0 },
        });
    }

    /// Feed due commands into this frame's `input`; app actions run later in `logic`. Called
    /// from `raw_input_hook`, before egui sees the frame's input.
    pub(crate) fn replay_queued_actions(
        &mut self,
        ctx: &egui::Context,
        input: &mut egui::RawInput,
    ) {
        if self.queued_actions.is_empty() {
            return;
        }
        let mut waiting = Vec::new();
        for mut queued in std::mem::take(&mut self.queued_actions) {
            if queued.wait > 0 {
                queued.wait -= 1;
                waiting.push(queued);
                continue;
            }
            match queued.action {
                Action::Keys(modifiers, key) => {
                    // The handlers read the frame's modifier state, which a click on a menu
                    // item or a palette row leaves empty.
                    input.modifiers = modifiers;
                    for pressed in [true, false] {
                        input.events.push(egui::Event::Key {
                            key,
                            physical_key: Some(key),
                            pressed,
                            repeat: false,
                            modifiers,
                        });
                    }
                }
                Action::Copy => input.events.push(egui::Event::Copy),
                Action::Cut => input.events.push(egui::Event::Cut),
                Action::Paste => {
                    let text = arboard::Clipboard::new()
                        .and_then(|mut clipboard| clipboard.get_text())
                        .unwrap_or_default()
                        .replace("\r\n", "\n");
                    if text.is_empty() {
                        // No text: let the composer's image paste see the chord.
                        input.modifiers = cmd();
                        input.events.push(egui::Event::Key {
                            key: Key::V,
                            physical_key: Some(Key::V),
                            pressed: true,
                            repeat: false,
                            modifiers: cmd(),
                        });
                    } else {
                        input.events.push(egui::Event::Paste(text));
                    }
                }
                Action::Run(action) => self.pending_app_actions.push(action),
                Action::Chord(modifiers, key) => {
                    input.modifiers = modifiers;
                    for pressed in [true, false] {
                        input.events.push(egui::Event::Key {
                            key: Key::K,
                            physical_key: Some(Key::K),
                            pressed,
                            repeat: false,
                            modifiers,
                        });
                    }
                    waiting.push(QueuedAction {
                        action: Action::Keys(modifiers, key),
                        wait: 0,
                    });
                }
            }
        }
        if !waiting.is_empty() {
            ctx.request_repaint();
        }
        self.queued_actions = waiting;
    }

    /// Run the app actions picked since the last frame. Called from `logic`.
    pub(crate) fn run_pending_app_actions(&mut self, ctx: &egui::Context) {
        for action in std::mem::take(&mut self.pending_app_actions) {
            self.run_app_action(ctx, action);
        }
    }

    fn run_app_action(&mut self, ctx: &egui::Context, action: AppAction) {
        match action {
            AppAction::CheckForUpdates => {
                self.conv.settings_page.tab = SettingsTab::About;
                self.open_settings_page();
                self.ensure_update_checked(ctx, true);
            }
            AppAction::KeyboardShortcuts => {
                self.conv.settings_page.tab = SettingsTab::About;
                self.open_settings_page();
            }
            AppAction::Quit => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
            AppAction::OpenFolder => self.open_workspace_folder(),
            AppAction::ConnectRemote => self.open_remote_dialog(),
            AppAction::ExportChat => self.export_active_session_markdown(),
            AppAction::AttachImage => self.pick_image_attachment(),
            AppAction::ActivityLog => super::activity_window::toggle_activity_window(ctx),
            AppAction::GotoLine => {
                if self.conv.editor.active_document().is_some() {
                    self.open_file_picker_with(":");
                }
            }
            AppAction::Website => {
                let _ = webbrowser::open(WEBSITE);
            }
            AppAction::ReleaseNotes => {
                let _ = webbrowser::open(RELEASE_NOTES);
            }
            AppAction::ReportIssue => {
                let _ = webbrowser::open(REPORT_ISSUE);
            }
            AppAction::ToggleWordWrap => self.toggle_word_wrap(),
            AppAction::ToggleMinimap => self.toggle_minimap(),
            AppAction::NewGeneralChat => self.new_general_chat(),
            AppAction::CloseAllEditorTabs => self.request_close_all_editor_tabs(),
            AppAction::ChatSplit => self.new_chat_split(),
            AppAction::WorktreeSplit => self.start_worktree_split(),
            AppAction::FocusSplitPane => self.focus_split_partner(),
            AppAction::ToggleSplitMirror => self.toggle_split_mirror(),
            AppAction::CloseSplit => self.close_split(),
            AppAction::GotoProject => self.open_file_picker_with("%"),
        }
    }

    /// Whether `panel` is showing, for the menu's check marks.
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    pub(crate) fn panel_shown(&self, ctx: &egui::Context, panel: Panel) -> bool {
        use super::state::SidebarMode;
        let sidebar = |mode| self.conv.sidebar.open && self.conv.sidebar.mode == mode;
        match panel {
            Panel::Chats => sidebar(SidebarMode::Chats),
            Panel::Explorer => sidebar(SidebarMode::Explorer),
            Panel::Search => sidebar(SidebarMode::Search),
            Panel::SourceControl => self.conv.git_ui.open,
            Panel::Terminal => self.conv.terminal_panel.open,
            Panel::ActivityLog => super::activity_window::activity_window_open(ctx),
            Panel::WordWrap => self.conv.settings.editor.word_wrap,
            Panel::Minimap => self.conv.settings.editor.minimap,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn palette_lists_app_and_editor_commands_with_unique_titles() {
        let titles: Vec<_> = palette_commands().iter().map(|(t, _)| t.as_str()).collect();
        assert!(titles.contains(&"View: Toggle Terminal"));
        assert!(titles.contains(&"Edit: Toggle Line Comment"));
        assert!(!titles.iter().any(|t| t.starts_with("Edit: Copy")));
        let mut unique = titles.clone();
        unique.sort_unstable();
        unique.dedup();
        assert_eq!(unique.len(), titles.len(), "{titles:?}");
    }

    #[test]
    fn shortcuts_are_bound_once() {
        let mut seen = Vec::new();
        for menu in menus() {
            for entry in &menu.entries {
                if let Entry::Command(command) = entry
                    && let Some(shortcut) = command.shortcut
                {
                    assert!(
                        !seen.contains(&shortcut),
                        "{} reuses a shortcut",
                        command.label
                    );
                    seen.push(shortcut);
                }
            }
        }
    }

    #[test]
    fn shortcut_labels_read_like_the_platform() {
        let label = shortcut_label(cmd_shift(), Key::P);
        if MAC {
            assert_eq!(label, "⇧⌘P");
        } else {
            assert_eq!(label, "Ctrl+Shift+P");
        }
    }
}
