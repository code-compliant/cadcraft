use std::path::{Path, PathBuf};

use cadcraft_doc::EntityKind;
use cadcraft_engine::Session;
use cadcraft_engine::lisp::{DialogControl, DialogEvent, DialogEventOutcome, DialogPrimitive};
use egui::{Rect, Shape, vec2};
use egui_kittest::Harness;
use egui_kittest::kittest::{Queryable, by};
use serde_json::json;

use crate::control::{ControlRequest, Outcome};
use crate::{CadApp, Services};

const DCL: &str = r#"parts : dialog {
  label = "Parts";
  : text { label = "Name"; }
  : column {
    : edit_box { key = "part"; label = "Part"; value = "bolt"; }
    : toggle { key = "stock"; label = "In stock"; value = "0"; }
  }
  : row {
    : button { key = "apply"; label = "Apply"; }
    ok_cancel;
  }
}
early_exit : dialog {
  ok_cancel;
  : edit_box { key = "part"; label = "Part"; value = "bolt"; }
}
two_fields : dialog {
  : edit_box { key = "part"; label = "Part"; value = "bolt"; }
  : edit_box { key = "other"; label = "Other"; value = "nut"; }
  ok_cancel;
}
messages : dialog {
  : text { value = "Opening message"; }
  : text { key = "message"; label = "Working"; }
  : text { label = "Explicitly blank message"; value = ""; }
  : button { key = "apply"; label = "Apply"; }
  : button { key = "clear"; label = "Clear message"; }
  ok_cancel;
}
tall_message : dialog {
  : button { key = "grow"; label = "Grow message"; }
  : text { key = "message"; value = "Short"; }
  ok_cancel;
}
grouped : dialog {
  : button { key = "enable"; label = "Enable group"; }
  : row {
    key = "group";
    label = "Group";
    ok_button;
    : column {
      : edit_box { key = "part"; label = "Part"; value = "bolt"; }
      : text { label = "Inside group"; }
    }
  }
  : text { key = "message"; value = "Notice"; }
  cancel_button;
}
"#;

struct Scratch {
    path: PathBuf,
}

impl Scratch {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "cadcraft-dcl-native-{}-{}",
            std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0)
        ));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).unwrap();
        std::fs::write(path.join("parts.dcl"), DCL).unwrap();
        Self { path }
    }

    fn dcl(&self) -> String {
        self.path.join("parts.dcl").display().to_string().replace('\\', "\\\\").replace('"', "\\\"")
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

fn harness(app: CadApp) -> Harness<'static, CadApp> {
    harness_with_dt(app, 0.25)
}

fn harness_with_dt(app: CadApp, step_dt: f32) -> Harness<'static, CadApp> {
    harness_with_contents(app, step_dt, crate::chrome::file_tabs)
}

fn harness_with_contents(app: CadApp, step_dt: f32, contents: fn(&mut CadApp, &mut egui::Ui)) -> Harness<'static, CadApp> {
    let mut h = Harness::builder().with_size(vec2(1200.0, 800.0)).with_step_dt(step_dt).build_ui_state(
        move |ui, app: &mut CadApp| {
            // Draw the test's part of the main window. The full dock needs a command board
            // that this craft-libs revision can install only in the real window driver.
            let ctx = ui.ctx().clone();
            app.logic(&ctx);
            crate::control::begin_frame(&ctx);
            contents(app, ui);
            super::dcl(app, &ctx);
        },
        app,
    );
    h.run_steps(4);
    h
}

fn start(extra: &str) -> (Scratch, Harness<'static, CadApp>) {
    start_with(Scratch::new(), extra)
}

fn start_with(scratch: Scratch, extra: &str) -> (Scratch, Harness<'static, CadApp>) {
    let app = app_with_dialog(&scratch, extra);
    (scratch, harness(app))
}

fn app_with_dialog(scratch: &Scratch, extra: &str) -> CadApp {
    let mut app = CadApp::new(Session::new(), Services::default());
    let code = format!("(progn (setq id (load_dialog \"{}\")) {extra})", scratch.dcl());
    let err = app.cmdline(&code);
    assert!(err.is_none(), "{err:?}");
    assert!(app.session.dialog_active(), "{code}\n{:?}", app.session.log);
    app
}

fn lisp(app: &CadApp, name: &str) -> String {
    cadcraft_engine::lisp::value::to_text(&app.session.lisp_var(name), true)
}

#[test]
fn native_dialog_canvas_click_answers_callback_point_prompt() {
    let scratch = Scratch::new();
    let app = app_with_dialog(
        &scratch,
        r#"(new_dialog "parts" id)
           (action_tile "apply" "(setq pt (getpoint \"Pick a point: \")) (done_dialog 3)")
           (setq status (start_dialog))"#,
    );
    let mut h = harness_with_contents(app, 0.25, crate::canvas::show);
    h.get_by_label("Apply").click();
    h.run_steps(2);
    assert!(h.state().session.prompt_text().contains("Pick a point"));

    let rect = h.state().canvas.rect.unwrap();
    let point = rect.left_top() + vec2(rect.width() * 0.25, rect.height() * 0.75);
    h.event(egui::Event::PointerMoved(point));
    h.run_steps(2);
    for pressed in [true, false] {
        h.event(egui::Event::PointerButton { pos: point, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE });
        h.step();
    }
    assert_eq!(lisp(h.state(), "STATUS"), "3");
    assert_ne!(lisp(h.state(), "PT"), "nil");
    assert!(!h.state().session.dialog_active());
}

fn element(h: &mut Harness<'static, CadApp>, label: &str, role: &str) -> serde_json::Value {
    let ctx = h.ctx.clone();
    let (req, _) = ControlRequest::new("ui.elements", json!({"label": label, "role": role}));
    match crate::control::handle(h.state_mut(), &ctx, &req) {
        Outcome::Done(value) => value["result"].as_array().and_then(|items| items.first().cloned()).unwrap_or(json!({})),
        Outcome::Screenshot { .. } => json!({}),
    }
}

fn outline(controls: &[DialogControl], out: &mut Vec<String>) {
    for control in controls {
        let key = control.key.as_deref().unwrap_or("");
        out.push(format!("{}:{key}:{}", control.kind, if control.row { "row" } else { "column" }));
        outline(&control.children, out);
    }
}

fn painted(h: &Harness<'static, CadApp>) -> Vec<(String, Rect)> {
    fn walk(shape: &Shape, clip: Rect, out: &mut Vec<(String, Rect)>) {
        match shape {
            Shape::Text(text) => {
                let rect = text.visual_bounding_rect().intersect(clip);
                if rect.is_positive() {
                    out.push((text.galley.text().to_string(), rect));
                }
            }
            Shape::Vec(shapes) => shapes.iter().for_each(|shape| walk(shape, clip, out)),
            _ => {}
        }
    }
    let mut out = Vec::new();
    for shape in &h.output().shapes {
        walk(&shape.shape, shape.clip_rect, &mut out);
    }
    out
}

fn at(texts: &[(String, Rect)], label: &str) -> Rect {
    texts.iter().find(|(text, _)| text == label).unwrap_or_else(|| panic!("missing {label}")).1
}

/// The native window follows the prepared order and shows the current values.
#[test]
fn native_dialog_shows_prepared_controls() {
    let (_scratch, mut h) = start("(new_dialog \"parts\" id) (start_dialog)");
    h.get_by_label("In stock");
    h.get_by_label("Name");
    h.get_by_label("Apply");
    h.get_by_label("OK");
    h.get_by_label("Cancel");
    h.get(by().role(egui::accesskit::Role::TextInput).value("bolt"));
    let view = h.state().session.dialog_view().expect("the waiting dialog");
    assert_eq!(view.title.as_ref(), "Parts");
    let mut order = Vec::new();
    outline(&view.controls, &mut order);
    assert_eq!(
        order,
        vec![
            "text::column",
            "column::column",
            "edit_box:part:column",
            "toggle:stock:column",
            "row::row",
            "button:apply:column",
            "ok_cancel::row",
            "ok_button:accept:column",
            "cancel_button:cancel:column",
        ]
    );
    let texts = painted(&h);
    let (name, part, stock, apply, ok, cancel) =
        (at(&texts, "Name"), at(&texts, "Part"), at(&texts, "In stock"), at(&texts, "Apply"), at(&texts, "OK"), at(&texts, "Cancel"));
    assert!(name.bottom() <= part.top() + 2.0, "Name is above Part: {name:?} {part:?}");
    assert!(part.bottom() <= stock.top() + 2.0, "the column stacks Part above In stock");
    assert!(stock.bottom() <= apply.top() + 2.0, "the buttons sit under the column");
    assert!((apply.center().y - ok.center().y).abs() < 4.0 && (ok.center().y - cancel.center().y).abs() < 4.0, "Apply, OK and Cancel share a row");
    assert!(apply.right() <= ok.left() + 2.0 && ok.right() <= cancel.left() + 2.0, "Apply, OK, Cancel stay in source order");
    if let Ok(path) = std::env::var("CADCRAFT_DCL_SHOT")
        && let Ok(image) = h.render()
    {
        image.save(Path::new(&path)).unwrap();
    }
}

/// An edit and a toggle reach the action, and Accept builds geometry from those values.
#[test]
fn native_dialog_edits_a_field_toggles_and_accepts() {
    let (scratch, mut h) = start(
        r#"(new_dialog "parts" id)
             (action_tile "part" "(setq saw (strcat $key \"=\" $value))")
             (action_tile "stock" "(setq saw (strcat $key \"=\" $value))")
             (setq st (start_dialog))
             (setq part (get_tile "part"))
             (setq stock (get_tile "stock"))
             (if (= st 1) (progn
                (command "-LAYER" "New" part "Set" part "")
                (command "_.LINE" "0,0" "12,0" "")
                (if (= stock "1") (command "_.CIRCLE" "6,4" "2"))))"#,
    );
    h.get(by().role(egui::accesskit::Role::TextInput).value("bolt")).focus();
    h.run_steps(2);
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
    h.run_steps(1);
    h.get(by().role(egui::accesskit::Role::TextInput).value("bolt")).type_text("Clamp");
    h.run_steps(1);
    h.key_press(egui::Key::Enter);
    h.run_steps(2);
    assert_eq!(lisp(h.state(), "SAW"), "\"part=Clamp\"");
    h.get(by().role(egui::accesskit::Role::TextInput).value("Clamp"));
    h.get_by_label("In stock").click();
    h.run_steps(2);
    assert_eq!(lisp(h.state(), "SAW"), "\"stock=1\"");
    h.get_by_label("OK").click();
    h.run_steps(3);
    assert!(!h.state().session.dialog_active());
    assert_eq!(lisp(h.state(), "ST"), "1");
    assert_eq!(lisp(h.state(), "PART"), "\"Clamp\"");
    assert_eq!(lisp(h.state(), "STOCK"), "\"1\"");
    let name = "Clamp";
    h.state_mut().session.execute("style", &json!({"name": name, "height": 2.5, "current": true})).unwrap();
    let before = snapshot(h.state().session.doc().unwrap());
    let picture = render(h.state_mut());
    let path = scratch.path.join("parts.dxf");
    let bytes = cadcraft_io::write(h.state().session.doc().unwrap(), "parts.dxf").unwrap();
    std::fs::write(&path, &bytes).unwrap();
    let opened = cadcraft_io::read(&std::fs::read(&path).unwrap(), "parts.dxf").unwrap();
    assert_eq!(snapshot(&opened), before);
    h.state_mut().session.open_drawing(opened, "parts", Some(path.display().to_string()));
    assert_eq!(render(h.state_mut()), picture);
}

fn snapshot(doc: &cadcraft_doc::Drawing) -> String {
    let mut entities = Vec::new();
    for entity in doc.model.iter() {
        let layer = entity.common.layer.clone();
        let body = match &entity.kind {
            EntityKind::Line(line) => format!("line {:.3},{:.3} {:.3},{:.3}", line.a.x, line.a.y, line.b.x, line.b.y),
            EntityKind::Circle(circle) => format!("circle {:.3},{:.3} {:.3}", circle.center.x, circle.center.y, circle.radius),
            other => format!("{other:?}"),
        };
        entities.push(format!("{layer} {body}"));
    }
    entities.sort();
    let mut layers: Vec<_> = doc.layers.iter().map(|layer| layer.name.clone()).collect();
    layers.sort();
    let mut styles: Vec<_> = doc.text_styles.iter().map(|style| format!("{}:{:.3}", style.name, style.height)).collect();
    styles.sort();
    let extents = doc.extents(&cadcraft_doc::Space::Model);
    format!(
        "layers={layers:?}\nstyles={styles:?}\nentities={entities:?}\nextents={:.3},{:.3} {:.3},{:.3}\ncurrent={}/{}",
        extents.min.x,
        extents.min.y,
        extents.max.x,
        extents.max.y,
        doc.header.str("CLAYER", "0"),
        doc.header.str("TEXTSTYLE", "Standard"),
    )
}

fn render(app: &mut CadApp) -> Vec<u8> {
    let ctx = egui::Context::default();
    let (req, _) = ControlRequest::new(
        "ui.render",
        json!({"window": [[-1.0, -1.0], [14.0, 8.0]], "width": 80, "height": 48, "background": "white", "fit": false}),
    );
    let Outcome::Done(value) = crate::control::handle(app, &ctx, &req) else { panic!("render") };
    assert_eq!(value["ok"], true, "{value}");
    cadcraft_engine::cmd::file::base64_decode(value["result"]["pngBase64"].as_str().unwrap_or_default()).unwrap()
}

#[test]
fn native_dialog_cancel_returns_drawing_input() {
    let (_scratch, mut h) = start("(new_dialog \"parts\" id) (setq st (start_dialog))");
    h.get_by_label("Cancel").click();
    h.run_steps(2);
    assert!(!h.state().session.dialog_active());
    assert_eq!(lisp(h.state(), "ST"), "0");
    assert!(h.state_mut().cmdline("LINE 1,1 2,2 ").is_none());
    let lines = h.state().session.doc().unwrap().model.iter().filter(|entity| matches!(entity.kind, EntityKind::Line(_))).count();
    assert_eq!(lines, 1);
}

#[test]
fn native_dialog_commits_the_edit_before_accept_in_source_order() {
    let (_scratch, mut h) = start("(new_dialog \"early_exit\" id) (mode_tile \"part\" 3) (setq st (start_dialog) text (get_tile \"part\"))");
    h.get(by().role(egui::accesskit::Role::TextInput).value("bolt")).type_text("clamp");
    h.run_steps(2);
    h.get(by().role(egui::accesskit::Role::TextInput).value("clamp"));
    h.get_by_label("OK").click();
    h.run_steps(2);
    assert_eq!(lisp(h.state(), "ST"), "1");
    assert_eq!(lisp(h.state(), "TEXT"), "\"clamp\"");
}

#[test]
fn native_dialog_keeps_accept_while_an_edit_callback_yields() {
    let (_scratch, mut h) = start(
        r#"(new_dialog "early_exit" id) (mode_tile "part" 3) (setq calls 0)
               (action_tile "part" "(repeat 200000 (setq i 1)) (setq calls (1+ calls))")
               (setq st (start_dialog) text (get_tile "part"))"#,
    );
    h.get(by().role(egui::accesskit::Role::TextInput).value("bolt")).type_text("clamp");
    h.run_steps(2);
    h.get_by_label("OK").click();
    h.run_steps(16);
    assert_eq!(lisp(h.state(), "CALLS"), "1");
    assert_eq!(lisp(h.state(), "ST"), "1");
    assert_eq!(lisp(h.state(), "TEXT"), "\"clamp\"");
}

#[test]
fn native_dialog_keeps_open_when_the_edit_exceeds_the_total_text_limit() {
    use std::fmt::Write as _;

    use cadcraft_engine::lisp::dcl::{MAX_INPUT_BYTES, MAX_STRING_BYTES};

    let scratch = Scratch::new();
    let count = MAX_INPUT_BYTES / MAX_STRING_BYTES;
    let mut fixture = format!(
        "Filler : spacer {{ value = \"{}\"; }}\n\
         limited : dialog {{ ok_cancel; : edit_box {{ key = \"part\"; label = \"Part\"; value = \"bolt\"; }}\n",
        "x".repeat(MAX_STRING_BYTES)
    );
    for index in 1..count {
        writeln!(fixture, ": Filler {{ key = \"k{index:02}\"; }}").unwrap();
    }
    let field_bytes: usize = ["accept", "OK", "cancel", "Cancel", "part", "Part", "bolt"].iter().map(|text| text.len()).sum();
    // Filler keys are three bytes each.
    let remainder = MAX_INPUT_BYTES - (count - 1) * MAX_STRING_BYTES - count * 3 - field_bytes;
    writeln!(fixture, ": spacer {{ key = \"end\"; value = \"{}\"; }} }}", "x".repeat(remainder)).unwrap();
    std::fs::write(scratch.path.join("parts.dcl"), fixture).unwrap();
    let (_scratch, mut h) =
        start_with(scratch, "(new_dialog \"limited\" id) (mode_tile \"part\" 3) (setq st (start_dialog) text (get_tile \"part\"))");
    h.get(by().role(egui::accesskit::Role::TextInput).value("bolt")).type_text("clamp");
    h.run_steps(2);
    h.get_by_label("OK").click();
    h.run_steps(3);
    assert!(h.state().session.dialog_active());
    assert_eq!(lisp(h.state(), "ST"), "nil");
    h.get(by().role(egui::accesskit::Role::TextInput).value("bolt"));
    assert_eq!(element(&mut h, "The edit exceeded the dialog text limit.", "label")["visible"], true);
    h.get_by_label("Cancel").click();
    h.run_steps(3);
    assert_eq!(lisp(h.state(), "ST"), "0");
    assert_eq!(lisp(h.state(), "TEXT"), "\"bolt\"");
}

#[test]
fn native_dialog_rejects_growth_past_the_byte_limit_without_truncating() {
    use cadcraft_engine::lisp::dcl::MAX_STRING_BYTES;
    use cadcraft_engine::lisp::value::Value;

    for initial in ["é".repeat(MAX_STRING_BYTES / 2), "x".repeat(MAX_STRING_BYTES)] {
        let (_scratch, mut h) = start(&format!(
            "(new_dialog \"early_exit\" id) (set_tile \"part\" \"{initial}\") (mode_tile \"part\" 2) \
             (setq st (start_dialog) text (get_tile \"part\"))"
        ));
        h.key_press(egui::Key::Home);
        h.run_steps(1);
        h.get(by().role(egui::accesskit::Role::TextInput).value(initial.as_str())).type_text("X");
        h.run_steps(2);
        h.get_by_label("OK").click();
        h.run_steps(3);
        assert!(h.state().session.dialog_active(), "an edit above the byte limit must keep the dialog open");
        assert_eq!(lisp(h.state(), "ST"), "nil");
        assert_eq!(element(&mut h, "The edit exceeded the dialog text limit.", "label")["visible"], true);
        h.get_by_label("Cancel").click();
        h.run_steps(3);
        assert_eq!(lisp(h.state(), "ST"), "0");
        assert!(matches!(h.state().session.lisp_var("TEXT"), Value::Str(value) if value.as_ref() == initial.as_str()));
    }
}

#[test]
fn native_dialog_uses_the_last_focus_request_in_lisp_order() {
    let (_scratch, mut h) = start("(new_dialog \"two_fields\" id) (mode_tile \"other\" 3) (mode_tile \"part\" 2) (start_dialog)");
    assert_eq!(element(&mut h, "Part", "textbox")["focused"], true);
    assert_eq!(element(&mut h, "Other", "textbox")["focused"], false);
}

#[test]
fn native_dialog_shows_keyless_and_keyed_text_values() {
    let (_scratch, mut h) = start(
        r#"(new_dialog "messages" id) (setq initial (get_tile "message"))
               (action_tile "apply" "(set_tile \"message\" \"Ready\")")
               (action_tile "clear" "(set_tile \"message\" \"\")")
               (setq status (start_dialog) cleared (get_tile "message"))"#,
    );
    assert_eq!(element(&mut h, "Opening message", "label")["visible"], true);
    assert_eq!(element(&mut h, "Working", "label")["visible"], true);
    assert_eq!(lisp(h.state(), "INITIAL"), "\"\"");
    h.get_by_label("Apply").click();
    h.run_steps(3);
    assert_eq!(element(&mut h, "Ready", "label")["visible"], true);
    h.get_by_label("Clear message").click();
    h.run_steps(3);
    assert_ne!(element(&mut h, "Working", "label")["visible"], true, "clear keeps the message blank");
    assert_ne!(element(&mut h, "Ready", "label")["visible"], true);
    assert_ne!(element(&mut h, "Explicitly blank message", "label")["visible"], true);
    h.get_by_label("Cancel").click();
    h.run_steps(3);
    assert_eq!(lisp(h.state(), "STATUS"), "0");
    assert_eq!(lisp(h.state(), "CLEARED"), "\"\"");
}

#[test]
fn native_dialog_scroll_reaches_both_exit_buttons_after_a_message_grows() {
    for (button, status) in [("OK", "1"), ("Cancel", "0")] {
        let (_scratch, mut h) = start(
            r#"(new_dialog "tall_message" id)
                   (action_tile "grow" "(setq message \"X\") (repeat 99 (setq message (strcat message (chr 10) \"X\"))) (set_tile \"message\" message)")
                   (setq status (start_dialog))"#,
        );
        h.get_by_label("Grow message").click();
        h.run_steps(4);
        assert!(matches!(h.state().session.lisp_var("MESSAGE"), cadcraft_engine::lisp::value::Value::Str(text)
            if text.len() == 199 && text.matches('\n').count() == 99));
        assert_ne!(element(&mut h, button, "button")["visible"], true, "the tall message overflows");

        h.event(egui::Event::PointerMoved(h.ctx.content_rect().center()));
        h.run_steps(1);
        h.event(egui::Event::MouseWheel {
            unit: egui::MouseWheelUnit::Point,
            delta: vec2(0.0, -2400.0),
            phase: egui::TouchPhase::Move,
            source: egui::MouseWheelSource::Unknown,
            modifiers: egui::Modifiers::NONE,
        });
        h.run_steps(12);
        assert_eq!(element(&mut h, button, "button")["visible"], true, "scroll reaches {button}");
        h.get_by_label(button).click();
        h.run_steps(3);
        assert_eq!(lisp(h.state(), "STATUS"), status);
    }
}

#[test]
fn native_dialog_disables_group_descendants_and_keyed_labels() {
    let (_scratch, mut h) = start(
        r#"(new_dialog "grouped" id) (mode_tile "part" 3)
               (action_tile "part" "(mode_tile \"group\" 1) (mode_tile \"message\" 1) (mode_tile \"part\" 1)")
               (action_tile "enable" "(mode_tile \"group\" 0) (mode_tile \"message\" 0)")
               (setq status (start_dialog) result (get_tile "part"))"#,
    );
    let id = h.state().session.dialog_id().unwrap();
    h.get(by().role(egui::accesskit::Role::TextInput).value("bolt")).type_text("clamp");
    h.run_steps(2);
    h.get_by_label("OK").click();
    h.run_steps(3);
    assert_eq!(h.state().session.dialog_id(), Some(id), "the edit callback disables the group before OK");
    for (label, role) in
        [("OK", "button"), ("Part", "textbox"), ("Part", "label"), ("Group", "label"), ("Inside group", "label"), ("Notice", "label")]
    {
        assert_eq!(element(&mut h, label, role)["enabled"], false, "{label}");
    }
    let late = h.state_mut().session.dialog_event(DialogEvent { id, key: "accept", reason: 1, value: None, position: None }).unwrap();
    assert_eq!(late, DialogEventOutcome::Ignored);
    assert_eq!(lisp(h.state(), "STATUS"), "nil");

    h.get_by_label("Enable group").click();
    h.run_steps(3);
    for (label, role) in [("OK", "button"), ("Group", "label"), ("Inside group", "label"), ("Notice", "label")] {
        assert_eq!(element(&mut h, label, role)["enabled"], true, "{label}");
    }
    assert_eq!(element(&mut h, "Part", "textbox")["enabled"], false);
    h.get_by_label("OK").click();
    h.run_steps(3);
    assert_eq!(lisp(h.state(), "STATUS"), "1");
    assert_eq!(lisp(h.state(), "RESULT"), "\"clamp\"");
}

#[test]
fn native_dialog_releases_editor_state_when_a_successor_opens() {
    let (_scratch, mut h) = start(
        r#"(new_dialog "early_exit" id) (mode_tile "part" 3) (start_dialog)
               (new_dialog "early_exit" id) (start_dialog)"#,
    );
    let first = super::control_id(h.state().session.dialog_id().unwrap(), "part");
    h.get(by().role(egui::accesskit::Role::TextInput).value("bolt")).type_text("clamp");
    h.run_steps(2);
    assert!(egui::TextEdit::load_state(&h.ctx, first).is_some());
    h.get_by_label("OK").click();
    h.run_steps(3);
    let second = super::control_id(h.state().session.dialog_id().unwrap(), "part");
    assert_ne!(first, second);
    assert!(egui::TextEdit::load_state(&h.ctx, first).is_none());
    assert!(egui::TextEdit::load_state(&h.ctx, second).is_some());
    h.get_by_label("Cancel").click();
    h.run_steps(3);
    assert!(egui::TextEdit::load_state(&h.ctx, second).is_none());
}

#[test]
fn native_dialog_applies_focus_selection_and_disable() {
    let (_scratch, mut h) = start(
        "(new_dialog \"parts\" id) \
         (mode_tile \"part\" 2) (mode_tile \"part\" 3) (mode_tile \"stock\" 4) (mode_tile \"apply\" 1) \
         (setq st (start_dialog))",
    );
    let part = element(&mut h, "Part", "textbox");
    assert_eq!(part["focused"], true, "{part}");
    let id = h.state().session.dialog_id().unwrap();
    let state = egui::TextEdit::load_state(&h.ctx, super::control_id(id, "part")).expect("edit state");
    let range = state.cursor.char_range().expect("selection");
    let ends = [range.primary.index.0, range.secondary.index.0];
    assert!(ends.contains(&0) && ends.contains(&4), "bolt is selected: {ends:?}");
    let stock = element(&mut h, "In stock", "checkbox");
    assert_eq!(stock["selected"], true, "{stock}");
    let apply = element(&mut h, "Apply", "button");
    assert_eq!(apply["enabled"], false, "{apply}");
    h.get_by_label("Apply").click();
    h.run_steps(2);
    assert!(h.state().session.dialog_active(), "a disabled button sends no action");
    assert_eq!(lisp(h.state(), "ST"), "nil");
    h.get_by_label("Cancel").click();
    h.run_steps(2);
    assert_eq!(lisp(h.state(), "ST"), "0");
}

#[test]
fn native_dialog_closes_from_an_action_and_ignores_a_late_event() {
    let (scratch, mut h) = start(
        "(new_dialog \"parts\" id) \
         (action_tile \"apply\" \"(setq saw $key) (done_dialog 4) (unload_dialog id)\") \
         (setq st (start_dialog))",
    );
    let first = h.state().session.dialog_id().unwrap();
    h.get_by_label("Apply").click();
    h.run_steps(3);
    assert!(!h.state().session.dialog_active());
    assert_eq!(lisp(h.state(), "ST"), "4");
    assert_eq!(lisp(h.state(), "SAW"), "\"apply\"");
    assert_eq!(lisp(h.state(), "PART"), "nil");
    let again = format!("(progn (setq id (load_dialog \"{}\")) (new_dialog \"parts\" id) (setq st (start_dialog)))", scratch.dcl());
    assert!(h.state_mut().cmdline(&again).is_none());
    h.run_steps(2);
    let second = h.state().session.dialog_id().unwrap();
    assert_ne!(first, second);
    h.state_mut().session.dialog_event(DialogEvent { id: first, key: "part", reason: 1, value: Some("stale"), position: None }).unwrap();
    h.run_steps(1);
    h.get(by().role(egui::accesskit::Role::TextInput).value("bolt"));
    assert!(h.state().session.dialog_active());
}

#[test]
fn native_dialog_new_tab_closes_and_keeps_the_original_drawing() {
    let scratch = Scratch::new();
    let mut app = CadApp::new(Session::new(), Services::default());
    app.session.execute("line", &json!({"points": [[0.0, 0.0], [8.0, 0.0]]})).unwrap();
    let code = format!("(progn (setq id (load_dialog \"{}\")) (new_dialog \"parts\" id) (start_dialog))", scratch.dcl());
    assert!(app.cmdline(&code).is_none());
    let mut h = harness(app);
    h.get_by_label("New Drawing").click();
    h.run_steps(3);
    assert!(!h.state().session.dialog_active());
    let kept = h.state().session.docs.iter().filter(|doc| doc.doc.model.iter().any(|entity| matches!(entity.kind, EntityKind::Line(_)))).count();
    assert_eq!(kept, 1, "the original drawing keeps its line");
    assert!(h.state_mut().cmdline("(+ 1 2)").is_none());
    assert!(h.state().session.log.iter().any(|line| line.trim() == "3"), "the blank drawing runs Lisp: {:?}", h.state().session.log);
}

const LISTS: &str = r#"catalog : list_box {
  list = "Small\nMedium\nLarge";
  value = "1";
}
wood : popup_list {
  list = "Oak\nPine";
}
choices : dialog {
  label = "Choices";
  : catalog { key = "sizes"; }
  : wood { key = "finish"; }
  : catalog { key = "local"; list = "Red\n\nBlue"; value = "0"; }
  : list_box { key = "many"; multiple_select = true; list = "One\nTwo\nThree"; }
  : popup_list { key = "plain"; }
  : list_box { key = "locked"; list = "Keep\nDrop"; value = "0"; }
  : button { key = "apply"; label = "Apply"; }
  ok_cancel;
}
"#;

fn tile_value(h: &Harness<'static, CadApp>, key: &str) -> String {
    fn walk(controls: &[DialogControl], key: &str) -> Option<String> {
        for control in controls {
            if control.key.as_deref() == Some(key) {
                return Some(control.value.to_string());
            }
            if let Some(found) = walk(&control.children, key) {
                return Some(found);
            }
        }
        None
    }
    let view = h.state().session.dialog_view().expect("dialog is showing");
    walk(&view.controls, key).unwrap_or_else(|| panic!("missing tile {key}"))
}

fn tile_rows(h: &Harness<'static, CadApp>, key: &str) -> Vec<String> {
    fn walk(controls: &[DialogControl], key: &str) -> Option<Vec<String>> {
        for control in controls {
            if control.key.as_deref() == Some(key) {
                return Some(control.rows.iter().map(|row| row.to_string()).collect());
            }
            if let Some(found) = walk(&control.children, key) {
                return Some(found);
            }
        }
        None
    }
    let view = h.state().session.dialog_view().expect("dialog is showing");
    walk(&view.controls, key).unwrap_or_else(|| panic!("missing tile {key}"))
}

fn open_lists(source: &str, extra: &str) -> (Scratch, Harness<'static, CadApp>) {
    open_lists_paced(source, extra, 0.25)
}

/// A short frame keeps two clicks inside the double-click interval. Each click is several frames.
fn open_lists_paced(source: &str, extra: &str, step_dt: f32) -> (Scratch, Harness<'static, CadApp>) {
    let scratch = Scratch::new();
    std::fs::write(scratch.path.join("parts.dcl"), source).unwrap();
    let mut app = CadApp::new(Session::new(), Services::default());
    let code = format!("(progn (setq id (load_dialog \"{}\")) {extra})", scratch.dcl());
    let err = app.cmdline(&code);
    assert!(err.is_none(), "{err:?}");
    assert!(app.session.dialog_active(), "{code}\n{:?}", app.session.log);
    (scratch, harness_with_dt(app, step_dt))
}

#[test]
fn native_dialog_list_double_click_commits_when_its_row_survives() {
    let source = r#"choices : dialog {
            : list_box { key = "rows"; list = "Panel\nBeam\nBrace\nPost\nRail\nCap\nStud\nJoist"; }
            ok_cancel;
        }"#;
    let (_scratch, mut h) = open_lists_paced(
        source,
        r#"(new_dialog "choices" id)
               (action_tile "rows" "(setq reasons (cons $reason reasons))
                 (if (= $reason 1)
                   (progn (start_list \"rows\" 2) (add_list \"Extra\") (end_list))
                   (done_dialog 7))")
               (setq status (start_dialog)) (setq choice (get_tile "rows"))"#,
        0.05,
    );
    h.get_by_label("Panel").click();
    h.get_by_label("Panel").click();
    h.run_steps(5);
    assert_eq!(lisp(h.state(), "STATUS"), "7");
    assert_eq!(lisp(h.state(), "CHOICE"), "\"0\"");
    let reasons = lisp(h.state(), "REASONS");
    assert!(reasons.starts_with("(4 1"), "{reasons}");
}

#[test]
fn native_dialog_list_double_click_drops_a_replaced_row() {
    let source = r#"choices : dialog {
            : list_box { key = "rows"; list = "Panel\nBeam"; }
            ok_cancel;
        }"#;
    let (_scratch, mut h) = open_lists_paced(
        source,
        r#"(new_dialog "choices" id)
               (action_tile "rows" "(setq reasons (cons $reason reasons))
                 (if (= $reason 1)
                   (progn (start_list \"rows\" 1 0) (add_list \"Panel\") (end_list))
                   (done_dialog 7))")
               (setq status (start_dialog))"#,
        0.05,
    );
    h.get_by_label("Panel").click();
    h.get_by_label("Panel").click();
    h.run_steps(5);
    let reasons = lisp(h.state(), "REASONS");
    assert!(reasons.starts_with("(1") && !reasons.contains('4'), "{reasons}");
    assert!(h.state().session.dialog_active());
    h.get_by_label("Cancel").click();
    h.run_steps(2);
    assert_eq!(lisp(h.state(), "STATUS"), "0");
}

#[test]
fn native_dialog_list_double_click_cannot_cross_a_first_click_replacement() {
    let source = r#"choices : dialog {
            : list_box { key = "rows"; list = "Panel\nBeam"; }
            ok_cancel;
        }"#;
    let (_scratch, mut h) = open_lists_paced(
        source,
        r#"(new_dialog "choices" id)
               (setq changed nil)
               (action_tile "rows" "(setq reasons (cons $reason reasons))
                 (if (= $reason 1)
                   (if (not changed)
                     (progn (setq changed T) (start_list \"rows\" 1 0) (add_list \"Panel\") (end_list)))
                   (done_dialog 7))")
               (setq status (start_dialog))"#,
        0.05,
    );
    h.get_by_label("Panel").click();
    h.run_steps(1);
    assert_eq!(lisp(h.state(), "CHANGED"), "T");
    assert_eq!(lisp(h.state(), "REASONS"), "(1)");
    h.get_by_label("Panel").click();
    h.run_steps(2);
    assert_eq!(lisp(h.state(), "REASONS"), "(1 1)");
    assert_eq!(tile_value(&h, "rows"), "0");
    assert!(h.state().session.dialog_active());
    h.get_by_label("Cancel").click();
    h.run_steps(2);
    assert_eq!(lisp(h.state(), "STATUS"), "0");
}

#[test]
fn native_dialog_list_double_click_cannot_pair_with_a_batched_right_click() {
    let source = r#"choices : dialog {
            : list_box { key = "rows"; list = "Panel\nBeam"; }
            ok_cancel;
        }"#;
    let (_scratch, mut h) = open_lists_paced(
        source,
        r#"(new_dialog "choices" id)
               (action_tile "rows" "(setq reasons (cons $reason reasons))
                 (if (= $reason 4) (done_dialog 7))")
               (setq status (start_dialog))"#,
        0.05,
    );
    h.get_by_label("Panel").click();
    h.run_steps(10);
    assert_eq!(lisp(h.state(), "REASONS"), "(1)");
    let position = h.get_by_label("Panel").rect().center();
    // Feed both releases in one frame. Node::click delivers each event in its own frame.
    for button in [egui::PointerButton::Secondary, egui::PointerButton::Primary] {
        for pressed in [true, false] {
            h.input_mut().events.push(egui::Event::PointerButton { pos: position, button, pressed, modifiers: egui::Modifiers::NONE });
        }
    }
    h.step();
    h.run_steps(2);
    assert_eq!(lisp(h.state(), "REASONS"), "(1 1)");
    assert!(h.state().session.dialog_active());
    h.get_by_label("Cancel").click();
    h.run_steps(2);
    assert_eq!(lisp(h.state(), "STATUS"), "0");
}

fn scroll_past_multiline_row(kind: &str, role: &str) {
    let source = format!(
        r#"choices : dialog {{
            : {kind} {{ key = "rows"; value = "1"; }}
            ok_cancel;
        }}"#
    );
    let (_scratch, mut h) = open_lists(
        &source,
        r#"(new_dialog "choices" id)
               (start_list "rows")
               (add_list "Tall\nSecond line\nThird line\nFourth line\nFifth line")
               (setq n 0)
               (repeat 24 (setq n (1+ n)) (add_list (strcat "Row " (itoa n))))
               (end_list)
               (action_tile "rows" "(setq picked $value)")
               (setq status (start_dialog))"#,
    );
    if kind == "popup_list" {
        h.get(by().role(egui::accesskit::Role::ComboBox).label("rows").value("Row 1")).click();
        h.run_steps(3);
    }
    let multiline = "Tall\nSecond line\nThird line\nFourth line\nFifth line";
    let tall = element(&mut h, multiline, role);
    let short = element(&mut h, "Row 1", role);
    let tall_height = tall["rect"][3].as_f64().expect("multiline row");
    let short_height = short["rect"][3].as_f64().expect("single line row");
    assert!((tall_height - short_height).abs() < 0.5, "row heights {tall_height} and {short_height}");
    let x = short["rect"][0].as_f64().unwrap() as f32 + 8.0;
    let y = short["rect"][1].as_f64().unwrap() as f32 + 8.0;
    h.hover_at(egui::pos2(x, y));
    // The scroll area must see the pointer over it before the wheel arrives.
    h.run_steps(1);
    h.event(egui::Event::MouseWheel {
        unit: egui::MouseWheelUnit::Point,
        delta: egui::vec2(0.0, -1000.0),
        phase: egui::TouchPhase::Move,
        source: egui::MouseWheelSource::Unknown,
        modifiers: egui::Modifiers::default(),
    });
    h.run_steps(6);
    assert_ne!(element(&mut h, multiline, role)["visible"], true);
    h.get_by_label("Row 24").click();
    h.run_steps(3);
    assert_eq!(lisp(h.state(), "PICKED"), "\"24\"");
    assert_eq!(tile_value(&h, "rows"), "24");
    assert_eq!(tile_rows(&h, "rows")[0], multiline);
    if kind == "popup_list" {
        h.get(by().role(egui::accesskit::Role::ComboBox).label("rows").value("Row 24"));
    }
}

#[test]
fn native_dialog_list_scrolls_past_a_multiline_row() {
    scroll_past_multiline_row("list_box", "listbox");
}

#[test]
fn native_dialog_popup_scrolls_past_a_multiline_row() {
    scroll_past_multiline_row("popup_list", "option");
}

#[test]
fn native_dialog_escape_closes_the_dropdown_before_the_form() {
    let (_scratch, mut h) = open_lists(LISTS, r#"(new_dialog "choices" id) (start_dialog)"#);
    h.get(by().role(egui::accesskit::Role::ComboBox).label("finish").value("Oak")).click();
    h.run_steps(3);
    assert_eq!(element(&mut h, "Pine", "option")["visible"], true);
    h.key_press(egui::Key::Escape);
    h.run_steps(3);
    assert!(h.state().session.dialog_active());
    assert_ne!(element(&mut h, "Pine", "option")["visible"], true);
    assert_eq!(tile_value(&h, "finish"), "0");
    h.key_press(egui::Key::Escape);
    h.run_steps(3);
    assert!(!h.state().session.dialog_active());
}

#[test]
fn native_dialog_focuses_the_dropdown_and_the_selected_list_row() {
    let (_scratch, mut h) = open_lists(
        LISTS,
        r#"(new_dialog "choices" id)
           (start_list "sizes")
           (setq n 0)
           (repeat 25 (setq n (1+ n)) (add_list (strcat "Row " (itoa n))))
           (end_list)
           (set_tile "sizes" "24")
           (mode_tile "finish" 2)
           (action_tile "apply" "(progn (setq requested_sizes T) (mode_tile \"sizes\" 2))")
           (start_dialog)"#,
    );
    assert!(h.get(by().role(egui::accesskit::Role::ComboBox).label("finish")).is_focused());
    assert_eq!(tile_value(&h, "sizes"), "24");
    assert_ne!(element(&mut h, "Row 25", "listbox")["visible"], true);
    let session = &mut h.state_mut().session;
    let id = session.dialog_id().unwrap();
    // Use the adapter's tile event to test the callback and focus request independently of
    // the Apply button's position in the larger list fixture.
    session.dialog_event(super::DialogEvent { id, key: "apply", reason: 1, value: None, position: None }).unwrap();
    h.run_steps(4);
    assert_eq!(lisp(h.state(), "REQUESTED_SIZES"), "T");
    assert!(h.get_by_label("Row 25").is_focused());
}

#[test]
fn native_dialog_selected_rows_have_readable_text() {
    use egui::{Color32, Rgba, Theme};

    fn luminance(color: Color32) -> f32 {
        let color = Rgba::from(color);
        0.2126 * color.r() + 0.7152 * color.g() + 0.0722 * color.b()
    }

    let ctx = egui::Context::default();
    for theme in [Theme::Dark, Theme::Light] {
        crate::theme::apply(&ctx, theme, false);
        let mut output = ctx.run_ui(Default::default(), |ui| {
            let height = super::list_row_height(ui);
            super::draw_list_row(ui, true, "Selected row", height);
        });
        output.textures_delta.clear();
        let foreground = output
            .shapes
            .iter()
            .find_map(|clipped| match &clipped.shape {
                Shape::Text(text) if text.galley.text() == "Selected row" => {
                    let color = text.galley.job.sections.first()?.format.color;
                    Some(text.override_text_color.unwrap_or(if color == Color32::PLACEHOLDER { text.fallback_color } else { color }))
                }
                _ => None,
            })
            .expect("the row paints its label");
        let foreground = luminance(foreground);
        let background = luminance(ctx.style_of(theme).visuals.selection.bg_fill);
        assert!((foreground.max(background) + 0.05) / (foreground.min(background) + 0.05) >= 4.5, "{theme:?}");
    }
    crate::theme::set_active(Theme::Dark);
}

/// Rows stay in order, a click and a popup agree with the tile, and OK draws from the choice.
///
/// The steps stay in one function so the same dialog run covers selection, refresh, and exit.
#[allow(clippy::cognitive_complexity)]
#[test]
fn native_dialog_list_shows_rows_selects_and_accepts() {
    let (scratch, mut h) = open_lists_paced(
        LISTS,
        r#"(new_dialog "choices" id)
             (setq sizes (get_tile "sizes"))
             (setq finish (get_tile "finish"))
             (setq localv (get_tile "local"))
             (setq manyv (get_tile "many"))
             (setq plain (get_tile "plain"))
             (start_list "local" 1 1)
             (setq first (add_list "Mid"))
             (setq second (add_list "No"))
             (end_list)
             (start_list "sizes" 2) (add_list "X") (add_list "Y") (end_list)
             (start_list "plain") (add_list "First") (add_list "Second") (end_list)
             (mode_tile "locked" 1)
             (action_tile "sizes" "(setq reasons (cons $reason reasons)) (setq saw (strcat $key \"=\" $value))")
             (action_tile "finish" "(setq finish_saw (strcat $key \"=\" $value \"/\" (itoa $reason)))")
             (action_tile "many" "(setq many_reasons (cons $reason many_reasons)) (setq many_saw (strcat $key \"=\" $value))")
             (action_tile "locked" "(setq locked_saw T)")
             (action_tile "apply" "(progn (start_list \"sizes\" 2) (add_list \"Fresh\") (end_list))")
             (setq st (start_dialog))
             (setq chosen (get_tile "sizes"))
             (if (= st 1) (progn
                (command "-LAYER" "New" (strcat "L" chosen) "Set" (strcat "L" chosen) "")
                (command "_.LINE" "0,0" "12,0" "")))"#,
        0.05,
    );
    assert_eq!(lisp(h.state(), "SIZES"), "\"1\"");
    assert_eq!(lisp(h.state(), "FINISH"), "\"0\"");
    assert_eq!(lisp(h.state(), "LOCALV"), "\"0\"");
    assert_eq!(lisp(h.state(), "MANYV"), "\"\"");
    assert_eq!(lisp(h.state(), "PLAIN"), "\"0\"");
    assert_eq!(lisp(h.state(), "FIRST"), "\"Mid\"");
    assert_eq!(lisp(h.state(), "SECOND"), "\"No\"");
    let texts = painted(&h);
    assert!(texts.iter().all(|(text, _)| text != "Mid"));
    let (small, medium, large) = (at(&texts, "Small"), at(&texts, "Medium"), at(&texts, "Large"));
    assert!(small.bottom() <= medium.top() + 2.0 && medium.bottom() <= large.top() + 2.0, "list rows stay in order");
    let same_column = |rect: &egui::Rect, above: egui::Rect| (rect.left() - above.left()).abs() < 40.0 && rect.top() >= above.bottom() - 2.0;
    let extra = texts.iter().find(|(text, rect)| text == "X" && same_column(rect, large)).unwrap_or_else(|| panic!("missing X below Large")).1;
    let last = texts.iter().find(|(text, rect)| text == "Y" && same_column(rect, extra)).unwrap_or_else(|| panic!("missing Y below X")).1;
    assert!(large.bottom() <= extra.top() + 2.0 && extra.bottom() <= last.top() + 2.0, "appended rows follow the original rows");
    let (red, changed, blue) = (at(&texts, "Red"), at(&texts, "No"), at(&texts, "Blue"));
    assert!(red.bottom() <= changed.top() + 2.0 && changed.bottom() <= blue.top() + 2.0, "a repeated change keeps the same row");
    at(&texts, "Oak");
    at(&texts, "First");
    at(&texts, "One");
    at(&texts, "Keep");
    assert_eq!(tile_value(&h, "sizes"), "1");
    assert_eq!(tile_value(&h, "finish"), "0");
    assert_eq!(tile_value(&h, "plain"), "0");
    assert_eq!(tile_value(&h, "many"), "");

    h.get_by_label("Large").click();
    h.run_steps(8);
    assert_eq!(lisp(h.state(), "SAW"), "\"sizes=2\"");
    assert_eq!(tile_value(&h, "sizes"), "2");
    h.get_by_label("Large").click();
    h.get_by_label("Large").click();
    h.run_steps(4);
    let reasons = lisp(h.state(), "REASONS");
    assert!(reasons.starts_with("(4 "), "a double-click follows the selection: {reasons}");
    assert_eq!(tile_value(&h, "sizes"), "2");

    h.get(by().role(egui::accesskit::Role::ComboBox).label("finish").value("Oak")).click();
    h.run_steps(2);
    h.get_by_label("Pine").click();
    h.run_steps(2);
    assert_eq!(lisp(h.state(), "FINISH_SAW"), "\"finish=1/1\"");
    assert_eq!(tile_value(&h, "finish"), "1");
    h.get(by().role(egui::accesskit::Role::ComboBox).label("finish").value("Pine"));

    h.get_by_label("One").click();
    h.run_steps(2);
    h.get_by_label("Three").click_modifiers(egui::Modifiers::COMMAND);
    h.run_steps(2);
    assert_eq!(lisp(h.state(), "MANY_SAW"), "\"many=0 2\"");
    assert_eq!(tile_value(&h, "many"), "0 2");
    h.get_by_label("One").click();
    h.run_steps(1);
    h.get_by_label("One").click();
    h.run_steps(2);
    let many_reasons = lisp(h.state(), "MANY_REASONS");
    assert!(!many_reasons.contains('4'), "a multiple list keeps reason 1: {many_reasons}");
    h.get_by_label("Keep").click();
    h.run_steps(2);
    assert_eq!(lisp(h.state(), "LOCKED_SAW"), "nil");
    assert_eq!(tile_value(&h, "locked"), "0");

    h.hover_at(egui::pos2(500.0, 400.0));
    h.event(egui::Event::MouseWheel {
        unit: egui::MouseWheelUnit::Point,
        delta: egui::vec2(0.0, -320.0),
        phase: egui::TouchPhase::Move,
        source: egui::MouseWheelSource::Unknown,
        modifiers: egui::Modifiers::default(),
    });
    h.run_steps(3);
    h.get_by_label("Apply").click();
    h.run_steps(2);
    assert_eq!(tile_rows(&h, "sizes").last().map(String::as_str), Some("Fresh"));
    h.hover_at(egui::pos2(500.0, 400.0));
    h.event(egui::Event::MouseWheel {
        unit: egui::MouseWheelUnit::Point,
        delta: egui::vec2(0.0, -160.0),
        phase: egui::TouchPhase::Move,
        source: egui::MouseWheelSource::Unknown,
        modifiers: egui::Modifiers::default(),
    });
    h.run_steps(2);
    h.get_by_label("OK").click();
    h.run_steps(3);
    assert!(!h.state().session.dialog_active());
    assert_eq!(lisp(h.state(), "ST"), "1");
    assert_eq!(lisp(h.state(), "CHOSEN"), "\"2\"");
    let lines = h.state().session.doc().unwrap().model.iter().filter(|entity| matches!(entity.kind, EntityKind::Line(_))).count();
    assert_eq!(lines, 1);
    assert!(h.state().session.doc().unwrap().layers.iter().any(|layer| layer.name == "L2"));
    let path = scratch.path.join("choices.dxf");
    let bytes = cadcraft_io::write(h.state().session.doc().unwrap(), "choices.dxf").unwrap();
    std::fs::write(&path, &bytes).unwrap();
    let opened = cadcraft_io::read(&std::fs::read(&path).unwrap(), "choices.dxf").unwrap();
    assert!(opened.layers.iter().any(|layer| layer.name == "L2"));
    assert_eq!(opened.model.iter().filter(|entity| matches!(entity.kind, EntityKind::Line(_))).count(), 1);
}

#[test]
fn native_dialog_list_drops_choice_when_edit_rebuilds_rows() {
    let source = r#"changes : dialog {
            : edit_box { key = "edit"; label = "Filter"; value = "A"; }
            : list_box { key = "rows"; list = "Original\nSecond"; }
            ok_cancel;
        }"#;
    let (_scratch, mut h) = open_lists(
        source,
        r#"(new_dialog "changes" id)
               (mode_tile "edit" 3)
               (action_tile "edit" "(start_list \"rows\") (add_list \"Replacement\") (end_list)")
               (action_tile "rows" "(setq picked $value)")
               (setq status (start_dialog))"#,
    );
    assert_eq!(element(&mut h, "Filter", "textbox")["focused"], true);
    h.event(egui::Event::Text("B".into()));
    h.run_steps(2);
    h.get_by_label("Original").click();
    h.run_steps(3);
    assert_eq!(tile_rows(&h, "rows"), ["Replacement"]);
    assert_eq!(tile_value(&h, "rows"), "");
    assert_eq!(lisp(h.state(), "PICKED"), "nil");
    h.get_by_label("Cancel").click();
    h.run_steps(2);
    assert_eq!(lisp(h.state(), "STATUS"), "0");
}

/// Cancel closes the dialog and does not build the drawing.
#[test]
fn native_dialog_list_cancel_keeps_the_drawing() {
    let (_scratch, mut h) = open_lists(
        LISTS,
        r#"(new_dialog "choices" id)
             (start_list "sizes") (add_list "Small") (add_list "Large") (end_list)
             (set_tile "sizes" "1")
             (setq st (start_dialog))
             (if (= st 1) (command "_.LINE" "0,0" "4,0" ""))"#,
    );
    h.get_by_label("Large").click();
    h.run_steps(2);
    assert_eq!(tile_value(&h, "sizes"), "1");
    h.get_by_label("Cancel").click();
    h.run_steps(2);
    assert!(!h.state().session.dialog_active());
    assert_eq!(lisp(h.state(), "ST"), "0");
    let lines = h.state().session.doc().unwrap().model.iter().filter(|entity| matches!(entity.kind, EntityKind::Line(_))).count();
    assert_eq!(lines, 0);
}

/// A long list leaves the exit button on screen and the first row selectable.
#[test]
fn native_dialog_list_stays_usable_when_the_list_is_long() {
    let mut rows = String::new();
    for index in 0..80 {
        if index > 0 {
            rows.push('\n');
        }
        rows.push_str(&format!("n{index:03}"));
    }
    let source = format!("long : dialog {{\n  label = \"Long\";\n  : list_box {{ key = \"rows\"; list = \"{rows}\"; }}\n  ok_cancel;\n}}\n");
    let (_scratch, mut h) = open_lists(
        &source,
        r#"(new_dialog "long" id) (setq seen (get_tile "rows")) (action_tile "rows" "(setq saw (strcat $key \"=\" $value))") (setq st (start_dialog))"#,
    );
    assert_eq!(lisp(h.state(), "SEEN"), "\"\"");
    let texts = painted(&h);
    at(&texts, "n000");
    at(&texts, "OK");
    assert!(texts.iter().all(|(text, _)| text != "n079"), "the far row stays in the list scroller");
    h.get_by_label("n000").click();
    h.run_steps(2);
    assert_eq!(lisp(h.state(), "SAW"), "\"rows=0\"");
    assert_eq!(tile_value(&h, "rows"), "0");
    h.get_by_label("Cancel").click();
    h.run_steps(2);
    assert_eq!(lisp(h.state(), "ST"), "0");
}

fn quote_lisp(text: &str) -> String {
    let mut out = String::from("\"");
    for c in text.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            _ => out.push(c),
        }
    }
    out.push('"');
    out
}

fn slide_word(out: &mut Vec<u8>, value: u16) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn ui_slide(color: u8, x1: u16, y1: u16, x2: u16, y2: u16) -> Vec<u8> {
    let mut out = b"AutoCAD Slide\r\n\x1a\x00".to_vec();
    out.extend_from_slice(&[0x56, 2]);
    slide_word(&mut out, 15);
    slide_word(&mut out, 15);
    out.extend_from_slice(&1_000_000u32.to_le_bytes());
    slide_word(&mut out, 0);
    slide_word(&mut out, 0x1234);
    slide_word(&mut out, 0xff00 | u16::from(color));
    slide_word(&mut out, x1);
    slide_word(&mut out, y1);
    slide_word(&mut out, x2);
    slide_word(&mut out, y2);
    slide_word(&mut out, 0xfc00);
    out
}

fn image_line(h: &Harness<'static, CadApp>) -> (u8, i32, i32, i32, i32) {
    fn find(controls: &[DialogControl]) -> Option<(u8, i32, i32, i32, i32)> {
        for control in controls {
            if control.key.as_deref() == Some("img") {
                let picture = control.image.as_ref()?.picture.as_ref()?;
                return picture.primitives.iter().find_map(|primitive| match primitive {
                    DialogPrimitive::Vector(line) => Some((line.color, line.x1, line.y1, line.x2, line.y2)),
                    DialogPrimitive::Solid(_) => None,
                });
            }
            if let Some(found) = find(&control.children) {
                return Some(found);
            }
        }
        None
    }
    find(&h.state().session.dialog_view().expect("dialog").controls).expect("image line")
}

fn painted_segments(h: &Harness<'static, CadApp>) -> Vec<([egui::Pos2; 2], egui::Color32)> {
    fn walk(shape: &Shape, out: &mut Vec<([egui::Pos2; 2], egui::Color32)>) {
        match shape {
            Shape::LineSegment { points, stroke } => out.push((*points, stroke.color)),
            Shape::Vec(shapes) => shapes.iter().for_each(|shape| walk(shape, out)),
            _ => {}
        }
    }
    let mut out = Vec::new();
    for shape in &h.output().shapes {
        walk(&shape.shape, &mut out);
    }
    out
}

fn line_count(app: &CadApp) -> usize {
    app.session.doc().expect("drawing").model.iter().filter(|entity| matches!(entity.kind, EntityKind::Line(_))).count()
}

#[test]
fn native_dialog_image_uses_canvas_background() {
    use cadcraft_engine::lisp::{DialogImage, DialogPicture, DialogSolid, DialogVector};
    use egui::{Color32, Theme};

    use crate::theme::{self, Canvas, Tokens};

    let control = DialogControl {
        kind: "image".into(),
        key: None,
        label: "".into(),
        value: "".into(),
        rows: Default::default(),
        multiple: false,
        enabled: true,
        row: false,
        children: Vec::new(),
        image: Some(DialogImage {
            dim_x: 15,
            dim_y: 15,
            picture: Some(
                DialogPicture {
                    primitives: vec![
                        DialogPrimitive::Vector(DialogVector { x1: 1, y1: 2, x2: 12, y2: 2, color: 255 }),
                        DialogPrimitive::Vector(DialogVector { x1: 1, y1: 4, x2: 12, y2: 4, color: 7 }),
                        DialogPrimitive::Solid(DialogSolid { triangles: vec![(2, 6), (8, 6), (8, 12)], color: 255 }),
                    ],
                }
                .into(),
            ),
        }),
    };
    let light_canvas = Color32::from_gray(0xe0);
    let ctx = egui::Context::default();
    for (appearance, model, background, contrast) in [
        (Theme::Dark, None, Tokens::DARK.canvas, Color32::WHITE),
        (Theme::Light, None, Tokens::LIGHT.canvas, Color32::WHITE),
        (Theme::Light, Some(light_canvas), light_canvas, Color32::BLACK),
    ] {
        theme::apply(&ctx, appearance, false);
        theme::set_canvas(Canvas { model, ..Default::default() });
        let mut output = ctx.run_ui(Default::default(), |ui| super::draw_image(ui, &control));
        output.textures_delta.clear();
        assert!(
            output
                .shapes
                .iter()
                .any(|clipped| matches!(&clipped.shape, Shape::Rect(rect) if rect.fill == background && rect.rect.size() == vec2(16.0, 16.0))),
            "image tile uses the canvas background {background:?} in {appearance:?}"
        );
        let lines: Vec<_> = output
            .shapes
            .iter()
            .filter_map(|clipped| match &clipped.shape {
                Shape::LineSegment { stroke, .. } => Some(stroke.color),
                _ => None,
            })
            .collect();
        assert_eq!(lines, [Color32::WHITE, contrast]);
        assert!(output.shapes.iter().any(|clipped| matches!(
            &clipped.shape, Shape::Mesh(mesh) if mesh.vertices.len() == 3 && mesh.vertices.iter().all(|vertex| vertex.color == Color32::WHITE)
        )));
    }
    theme::set_canvas(Canvas::default());
    theme::set_active(Theme::Dark);
}

/// The tile shows the placed slide, a button can replace it, and the two exits disagree.
#[test]
fn native_dialog_draws_a_slide_refreshes_it_and_uses_both_exits() {
    let path = std::env::temp_dir().join(format!(
        "cadcraft-dcl-image-ui-{}-{}",
        std::process::id(),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
    ));
    let _ = std::fs::remove_dir_all(&path);
    std::fs::create_dir_all(&path).unwrap();
    std::fs::write(
        path.join("show.dcl"),
        "show : dialog {\n  label = \"Slides\";\n  : image { key = \"img\"; label = \"Slide preview\"; width = 2; height = 1; }\n  : button { key = \"show\"; label = \"Show\"; }\n  ok_cancel;\n}\n",
    )
    .unwrap();
    let red = path.join("red.sld");
    let green = path.join("green.sld");
    std::fs::write(&red, ui_slide(1, 0, 0, 15, 15)).unwrap();
    std::fs::write(&green, ui_slide(3, 0, 15, 15, 0)).unwrap();
    let action = format!("(start_image \"img\") (slide_image 0 0 15 15 {}) (end_image)", quote_lisp(&green.display().to_string()));
    let code = format!(
        "(progn (setq id (load_dialog {})) (new_dialog \"show\" id) (start_image \"img\") (slide_image 0 0 15 15 {}) (end_image) (action_tile \"show\" {}) (setq st (start_dialog)) (if (= st 1) (command \"_.LINE\" \"0,0\" \"4,0\" \"\")))",
        quote_lisp(&path.join("show.dcl").display().to_string()),
        quote_lisp(&red.display().to_string()),
        quote_lisp(&action)
    );
    let mut app = CadApp::new(Session::new(), Services::default());
    let err = app.cmdline(&code);
    assert!(err.is_none(), "{err:?} {:?}", app.session.log);
    let mut h = harness(app);
    assert_eq!(image_line(&h), (1, 0, 15, 15, 0));
    h.get(by().role(egui::accesskit::Role::Image).label("Slide preview"));
    let widget = element(&mut h, "img", "image");
    let left = widget["rect"][0].as_f64().expect("left") as f32;
    let top = widget["rect"][1].as_f64().expect("top") as f32;
    let width = widget["rect"][2].as_f64().expect("width");
    let height = widget["rect"][3].as_f64().expect("height");
    assert!((width - 16.0).abs() < 0.5 && (height - 16.0).abs() < 0.5, "tile is 16 by 16, got {width} by {height}");
    let tile = egui::Rect::from_min_size(egui::pos2(left, top), egui::vec2(width as f32, height as f32));
    let red_color = egui::Color32::from_rgb(255, 0, 0);
    let red_line = painted_segments(&h).into_iter().find(|(_, color)| *color == red_color).expect("red segment");
    assert!(tile.contains(red_line.0[0]) && tile.contains(red_line.0[1]), "the slide is clipped to the tile: {:?} vs {tile:?}", red_line.0);
    let near = |point: egui::Pos2, x: f32, y: f32| (point.x - (left + x)).abs() < 1.0 && (point.y - (top + y)).abs() < 1.0;
    assert!(near(red_line.0[0], 0.0, 15.0) && near(red_line.0[1], 15.0, 0.0), "placed segment {:?}", red_line.0);

    h.get_by_label("Show").click();
    h.run_steps(4);
    assert_eq!(image_line(&h), (3, 0, 0, 15, 15));
    let green_color = egui::Color32::from_rgb(0, 255, 0);
    let green_line = painted_segments(&h).into_iter().find(|(_, color)| *color == green_color).expect("green segment");
    assert!(tile.contains(green_line.0[0]) && tile.contains(green_line.0[1]));

    h.get_by_label("OK").click();
    h.run_steps(4);
    assert_eq!(lisp(h.state(), "ST"), "1");
    assert_eq!(line_count(h.state()), 1);

    let cancel = "(progn (new_dialog \"show\" id) (setq st (start_dialog)) (if (= st 1) (command \"_.LINE\" \"0,0\" \"8,0\" \"\")))";
    assert!(h.state_mut().cmdline(cancel).is_none(), "{:?}", h.state().session.log);
    h.run_steps(4);
    h.get_by_label("Cancel").click();
    h.run_steps(4);
    assert_eq!(lisp(h.state(), "ST"), "0");
    assert_eq!(line_count(h.state()), 1);
    let _ = std::fs::remove_dir_all(&path);
}
