//! `@` file and folder mention autocomplete popup and inline validation chips for the composer.
//!
//! Mirrors [`super::slash_menu`]: typing `@` (at the start of input or after whitespace) opens a
//! fuzzy-filtered popup of open tabs, git-changed files, workspace files, and directories.
//! `↑`/`↓` move, `Tab`/`Enter` insert `@path `, and `Esc` dismisses until the input changes.

use std::collections::HashSet;
use std::path::Path;

use eframe::egui::{
    self, Color32, CornerRadius, Frame, Id, Margin, Order, RichText, Sense, Stroke, TextEdit, Ui,
    text::CCursor, text::CCursorRange,
};

use crate::agent::tools::resolve_under_cwd;
use crate::app::file_explorer::fuzzy_path_score;
use crate::theme::*;

use super::*;

const ROW_H: f32 = 26.0;
const MENU_MAX_H: f32 = 260.0;
const MAX_RESULTS: usize = 18;
/// Must match `MENTION_FILE_MAX_BYTES` in `src/app/mentions.rs`.
const MENTION_FILE_MAX_BYTES: u64 = 32 * 1024;

#[derive(Clone, Default)]
struct MentionMenuState {
    /// Active `@query` token the selection belongs to; typing resets selection to 0.
    token: String,
    selected: usize,
    /// Input on which the user pressed Esc; stays hidden until input changes.
    dismissed: Option<String>,
    /// Selection moved by keyboard this frame and should scroll into view.
    scroll: bool,
}

#[derive(Clone)]
struct MentionCandidate {
    path: String,
    is_dir: bool,
    badge: Option<&'static str>,
}

#[derive(Clone)]
struct MentionChip {
    raw_token: String,
    byte_start: usize,
    byte_end: usize,
    is_dir: bool,
    oversized: bool,
}

#[derive(Clone, Default)]
struct MentionChipCache {
    root: String,
    input: String,
    chips: Vec<MentionChip>,
}

fn state_id() -> Id {
    Id::new("composer_mention_menu_state")
}

fn chip_cache_id() -> Id {
    Id::new("composer_mention_chip_cache")
}

fn load_state(ctx: &egui::Context) -> MentionMenuState {
    ctx.data(|d| d.get_temp::<MentionMenuState>(state_id()))
        .unwrap_or_default()
}

fn store_state(ctx: &egui::Context, state: MentionMenuState) {
    ctx.data_mut(|d| d.insert_temp(state_id(), state));
}

/// Active `@query` span immediately before the caret, if any.
/// Returns `(byte_start_of_at, byte_end_at_caret, query_without_at)`.
pub(super) fn active_mention_span(
    ctx: &egui::Context,
    input_id: Id,
    input: &str,
) -> Option<(usize, usize, String)> {
    if !input.contains('@') {
        return None;
    }
    let char_idx: usize = TextEdit::load_state(ctx, input_id)
        .and_then(|state| state.cursor.char_range())
        .map(|range| usize::from(range.primary.index))
        .unwrap_or_else(|| input.chars().count());
    let byte_end = input
        .char_indices()
        .nth(char_idx)
        .map(|(idx, _)| idx)
        .unwrap_or(input.len());
    let before = &input[..byte_end];
    let token_start = before
        .rfind(|c: char| c.is_whitespace())
        .map(|idx| idx + before[idx..].chars().next().map_or(1, char::len_utf8))
        .unwrap_or(0);
    let token = &before[token_start..];
    let query = token.strip_prefix('@')?;
    // Ignore email-like tokens or anything already containing another `@`.
    if query.contains('@') {
        return None;
    }
    Some((token_start, byte_end, query.to_owned()))
}

impl OxiApp {
    /// Ranked file and directory candidates matching `query` (open tabs and git changes first).
    fn mention_candidates(
        &mut self,
        ctx: &egui::Context,
        query: &str,
    ) -> (Vec<MentionCandidate>, bool) {
        let root_str = self.active_workspace().root_path.clone();
        let root = Path::new(&root_str);
        let (all_files, loading) = self.workspace_files_for_mentions(ctx);

        let mut open_set = HashSet::new();
        let mut open_paths = Vec::new();
        for doc in &self.conv.editor.documents {
            if doc.is_scratchpad {
                continue;
            }
            if let Ok(rel) = doc.path.strip_prefix(root) {
                let display = rel.to_string_lossy().replace('\\', "/");
                if open_set.insert(display.clone()) {
                    open_paths.push(display);
                }
            }
        }

        let mut changed_set = HashSet::new();
        let mut changed_paths = Vec::new();
        for entry in self
            .conv
            .git
            .staged
            .iter()
            .chain(self.conv.git.unstaged.iter())
        {
            if changed_set.insert(entry.path.clone()) {
                changed_paths.push(entry.path.clone());
            }
        }

        // Collect unique top-level and nested directory paths so `@src/` works too.
        let mut dirs_set = HashSet::new();
        for file in &all_files {
            let mut rest = file.as_str();
            while let Some((dir, _)) = rest.rsplit_once('/') {
                if !dirs_set.insert(dir.to_owned()) {
                    break;
                }
                rest = dir;
            }
        }

        let mut seen = HashSet::new();
        let mut scored: Vec<(i64, MentionCandidate)> = Vec::new();
        let mut push_candidate =
            |path: String, is_dir: bool, badge: Option<&'static str>, boost: i64| {
                if !seen.insert(path.clone()) {
                    return;
                }
                let display = if is_dir { format!("{path}/") } else { path };
                let score = if query.is_empty() {
                    Some(boost)
                } else {
                    fuzzy_path_score(&display, query).map(|s| s + boost)
                };
                if let Some(score) = score {
                    scored.push((
                        score,
                        MentionCandidate {
                            path: display,
                            is_dir,
                            badge,
                        },
                    ));
                }
            };

        for path in open_paths {
            let badge = if changed_set.contains(&path) {
                Some("open · modified")
            } else {
                Some("open")
            };
            push_candidate(path, false, badge, 400);
        }
        for path in changed_paths {
            push_candidate(path, false, Some("modified"), 250);
        }
        for file in all_files {
            push_candidate(file, false, None, 0);
        }
        let mut dirs: Vec<String> = dirs_set.into_iter().collect();
        dirs.sort();
        for dir in dirs {
            push_candidate(dir, true, Some("folder"), -20);
        }

        scored.sort_by(|(sa, a), (sb, b)| sb.cmp(sa).then_with(|| a.path.cmp(&b.path)));
        scored.truncate(MAX_RESULTS);
        (scored.into_iter().map(|(_, c)| c).collect(), loading)
    }

    /// Handle keys for the `@` mention menu before `TextEdit` sees them. Returns true while open.
    pub(super) fn mention_menu_keys(&mut self, ui: &mut Ui, input_id: Id, focused: bool) -> bool {
        if !focused || self.confirm_prompt_open() {
            return false;
        }
        if load_state(ui.ctx()).dismissed.as_deref() == Some(self.conv.composer.input.as_str()) {
            return false;
        }
        let Some((start, end, query)) =
            active_mention_span(ui.ctx(), input_id, &self.conv.composer.input)
        else {
            return false;
        };
        let (items, _loading) = self.mention_candidates(ui.ctx(), &query);
        if items.is_empty() {
            return false;
        }
        let mut state = load_state(ui.ctx());
        if state.token != query {
            state.token = query;
            state.selected = 0;
        }
        state.selected = state.selected.min(items.len() - 1);
        state.scroll = false;
        let none = egui::Modifiers::NONE;
        let (down, up, tab, enter, escape) = ui.input_mut(|i| {
            (
                i.consume_key(none, egui::Key::ArrowDown),
                i.consume_key(none, egui::Key::ArrowUp),
                i.consume_key(none, egui::Key::Tab),
                i.consume_key(none, egui::Key::Enter),
                i.consume_key(none, egui::Key::Escape),
            )
        });
        if down {
            state.selected = (state.selected + 1) % items.len();
            state.scroll = true;
        }
        if up {
            state.selected = (state.selected + items.len() - 1) % items.len();
            state.scroll = true;
        }
        if escape {
            state.dismissed = Some(self.conv.composer.input.clone());
        }
        let chosen = items[state.selected].clone();
        store_state(ui.ctx(), state);
        if tab || enter {
            self.accept_mention(ui.ctx(), input_id, start, end, &chosen);
        }
        true
    }

    fn accept_mention(
        &mut self,
        ctx: &egui::Context,
        input_id: Id,
        start: usize,
        end: usize,
        candidate: &MentionCandidate,
    ) {
        let replacement = format!("@{} ", candidate.path.trim_end_matches('/'));
        let mut next = String::with_capacity(self.conv.composer.input.len() + replacement.len());
        next.push_str(&self.conv.composer.input[..start]);
        next.push_str(&replacement);
        let caret_bytes = next.len();
        let tail = &self.conv.composer.input[end..];
        // Avoid doubling an existing trailing space right after the replaced span.
        next.push_str(tail.strip_prefix(' ').unwrap_or(tail));
        self.conv.composer.input = next;

        let caret_chars = self.conv.composer.input[..caret_bytes].chars().count();
        let mut te_state = TextEdit::load_state(ctx, input_id).unwrap_or_default();
        te_state
            .cursor
            .set_char_range(Some(CCursorRange::one(CCursor::new(caret_chars))));
        te_state.store(ctx, input_id);
        ctx.memory_mut(|m| m.request_focus(input_id));
    }

    /// Draw the `@` mention popup above the composer input.
    pub(super) fn render_mention_menu(
        &mut self,
        ui: &Ui,
        input_id: Id,
        anchor: egui::Rect,
        focused: bool,
    ) {
        if !focused {
            return;
        }
        let state = load_state(ui.ctx());
        if state.dismissed.as_deref() == Some(self.conv.composer.input.as_str()) {
            return;
        }
        let Some((start, end, query)) =
            active_mention_span(ui.ctx(), input_id, &self.conv.composer.input)
        else {
            return;
        };
        let (items, loading) = self.mention_candidates(ui.ctx(), &query);
        if items.is_empty() && !loading {
            return;
        }
        let selected = if state.token == query && !items.is_empty() {
            state.selected.min(items.len() - 1)
        } else {
            0
        };
        let width = ui.max_rect().width() + 2.0 * COMPOSER_FRAME_MARGIN;
        let pos = egui::pos2(
            anchor.left() - COMPOSER_FRAME_MARGIN,
            anchor.top() - COMPOSER_FRAME_MARGIN - 6.0,
        );
        let mut clicked = None;
        egui::Area::new(Id::new("composer_mention_menu"))
            .order(Order::Foreground)
            .pivot(egui::Align2::LEFT_BOTTOM)
            .fixed_pos(pos)
            .show(ui.ctx(), |ui| {
                Frame::new()
                    .fill(c_bg_elevated())
                    .stroke(Stroke::new(1.0, c_border()))
                    .corner_radius(crate::theme::RADIUS_PANEL)
                    .shadow(egui::epaint::Shadow {
                        offset: [0, 4],
                        blur: 16,
                        spread: 0,
                        color: Color32::from_black_alpha(90),
                    })
                    .inner_margin(Margin::same(4))
                    .show(ui, |ui| {
                        ui.set_width((width - 8.0).max(200.0));
                        if items.is_empty() {
                            ui.horizontal(|ui| {
                                ui.add_space(6.0);
                                small_spinner(ui);
                                ui.label(
                                    RichText::new("Indexing workspace files…")
                                        .size(FS_SMALL)
                                        .color(c_text_muted()),
                                );
                            });
                            return;
                        }
                        let list_h = (items.len() as f32 * ROW_H).min(MENU_MAX_H);
                        egui::ScrollArea::vertical()
                            .id_salt("composer_mention_scroll")
                            .max_height(list_h)
                            .min_scrolled_height(list_h)
                            .auto_shrink([false, true])
                            .show(ui, |ui| {
                                ui.spacing_mut().item_spacing.y = 0.0;
                                for (i, item) in items.iter().enumerate() {
                                    let is_selected = i == selected;
                                    let resp = mention_menu_row(ui, item, is_selected);
                                    if is_selected && state.scroll {
                                        resp.scroll_to_me(None);
                                    }
                                    if resp.clicked() {
                                        clicked = Some(item.clone());
                                    }
                                }
                            });
                    });
            });
        if let Some(item) = clicked {
            self.accept_mention(ui.ctx(), input_id, start, end, &item);
        }
    }

    /// Subtle removable chips showing resolved `@path` mentions currently in the draft, with a
    /// warning badge if a file exceeds the 32 KiB inline-context cap.
    pub(super) fn render_mention_chips(&mut self, ui: &mut Ui) {
        if !self.conv.composer.input.contains('@') {
            return;
        }
        let root = self.active_workspace().root_path.clone();
        let chips = cached_mention_chips(ui.ctx(), &root, &self.conv.composer.input);
        if chips.is_empty() {
            return;
        }
        let mut remove_span: Option<(usize, usize)> = None;
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = egui::vec2(6.0, 5.0);
            for chip in &chips {
                let border = if chip.oversized {
                    c_warning_stroke()
                } else {
                    c_border_subtle()
                };
                let fill = if chip.oversized {
                    c_warning_bg()
                } else {
                    c_bg_input()
                };
                let icon = if chip.is_dir { ICON_FOLDER } else { ICON_FILE };
                let icon_col = if chip.oversized {
                    c_warning_fg()
                } else {
                    c_accent()
                };
                Frame::new()
                    .fill(fill)
                    .stroke(Stroke::new(1.0, border))
                    .corner_radius(CornerRadius::same(RADIUS_CHIP))
                    .inner_margin(Margin::symmetric(7, 3))
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 5.0;
                            ui.label(crate::ui::chrome::icon_glyph_rich(icon, FS_TINY, icon_col));
                            let label = if chip.oversized {
                                format!("{} (>32 KB)", chip.raw_token)
                            } else {
                                chip.raw_token.clone()
                            };
                            let resp = ui.label(
                                RichText::new(label)
                                    .size(FS_TINY)
                                    .color(if chip.oversized {
                                        c_warning_fg()
                                    } else {
                                        c_text()
                                    }),
                            );
                            if chip.oversized {
                                resp.on_hover_text(
                                    "Larger than 32 KiB — oxi will mention the path so the agent can read it with its tool",
                                );
                            } else {
                                resp.on_hover_text("Included as inline @mention context on send");
                            }
                            if crate::ui::chrome::dismiss_button(ui)
                                .on_hover_text("Remove mention")
                                .clicked()
                            {
                                remove_span = Some((chip.byte_start, chip.byte_end));
                            }
                        });
                    });
            }
        });
        ui.add_space(COMPOSER_GAP);
        if let Some((start, end)) = remove_span
            && end <= self.conv.composer.input.len()
            && self.conv.composer.input.is_char_boundary(start)
            && self.conv.composer.input.is_char_boundary(end)
        {
            let tail_trim = usize::from(self.conv.composer.input[end..].starts_with(' '));
            self.conv
                .composer
                .input
                .replace_range(start..end + tail_trim, "");
        }
    }
}

fn cached_mention_chips(ctx: &egui::Context, root: &str, input: &str) -> Vec<MentionChip> {
    if let Some(cache) = ctx.data(|d| d.get_temp::<MentionChipCache>(chip_cache_id()))
        && cache.root == root
        && cache.input == input
    {
        return cache.chips;
    }
    let cwd = Path::new(root);
    let mut chips = Vec::new();
    let mut seen = HashSet::new();
    let mut offset = 0usize;
    while let Some(rel_at) = input[offset..].find('@') {
        let at = offset + rel_at;
        let after = &input[at + 1..];
        let end_rel = after
            .find(|c: char| c.is_whitespace() || matches!(c, ',' | ';' | ')' | ']' | '"' | '\''))
            .unwrap_or(after.len());
        let token = &after[..end_rel];
        offset = at + 1 + end_rel;
        if token.is_empty() || token.contains('@') {
            continue;
        }
        // Skip emails where `@` is immediately preceded by an alphanumeric char.
        if at > 0
            && input[..at]
                .chars()
                .next_back()
                .is_some_and(|c| !c.is_whitespace())
        {
            continue;
        }
        if !seen.insert(token.to_owned()) {
            continue;
        }
        if let Ok(abs) = resolve_under_cwd(cwd, token)
            && let Ok(meta) = std::fs::metadata(&abs)
        {
            let is_dir = meta.is_dir();
            if is_dir || meta.is_file() {
                chips.push(MentionChip {
                    raw_token: token.to_owned(),
                    byte_start: at,
                    byte_end: at + 1 + end_rel,
                    is_dir,
                    oversized: !is_dir && meta.len() > MENTION_FILE_MAX_BYTES,
                });
            }
        }
    }
    ctx.data_mut(|d| {
        d.insert_temp(
            chip_cache_id(),
            MentionChipCache {
                root: root.to_owned(),
                input: input.to_owned(),
                chips: chips.clone(),
            },
        );
    });
    chips
}

fn mention_menu_row(ui: &mut Ui, item: &MentionCandidate, selected: bool) -> egui::Response {
    let (rect, resp) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), ROW_H), Sense::click());
    let resp = resp.on_hover_cursor(egui::CursorIcon::PointingHand);
    let painter = ui.painter();
    if selected || resp.hovered() {
        painter.rect_filled(rect, CornerRadius::same(RADIUS_CHIP), c_row_hover());
    }
    let icon = if item.is_dir { ICON_FOLDER } else { ICON_FILE };
    let icon_col = if selected { c_accent() } else { c_text_muted() };
    painter.text(
        egui::pos2(rect.left() + 14.0, rect.center().y),
        egui::Align2::CENTER_CENTER,
        icon,
        egui::FontId::new(FS_TINY, icon_font()),
        icon_col,
    );

    let mut right = rect.right() - 10.0;
    if let Some(badge) = item.badge {
        let galley = painter.layout_no_wrap(
            badge.to_owned(),
            egui::FontId::proportional(FS_TINY),
            c_text_faint(),
        );
        let bx = right - galley.size().x;
        painter.galley(
            egui::pos2(bx, rect.center().y - galley.size().y * 0.5),
            galley,
            c_text_faint(),
        );
        right = bx - 8.0;
    }

    let x = rect.left() + 26.0;
    let mut job = egui::text::LayoutJob::single_section(
        item.path.clone(),
        egui::TextFormat::simple(
            egui::FontId::proportional(FS_SMALL),
            if selected { c_text_strong() } else { c_text() },
        ),
    );
    job.wrap = egui::text::TextWrapping::truncate_at_width((right - x).max(20.0));
    let galley = painter.layout_job(job);
    painter.galley(
        egui::pos2(x, rect.center().y - galley.size().y * 0.5),
        galley,
        c_text(),
    );
    resp
}

#[cfg(test)]
mod tests {
    use super::active_mention_span;
    use eframe::egui;

    #[test]
    fn active_mention_span_detects_at_tokens_and_skips_emails() {
        let ctx = egui::Context::default();
        let id = egui::Id::new("test_composer_mention");

        assert_eq!(active_mention_span(&ctx, id, "plain text"), None);
        assert_eq!(active_mention_span(&ctx, id, "mail user@example.com"), None);
        assert_eq!(
            active_mention_span(&ctx, id, "@"),
            Some((0, 1, String::new()))
        );
        assert_eq!(
            active_mention_span(&ctx, id, "fix @src/app/comp"),
            Some((4, 17, "src/app/comp".to_owned()))
        );
        assert_eq!(
            active_mention_span(&ctx, id, "verifică @README.md"),
            Some((
                "verifică ".len(),
                "verifică @README.md".len(),
                "README.md".to_owned()
            ))
        );
    }
}
