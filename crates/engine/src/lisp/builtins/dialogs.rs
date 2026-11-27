//! `load_dialog`, `unload_dialog`, `new_dialog`, `get_tile`, `set_tile`, `action_tile`,
//! `mode_tile`, `start_list`, `add_list`, `end_list`, `dimx_tile`, `dimy_tile`, `start_image`,
//! `slide_image`, `end_image`, `start_dialog`, `done_dialog`, and `term_dialog`.
//!
//! A load failure returns a negative integer and does not change the drawing's handle store.
//! `unload_dialog` removes only the handle it is given and always returns nil. When that handle
//! is the prepared dialog, the tile values are cleared too.
//!
//! `new_dialog` prepares one dialog for this drawing. A failed preparation leaves the previous
//! dialog in place. `get_tile` and `set_tile` use the case-sensitive key. They return nil when
//! no dialog is prepared or the key is absent. `start_list` returns the key when a list or popup
//! can be changed. `add_list` returns the accepted row. `end_list` returns nil. A rejected list
//! call returns nil and leaves the committed rows and any valid pending update in place.
//!
//! `action_tile` stores one callback for a keyed tile and returns nil when the key is absent or
//! the text does not fit. `mode_tile` always returns nil. `start_dialog` waits only from an
//! interactive run; the session turns a blocking call into an error before the dialog is active.
//! `done_dialog` and `term_dialog` affect `start_dialog` only while a callback is running.

use super::args::{arity, int, point, string};
use super::{Ctx, Flow, LispError, Value};
use crate::lisp::dcl;
use crate::lisp::dcl_image;
use crate::lisp::dcl_load::{self, LoadedDialog};
use crate::lisp::dcl_prepare;
use crate::lisp::eval::Need;

pub(super) fn load_dialog(ctx: &mut Ctx<'_>, a: Vec<Value>) -> Result<Flow, LispError> {
    arity(&a, 1, 1)?;
    let name = string(&a[0])?;
    if ctx.env.dialogs.len() >= dcl_load::MAX_DIALOGS {
        return Ok(failed());
    }
    let Some(dialog) = dcl_load::load_named(name.as_ref(), ctx.s.lisp_support_dir())? else {
        return Ok(failed());
    };
    let Some(id) = next_dialog_id(&ctx.env.dialogs, ctx.env.next_dialog) else {
        return Ok(failed());
    };
    ctx.env.next_dialog = if id == i32::MAX { 1 } else { id + 1 };
    ctx.env.dialogs.insert(id, dialog);
    Ok(Flow::Value(Value::Int(id)))
}

pub(super) fn unload_dialog(ctx: &mut Ctx<'_>, a: Vec<Value>) -> Result<Flow, LispError> {
    arity(&a, 1, 1)?;
    let id = int(&a[0])?;
    ctx.env.dialogs.remove(&id);
    if ctx.env.prepared.as_ref().is_some_and(|dialog| dialog.handle == id) {
        ctx.env.prepared = None;
        // A callback that unloads its own dialog leaves no tiles to handle more events.
        ctx.env.dialog.term();
    }
    Ok(Flow::Value(Value::Nil))
}

pub(super) fn new_dialog(ctx: &mut Ctx<'_>, a: Vec<Value>) -> Result<Flow, LispError> {
    arity(&a, 2, 4)?;
    let name = string(&a[0])?;
    let id = int(&a[1])?;
    let action = a.get(2).map(string).transpose()?;
    let position = a.get(3).map(point).transpose()?.map(|[x, y, _]| (x, y));
    if action.as_ref().is_some_and(|text| text.len() > dcl::MAX_STRING_BYTES) || position.is_some_and(|(x, y)| !x.is_finite() || !y.is_finite()) {
        return Ok(nil());
    }
    if ctx.env.dialog.active() {
        return Ok(nil());
    }
    let Some(loaded) = ctx.env.dialogs.get(&id) else { return Ok(nil()) };
    let Some(prepared) = dcl_prepare::prepare(loaded, name, id, action, position)? else { return Ok(nil()) };
    ctx.env.prepared = Some(prepared);
    Ok(Flow::Value(Value::t()))
}

pub(super) fn get_tile(ctx: &mut Ctx<'_>, a: Vec<Value>) -> Result<Flow, LispError> {
    arity(&a, 1, 1)?;
    let key = string(&a[0])?;
    let Some(dialog) = ctx.env.prepared.as_ref() else { return Ok(nil()) };
    match dialog.get_tile(key.as_ref()) {
        Some(value) => Ok(Flow::Value(Value::Str(value))),
        None => Ok(nil()),
    }
}

pub(super) fn set_tile(ctx: &mut Ctx<'_>, a: Vec<Value>) -> Result<Flow, LispError> {
    arity(&a, 2, 2)?;
    let key = string(&a[0])?;
    let value = string(&a[1])?;
    let Some(dialog) = ctx.env.prepared.as_mut() else { return Ok(nil()) };
    match dialog.set_tile(key.as_ref(), value) {
        Some(stored) => Ok(Flow::Value(Value::Str(stored))),
        None => Ok(nil()),
    }
}

pub(super) fn action_tile(ctx: &mut Ctx<'_>, a: Vec<Value>) -> Result<Flow, LispError> {
    arity(&a, 2, 2)?;
    let key = string(&a[0])?;
    let action = string(&a[1])?;
    let Some(dialog) = ctx.env.prepared.as_mut() else { return Ok(nil()) };
    match dialog.set_action(key.as_ref(), action) {
        Ok(true) => Ok(Flow::Value(Value::t())),
        Ok(false) => Ok(nil()),
        Err(error) => Err(error),
    }
}

pub(super) fn start_list(ctx: &mut Ctx<'_>, a: Vec<Value>) -> Result<Flow, LispError> {
    arity(&a, 1, 3)?;
    let key = string(&a[0])?;
    let operation = match a.get(1) {
        Some(value) => int(value)?,
        None => 3,
    };
    let index = match a.get(2) {
        Some(value) => int(value)?,
        None => 0,
    };
    let Some(dialog) = ctx.env.prepared.as_mut() else { return Ok(nil()) };
    match dialog.start_list(key.as_ref(), operation, index) {
        Some(key) => Ok(Flow::Value(Value::Str(key))),
        None => Ok(nil()),
    }
}

pub(super) fn add_list(ctx: &mut Ctx<'_>, a: Vec<Value>) -> Result<Flow, LispError> {
    arity(&a, 1, 1)?;
    let row = string(&a[0])?;
    let Some(dialog) = ctx.env.prepared.as_mut() else { return Ok(nil()) };
    match dialog.add_list(row.as_ref())? {
        Some(row) => Ok(Flow::Value(Value::Str(row))),
        None => Ok(nil()),
    }
}

pub(super) fn end_list(ctx: &mut Ctx<'_>, a: Vec<Value>) -> Result<Flow, LispError> {
    arity(&a, 0, 0)?;
    if let Some(dialog) = ctx.env.prepared.as_mut() {
        dialog.end_list()?;
    }
    Ok(nil())
}

pub(super) fn mode_tile(ctx: &mut Ctx<'_>, a: Vec<Value>) -> Result<Flow, LispError> {
    arity(&a, 2, 2)?;
    let key = string(&a[0])?;
    let mode = int(&a[1])?;
    if let Some(dialog) = ctx.env.prepared.as_mut() {
        dialog.set_mode(key.as_ref(), mode)?;
    }
    Ok(nil())
}

pub(super) fn dimx_tile(ctx: &mut Ctx<'_>, a: Vec<Value>) -> Result<Flow, LispError> {
    tile_span(ctx, a, true)
}

pub(super) fn dimy_tile(ctx: &mut Ctx<'_>, a: Vec<Value>) -> Result<Flow, LispError> {
    tile_span(ctx, a, false)
}

fn tile_span(ctx: &mut Ctx<'_>, a: Vec<Value>, horizontal: bool) -> Result<Flow, LispError> {
    arity(&a, 1, 1)?;
    let key = string(&a[0])?;
    let Some(dialog) = ctx.env.prepared.as_ref() else { return Ok(nil()) };
    let span = if horizontal { dialog.dim_x(key.as_ref()) } else { dialog.dim_y(key.as_ref()) };
    Ok(match span {
        Some(span) => Flow::Value(Value::Int(span)),
        None => nil(),
    })
}

pub(super) fn start_image(ctx: &mut Ctx<'_>, a: Vec<Value>) -> Result<Flow, LispError> {
    arity(&a, 1, 1)?;
    let key = string(&a[0])?;
    let Some(dialog) = ctx.env.prepared.as_mut() else { return Ok(nil()) };
    Ok(match dialog.start_image(key.as_ref()) {
        Some(key) => Flow::Value(Value::Str(key)),
        None => nil(),
    })
}

pub(super) fn slide_image(ctx: &mut Ctx<'_>, a: Vec<Value>) -> Result<Flow, LispError> {
    arity(&a, 5, 5)?;
    let x1 = int(&a[0])?;
    let y1 = int(&a[1])?;
    let width = int(&a[2])?;
    let height = int(&a[3])?;
    let name = string(&a[4])?;
    let Some(dialog) = ctx.env.prepared.as_mut() else { return Ok(nil()) };
    if dcl_image::show_slide(dialog, x1, y1, width, height, name.as_ref(), ctx.s.lisp_support_dir())? {
        Ok(Flow::Value(Value::Str(name)))
    } else {
        Ok(nil())
    }
}

pub(super) fn end_image(ctx: &mut Ctx<'_>, a: Vec<Value>) -> Result<Flow, LispError> {
    arity(&a, 0, 0)?;
    if let Some(dialog) = ctx.env.prepared.as_mut() {
        dialog.end_image();
    }
    Ok(nil())
}

pub(super) fn start_dialog(ctx: &mut Ctx<'_>, a: Vec<Value>) -> Result<Flow, LispError> {
    arity(&a, 0, 0)?;
    if ctx.env.dialog.active() {
        return Err(LispError::new("a dialog is already active"));
    }
    if ctx.env.prepared.is_none() {
        return Ok(nil());
    }
    // The session sees this wait before it marks the dialog active, and rejects a blocking run.
    Ok(Flow::Wait(Need::Dialog))
}

pub(super) fn done_dialog(ctx: &mut Ctx<'_>, a: Vec<Value>) -> Result<Flow, LispError> {
    arity(&a, 0, 1)?;
    let status = match a.first() {
        Some(value) => int(value)?,
        None => 0,
    };
    match ctx.env.dialog.done(status) {
        Some((x, y)) => Ok(Flow::Value(Value::point(&[x, y]))),
        None => Ok(nil()),
    }
}

pub(super) fn term_dialog(ctx: &mut Ctx<'_>, a: Vec<Value>) -> Result<Flow, LispError> {
    arity(&a, 0, 0)?;
    ctx.env.dialog.term();
    Ok(nil())
}

fn nil() -> Flow {
    Flow::Value(Value::Nil)
}

fn failed() -> Flow {
    Flow::Value(Value::Int(dcl_load::LOAD_FAILED))
}

/// The next free positive handle. At most [`dcl_load::MAX_DIALOGS`] live handles, so a short
/// scan finds a free one or reports that the store cannot take another.
fn next_dialog_id(used: &std::collections::HashMap<i32, LoadedDialog>, start: i32) -> Option<i32> {
    let mut id = if start > 0 { start } else { 1 };
    for _ in 0..=dcl_load::MAX_DIALOGS {
        if !used.contains_key(&id) {
            return Some(id);
        }
        id = if id == i32::MAX { 1 } else { id + 1 };
    }
    None
}
