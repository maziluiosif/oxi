//! Quiet dropdown with a searchable option list, used by the composer's provider / model /
//! effort pickers and by the options an ACP agent advertises.
//!
//! egui's `ComboBox` lays its list out inside last frame's popup size, so a list that was once
//! short (two models before the fetch finished, or a first frame that opened downwards off the
//! bottom of the window) stays short. Here the list height is computed from the rows and the
//! room around the button, and the popup opens upwards first since the pickers sit at the
//! bottom of the window.

use eframe::egui::{
    self, Color32, CornerRadius, Frame, Id, Margin, PopupCloseBehavior, RectAlign, Response,
    RichText, Sense, Stroke, TextEdit, Ui,
};

use crate::theme::*;

const ROW_H: f32 = 28.0;
const HEADER_H: f32 = 24.0;
const LIST_MAX_H: f32 = 360.0;
/// At least this many rows stay visible, even when the window is short.
const MIN_ROWS: f32 = 3.0;
const POPUP_MAX_W: f32 = 380.0;
/// Lists this short are scanned at a glance; a search field would only get in the way.
const SEARCH_MIN_OPTIONS: usize = 4;
const CHEVRON_W: f32 = 14.0;

/// One choice in a [`SelectMenu`].
#[derive(Clone, Debug, Default)]
pub struct SelectOption {
    pub label: String,
    /// Shown on hover; also searched.
    pub detail: String,
    /// Section header shown above the first option of each group.
    pub group: Option<String>,
    pub selected: bool,
}

impl SelectOption {
    pub fn new(label: impl Into<String>, selected: bool) -> Self {
        Self {
            label: label.into(),
            selected,
            ..Self::default()
        }
    }

    pub fn detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = detail.into();
        self
    }

    pub fn group(mut self, group: Option<String>) -> Self {
        self.group = group;
        self
    }
}

/// Per-popup state while it is open.
#[derive(Clone, Default)]
struct MenuState {
    query: String,
    /// Index into the filtered list.
    highlighted: usize,
    /// The highlight moved by keyboard this frame, so its row scrolls into view.
    scroll: bool,
}

pub struct SelectMenu<'a> {
    id: Id,
    text: &'a str,
    hover: &'a str,
    max_w: f32,
    min_popup_w: f32,
}

impl<'a> SelectMenu<'a> {
    pub fn new(id_salt: impl std::hash::Hash + std::fmt::Debug, text: &'a str) -> Self {
        Self {
            id: Id::new(("select_menu", id_salt)),
            text,
            hover: "",
            max_w: 160.0,
            min_popup_w: 180.0,
        }
    }

    /// Hover text for the closed button.
    pub fn hover(mut self, hover: &'a str) -> Self {
        self.hover = hover;
        self
    }

    /// Widest the closed button gets (label + chevron); longer labels truncate.
    pub fn max_width(mut self, max_w: f32) -> Self {
        self.max_w = max_w;
        self
    }

    /// Returns the index (into the list `options` built) of the option the user picked this
    /// frame. `options` only runs while the list is open.
    pub fn show(self, ui: &mut Ui, options: impl FnOnce() -> Vec<SelectOption>) -> Option<usize> {
        let popup_id = self.id.with("popup");
        let open = egui::Popup::is_id_open(ui.ctx(), popup_id);
        let button = quiet_button(ui, self.text, self.max_w, open);
        let button = if self.hover.is_empty() || open {
            button
        } else {
            button.on_hover_text(self.hover)
        };
        if button.clicked() && !open {
            // A fresh search every time the list opens.
            ui.ctx().data_mut(|d| d.remove::<MenuState>(popup_id));
        }

        // The popup toggles on this click below; build the list only when it will be shown.
        if open == button.clicked() {
            // Keep egui's open state in step with the click handled by the popup.
            egui::Popup::from_toggle_button_response(&button)
                .id(popup_id)
                .show(|_| ());
            return None;
        }
        let options = options();
        let width = popup_width(ui, &options, self.min_popup_w);
        let room = room_around(ui.ctx(), button.rect);
        let mut picked = None;
        egui::Popup::from_toggle_button_response(&button)
            .id(popup_id)
            .align(RectAlign::TOP_START)
            .align_alternatives(&[
                RectAlign::BOTTOM_START,
                RectAlign::TOP_END,
                RectAlign::BOTTOM_END,
            ])
            .gap(4.0)
            .close_behavior(PopupCloseBehavior::CloseOnClickOutside)
            .frame(
                Frame::new()
                    .fill(c_bg_elevated())
                    .stroke(Stroke::new(1.0, c_border()))
                    .corner_radius(CornerRadius::same(RADIUS_CARD))
                    .inner_margin(Margin::same(4)),
            )
            .show(|ui| {
                ui.set_width(width);
                picked = list(ui, &options, popup_id, room);
            });
        if picked.is_some() {
            egui::Popup::close_id(ui.ctx(), popup_id);
        }
        picked
    }
}

fn popup_width(ui: &Ui, options: &[SelectOption], min_w: f32) -> f32 {
    let widest = options
        .iter()
        .map(|o| {
            ui.painter()
                .layout_no_wrap(
                    o.label.clone(),
                    egui::FontId::proportional(FS_BODY),
                    c_text(),
                )
                .size()
                .x
        })
        .fold(0.0, f32::max);
    // Row padding on both sides plus the check mark column.
    (widest + 2.0 * 10.0 + 28.0).clamp(min_w, POPUP_MAX_W)
}

fn list(ui: &mut Ui, options: &[SelectOption], popup_id: Id, room: f32) -> Option<usize> {
    let mut state = ui
        .ctx()
        .data(|d| d.get_temp::<MenuState>(popup_id))
        .unwrap_or_else(|| MenuState {
            // Just opened: start on the current choice, scrolled into view.
            highlighted: options.iter().position(|o| o.selected).unwrap_or(0),
            scroll: true,
            ..MenuState::default()
        });
    // Before the search field, so ↑/↓/Enter don't also move its cursor or drop its focus.
    let before_typing = filter(options, &state.query);
    let picked = handle_keys(ui, &mut state, &before_typing, popup_id);
    let mut search_h = 0.0;
    if options.len() >= SEARCH_MIN_OPTIONS {
        let search = search_field(ui, popup_id, &mut state.query);
        search_h = search.rect.height() + ui.spacing().item_spacing.y;
        if search.changed() {
            state.highlighted = 0;
        }
    }
    let matches = filter(options, &state.query);
    state.highlighted = state.highlighted.min(matches.len().saturating_sub(1));

    let groups = matches
        .iter()
        .enumerate()
        .filter(|(n, i)| {
            let group = &options[**i].group;
            group.is_some() && (*n == 0 || options[matches[n - 1]].group != *group)
        })
        .count();
    let content_h = (matches.len().max(1)) as f32 * ROW_H + groups as f32 * HEADER_H;
    let list_h = content_h
        .min(LIST_MAX_H)
        .min((room - search_h - 16.0).max(MIN_ROWS * ROW_H));

    let mut clicked = None;
    egui::ScrollArea::vertical()
        .id_salt(popup_id.with("scroll"))
        .max_height(list_h)
        .min_scrolled_height(list_h)
        .auto_shrink([false, true])
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            if matches.is_empty() {
                let (rect, _) =
                    ui.allocate_exact_size(egui::vec2(ui.available_width(), ROW_H), Sense::hover());
                ui.painter().text(
                    egui::pos2(rect.left() + 10.0, rect.center().y),
                    egui::Align2::LEFT_CENTER,
                    "No matches",
                    egui::FontId::proportional(FS_SMALL),
                    c_text_faint(),
                );
            }
            let mut last_group: Option<&Option<String>> = None;
            for (n, &i) in matches.iter().enumerate() {
                let option = &options[i];
                if option.group.is_some() && last_group != Some(&option.group) {
                    group_header(ui, option.group.as_deref().unwrap_or_default());
                }
                last_group = Some(&option.group);
                let resp = option_row(ui, option, n == state.highlighted);
                if n == state.highlighted && state.scroll {
                    resp.scroll_to_me(None);
                }
                if resp.hovered() && ui.ctx().input(|i| i.pointer.delta() != egui::Vec2::ZERO) {
                    state.highlighted = n;
                }
                if resp.clicked() {
                    clicked = Some(i);
                }
            }
        });
    state.scroll = false;
    ui.ctx().data_mut(|d| d.insert_temp(popup_id, state));
    picked.or(clicked)
}

/// Indices of the options matching `query` (case-insensitive, label or detail), in order.
fn filter(options: &[SelectOption], query: &str) -> Vec<usize> {
    let query = query.trim().to_lowercase();
    options
        .iter()
        .enumerate()
        .filter(|(_, o)| {
            query.is_empty()
                || o.label.to_lowercase().contains(&query)
                || o.detail.to_lowercase().contains(&query)
                || o.group
                    .as_deref()
                    .is_some_and(|g| g.to_lowercase().contains(&query))
        })
        .map(|(i, _)| i)
        .collect()
}

/// ↑/↓ move the highlight, Enter picks it, Escape closes the list.
fn handle_keys(
    ui: &mut Ui,
    state: &mut MenuState,
    matches: &[usize],
    popup_id: Id,
) -> Option<usize> {
    let none = egui::Modifiers::NONE;
    let (down, up, enter, escape) = ui.input_mut(|i| {
        (
            i.consume_key(none, egui::Key::ArrowDown),
            i.consume_key(none, egui::Key::ArrowUp),
            i.consume_key(none, egui::Key::Enter),
            i.consume_key(none, egui::Key::Escape),
        )
    });
    if escape {
        egui::Popup::close_id(ui.ctx(), popup_id);
        return None;
    }
    if matches.is_empty() {
        state.highlighted = 0;
        return None;
    }
    state.highlighted = state.highlighted.min(matches.len() - 1);
    if down {
        state.highlighted = (state.highlighted + 1) % matches.len();
        state.scroll = true;
    }
    if up {
        state.highlighted = (state.highlighted + matches.len() - 1) % matches.len();
        state.scroll = true;
    }
    enter.then(|| matches[state.highlighted])
}

/// Vertical space the list may use: the larger of the gaps above and below the button.
fn room_around(ctx: &egui::Context, button: egui::Rect) -> f32 {
    let screen = ctx.content_rect();
    (button.top() - screen.top()).max(screen.bottom() - button.bottom()) - 12.0
}

fn search_field(ui: &mut Ui, popup_id: Id, query: &mut String) -> Response {
    let id = popup_id.with("search");
    let resp = Frame::new()
        .inner_margin(Margin::symmetric(6, 4))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                ui.label(crate::ui::chrome::icon_glyph_rich(
                    ICON_SEARCH,
                    FS_SMALL,
                    c_text_faint(),
                ));
                ui.add(
                    TextEdit::singleline(query)
                        .id(id)
                        .hint_text(
                            RichText::new("Select an option…")
                                .size(FS_BODY)
                                .color(c_text_faint()),
                        )
                        .font(egui::FontId::proportional(FS_BODY))
                        .frame(Frame::NONE)
                        .desired_width(f32::INFINITY),
                )
            })
            .inner
        })
        .inner;
    // Typing goes straight into the search: focus it whenever the list is open.
    if !resp.has_focus() {
        resp.request_focus();
    }
    let rect = ui.min_rect();
    ui.painter().hline(
        rect.x_range(),
        rect.bottom() + ui.spacing().item_spacing.y * 0.5,
        Stroke::new(1.0, c_border_subtle()),
    );
    ui.add_space(2.0);
    resp
}

fn group_header(ui: &mut Ui, label: &str) {
    let (rect, _) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), HEADER_H), Sense::hover());
    ui.painter().text(
        egui::pos2(rect.left() + 10.0, rect.bottom() - 6.0),
        egui::Align2::LEFT_BOTTOM,
        label,
        egui::FontId::proportional(FS_TINY),
        c_text_faint(),
    );
}

fn option_row(ui: &mut Ui, option: &SelectOption, highlighted: bool) -> Response {
    let (rect, resp) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), ROW_H), Sense::click());
    let resp = resp.on_hover_cursor(egui::CursorIcon::PointingHand);
    let painter = ui.painter();
    if highlighted {
        painter.rect_filled(rect, CornerRadius::same(RADIUS_ROW), c_row_hover());
    }
    let check_w = 24.0;
    let mut job = egui::text::LayoutJob::single_section(
        option.label.clone(),
        egui::TextFormat::simple(egui::FontId::proportional(FS_BODY), c_text()),
    );
    job.wrap =
        egui::text::TextWrapping::truncate_at_width((rect.width() - 20.0 - check_w).max(0.0));
    let galley = painter.layout_job(job);
    painter.galley(
        egui::pos2(rect.left() + 10.0, rect.center().y - galley.size().y * 0.5),
        galley,
        c_text(),
    );
    if option.selected {
        painter.text(
            egui::pos2(rect.right() - 12.0, rect.center().y),
            egui::Align2::RIGHT_CENTER,
            ICON_CHECK,
            egui::FontId::new(FS_SMALL, icon_font()),
            c_accent(),
        );
    }
    if option.detail.is_empty() {
        resp
    } else {
        resp.on_hover_text(&option.detail)
    }
}

/// Transparent label + chevron that gets a soft pill on hover and while its list is open.
fn quiet_button(ui: &mut Ui, text: &str, max_w: f32, open: bool) -> Response {
    let pad = ui.spacing().button_padding;
    let color = if open { c_accent() } else { c_text_muted() };
    let mut job = egui::text::LayoutJob::single_section(
        text.to_owned(),
        egui::TextFormat::simple(egui::FontId::proportional(FS_SMALL), color),
    );
    job.wrap = egui::text::TextWrapping::truncate_at_width((max_w - CHEVRON_W).max(8.0));
    let galley = ui.painter().layout_job(job);
    let height = ui
        .spacing()
        .interact_size
        .y
        .max(galley.size().y + 2.0 * pad.y);
    let size = egui::vec2(galley.size().x + CHEVRON_W + 2.0 * pad.x, height);
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
    let resp = resp.on_hover_cursor(egui::CursorIcon::PointingHand);
    if ui.is_rect_visible(rect) {
        let painter = ui.painter();
        if open || resp.hovered() {
            painter.rect_filled(rect, CornerRadius::same(RADIUS_ROW), c_row_hover());
        }
        let text_pos = egui::pos2(rect.left() + pad.x, rect.center().y - galley.size().y * 0.5);
        painter.galley(text_pos, galley, color);
        painter.text(
            egui::pos2(rect.right() - pad.x - CHEVRON_W * 0.5, rect.center().y),
            egui::Align2::CENTER_CENTER,
            if open { ICON_ANGLE_UP } else { ICON_ANGLE_DOWN },
            egui::FontId::new(9.0, icon_font()),
            if resp.hovered() || open {
                color
            } else {
                c_text_faint()
            },
        );
    }
    resp
}

/// Label followed by a small on/off switch, for boolean agent options such as "Fast mode".
/// Returns the new value when clicked.
pub fn labeled_switch(ui: &mut Ui, label: &str, on: bool, hover: &str) -> Option<bool> {
    let pad = ui.spacing().button_padding;
    let galley = ui.painter().layout_no_wrap(
        label.to_owned(),
        egui::FontId::proportional(FS_SMALL),
        c_text_muted(),
    );
    let track = egui::vec2(28.0, 16.0);
    let height = ui.spacing().interact_size.y;
    let size = egui::vec2(galley.size().x + 6.0 + track.x + 2.0 * pad.x, height);
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
    let resp = resp.on_hover_cursor(egui::CursorIcon::PointingHand);
    let resp = if hover.is_empty() {
        resp
    } else {
        resp.on_hover_text(hover)
    };
    if ui.is_rect_visible(rect) {
        let t = ui.ctx().animate_bool_with_time(resp.id, on, 0.12);
        let painter = ui.painter();
        if resp.hovered() {
            painter.rect_filled(rect, CornerRadius::same(RADIUS_ROW), c_row_hover());
        }
        painter.galley(
            egui::pos2(rect.left() + pad.x, rect.center().y - galley.size().y * 0.5),
            galley,
            c_text_muted(),
        );
        let track_rect = egui::Rect::from_min_size(
            egui::pos2(
                rect.right() - pad.x - track.x,
                rect.center().y - track.y * 0.5,
            ),
            track,
        );
        let off_fill = c_bg_elevated_2();
        let fill = lerp_color(off_fill, c_accent(), t);
        painter.rect(
            track_rect,
            CornerRadius::same(255),
            fill,
            Stroke::new(1.0, if on { Color32::TRANSPARENT } else { c_border() }),
            egui::StrokeKind::Inside,
        );
        let knob_r = track.y * 0.5 - 2.5;
        let knob_x = egui::lerp(
            (track_rect.left() + knob_r + 2.5)..=(track_rect.right() - knob_r - 2.5),
            t,
        );
        painter.circle_filled(
            egui::pos2(knob_x, track_rect.center().y),
            knob_r,
            lerp_color(c_text_muted(), c_on_accent(), t),
        );
    }
    resp.clicked().then_some(!on)
}

fn lerp_color(a: Color32, b: Color32, t: f32) -> Color32 {
    let mix = |x: u8, y: u8| egui::lerp(x as f32..=y as f32, t).round() as u8;
    Color32::from_rgba_unmultiplied(
        mix(a.r(), b.r()),
        mix(a.g(), b.g()),
        mix(a.b(), b.b()),
        mix(a.a(), b.a()),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filter_matches_label_detail_and_group_case_insensitively() {
        let options = vec![
            SelectOption::new("Opus 5.5", true),
            SelectOption::new("Sonnet 5.5", false).detail("Fast everyday model"),
            SelectOption::new("gpt-5", false).group(Some("OpenAI".into())),
        ];
        assert_eq!(filter(&options, ""), [0, 1, 2]);
        assert_eq!(filter(&options, "OPUS"), [0]);
        assert_eq!(filter(&options, "everyday"), [1]);
        assert_eq!(filter(&options, "openai"), [2]);
        assert!(filter(&options, "haiku").is_empty());
    }
}
