use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;

use cadcraft_color::{Rgb, aci_rgb, display_rgb};
use cadcraft_engine::lisp::dcl::MAX_STRING_BYTES;
use cadcraft_engine::lisp::{DialogControl, DialogEvent, DialogEventOutcome, DialogPrimitive, DialogRequest};
use egui::{pos2, vec2};

use crate::CadApp;
use crate::theme::Tokens;

/// Id of one keyed DCL control. The native test reads the edit-box selection through this id.
fn control_id(dialog: u64, key: &str) -> egui::Id {
    egui::Id::unique(("cadcraft-dcl-control", dialog, key))
}

fn dcl_state_id() -> egui::Id {
    egui::Id::unique("cadcraft-dcl-state")
}

#[derive(Clone, Default)]
struct DclState {
    dialog: u64,
    focus: Option<String>,
    select: HashSet<String>,
    highlight: Vec<String>,
    drafts: HashMap<Arc<str>, String>,
    editors: HashSet<egui::Id>,
    position: Option<(f64, f64)>,
    pending: VecDeque<TileInput>,
    text_limit: Option<Arc<str>>,
    popup: Option<egui::Id>,
    pointer_releases: u64,
    last_list_click: Option<ListClick>,
}

#[derive(Clone)]
struct TileInput {
    key: Arc<str>,
    value: Option<String>,
    reason: i32,
    edit: bool,
    list: Option<ListChoice>,
}

#[derive(Clone)]
struct ListChoice {
    /// Rows visible when the selection was made.
    rows: Arc<Vec<Arc<str>>>,
    click: Option<ListClick>,
    commit: bool,
}

#[derive(Clone)]
struct ListClick {
    key: Arc<str>,
    index: usize,
    row: Arc<str>,
    number: u64,
}

impl ListClick {
    fn follows(&self, earlier: &Self) -> bool {
        earlier.number.checked_add(1) == Some(self.number)
            && self.key == earlier.key
            && self.index == earlier.index
            && Arc::ptr_eq(&self.row, &earlier.row)
    }
}

#[derive(Default)]
struct PendingInput {
    edits: Vec<TileInput>,
    activations: Vec<TileInput>,
    single_release: Option<u64>,
}

/// Draw the dialog `start_dialog` is waiting on. Tile state stays in the engine.
pub(super) fn dcl(app: &mut CadApp, ctx: &egui::Context) {
    let previous = ctx.data_mut(|data| data.remove_temp::<DclState>(dcl_state_id())).unwrap_or_default();
    let Some(dialog) = app.session.dialog_id() else {
        forget_editors(ctx, previous.editors);
        return;
    };
    let mut state = if previous.dialog == dialog {
        previous
    } else {
        forget_editors(ctx, previous.editors);
        DclState { dialog, ..DclState::default() }
    };
    // Count every release, including releases during a callback wait. A widget
    // response summarizes the update, so only a lone release can establish a click.
    let releases =
        ctx.input(|input| input.raw.events.iter().filter(|event| matches!(event, egui::Event::PointerButton { pressed: false, .. })).count());
    state.pointer_releases = state.pointer_releases.saturating_add(u64::try_from(releases).unwrap_or(u64::MAX));
    let single_release = (releases == 1).then_some(state.pointer_releases);
    if app.session.dialog_waiting_for_tile() {
        dcl_window(app, ctx, &mut state, single_release);
    }
    if app.session.dialog_id() == Some(dialog) {
        ctx.data_mut(|data| data.insert_temp(dcl_state_id(), state));
    } else {
        forget_editors(ctx, state.editors);
    }
}

fn forget_editors(ctx: &egui::Context, editors: HashSet<egui::Id>) {
    if !editors.is_empty() {
        ctx.data_mut(|data| {
            for id in editors {
                data.remove::<egui::text_edit::TextEditState>(id);
            }
        });
    }
}

fn dcl_window(app: &mut CadApp, ctx: &egui::Context, state: &mut DclState, single_release: Option<u64>) {
    // An open dropdown handles Escape before the dialog itself can close.
    let popup_open = state.popup.is_some_and(|id| egui::ComboBox::is_open(ctx, id));
    if !popup_open && ctx.input_mut(|input| input.consume_key(egui::Modifiers::NONE, egui::Key::Escape)) {
        app.session.cancel();
        resume_drawing_input(ctx);
        return;
    }
    // Deliver a saved button click once the edit callback has returned to this dialog.
    send_pending(app, ctx, state);
    let Some(view) = app.session.dialog_view().filter(|view| view.id == state.dialog) else { return };
    state.popup = None;
    for request in app.session.take_dialog_requests() {
        match request {
            DialogRequest::Focus(key) => state.focus = Some(key),
            DialogRequest::Select(key) => {
                state.focus = Some(key.clone());
                state.select.insert(key);
            }
            DialogRequest::Highlight(key) => {
                if state.highlight.len() < 64 && !state.highlight.iter().any(|item| item == &key) {
                    state.highlight.push(key);
                }
            }
        }
    }
    let mut input = PendingInput { single_release, ..PendingInput::default() };
    let content = ctx.content_rect();
    // egui uses logical points. Include the viewport's screen offset when the platform has it.
    let origin = ctx.input(|input| input.viewport().inner_rect.map_or(egui::Pos2::ZERO, |rect| rect.min));
    let (pivot, position) = match view.position {
        Some((x, y)) if (x, y) != (-1.0, -1.0) => (
            egui::Align2::LEFT_TOP,
            pos2(
                (x - f64::from(origin.x)).clamp(f64::from(content.left()), f64::from(content.right())) as f32,
                (y - f64::from(origin.y)).clamp(f64::from(content.top()), f64::from(content.bottom())) as f32,
            ),
        ),
        _ => (egui::Align2::CENTER_CENTER, content.center()),
    };
    let window = egui::Window::new(view.title.as_ref())
        .id(egui::Id::unique(("cadcraft-dcl-dialog", view.id)))
        .collapsible(false)
        .resizable(false)
        .pivot(pivot)
        .default_pos(position)
        .show(ctx, |ui| {
            // A callback can grow a tile after the window opens. Keep the exit controls reachable.
            egui::ScrollArea::both().min_scrolled_width(0.0).min_scrolled_height(0.0).show(ui, |ui| {
                ui.set_min_width(300.0);
                ui.vertical(|ui| {
                    for control in &view.controls {
                        draw_control(ui, control, state, &mut input);
                    }
                    if state.text_limit.is_some() {
                        let text = crate::tl!("The edit exceeded the dialog text limit.");
                        let response = ui.colored_label(ui.visuals().error_fg_color, text);
                        crate::control::record_widget(ui, &response, text, "label", None);
                    }
                });
            });
        });
    if let Some(window) = window {
        // A button can appear before an edit box in the layout. Finish the edit before running
        // the button action, even if egui reports the focus change on the next pass.
        if !input.activations.is_empty() {
            input.edits.extend(state.drafts.drain().map(|(key, value)| TileInput { key, value: Some(value), reason: 2, edit: true, list: None }));
        }
        let corner = window.response.rect.min;
        state.position = Some((f64::from(corner.x) + f64::from(origin.x), f64::from(corner.y) + f64::from(origin.y)));
        state.pending.extend(input.edits.into_iter().chain(input.activations));
        send_pending(app, ctx, state);
    }
}

fn send_pending(app: &mut CadApp, ctx: &egui::Context, state: &mut DclState) {
    while app.session.dialog_id() == Some(state.dialog) && app.session.dialog_waiting_for_tile() {
        let Some(event) = state.pending.pop_front() else { break };
        let check_at = event.list.as_ref().and_then(|choice| choice.click.as_ref()).filter(|_| event.reason == 4).map(|click| click.index);
        if event.list.as_ref().is_some_and(|choice| !app.session.dialog_list_rows_current(&event.key, &choice.rows, check_at)) {
            continue;
        }
        let result = app.session.dialog_event(DialogEvent {
            id: state.dialog,
            key: &event.key,
            reason: event.reason,
            value: event.value.as_deref(),
            position: state.position,
        });
        match result {
            Ok(DialogEventOutcome::TextLimit) => {
                if event.edit {
                    state.focus = Some(event.key.to_string());
                }
                state.text_limit = Some(event.key.clone());
            }
            Ok(DialogEventOutcome::Applied) if state.text_limit.as_ref() == Some(&event.key) => state.text_limit = None,
            _ => {}
        }
        if event.edit && !matches!(result, Ok(DialogEventOutcome::Applied)) {
            // The draft did not reach Lisp. Keep independent edits and allow Cancel to discard it.
            state.pending.retain(|queued| queued.edit || queued.key.as_ref() == "cancel");
        }
        if event.reason == 1
            && matches!(result, Ok(DialogEventOutcome::Applied))
            && let Some(choice) = &event.list
        {
            state.last_list_click.clone_from(&choice.click);
            if choice.commit {
                state.pending.push_front(TileInput { reason: 4, ..event });
            }
        }
        ctx.request_repaint();
    }
    if app.session.dialog_id() != Some(state.dialog) || !app.session.dialog_waiting_for_tile() {
        resume_drawing_input(ctx);
    }
}

fn draw_control(ui: &mut egui::Ui, control: &DialogControl, state: &mut DclState, input: &mut PendingInput) {
    if !control.children.is_empty() {
        let _ = draw_label(ui, control);
        let row = control.row;
        let place = |ui: &mut egui::Ui| {
            for child in &control.children {
                draw_control(ui, child, state, input);
            }
        };
        if row {
            ui.horizontal(place)
        } else {
            ui.vertical(place)
        };
        return;
    }
    match control.kind.as_ref() {
        "edit_box" => draw_edit(ui, control, state, input),
        "list_box" => draw_list(ui, control, state, input),
        "image" => draw_image(ui, control),
        "popup_list" => draw_popup(ui, control, state, input),
        "toggle" => draw_toggle(ui, control, state, input),
        "button" | "ok_button" | "cancel_button" | "help_button" | "info_button" | "default_button" | "retirement_button" => {
            draw_button(ui, control, state, input)
        }
        _ => {
            let _ = draw_label(ui, control);
        }
    }
}

fn draw_label(ui: &mut egui::Ui, control: &DialogControl) -> Option<egui::Id> {
    if control.label.is_empty() {
        return None;
    }
    let text = control.label.as_ref();
    let response = ui.add_enabled(control.enabled, egui::Label::new(text));
    crate::control::record_widget(ui, &response, text, "label", None);
    Some(response.id)
}

fn draw_image(ui: &mut egui::Ui, control: &DialogControl) {
    let Some(image) = &control.image else { return };
    let width = tile_px(image.dim_x.saturating_add(1));
    let height = tile_px(image.dim_y.saturating_add(1));
    let (rect, response) = ui.allocate_exact_size(vec2(width, height), egui::Sense::hover());
    let name = if control.label.is_empty() { control.key.as_deref().unwrap_or_else(|| crate::tl!("Image")) } else { control.label.as_ref() };
    response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Image, control.enabled && response.enabled(), name));
    crate::control::record_widget(ui, &response, control.key.as_deref().unwrap_or(""), "image", None);
    let painter = ui.painter().with_clip_rect(rect);
    let canvas = Tokens::get().canvas;
    painter.rect_filled(rect, 0.0, canvas);
    let Some(picture) = &image.picture else { return };
    let background = Rgb(canvas.r(), canvas.g(), canvas.b());
    let point = |x, y| pos2(rect.min.x + tile_px(x), rect.min.y + tile_px(y));
    for primitive in &picture.primitives {
        match primitive {
            DialogPrimitive::Solid(solid) => {
                let mut mesh = egui::Mesh::default();
                let color = slide_color(solid.color, background);
                for triangle in solid.triangles.as_chunks::<3>().0 {
                    let Ok(first) = u32::try_from(mesh.vertices.len()) else { break };
                    for &(x, y) in triangle {
                        mesh.colored_vertex(point(x, y), color);
                    }
                    mesh.add_triangle(first, first + 1, first + 2);
                }
                painter.add(egui::Shape::mesh(mesh));
            }
            DialogPrimitive::Vector(line) => {
                let segment = [point(line.x1, line.y1), point(line.x2, line.y2)];
                painter.line_segment(segment, egui::Stroke::new(1.0, slide_color(line.color, background)));
            }
        }
    }
}

/// Tile coordinates fit in a few thousand pixels, which `f32` represents exactly.
#[allow(clippy::cast_precision_loss)]
fn tile_px(value: i32) -> f32 {
    let value = value.max(0);
    value as f32
}

fn slide_color(color: u8, background: Rgb) -> egui::Color32 {
    let rgb = display_rgb(aci_rgb(color), color == 7, background);
    egui::Color32::from_rgb(rgb.0, rgb.1, rgb.2)
}

fn draw_edit(ui: &mut egui::Ui, control: &DialogControl, state: &mut DclState, input: &mut PendingInput) {
    let Some(key) = control.key.as_ref() else { return };
    ui.horizontal(|ui| {
        let label_id = draw_label(ui, control);
        let mut text = state.drafts.remove(key).unwrap_or_else(|| control.value.to_string());
        let id = control_id(state.dialog, key);
        state.editors.insert(id);
        // Keep one character beyond the byte limit so validation can reject an oversized edit.
        let widget = egui::TextEdit::singleline(&mut text).id(id).desired_width(180.0).char_limit(MAX_STRING_BYTES + 1);
        let mut response = ui.add_enabled(control.enabled, widget);
        if let Some(label) = label_id {
            response = response.labelled_by(label);
        }
        apply_mode(ui, &response, key, &text, state);
        mark(ui, &response, control.label.as_ref(), "textbox", state, key);
        if !control.enabled || text == control.value.as_ref() {
            return;
        }
        // The editor uses logical focus when it handles input. Response::has_focus also depends
        // on OS window focus and can discard text the editor has already accepted.
        if response.lost_focus() || !ui.memory(|memory| memory.has_focus(response.id)) {
            let reason = if ui.input(|input| input.key_pressed(egui::Key::Enter)) { 1 } else { 2 };
            input.edits.push(TileInput { key: key.clone(), value: Some(text), reason, edit: true, list: None });
        } else {
            state.drafts.insert(key.clone(), text);
        }
    });
}

fn draw_list(ui: &mut egui::Ui, control: &DialogControl, state: &mut DclState, input: &mut PendingInput) {
    let Some(key) = control.key.clone() else { return };
    ui.vertical(|ui| {
        let _ = draw_label(ui, control);
        let row_height = list_row_height(ui);
        let rows = control.rows.len();
        let picked: HashSet<_> = if control.multiple {
            control.value.split_whitespace().filter_map(row_index).filter(|index| *index < rows).collect()
        } else {
            row_index(control.value.trim()).filter(|index| *index < rows).into_iter().collect()
        };
        let focus_row =
            (control.enabled && rows > 0 && state.focus.as_deref() == Some(key.as_ref())).then(|| picked.iter().copied().min().unwrap_or(0));
        ui.add_enabled_ui(control.enabled, |ui| {
            ui.set_min_width(180.0);
            let spacing = ui.spacing().item_spacing.y;
            let mut scroll = egui::ScrollArea::vertical()
                .id_salt(("cadcraft-dcl-list", state.dialog, key.as_ref()))
                .max_height(row_height * 8.0 + spacing * 7.0)
                .auto_shrink([false, true]);
            if let Some(row) = focus_row.and_then(|index| u16::try_from(index.min(rows.saturating_sub(8))).ok()) {
                scroll = scroll.vertical_scroll_offset(f32::from(row) * (row_height + spacing));
            }
            scroll.show_rows(ui, row_height, rows, |ui, range| {
                for index in range {
                    let text = control.rows.get(index).map(|row| row.as_ref()).unwrap_or("");
                    let selected = picked.contains(&index);
                    ui.set_min_width(180.0);
                    let response = draw_list_row(ui, selected, text, row_height);
                    if focus_row == Some(index) {
                        apply_mode(ui, &response, &key, text, state);
                    }
                    if text.is_empty() {
                        let name = format!("{key} {index}");
                        ui.ctx().accesskit_node_builder(response.id, |node| node.set_label(name));
                    }
                    crate::control::record_widget(ui, &response, text, "listbox", Some(selected));
                    if !response.clicked() && !response.double_clicked() {
                        continue;
                    }
                    let extend = ui.input(|input| input.modifiers.command || input.modifiers.ctrl);
                    let value = if control.multiple && extend { toggle_row(&picked, index) } else { index.to_string() };
                    let click = input
                        .single_release
                        .filter(|_| !control.multiple && response.clicked_by(egui::PointerButton::Primary))
                        .and_then(|number| control.rows.get(index).map(|row| ListClick { key: key.clone(), index, row: row.clone(), number }));
                    let commit = response.double_clicked()
                        && click.as_ref().zip(state.last_list_click.as_ref()).is_some_and(|(click, earlier)| click.follows(earlier));
                    input.activations.push(TileInput {
                        key: key.clone(),
                        value: Some(value),
                        reason: 1,
                        edit: false,
                        list: Some(ListChoice { rows: control.rows.clone(), click, commit }),
                    });
                }
            });
        });
    });
}

fn draw_popup(ui: &mut egui::Ui, control: &DialogControl, state: &mut DclState, input: &mut PendingInput) {
    let Some(key) = control.key.clone() else { return };
    ui.horizontal(|ui| {
        let _ = draw_label(ui, control);
        let rows = control.rows.len();
        let selected = row_index(control.value.trim()).filter(|index| *index < rows);
        let text = selected.and_then(|index| control.rows.get(index)).map(|row| row.as_ref()).unwrap_or("");
        let row_height = list_row_height(ui);
        ui.add_enabled_ui(control.enabled, |ui| {
            let shown = egui::ComboBox::from_id_salt(("cadcraft-dcl-popup", state.dialog, key.as_ref()))
                .selected_text(text)
                .width(180.0)
                .height(row_height * 8.0)
                .show_ui(ui, |ui| {
                    ui.set_min_width(180.0);
                    egui::ScrollArea::vertical()
                        .id_salt(("cadcraft-dcl-popup-rows", state.dialog, key.as_ref()))
                        .max_height(row_height * 8.0)
                        .auto_shrink([false, true])
                        .show_rows(ui, row_height, rows, |ui, range| {
                            for index in range {
                                let row = control.rows.get(index).map(|row| row.as_ref()).unwrap_or("");
                                let response = draw_list_row(ui, selected == Some(index), row, row_height);
                                crate::control::record_widget(ui, &response, row, "option", Some(selected == Some(index)));
                                if response.clicked() {
                                    input.activations.push(TileInput {
                                        key: key.clone(),
                                        value: Some(index.to_string()),
                                        reason: 1,
                                        edit: false,
                                        list: Some(ListChoice { rows: control.rows.clone(), click: None, commit: false }),
                                    });
                                }
                            }
                        });
                });
            if egui::ComboBox::is_open(ui.ctx(), shown.response.id) {
                state.popup = Some(shown.response.id);
            }
            apply_mode(ui, &shown.response, &key, text, state);
            let name = if control.label.is_empty() { key.as_ref() } else { control.label.as_ref() };
            ui.ctx().accesskit_node_builder(shown.response.id, |node| {
                node.set_label(name);
                node.set_value(text);
            });
            crate::control::record_widget(ui, &shown.response, text, "popup", Some(selected.is_some()));
        });
    });
}

fn list_row_height(ui: &egui::Ui) -> f32 {
    let spacing = ui.spacing();
    spacing.interact_size.y.max(ui.text_style_height(&egui::TextStyle::Button) + 2.0 * spacing.button_padding.y)
}

fn draw_list_row(ui: &mut egui::Ui, selected: bool, text: &str, height: f32) -> egui::Response {
    // Virtual scrolling uses one height per row. The button keeps the full text
    // for accessibility and truncates the painted label to a single line.
    let mut label = egui::RichText::new(text);
    if selected && ui.is_enabled() {
        // Choose the text color with greater contrast against the theme's selection fill.
        let fill = egui::Rgba::from(ui.visuals().selection.bg_fill);
        let luminance = 0.2126 * fill.r() + 0.7152 * fill.g() + 0.0722 * fill.b() + 0.05;
        label = label.color(if luminance / 0.05 > 1.05 / luminance { egui::Color32::BLACK } else { egui::Color32::WHITE });
    }
    ui.add(egui::Button::selectable(selected, label).truncate().min_size(vec2(0.0, height)))
}

fn row_index(text: &str) -> Option<usize> {
    if text.is_empty() || !text.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    text.parse().ok()
}

fn toggle_row(picked: &HashSet<usize>, index: usize) -> String {
    let mut next: Vec<_> = picked.iter().copied().filter(|row| *row != index).collect();
    if !picked.contains(&index) {
        next.push(index);
    }
    next.sort_unstable();
    next.iter().map(usize::to_string).collect::<Vec<_>>().join(" ")
}

fn draw_toggle(ui: &mut egui::Ui, control: &DialogControl, state: &mut DclState, input: &mut PendingInput) {
    let Some(key) = control.key.as_ref() else { return };
    let mut on = control.value.as_ref() == "1";
    let response = ui.add_enabled(control.enabled, egui::Checkbox::new(&mut on, control.label.as_ref()));
    apply_mode(ui, &response, key, "", state);
    mark(ui, &response, control.label.as_ref(), "checkbox", state, key);
    if response.changed() && control.enabled {
        let value = if on { "1" } else { "0" };
        input.activations.push(TileInput { key: key.clone(), value: Some(value.to_owned()), reason: 1, edit: false, list: None });
    }
}

fn draw_button(ui: &mut egui::Ui, control: &DialogControl, state: &mut DclState, input: &mut PendingInput) {
    let Some(key) = control.key.as_ref() else { return };
    let text = if control.label.is_empty() { key } else { control.label.as_ref() };
    let response = ui.add_enabled(control.enabled, egui::Button::new(text).min_size(vec2(72.0, 24.0)));
    apply_mode(ui, &response, key, "", state);
    mark(ui, &response, text, "button", state, key);
    if response.clicked() && control.enabled {
        input.activations.push(TileInput { key: key.clone(), value: None, reason: 1, edit: false, list: None });
    }
}

fn apply_mode(ui: &mut egui::Ui, response: &egui::Response, key: &str, text: &str, state: &mut DclState) {
    // The window's first sizing pass has no interactive widgets. Keep requests for the next pass.
    if !response.enabled() {
        return;
    }
    if state.select.remove(key)
        && let Some(mut editor) = egui::TextEdit::load_state(ui.ctx(), response.id)
    {
        let end = text.chars().count();
        editor.cursor.set_char_range(Some(egui::text::CCursorRange::two(egui::text::CCursor::new(0), egui::text::CCursor::new(end))));
        editor.store(ui.ctx(), response.id);
    }
    if state.focus.as_deref() == Some(key) {
        state.focus = None;
        response.request_focus();
    }
}

fn mark(ui: &mut egui::Ui, response: &egui::Response, label: &str, role: &'static str, state: &DclState, key: &str) {
    let highlighted = state.highlight.iter().any(|item| item == key);
    if highlighted && response.rect.is_positive() {
        ui.painter().rect_stroke(response.rect.expand(2.0), 2.0, egui::Stroke::new(2.0, Tokens::get().accent), egui::StrokeKind::Inside);
        ui.ctx().accesskit_node_builder(response.id, |node| node.set_selected(true));
    }
    crate::control::record_widget(ui, response, label, role, Some(highlighted));
}

fn resume_drawing_input(ctx: &egui::Context) {
    ctx.memory_mut(|memory| {
        if let Some(id) = memory.focused() {
            memory.surrender_focus(id);
        }
    });
    ctx.request_repaint();
}

#[cfg(test)]
mod tests;
