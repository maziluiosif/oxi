//! Editor tab strip and tab-level actions.

use std::path::PathBuf;

use eframe::egui::scroll_area::ScrollBarVisibility;
use eframe::egui::{self, Align, FontId, Frame, Layout, Margin, ScrollArea, Ui};

use crate::theme::*;
use crate::ui::chrome::icon_glyph_rich;

use super::super::{OxiApp, state::FileOperation};

impl OxiApp {
    pub(super) fn render_editor_tabs(&mut self, ui: &mut Ui) {
        let mut select = None;
        let mut close = None;
        let mut select_preview = None;
        let mut close_preview = None;
        let mut save = false;
        let mut reveal = false;
        let mut diff_request: Option<(usize, Option<super::DiffSource>)> = None;
        let mut select_git_diff = false;
        let mut close_git_diff = false;
        let mut close_all = false;
        let mut new_file = false;
        let mut navigate_back = false;
        let mut navigate_forward = false;
        let git_diff_tab = self.conv.diff_view.open && self.conv.git.diff.is_some();
        let git_diff_active = git_diff_tab && self.conv.editor.diff_tab_active;
        let can_go_back = !self.conv.editor.navigation_back.is_empty();
        let can_go_forward = !self.conv.editor.navigation_forward.is_empty();
        let sidebar_open = self.conv.sidebar.open;
        let workspace_root = PathBuf::from(&self.active_workspace().root_path);
        let markdown_available = !git_diff_active
            && self
                .conv
                .editor
                .active_document()
                .is_some_and(|document| document.supports_markdown_preview());
        let tab_strip_width = (ui.available_width()
            - if markdown_available { 74.0 } else { 0.0 }
            - 126.0
            - if sidebar_open {
                0.0
            } else {
                crate::ui::window_chrome::TRAFFIC_LIGHTS_W
            })
        .max(80.0);

        Frame::new()
            .fill(c_bg_elevated_2())
            .inner_margin(Margin::symmetric(6, 0))
            .show(ui, |ui| {
                ui.set_height(34.0);
                // Not `ui.horizontal`: that caps the row at `interact_size.y`, so the tabs would
                // stop short of the strip's lower edge and a hovered tab would float above it.
                ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                    ui.spacing_mut().item_spacing.x = 2.0;
                    if !sidebar_open {
                        // The macOS traffic lights sit over the window's top-left corner.
                        ui.add_space(crate::ui::window_chrome::TRAFFIC_LIGHTS_W);
                    }
                    let back = ui.add_enabled(
                        can_go_back,
                        egui::Button::new(icon_glyph_rich(
                            ICON_CHEVRON_LEFT,
                            FS_SMALL,
                            c_text_muted(),
                        ))
                        .frame(false)
                        .min_size(egui::vec2(24.0, 30.0)),
                    );
                    if back.clicked() {
                        navigate_back = true;
                    }
                    let forward = ui.add_enabled(
                        can_go_forward,
                        egui::Button::new(icon_glyph_rich(
                            ICON_CHEVRON_RIGHT,
                            FS_SMALL,
                            c_text_muted(),
                        ))
                        .frame(false)
                        .min_size(egui::vec2(24.0, 30.0)),
                    );
                    if forward.clicked() {
                        navigate_forward = true;
                    }

                    ScrollArea::horizontal()
                        .id_salt("editor_tabs")
                        .max_width(tab_strip_width)
                        .scroll_bar_visibility(ScrollBarVisibility::AlwaysHidden)
                        .show(ui, |ui| {
                            ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                                ui.spacing_mut().item_spacing.x = 0.0;
                                for (index, document) in
                                    self.conv.editor.documents.iter().enumerate()
                                {
                                    let name = if document.is_scratchpad {
                                        std::borrow::Cow::Borrowed("Scratchpad")
                                    } else {
                                        document
                                            .path
                                            .file_name()
                                            .unwrap_or_default()
                                            .to_string_lossy()
                                    };
                                    let label = match &document.diff {
                                        Some(diff) => std::borrow::Cow::Owned(format!(
                                            "{name} ({})",
                                            diff.source.tab_label()
                                        )),
                                        None => name.clone(),
                                    };
                                    let active = !git_diff_active
                                        && !self.conv.editor.markdown_preview_active
                                        && self.conv.editor.active == Some(index);
                                    let tab = editor_tab(
                                        ui,
                                        ui.id().with(("editor_tab", index)),
                                        &label,
                                        active,
                                        document.is_dirty(),
                                    );
                                    let response = tab.response;
                                    if tab.close_clicked || response.middle_clicked() {
                                        close = Some(index);
                                    } else if response.clicked() {
                                        select = Some(index);
                                    }
                                    let response = if document.is_scratchpad {
                                        response.on_hover_text("Scratchpad")
                                    } else {
                                        response.on_hover_ui(|ui| {
                                            ui.label(super::support::display_path(
                                                &workspace_root,
                                                &document.path,
                                            ));
                                        })
                                    };
                                    response.context_menu(|ui| {
                                        let save_label = if document.is_scratchpad {
                                            "Save As…"
                                        } else {
                                            "Save"
                                        };
                                        if ui.button(save_label).clicked() {
                                            select = Some(index);
                                            save = true;
                                            ui.close();
                                        }
                                        if !document.is_scratchpad
                                            && ui.button("Reveal in Explorer").clicked()
                                        {
                                            select = Some(index);
                                            reveal = true;
                                            ui.close();
                                        }
                                        if document.diff.is_some() {
                                            if ui.button("Close Diff").clicked() {
                                                select = Some(index);
                                                diff_request = Some((index, None));
                                                ui.close();
                                            }
                                        } else if !document.is_scratchpad
                                            && document.media.is_none()
                                        {
                                            if ui.button("Show Git Changes").clicked() {
                                                select = Some(index);
                                                diff_request = Some((
                                                    index,
                                                    Some(super::DiffSource::WorkTree),
                                                ));
                                                ui.close();
                                            }
                                            if ui.button("Compare with Saved").clicked() {
                                                select = Some(index);
                                                diff_request =
                                                    Some((index, Some(super::DiffSource::Unsaved)));
                                                ui.close();
                                            }
                                        }
                                        if ui.button("Close").clicked() {
                                            close = Some(index);
                                            ui.close();
                                        }
                                        if ui.button("Close All Tabs").clicked() {
                                            close_all = true;
                                            ui.close();
                                        }
                                    });
                                    if document.markdown_preview_open
                                        && document.supports_markdown_preview()
                                    {
                                        let label = format!("{name} (Preview)");
                                        let tab = editor_tab(
                                            ui,
                                            ui.id().with(("markdown_preview_tab", index)),
                                            &label,
                                            !git_diff_active
                                                && self.conv.editor.markdown_preview_active
                                                && self.conv.editor.active == Some(index),
                                            false,
                                        );
                                        if tab.close_clicked || tab.response.middle_clicked() {
                                            close_preview = Some(index);
                                        } else if tab.response.clicked() {
                                            select_preview = Some(index);
                                        }
                                        tab.response.on_hover_text(
                                            "Markdown preview · includes unsaved changes",
                                        );
                                    }
                                }

                                // Git diff pseudo-tab: keeps the diff one click away from the
                                // editable file tabs instead of replacing the whole chat area.
                                if git_diff_tab {
                                    let label = self.git_diff_tab_label();
                                    let tab = editor_tab(
                                        ui,
                                        ui.id().with("editor_git_diff_tab"),
                                        &label,
                                        git_diff_active,
                                        false,
                                    );
                                    let response = tab.response;
                                    if tab.close_clicked || response.middle_clicked() {
                                        close_git_diff = true;
                                    } else if response.clicked() {
                                        select_git_diff = true;
                                    }
                                    let hover = self.git_diff_tab_hover();
                                    response.on_hover_text(hover);
                                }
                            });
                        });

                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if markdown_available
                            && ui
                                .button("Preview")
                                .on_hover_text("Open Markdown preview in a separate tab")
                                .clicked()
                        {
                            select_preview = self.conv.editor.active;
                        }
                        // Frameless like the neighbouring buttons: a boxed button here read
                        // as a stray rectangle at the end of the tab strip.
                        egui::containers::menu::MenuButton::from_button(
                            egui::Button::new(icon_glyph_rich(
                                ICON_ANGLE_DOWN,
                                FS_SMALL,
                                c_text_muted(),
                            ))
                            .frame(false)
                            .min_size(egui::vec2(24.0, 30.0)),
                        )
                        .ui(ui, |ui| {
                            ui.set_min_width(180.0);
                            for (index, document) in self.conv.editor.documents.iter().enumerate() {
                                let name = if document.is_scratchpad {
                                    std::borrow::Cow::Borrowed("Scratchpad")
                                } else {
                                    document
                                        .path
                                        .file_name()
                                        .unwrap_or_default()
                                        .to_string_lossy()
                                };
                                if ui
                                    .selectable_label(
                                        !git_diff_active
                                            && !self.conv.editor.markdown_preview_active
                                            && self.conv.editor.active == Some(index),
                                        name,
                                    )
                                    .clicked()
                                {
                                    select = Some(index);
                                    ui.close();
                                }
                            }
                            if !self.conv.editor.documents.is_empty() || git_diff_tab {
                                ui.separator();
                                if ui.button("Close All Tabs").clicked() {
                                    close_all = true;
                                    ui.close();
                                }
                            }
                        })
                        .0
                        .on_hover_text("Open tabs");
                        if ui
                            .add(
                                egui::Button::new(icon_glyph_rich(
                                    ICON_PLUS,
                                    FS_SMALL,
                                    c_text_muted(),
                                ))
                                .frame(false)
                                .min_size(egui::vec2(24.0, 30.0)),
                            )
                            .on_hover_text("New file")
                            .clicked()
                        {
                            new_file = true;
                        }
                    });
                });
            });
        if new_file {
            let root = PathBuf::from(&self.active_workspace().root_path);
            self.start_file_operation(FileOperation::NewFile(root));
        }
        if navigate_back {
            self.navigate_editor_history(false);
        } else if navigate_forward {
            self.navigate_editor_history(true);
        }
        if let Some(index) = select {
            self.select_editor_tab(index);
        }
        if let Some(index) = select_preview {
            self.conv.editor.documents[index].markdown_preview_open = true;
            self.conv.editor.active = Some(index);
            self.conv.editor.markdown_preview_active = true;
            self.conv.editor.diff_tab_active = false;
            self.conv.editor.focus_editor_next_frame = false;
        }
        if let Some(index) = close_preview {
            self.conv.editor.documents[index].markdown_preview_open = false;
            if self.conv.editor.active == Some(index) {
                self.conv.editor.markdown_preview_active = false;
                self.conv.editor.focus_editor_next_frame = true;
            }
        }
        if select_git_diff {
            self.conv.editor.diff_tab_active = true;
        }
        if close_git_diff {
            self.close_editor_git_diff();
        }
        if save {
            self.save_editor_file(ui.ctx());
        }
        if reveal {
            self.reveal_active_file();
        }
        match diff_request {
            Some((index, Some(source))) => self.open_document_diff(index, source),
            Some((index, None)) => self.close_document_diff(index),
            None => {}
        }
        if let Some(index) = close {
            self.request_close_editor_tab(index);
        }
        if close_all {
            self.request_close_all_editor_tabs();
        }
    }

    /// Show document `index` as the active tab, as a click on it does.
    fn select_editor_tab(&mut self, index: usize) {
        self.conv.editor.git_full_highlight_path = None;
        self.conv.editor.active = Some(index);
        self.conv.editor.diff_tab_active = false;
        self.conv.editor.markdown_preview_active = false;
        self.conv.editor.focus_editor_next_frame = true;
        if let Some(path) = self
            .conv
            .editor
            .documents
            .get(index)
            .filter(|document| !document.is_scratchpad)
            .map(|document| document.path.clone())
        {
            self.reveal_editor_file_in_explorer(&path);
        }
    }

    /// Sublime's Ctrl+Tab in the editor: the next (or previous) document tab, wrapping.
    pub(crate) fn cycle_editor_tab(&mut self, back: bool) {
        let count = self.conv.editor.documents.len();
        let Some(active) = self.conv.editor.active.filter(|_| count > 1) else {
            return;
        };
        let next = if back {
            (active + count - 1) % count
        } else {
            (active + 1) % count
        };
        self.select_editor_tab(next);
    }

    /// Sublime's tab keys while a document shows: Cmd+Shift+] / [ and Cmd+Alt+Right / Left for
    /// the next / previous tab (Ctrl+PageDown / PageUp elsewhere), Cmd+1…9 (Alt+1…9 elsewhere)
    /// for the nth tab, 9 being the last.
    pub(crate) fn handle_editor_tab_keys(&mut self, ctx: &egui::Context) {
        use egui::Event;
        if self.conv.editor.active.is_none()
            || super::chord_pending(ctx)
            || self.conv.settings_page.open
            || self.conv.editor.file_picker_open
        {
            return;
        }
        let mac = cfg!(target_os = "macos");
        let step = ctx.input_mut(|input| {
            let position = input.events.iter().position(|event| {
                let Event::Key {
                    key,
                    physical_key,
                    pressed: true,
                    modifiers,
                    ..
                } = event
                else {
                    return false;
                };
                tab_key(*key, *physical_key, *modifiers, mac).is_some()
            })?;
            let Event::Key {
                key,
                physical_key,
                modifiers,
                ..
            } = input.events.remove(position)
            else {
                return None;
            };
            tab_key(key, physical_key, modifiers, mac)
        });
        let count = self.conv.editor.documents.len();
        match step {
            Some(TabKey::Next) => self.cycle_editor_tab(false),
            Some(TabKey::Previous) => self.cycle_editor_tab(true),
            Some(TabKey::Back) => self.navigate_editor_history(false),
            Some(TabKey::Forward) => self.navigate_editor_history(true),
            Some(TabKey::Nth(n)) if count > 0 => {
                let index = if n == 9 { count - 1 } else { n - 1 };
                if index < count {
                    self.select_editor_tab(index);
                }
            }
            _ => {}
        }
    }

    /// Cmd/Ctrl+W: close whatever tab is showing — the git diff tab, a Markdown preview, or the
    /// active document (asking first when it has unsaved edits).
    pub(crate) fn close_active_editor_tab(&mut self) {
        if self.conv.editor.diff_tab_active {
            self.close_editor_git_diff();
            return;
        }
        let Some(index) = self.conv.editor.active else {
            return;
        };
        if self.conv.editor.markdown_preview_active {
            self.conv.editor.documents[index].markdown_preview_open = false;
            self.conv.editor.markdown_preview_active = false;
            self.conv.editor.focus_editor_next_frame = true;
        } else {
            self.request_close_editor_tab(index);
        }
    }

    /// VS Code-style diff tab title: `stats.py (Working Tree)`, `stats.py (Staged)` or
    /// `Commit 1a2b3c4`.
    fn git_diff_tab_label(&self) -> String {
        let title = self.conv.git.diff.as_ref().map_or("", |(title, _)| title);
        if let Some(hash) = title.strip_prefix("Commit ") {
            return format!("Commit {}", &hash[..hash.len().min(7)]);
        }
        let path = self.conv.git.current_diff_path.as_deref().unwrap_or("diff");
        let file = path.rsplit_once('/').map_or(path, |(_, file)| file);
        if let Some(base) = compare_base(title) {
            format!("{file} (vs {})", crate::git::checkpoint::base_label(base))
        } else if self.conv.git.current_diff_staged == Some(true) {
            format!("{file} (Staged)")
        } else {
            format!("{file} (Working Tree)")
        }
    }

    /// Diff tab tooltip: the full repo-relative path and side (the tab itself only fits the
    /// file name), or the full hash and subject for a commit.
    fn git_diff_tab_hover(&self) -> String {
        let title = self.conv.git.diff.as_ref().map_or("", |(title, _)| title);
        let path = self
            .conv
            .git
            .current_diff_path
            .as_deref()
            .unwrap_or_default();
        if title.starts_with("Commit ") {
            return title.to_owned();
        }
        if let Some(base) = compare_base(title) {
            format!(
                "{path} · Changes since {}",
                crate::git::checkpoint::base_label(base)
            )
        } else if self.conv.git.current_diff_staged == Some(true) {
            format!("{path} · Staged changes")
        } else {
            format!("{path} · Working tree changes")
        }
    }
}

/// The base branch of a branch-compare diff title.
pub(crate) fn compare_base(title: &str) -> Option<&str> {
    let rest = title.strip_prefix(crate::git::COMPARE_TITLE_PREFIX)?;
    rest.split_once(": ").map(|(base, _)| base)
}

struct EditorTab {
    response: egui::Response,
    close_clicked: bool,
}

/// One Sublime-style tab. The active tab takes the editor's background and merges into it;
/// a hovered tab lights up within its own outline only (no pill spilling into the editor).
/// The close button shows on the active and hovered tabs; an unsaved tab shows a dot there
/// instead until hovered.
fn editor_tab(ui: &mut Ui, id: egui::Id, label: &str, active: bool, dirty: bool) -> EditorTab {
    const PADDING_LEFT: f32 = 12.0;
    const CLOSE_SLOT: f32 = 26.0;
    const TOP_INSET: f32 = 8.0;
    let font = FontId::proportional(FS_SMALL);
    let galley = ui.fonts_mut(|fonts| fonts.layout_no_wrap(label.to_owned(), font, c_text()));
    // Sublime keeps tabs at a roomy minimum width instead of hugging the name; the strip
    // scrolls when they no longer fit.
    const MIN_WIDTH: f32 = 140.0;
    let width = (galley.rect.width() + PADDING_LEFT + CLOSE_SLOT).max(MIN_WIDTH);
    let (rect, response) = ui.allocate_exact_size(
        egui::vec2(width, ui.available_height()),
        egui::Sense::click(),
    );
    response.widget_info(|| {
        egui::WidgetInfo::selected(
            egui::WidgetType::SelectableLabel,
            ui.is_enabled(),
            active,
            label,
        )
    });
    // The close target overlaps the tab; test the whole tab so both share one hover state.
    let hovered = ui.rect_contains_pointer(rect);
    let body = egui::Rect::from_min_max(egui::pos2(rect.left(), rect.top() + TOP_INSET), rect.max);
    let top_rounded = egui::CornerRadius {
        nw: RADIUS_ROW,
        ne: RADIUS_ROW,
        sw: 0,
        se: 0,
    };
    if active {
        // Reach past the strip's lower edge so the tab and the editor read as one surface.
        let mut fill = body;
        fill.max.y += 3.0;
        ui.painter().rect_filled(fill, top_rounded, c_bg_main());
    } else if hovered {
        // Well short of the active tab's color, and inset from the neighbours, so a hovered tab
        // next to the active one never reads as one merged shape.
        let fill = c_bg_elevated_2().lerp_to_gamma(c_bg_main(), 0.3);
        ui.painter()
            .rect_filled(body.shrink2(egui::vec2(2.0, 0.0)), top_rounded, fill);
    }
    let text_color = if active {
        c_text_strong()
    } else if hovered {
        c_text()
    } else {
        c_text_muted()
    };
    ui.painter().galley(
        egui::pos2(
            body.left() + PADDING_LEFT,
            rect.center().y - galley.size().y / 2.0,
        ),
        galley,
        text_color,
    );

    let close_rect = egui::Rect::from_center_size(
        egui::pos2(body.right() - CLOSE_SLOT / 2.0, rect.center().y),
        egui::vec2(18.0, 18.0),
    );
    let close = ui.interact(close_rect, id.with("close"), egui::Sense::click());
    close.widget_info(|| {
        egui::WidgetInfo::labeled(
            egui::WidgetType::Button,
            ui.is_enabled(),
            format!("Close {label}"),
        )
    });
    if close.hovered() {
        ui.painter().rect_filled(
            close_rect,
            egui::CornerRadius::same(4),
            c_bg_elevated_2().lerp_to_gamma(c_text_faint(), 0.25),
        );
    }
    if hovered || active && !dirty {
        ui.painter().text(
            close_rect.center(),
            egui::Align2::CENTER_CENTER,
            ICON_CLOSE,
            FontId::new(FS_TINY, icon_font()),
            if close.hovered() {
                c_text_strong()
            } else {
                c_text_faint()
            },
        );
    } else if dirty {
        ui.painter().circle_filled(
            close_rect.center(),
            3.5,
            if active { c_text() } else { c_text_muted() },
        );
    }
    if hovered {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    EditorTab {
        response,
        close_clicked: close.clicked(),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TabKey {
    Next,
    Previous,
    /// Sublime's Jump Back / Jump Forward through the navigation history.
    Back,
    Forward,
    /// 1-based; 9 means the last tab.
    Nth(usize),
}

fn tab_key(
    key: egui::Key,
    physical_key: Option<egui::Key>,
    modifiers: egui::Modifiers,
    mac: bool,
) -> Option<TabKey> {
    use egui::Key;
    let physical = physical_key.unwrap_or(key);
    // Jump Back / Forward: Ctrl+- on macOS, Alt+- elsewhere; with Shift, forward.
    let jump = if mac {
        modifiers.ctrl && !modifiers.mac_cmd && !modifiers.alt
    } else {
        modifiers.alt && !modifiers.ctrl
    };
    if jump && physical == Key::Minus {
        return Some(if modifiers.shift {
            TabKey::Forward
        } else {
            TabKey::Back
        });
    }
    if mac {
        let cmd = modifiers.mac_cmd && !modifiers.ctrl;
        if cmd && modifiers.shift && !modifiers.alt {
            return match physical {
                Key::CloseBracket => Some(TabKey::Next),
                Key::OpenBracket => Some(TabKey::Previous),
                _ => None,
            };
        }
        if cmd && modifiers.alt && !modifiers.shift {
            return match key {
                Key::ArrowRight => Some(TabKey::Next),
                Key::ArrowLeft => Some(TabKey::Previous),
                _ => None,
            };
        }
        if cmd && !modifiers.alt && !modifiers.shift {
            return digit(physical).map(TabKey::Nth);
        }
        return None;
    }
    if modifiers.ctrl && !modifiers.alt && !modifiers.shift {
        return match key {
            Key::PageDown => Some(TabKey::Next),
            Key::PageUp => Some(TabKey::Previous),
            _ => None,
        };
    }
    if modifiers.alt && !modifiers.ctrl && !modifiers.shift {
        return digit(physical).map(TabKey::Nth);
    }
    None
}

fn digit(key: egui::Key) -> Option<usize> {
    use egui::Key;
    Some(match key {
        Key::Num1 => 1,
        Key::Num2 => 2,
        Key::Num3 => 3,
        Key::Num4 => 4,
        Key::Num5 => 5,
        Key::Num6 => 6,
        Key::Num7 => 7,
        Key::Num8 => 8,
        Key::Num9 => 9,
        _ => return None,
    })
}

#[cfg(test)]
mod tab_key_tests {
    use super::{TabKey, tab_key};
    use eframe::egui::{Key, Modifiers};

    #[test]
    fn sublime_tab_bindings() {
        let cmd = Modifiers::MAC_CMD.plus(Modifiers::COMMAND);
        let shifted = cmd.plus(Modifiers::SHIFT);
        // Shift+] arrives as `}` on a US layout; the physical key is still `]`.
        assert_eq!(
            tab_key(
                Key::CloseCurlyBracket,
                Some(Key::CloseBracket),
                shifted,
                true
            ),
            Some(TabKey::Next)
        );
        assert_eq!(
            tab_key(Key::OpenBracket, None, shifted, true),
            Some(TabKey::Previous)
        );
        assert_eq!(tab_key(Key::Num3, None, cmd, true), Some(TabKey::Nth(3)));
        // Cmd+] alone is Indent.
        assert_eq!(tab_key(Key::CloseBracket, None, cmd, true), None);
        assert_eq!(
            tab_key(Key::ArrowLeft, None, cmd.plus(Modifiers::ALT), true),
            Some(TabKey::Previous)
        );
        assert_eq!(
            tab_key(
                Key::PageDown,
                None,
                Modifiers::CTRL.plus(Modifiers::COMMAND),
                false
            ),
            Some(TabKey::Next)
        );
        assert_eq!(
            tab_key(Key::Num9, None, Modifiers::ALT, false),
            Some(TabKey::Nth(9))
        );
    }
}
