//! Native macOS menu bar, built from the shared command table in [`super::commands`], plus the
//! standard macOS pieces (About, Services, Hide, Window, Full Screen).
//!
//! AppKit runs a menu item's key equivalent before the key reaches the window, so once the menu
//! is installed ⌘N, ⌘C and friends never arrive as keystrokes. Every item therefore goes through
//! [`OxiApp::queue_action`], which replays its keys into egui's input: pressing the keys and
//! clicking the item do the same thing, decided by the existing handlers where focus is.

use std::collections::HashMap;
use std::sync::mpsc;

use eframe::egui::{self, Key, Modifiers};
use muda::accelerator::{Accelerator, Code, Modifiers as AccelModifiers};
use muda::{
    AboutMetadata, CheckMenuItem, IsMenuItem, Menu, MenuEvent, MenuId, MenuItem,
    PredefinedMenuItem, Submenu,
};

use super::OxiApp;
use super::commands::{Action, AppAction, Command, Entry, Panel, menus};

pub(crate) struct MenuBar {
    /// AppKit holds the items only weakly through muda; dropping this tears the menu down.
    _menu: Menu,
    actions: HashMap<MenuId, Action>,
    panels: Vec<(Panel, CheckMenuItem)>,
    events: mpsc::Receiver<MenuId>,
}

fn accelerator(modifiers: Modifiers, key: Key) -> Option<Accelerator> {
    let mut mods = AccelModifiers::empty();
    if modifiers.command || modifiers.mac_cmd {
        mods |= AccelModifiers::META;
    }
    if modifiers.ctrl {
        mods |= AccelModifiers::CONTROL;
    }
    if modifiers.shift {
        mods |= AccelModifiers::SHIFT;
    }
    if modifiers.alt {
        mods |= AccelModifiers::ALT;
    }
    Some(Accelerator::new(mods, code(key)?))
}

fn code(key: Key) -> Option<Code> {
    Some(match key {
        Key::A => Code::KeyA,
        Key::B => Code::KeyB,
        Key::C => Code::KeyC,
        Key::D => Code::KeyD,
        Key::E => Code::KeyE,
        Key::F => Code::KeyF,
        Key::G => Code::KeyG,
        Key::H => Code::KeyH,
        Key::I => Code::KeyI,
        Key::J => Code::KeyJ,
        Key::K => Code::KeyK,
        Key::L => Code::KeyL,
        Key::M => Code::KeyM,
        Key::N => Code::KeyN,
        Key::O => Code::KeyO,
        Key::P => Code::KeyP,
        Key::Q => Code::KeyQ,
        Key::R => Code::KeyR,
        Key::S => Code::KeyS,
        Key::T => Code::KeyT,
        Key::U => Code::KeyU,
        Key::V => Code::KeyV,
        Key::W => Code::KeyW,
        Key::X => Code::KeyX,
        Key::Z => Code::KeyZ,
        Key::Backtick => Code::Backquote,
        Key::Comma => Code::Comma,
        Key::Period => Code::Period,
        Key::Slash => Code::Slash,
        Key::OpenBracket => Code::BracketLeft,
        Key::CloseBracket => Code::BracketRight,
        Key::Space => Code::Space,
        Key::Equals => Code::Equal,
        Key::Minus => Code::Minus,
        Key::Num0 => Code::Digit0,
        Key::ArrowUp => Code::ArrowUp,
        Key::ArrowDown => Code::ArrowDown,
        Key::ArrowLeft => Code::ArrowLeft,
        Key::ArrowRight => Code::ArrowRight,
        Key::PageUp => Code::PageUp,
        Key::PageDown => Code::PageDown,
        Key::F2 => Code::F2,
        Key::F3 => Code::F3,
        Key::F5 => Code::F5,
        Key::F7 => Code::F7,
        Key::F9 => Code::F9,
        Key::F11 => Code::F11,
        Key::F12 => Code::F12,
        _ => return None,
    })
}

fn about_metadata() -> AboutMetadata {
    let icon = crate::app_icon();
    AboutMetadata {
        name: Some("oxi".into()),
        version: Some(env!("CARGO_PKG_VERSION").into()),
        comments: Some(env!("CARGO_PKG_DESCRIPTION").into()),
        website: Some("https://maziluiosif.github.io/oxi/".into()),
        website_label: Some("maziluiosif.github.io/oxi".into()),
        icon: muda::Icon::from_rgba(icon.rgba, icon.width, icon.height).ok(),
        ..Default::default()
    }
}

#[derive(Default)]
struct Builder {
    actions: HashMap<MenuId, Action>,
    panels: Vec<(Panel, CheckMenuItem)>,
}

impl Builder {
    fn add(&mut self, submenu: &Submenu, item: &dyn IsMenuItem) {
        if let Err(e) = submenu.append(item) {
            log::warn!("menu bar: {e}");
        }
    }

    fn command(&mut self, submenu: &Submenu, command: &Command) {
        let accelerator = command
            .shortcut
            .and_then(|(modifiers, key)| accelerator(modifiers, key));
        if let Some(panel) = command.panel {
            let item = CheckMenuItem::new(command.label, true, false, accelerator);
            self.actions.insert(item.id().clone(), command.action);
            self.add(submenu, &item);
            self.panels.push((panel, item));
        } else {
            let item = MenuItem::new(command.label, true, accelerator);
            self.actions.insert(item.id().clone(), command.action);
            self.add(submenu, &item);
        }
    }
}

impl MenuBar {
    /// Build the menu and make it the application's main menu. Call once the event loop runs.
    pub(crate) fn install(ctx: &egui::Context) -> Option<Self> {
        let mut b = Builder::default();
        let menu = Menu::new();
        let mut help = None;
        for spec in menus() {
            let submenu = Submenu::new(spec.title, true);
            if spec.title == "oxi" {
                b.add(
                    &submenu,
                    &PredefinedMenuItem::about(Some("About oxi"), Some(about_metadata())),
                );
            }
            for entry in &spec.entries {
                match entry {
                    Entry::Separator => b.add(&submenu, &PredefinedMenuItem::separator()),
                    Entry::Command(command) => {
                        if command.action == Action::Run(AppAction::Quit) {
                            for item in [
                                PredefinedMenuItem::services(None),
                                PredefinedMenuItem::separator(),
                                PredefinedMenuItem::hide(Some("Hide oxi")),
                                PredefinedMenuItem::hide_others(None),
                                PredefinedMenuItem::show_all(None),
                                PredefinedMenuItem::separator(),
                            ] {
                                b.add(&submenu, &item);
                            }
                        }
                        b.command(&submenu, command);
                    }
                }
            }
            if spec.title == "View" {
                b.add(&submenu, &PredefinedMenuItem::separator());
                b.add(&submenu, &PredefinedMenuItem::fullscreen(None));
            }
            if spec.title == "Help" {
                // The Window menu sits just before Help, as on every Mac app.
                let window = Submenu::new("Window", true);
                for item in [
                    PredefinedMenuItem::minimize(None),
                    PredefinedMenuItem::maximize(Some("Zoom")),
                    PredefinedMenuItem::separator(),
                    PredefinedMenuItem::bring_all_to_front(None),
                ] {
                    b.add(&window, &item);
                }
                menu.append(&window)
                    .inspect_err(|e| log::warn!("menu bar: {e}"))
                    .ok()?;
                window.set_as_windows_menu_for_nsapp();
                help = Some(submenu.clone());
            }
            menu.append(&submenu)
                .inspect_err(|e| log::warn!("menu bar: {e}"))
                .ok()?;
        }
        menu.init_for_nsapp();
        if let Some(help) = help {
            help.set_as_help_menu_for_nsapp();
        }

        let (sender, events) = mpsc::channel();
        let repaint = ctx.clone();
        MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
            let _ = sender.send(event.id);
            repaint.request_repaint();
        }));
        Some(Self {
            _menu: menu,
            actions: b.actions,
            panels: b.panels,
            events,
        })
    }
}

impl OxiApp {
    /// Queue the menu items chosen since the last frame. Called from `raw_input_hook`.
    pub(crate) fn take_menu_actions(&mut self) {
        let Some(menu) = &self.menu_bar else {
            return;
        };
        let actions: Vec<_> = menu
            .events
            .try_iter()
            .filter_map(|id| menu.actions.get(&id).copied())
            .collect();
        for action in actions {
            self.queue_action(action, false);
        }
    }

    /// Keep the View check marks in step with the panels, however they were toggled. Called at
    /// the end of the frame, after every toggle.
    pub(crate) fn sync_menu_bar(&mut self, ctx: &egui::Context) {
        let Some(menu) = &self.menu_bar else {
            return;
        };
        for (panel, item) in &menu.panels {
            // AppKit flips a check item on click, so compare with the item, not a cache.
            let shown = self.panel_shown(ctx, *panel);
            if item.is_checked() != shown {
                item.set_checked(shown);
            }
        }
    }
}
