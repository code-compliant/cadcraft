//! AutoLISP: load `.lsp` files, evaluate expressions typed at the command line, and run
//! commands defined with `(defun c:NAME …)`.
//!
//! The language runs on an explicit-stack machine ([`eval`]) that can stop for input
//! (`getpoint`, `pause`) and carry on later, so the interface keeps drawing while a routine
//! waits. Each drawing has its own symbols, as in AutoCAD. A run is one undo step.

/// A block-definition entity and its successor in the block, by handle.
pub type BlockSlots = HashMap<Handle, (Arc<cadcraft_doc::Entity>, Option<Handle>)>;

pub mod builtins;
pub mod dcl;
mod dcl_image;
mod dcl_load;
mod dcl_prepare;
mod drawing;
pub mod eval;
pub mod input;
mod objects;
pub mod read;
mod startup;
pub mod value;

use std::collections::HashMap;
use std::sync::Arc;

use cadcraft_doc::{Drawing, Handle};

use crate::{Input, Prompt, Session, undoctl};
use eval::{Ctx, Machine, Need, Outcome};
use value::{Sym, Value};

/// A LISP error: the message `*error*` receives (`bad argument type: numberp: nil`…).
#[derive(Clone, Debug)]
pub struct LispError {
    pub msg: String,
    /// `exit`/`quit`, or a cancelled input: not caught by `vl-catch-all-apply`.
    pub quit: bool,
}

impl LispError {
    pub fn new(msg: impl Into<String>) -> LispError {
        LispError { msg: msg.into(), quit: false }
    }
    pub fn bad_arg(pred: &str, v: &Value) -> LispError {
        LispError::new(format!("bad argument type: {pred}: {}", value::to_text(v, true)))
    }
    pub fn quit(msg: impl Into<String>) -> LispError {
        LispError { msg: msg.into(), quit: true }
    }
}

/// One drawing's LISP environment.
pub struct Env {
    pub globals: HashMap<Sym, Value>,
    /// Printed text not yet ended by a newline.
    pub out: String,
    /// `initget` settings for the next `get…` call.
    pub initget: Option<input::Initget>,
    pub next_pickset: u32,
    pub files: HashMap<u32, builtins::LispFile>,
    pub next_file: u32,
    /// DCL files `load_dialog` has loaded, by the positive handle returned to Lisp.
    dialogs: HashMap<i32, dcl_load::LoadedDialog>,
    /// Next handle to try. Unload frees a handle so a later load can reuse it.
    next_dialog: i32,
    /// The one dialog `new_dialog` prepared in this drawing, if any.
    prepared: Option<dcl_prepare::PreparedDialog>,
    /// The dialog `start_dialog` is waiting on, if one is active.
    dialog: dcl_prepare::DialogRun,
    /// Entities `entdel` deleted (it brings them back when called again), with their space.
    pub deleted: HashMap<Handle, (cadcraft_doc::Space, Arc<cadcraft_doc::Entity>)>,
    /// Every entity's handle in creation order (`entlast`, `entnext`), with clones of the spaces
    /// it was taken from (model space, then each layout): it is reused only while the drawing's
    /// spaces still share every chunk with them ([`cadcraft_doc::EntityStore::same_as`]).
    pub order: Option<(Vec<cadcraft_doc::EntityStore>, Arc<Vec<Handle>>)>,
    /// Each block-definition entity with its successor in its block (None for the last), for the drawing
    /// it was taken from: `entnext`'s indexed lookup, which also tells whether a handle is a block's.
    pub block_next: Option<(usize, Arc<BlockSlots>)>,
    /// `tblnext`'s place in each table.
    pub tblnext: HashMap<String, usize>,
    /// A block definition `entmake` has begun (a BLOCK list) and not yet ended (ENDBLK): the
    /// entities made meanwhile go into it, numbered when it ends.
    pub pending_block: Option<(cadcraft_doc::Block, Vec<cadcraft_doc::Entity>)>,
    /// Entity names of non-graphic objects (dictionaries, table records, layouts).
    pub(crate) objects: objects::Names,
    /// The command history's length after LISP last printed a line: when something else wrote since
    /// (a prompt, a command's echo), a leading newline only ends that line.
    pub printed_at: Option<usize>,
}

impl Default for Env {
    fn default() -> Self {
        let mut e = Env {
            globals: HashMap::new(),
            out: String::new(),
            initget: None,
            next_pickset: 1,
            files: HashMap::new(),
            next_file: 1,
            dialogs: HashMap::new(),
            next_dialog: 1,
            prepared: None,
            dialog: dcl_prepare::DialogRun::default(),
            deleted: HashMap::new(),
            order: None,
            block_next: None,
            tblnext: HashMap::new(),
            pending_block: None,
            objects: objects::Names::default(),
            printed_at: None,
        };
        builtins::install(&mut e);
        e
    }
}

impl Env {
    /// Set a variable (nil unbinds it).
    pub fn set(&mut self, s: &Sym, v: Value) {
        if v.is_nil() {
            self.globals.remove(s);
        } else {
            self.globals.insert(s.clone(), v);
        }
    }
    pub fn get(&self, name: &str) -> Value {
        eval::lookup(self, &Sym::new(name))
    }
    /// Text printed by `princ` and friends.
    pub fn print(&mut self, text: &str) {
        self.out.push_str(text);
    }
}

/// The session's LISP: each drawing's environment and the routine running, if any.
#[derive(Default)]
pub struct LispState {
    envs: HashMap<u64, Env>,
    run: Option<Box<Run>>,
    /// A routine runs, from its start to its end (also while its machine is out of `run`).
    active: bool,
    /// The running routine's own undo steps (also while its machine is out of `run`).
    steps: RoutineSteps,
    /// Files loaded by APPLOAD, by name, for `(load name)` where there is no file system.
    files: HashMap<String, String>,
    /// Where startup files are read from (see `startup.rs`).
    support_dir: Option<std::path::PathBuf>,
    /// Drawings that had their startup files.
    autoloaded: std::collections::HashSet<u64>,
    /// `acad.lsp` was loaded this session.
    acad_loaded: bool,
    /// Next session-unique id for a dialog `start_dialog` activates.
    next_dialog_id: u64,
    /// Why this slice must refuse `start_dialog`. Empty for an interactive run.
    dialog_refusal: Option<&'static str>,
    /// Memory caps (lowered by tests).
    pub(crate) limits: Limits,
    /// Interpreter steps all synchronous callbacks have run, nested ones included. A callback's
    /// allowance counts what ran since it started, so the callbacks it calls spend its steps.
    pub(crate) callback_used: u64,
    /// Work synchronous callbacks did inside the running step, already counted in `callback_used`:
    /// the step is charged it without charging the enclosing callback a second time.
    pub(crate) callback_work: u64,
    /// The `callback_used` value the callback running now (and so every one it calls) must not
    /// pass: the lowest of its own and its callers' allowances.
    pub(crate) callback_ceiling: Option<u64>,
}

/// What U and REDO inside the running routine take back: the commands it ran, each as the drawing
/// was before it, last last. The routine is still one undo step once it ends.
#[derive(Default)]
struct RoutineSteps {
    /// The drawing the routine runs in.
    uid: u64,
    undo: std::collections::VecDeque<crate::Snapshot>,
    redo: std::collections::VecDeque<crate::Snapshot>,
    /// Where the routine's undo step starts, when U or REDO inside it went on to the drawing's
    /// own steps; else where the routine started.
    base: Option<(Arc<Drawing>, Vec<Handle>)>,
}

/// What a run may use (see [`value`]'s memory notes).
#[derive(Clone, Copy, Debug)]
pub(crate) struct Limits {
    /// Live cons cells (all sessions).
    pub cells: usize,
    /// Bytes of strings, printed text and files read per run.
    pub string_bytes: u64,
    /// Maximum interpreter steps per synchronous callback.
    pub callback_steps: u64,
}

impl Default for Limits {
    /// Provide finite cons-cell, string-byte, and synchronous-callback limits for a LISP run.
    fn default() -> Self {
        Limits { cells: value::MAX_CELLS, string_bytes: value::STRING_BUDGET, callback_steps: 500_000_000 }
    }
}

impl std::fmt::Debug for LispState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "LispState {{ drawings: {}, running: {} }}", self.envs.len(), self.run.is_some())
    }
}

/// A routine on its way: the machine, what it waits for, and its undo step.
struct Run {
    machine: Machine,
    /// The drawing it runs in.
    uid: u64,
    waiting: Option<Need>,
    /// A command-line expression: its value is printed at the end.
    print: bool,
    /// Undo label: the command (`c:` function) or "LISP".
    label: String,
    /// The drawing and selection before the run (one undo step for the whole run).
    before: Option<(Arc<Drawing>, Vec<Handle>)>,
    /// Inputs given up front (`lisp {code, inputs}`): what `get…` calls answer.
    inputs: std::collections::VecDeque<Value>,
    /// Run to the end without waiting (programmatic calls): `get…` without an input gets nil.
    blocking: bool,
    /// The run began inside a script. A later yield must not drop that rule.
    scripted: bool,
    /// Steps taken, for the cap on blocking runs.
    steps: u64,
    /// The `*error*` handler is the one running.
    handler: bool,
    /// A drawing change needs the handler's native command to end within this undo group.
    finish_native_command: bool,
    /// Bytes of strings and printed text the run has made ([`value::charge_bytes`]).
    string_bytes: u64,
    /// Cons cells alive when the run started: cells the drawing's variables already hold don't
    /// stop a run (it can still set them to nil) unless it adds more.
    cells_at_start: usize,
}

/// Steps per slice before the interface gets a frame.
const SLICE: u64 = 400_000;
/// Steps a blocking run (a programmatic call) may take before it is stopped.
const BLOCKING_CAP: u64 = 500_000_000;

impl Session {
    /// The LISP environment of drawing `uid`, not whichever drawing is active now.
    fn lisp_env_take(&mut self, uid: u64) -> Env {
        self.lisp.envs.remove(&uid).unwrap_or_default()
    }

    /// Put a drawing's environment back, unless the drawing was closed meanwhile (a routine
    /// cancelled by CLOSE ends after its drawing went away).
    fn lisp_env_put(&mut self, uid: u64, env: Env) {
        if uid == 0 || self.docs.iter().any(|d| d.uid == uid) {
            self.lisp.envs.insert(uid, env);
        }
    }

    /// Drop the environments of drawings no longer open: one holds clones of its drawing's
    /// entity stores ([`Env::order`]), which would otherwise outlive the drawing.
    pub(crate) fn lisp_forget_closed(&mut self) {
        let docs = &self.docs;
        self.lisp.envs.retain(|uid, _| *uid == 0 || docs.iter().any(|d| d.uid == *uid));
    }

    /// A routine is running (possibly waiting for input).
    pub fn lisp_running(&self) -> bool {
        self.lisp.active
    }

    /// Whether a drawing command, LISP input prompt, or modal dialog owns the input.
    /// Idle drawing input becomes available when all of these finish.
    pub fn prompting(&self) -> bool {
        self.running.is_some() || self.lisp_prompt().is_some() || self.dialog_active()
    }

    /// A routine is stopped, waiting for input or for a frame.
    pub(crate) fn lisp_waiting(&self) -> bool {
        self.lisp.run.is_some()
    }

    /// The value of a LISP variable in the active drawing (nil when unbound).
    pub fn lisp_var(&self, name: &str) -> Value {
        let uid = self.state().map(|s| s.uid).unwrap_or(0);
        self.lisp.envs.get(&uid).map(|e| e.get(name)).unwrap_or_default()
    }

    /// Evaluate LISP source (a command-line expression or a file's contents). `print`: show the
    /// value on the command line. `blocking`: run to the end, answering `get…` calls from
    /// `inputs` (nil when they run out), as programmatic calls must; otherwise the routine may
    /// wait for the user.
    pub fn lisp_eval(
        &mut self,
        src: &str,
        print: bool,
        blocking: bool,
        inputs: Vec<Value>,
    ) -> crate::Result<Option<std::result::Result<Value, String>>> {
        let forms = read::read_all(src).map_err(|e| crate::EngineError::Other(format!("; error: {}", e.msg)))?;
        self.lisp_start(Machine::forms(forms), print, blocking, inputs, "LISP".into())
    }

    /// Call a LISP function (`c:` commands typed at the command line).
    fn lisp_call(&mut self, f: Value, label: String) -> crate::Result<Option<std::result::Result<Value, String>>> {
        self.lisp_start(Machine::calling(f, Vec::new()), false, false, Vec::new(), label)
    }

    fn lisp_start(
        &mut self,
        machine: Machine,
        print: bool,
        blocking: bool,
        inputs: Vec<Value>,
        label: String,
    ) -> crate::Result<Option<std::result::Result<Value, String>>> {
        if self.lisp.active {
            return Err(crate::EngineError::Other("a LISP routine is already running".into()));
        }
        self.lisp.active = true;
        let st = self.state()?;
        let before = Some((st.doc.clone(), st.selection.clone()));
        let uid = st.uid;
        self.lisp.steps = RoutineSteps { uid, ..RoutineSteps::default() };
        self.lisp.run = Some(Box::new(Run {
            machine,
            uid,
            waiting: None,
            print,
            label,
            before,
            inputs: inputs.into(),
            blocking,
            scripted: self.in_script,
            steps: 0,
            handler: false,
            finish_native_command: false,
            string_bytes: 0,
            cells_at_start: value::live_cells(),
        }));
        self.lisp_drive();
        if blocking {
            // A blocking run that waits for a command's input (`pause`) can't get it: cancel.
            if self.lisp.run.is_some() {
                self.lisp_abort("Function cancelled");
            }
        }
        Ok(self.lisp_last.take())
    }

    /// Call once per frame: loads the startup files into a drawing seen for the first time, and
    /// carries on a routine that gave the interface a frame.
    pub fn lisp_tick(&mut self) {
        // Called every frame, outside any command: a panic in it ends the routine with an error
        // instead of taking down the app.
        if let Err(p) = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| self.lisp_tick_unguarded())) {
            self.lisp_internal_error(&crate::guard::message(&*p));
        }
    }

    fn lisp_tick_unguarded(&mut self) {
        // A drawing seen for the first time gets its startup files.
        self.lisp_autoload();
        if self.lisp.run.as_ref().is_some_and(|r| matches!(r.waiting, Some(Need::Yield))) {
            if let Some(r) = self.lisp.run.as_mut() {
                r.waiting = None;
            }
            self.lisp_drive();
        }
    }

    /// Whether the routine needs ticks ([`Session::lisp_tick`]) to go on.
    pub fn lisp_busy(&self) -> bool {
        self.lisp.run.as_ref().is_some_and(|r| matches!(r.waiting, Some(Need::Yield)))
    }

    /// Run the machine until it finishes or waits.
    fn lisp_drive(&mut self) {
        loop {
            let Some(mut run) = self.lisp.run.take() else { return };
            if run.waiting.is_some() {
                self.lisp.run = Some(run);
                return;
            }
            // The routine belongs to its drawing; another one became active: stop it.
            if self.state().map(|s| s.uid).ok() != Some(run.uid) {
                self.lisp.run = Some(run);
                self.lisp_abort("Function cancelled");
                return;
            }
            let mut env = self.lisp_env_take(run.uid);
            value::set_string_budget(run.string_bytes, self.lisp.limits.string_bytes);
            value::set_cell_limit(self.lisp.limits.cells.max(run.cells_at_start));
            // What the routine feeds the command line is the program's input: the command it
            // starts, even when another one is running and the token reaches
            // [`Session::typed`] first, never opens a dialog. Restored after the slice, a panic
            // in it included, so a person's later command keeps its own origin.
            let program_input = std::mem::replace(&mut self.program_input, true);
            self.lisp.dialog_refusal = dialog_refusal_of(&run);
            let outcome = {
                let machine = &mut run.machine;
                let mut ctx = Ctx { s: self, env: &mut env, depth: 0 };
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| machine.run(&mut ctx, SLICE)))
            };
            self.program_input = program_input;
            self.lisp.dialog_refusal = None;
            run.string_bytes = value::string_bytes_used();
            let outcome = match outcome {
                Ok(o) => o,
                Err(p) => {
                    // A panic inside a built-in: put the drawing back as the routine found it.
                    if let Some((doc, sel)) = self.lisp.steps.base.as_ref().or(run.before.as_ref())
                        && let Some(st) = self.docs.iter_mut().find(|d| d.uid == run.uid)
                    {
                        st.doc = doc.clone();
                        st.selection = sel.clone();
                    }
                    run.handler = true;
                    Outcome::Error(LispError::quit(format!("internal error: {}", crate::guard::message(&*p))))
                }
            };
            let uid = run.uid;
            self.lisp_flush(&mut env, false);
            self.lisp_env_put(uid, env);
            // The work the slice did, which one step can take far past the slice. After a panic
            // `used` is still the previous slice's; the run ends with the error anyway.
            run.steps = run.steps.saturating_add(run.machine.used);
            if run.blocking && run.steps > BLOCKING_CAP && matches!(outcome, Outcome::Wait(_)) {
                self.lisp.run = Some(run);
                self.lisp_error(LispError::quit(format!("routine stopped after {BLOCKING_CAP} steps")));
                return;
            }
            // A command in this slice may have changed drawings. A wait needs its owning drawing.
            if matches!(&outcome, Outcome::Wait(_)) && self.state().map(|s| s.uid).ok() != Some(uid) {
                self.lisp.run = Some(run);
                self.lisp_abort("Function cancelled");
                return;
            }
            match outcome {
                Outcome::Done(v) => {
                    self.lisp.run = Some(run);
                    self.lisp_end(Ok(v));
                    return;
                }
                Outcome::Error(e) => {
                    self.lisp.run = Some(run);
                    self.lisp_error(e);
                    return;
                }
                Outcome::Wait(Need::Yield) => {
                    // A script must finish this slice before its next line. The script flag
                    // itself is gone once `script` returns.
                    if run.blocking || run.scripted {
                        if run.steps > BLOCKING_CAP {
                            self.lisp.run = Some(run);
                            self.lisp_error(LispError::quit(format!("routine stopped after {BLOCKING_CAP} steps")));
                            return;
                        }
                        self.lisp.run = Some(run);
                        continue;
                    }
                    run.waiting = Some(Need::Yield);
                    self.lisp.run = Some(run);
                    return;
                }
                Outcome::Wait(Need::Pause) => {
                    // The command `command` left running takes the next input; with none
                    // running there is nothing to wait for.
                    if self.running.is_none() || run.blocking {
                        run.machine.resume(Value::Nil);
                        if run.blocking && self.running.is_some() {
                            self.cancel();
                        }
                        self.lisp.run = Some(run);
                        continue;
                    }
                    run.waiting = Some(Need::Pause);
                    self.lisp.run = Some(run);
                    return;
                }
                Outcome::Wait(Need::Dialog) => {
                    // A callback that finished without closing returns here with the dialog already
                    // active. Activating again would drop the status and the position it recorded.
                    // A blocking or scripted start never reaches this arm: the evaluator raises it.
                    if self.lisp.envs.get(&run.uid).is_some_and(|env| env.dialog.active()) {
                        run.waiting = Some(Need::Dialog);
                        self.lisp.run = Some(run);
                        return;
                    }
                    let position = self.lisp.envs.get(&run.uid).and_then(|env| env.prepared.as_ref().map(|dialog| dialog.position));
                    if position.is_none() {
                        run.machine.resume(Value::Nil);
                        self.lisp.run = Some(run);
                        continue;
                    }
                    self.lisp.next_dialog_id = self.lisp.next_dialog_id.saturating_add(1);
                    let id = self.lisp.next_dialog_id;
                    if let Some(env) = self.lisp.envs.get_mut(&run.uid) {
                        env.dialog.activate(id, position.flatten());
                    }
                    run.waiting = Some(Need::Dialog);
                    self.lisp.run = Some(run);
                    return;
                }
                Outcome::Wait(Need::Get(req)) => {
                    if let Some(v) = run.inputs.pop_front() {
                        run.machine.resume(v);
                        self.lisp.run = Some(run);
                        continue;
                    }
                    if run.blocking {
                        run.machine.resume(Value::Nil);
                        self.lisp.run = Some(run);
                        continue;
                    }
                    run.waiting = Some(Need::Get(req));
                    self.lisp.run = Some(run);
                    return;
                }
            }
        }
    }

    /// Move printed text to the command history: complete lines, and with `all` the rest too.
    fn lisp_flush(&mut self, env: &mut Env, all: bool) {
        // One pass over the buffer, dropping the printed lines once at the end: draining each line
        // from the front shifted the rest every time, quadratic in newline-dense output.
        let mut done = 0;
        while let Some(i) = env.out.get(done..).and_then(|rest| rest.find('\n')) {
            let line = env.out.get(done..done + i).unwrap_or_default().trim_end_matches('\r').to_string();
            done += i + 1;
            // `(princ "\nText")` after a prompt or a command's echo ends that line instead of
            // leaving an empty one; after LISP's own line it does print an empty line.
            if line.is_empty() && env.printed_at != Some(self.log.len()) {
                env.printed_at = Some(self.log.len());
                continue;
            }
            self.echo(line);
            env.printed_at = Some(self.log.len());
        }
        env.out.drain(..done);
        if all && !env.out.is_empty() {
            let line = std::mem::take(&mut env.out);
            self.echo(line);
            env.printed_at = Some(self.log.len());
        }
    }

    /// The routine finished: print its value (command-line expressions), record its undo step.
    fn lisp_end(&mut self, result: std::result::Result<Value, String>) {
        let Some(run) = self.lisp.run.take() else { return };
        if run.finish_native_command && self.running.is_some() && self.state().map(|st| st.uid).ok() == Some(run.uid) {
            // Keep the routine's undo group open until its command finishes in this drawing.
            self.cancel();
        }
        self.lisp.active = false;
        if let Some(env) = self.lisp.envs.get_mut(&run.uid) {
            env.dialog.deactivate();
        }
        let steps = std::mem::take(&mut self.lisp.steps);
        let mut env = self.lisp.envs.remove(&run.uid).unwrap_or_default();
        self.lisp_flush(&mut env, true);
        self.lisp_env_put(run.uid, env);
        if let (Ok(v), true) = (&result, run.print) {
            let text = value::to_text(v, true);
            if !builtins::is_null_symbol(v) {
                self.echo(text);
            }
        }
        if let Some((doc, sel)) = steps.base.or(run.before)
            && let Some(st) = self.docs.iter_mut().find(|d| d.uid == run.uid)
            && !Arc::ptr_eq(&doc, &st.doc)
        {
            st.push_undo(crate::Snapshot { label: run.label.clone(), doc, selection: sel });
            st.revision += 1;
        }
        self.lisp_last = Some(result);
    }

    /// A LISP error: run `*error*` when the routine defined one, else print it.
    fn lisp_error(&mut self, e: LispError) {
        let Some(mut run) = self.lisp.run.take() else { return };
        let uid = run.uid;
        // Keep the handler's locals until it returns. A normal error already restored these.
        let restores = run.machine.take_unwind_bindings();
        // A block definition the failed routine began is dropped: a later routine's entities
        // would otherwise go into it instead of the drawing (#1662). A definition still spans
        // runs that end normally.
        if let Some(env) = self.lisp.envs.get_mut(&uid) {
            env.pending_block = None;
            env.dialog.deactivate();
        }
        // The command a failed routine left running is cancelled.
        if self.running.is_some() {
            self.cancel();
        }
        let handler = self.lisp.envs.get(&uid).map(|env| env.get("*ERROR*")).unwrap_or_default();
        if !run.handler && self.state().map(|s| s.uid).ok() == Some(uid) && matches!(handler, Value::Func(_) | Value::Subr(_)) {
            run.handler = true;
            run.waiting = None;
            run.print = false;
            run.machine = Machine::calling_then_restore(handler, vec![Value::str(&e.msg)], restores);
            self.lisp.run = Some(run);
            self.lisp_drive();
            // The routine failed whatever its handler returned.
            if let Some(Ok(_)) = &self.lisp_last {
                self.lisp_last = Some(Err(e.msg));
            }
            return;
        }
        if let Some(env) = self.lisp.envs.get_mut(&uid) {
            for group in restores {
                eval::restore_bindings(env, group);
            }
        }
        self.lisp.run = Some(run);
        let mut env = self.lisp_env_take(uid);
        self.lisp_flush(&mut env, true);
        self.lisp_env_put(uid, env);
        self.echo(format!("; error: {}", e.msg));
        self.lisp_end(Err(e.msg));
    }

    /// A panic escaped the LISP machinery outside a routine's steps: end the routine, if any,
    /// with an error, so the session stays usable.
    fn lisp_internal_error(&mut self, msg: &str) {
        let msg = format!("internal error: {msg}");
        if self.lisp.run.is_some() {
            if let Some(r) = self.lisp.run.as_mut() {
                // Don't run the routine's own `*error*` after an internal failure.
                r.handler = true;
            }
            self.lisp_error(LispError::quit(msg));
        } else if self.lisp.active {
            self.lisp.active = false;
            self.lisp.steps = RoutineSteps::default();
            self.echo(format!("; error: {msg}"));
            self.lisp_last = Some(Err(msg));
        } else {
            self.echo(format!("; error: {msg}"));
        }
    }

    /// End input and unwind a cancellation handler that waits during a drawing change.
    pub(crate) fn cancel_for_drawing_change(&mut self) {
        if let Some(run) = self.lisp.run.as_mut() {
            run.finish_native_command = true;
        }
        self.cancel();
        if self.lisp_waiting() {
            self.lisp_abort("Function cancelled");
        }
        // A handler may itself switch drawings and begin a command there.
        if self.running.is_some() {
            self.cancel();
        }
    }

    /// Stop the routine (Esc while it waits, its drawing closed) with `msg` as its error.
    pub fn lisp_abort(&mut self, msg: &str) {
        if self.lisp.run.is_some() {
            // Leave the local `*error*` handler and its locals in place. `lisp_error` restores
            // the outer bindings after that handler returns.
            self.lisp_error(LispError::quit(msg));
        }
    }

    /// An event or callback failed. Unwind to a surrounding `vl-catch-all-apply` when there is one.
    fn raise_dialog_error(&mut self, mut run: Box<Run>, mut env: Env, error: LispError) {
        let uid = run.uid;
        let message = error.msg.clone();
        let quit = error.quit;
        let caught = {
            let mut ctx = Ctx { s: self, env: &mut env, depth: 0 };
            run.machine.raise(&mut ctx, error).is_none()
        };
        if !caught || !env.dialog.in_action() {
            env.dialog.deactivate();
        }
        self.lisp_env_put(uid, env);
        run.waiting = None;
        self.lisp.run = Some(run);
        if caught {
            self.lisp_drive();
        } else if quit {
            self.lisp_error(LispError::quit(message));
        } else {
            self.lisp_error(LispError::new(message));
        }
    }

    /// A routine waits for the user's answer to a `get…` function: its prompt.
    pub(crate) fn lisp_prompt(&self) -> Option<Prompt> {
        match self.lisp.run.as_ref()?.waiting.as_ref()? {
            Need::Get(req) => Some(req.prompt(&self.input_ucs())),
            _ => None,
        }
    }

    /// The rubber band of a waiting `get…` function: a rectangle from `getcorner`'s corner, a
    /// line from the base point of the others. `None` when no routine waits for input.
    pub(crate) fn lisp_preview(&self, cursor: cadcraft_geom::Vec2) -> Option<Vec<cadcraft_doc::EntityKind>> {
        let Need::Get(req) = self.lisp.run.as_ref()?.waiting.as_ref()? else { return None };
        let line =
            |a: cadcraft_geom::Vec2, b: cadcraft_geom::Vec2| cadcraft_doc::EntityKind::Line(cadcraft_doc::Line { a: a.to3(0.0), b: b.to3(0.0) });
        // The base is a UCS point (as the routine gave it); the preview is drawn in world space.
        let ucs = self.input_ucs();
        let Some(bu) = req.first.or(req.base).map(|p| cadcraft_geom::Vec2::new(p[0], p[1])) else { return Some(Vec::new()) };
        let b = ucs.to_wcs(bu);
        Some(match req.kind {
            input::GetKind::Corner => {
                // The rectangle runs along the UCS axes, as the corner it returns is measured.
                let cu = ucs.to_ucs(cursor);
                let (c1, c2) = (ucs.to_wcs(cadcraft_geom::Vec2::new(cu.x, bu.y)), ucs.to_wcs(cadcraft_geom::Vec2::new(bu.x, cu.y)));
                vec![line(b, c1), line(c1, cursor), line(cursor, c2), line(c2, b)]
            }
            input::GetKind::Point | input::GetKind::Dist | input::GetKind::Angle { .. } => vec![line(b, cursor)],
            _ => Vec::new(),
        })
    }

    /// Give a tile event to the dialog `start_dialog` is waiting on. An event whose id is not the
    /// active dialog, including one from a dialog that already closed, leaves the session unchanged.
    pub fn dialog_event(&mut self, event: DialogEvent<'_>) -> crate::Result<DialogEventOutcome> {
        if !self.lisp.run.as_ref().is_some_and(|run| matches!(run.waiting, Some(Need::Dialog))) {
            return Ok(DialogEventOutcome::Ignored);
        }
        let Some(mut run) = self.lisp.run.take() else { return Ok(DialogEventOutcome::Ignored) };
        let uid = run.uid;
        if self.state().map(|st| st.uid).ok() != Some(uid) {
            self.lisp.run = Some(run);
            self.lisp_abort("Function cancelled");
            return Ok(DialogEventOutcome::Ignored);
        }
        let mut env = self.lisp_env_take(uid);
        if env.dialog.id() != event.id || event.position.is_some_and(|(x, y)| !x.is_finite() || !y.is_finite()) {
            self.lisp_env_put(uid, env);
            self.lisp.run = Some(run);
            return Ok(DialogEventOutcome::Ignored);
        }
        let dispatch = env.prepared.as_ref().map(|dialog| dialog.action_for(event.key)).unwrap_or(dcl_prepare::DialogDispatch::Missing);
        if let Some(text) = event.value
            && !matches!(dispatch, dcl_prepare::DialogDispatch::Missing | dcl_prepare::DialogDispatch::Ignore)
            && let Some(dialog) = env.prepared.as_ref()
        {
            let rejection = if !dialog.can_set_tile(event.key, text.len()) {
                Some(DialogEventOutcome::TextLimit)
            } else if !dialog.valid_event_value(event.key, text) {
                Some(DialogEventOutcome::Ignored)
            } else {
                None
            };
            if let Some(outcome) = rejection {
                self.lisp_env_put(uid, env);
                self.lisp.run = Some(run);
                return Ok(outcome);
            }
        }
        match dispatch {
            dcl_prepare::DialogDispatch::Missing | dcl_prepare::DialogDispatch::Ignore => {
                self.lisp_env_put(uid, env);
                self.lisp.run = Some(run);
                Ok(DialogEventOutcome::Ignored)
            }
            dcl_prepare::DialogDispatch::Update | dcl_prepare::DialogDispatch::Close(_) => {
                if let Err(error) = charge_dialog_event(&mut run, self.lisp.limits.string_bytes, 0, event.value, None) {
                    self.raise_dialog_error(run, env, error);
                    return Ok(DialogEventOutcome::Ignored);
                }
                apply_dialog_edit(&mut env, event.key, event.value, event.position);
                if let dcl_prepare::DialogDispatch::Close(status) = dispatch {
                    env.dialog.deactivate();
                    run.waiting = None;
                    run.machine.resume(Value::Int(status));
                }
                self.lisp_env_put(uid, env);
                let resume = run.waiting.is_none();
                self.lisp.run = Some(run);
                if resume {
                    self.lisp_drive();
                }
                Ok(DialogEventOutcome::Applied)
            }
            dcl_prepare::DialogDispatch::Run(action) => match self.prepare_dialog_action(&mut env, &mut run, &event, &action) {
                Ok(()) => {
                    self.lisp_env_put(uid, env);
                    self.lisp.run = Some(run);
                    self.lisp_drive();
                    Ok(DialogEventOutcome::Applied)
                }
                Err(error) => {
                    self.raise_dialog_error(run, env, error);
                    Ok(DialogEventOutcome::Ignored)
                }
            },
        }
    }

    /// Parse and bind one callback. The run's string budget pays before the parse and the new strings.
    fn prepare_dialog_action(&mut self, env: &mut Env, run: &mut Run, event: &dcl_prepare::DialogEvent<'_>, action: &str) -> Result<(), LispError> {
        charge_dialog_event(run, self.lisp.limits.string_bytes, event.key.len(), event.value, Some(action))?;
        let forms = read::read_all(action)?;
        apply_dialog_edit(env, event.key, event.value, event.position);
        let value = env.prepared.as_ref().and_then(|dialog| dialog.get_tile(event.key)).unwrap_or_else(|| Arc::from(""));
        let saved = bind_dialog_action(env, event.key, value, event.reason);
        env.dialog.begin_action();
        run.waiting = None;
        run.machine.run_dialog_action(forms, saved);
        Ok(())
    }

    /// The session-unique id of the active dialog, for the native adapter.
    pub fn dialog_id(&self) -> Option<u64> {
        let uid = self.state().ok()?.uid;
        let dialog = &self.lisp.envs.get(&uid)?.dialog;
        dialog.active().then_some(dialog.id()).filter(|id| *id != 0)
    }

    /// Whether `start_dialog` is waiting, or one of its callbacks is running.
    pub fn dialog_active(&self) -> bool {
        let Ok(uid) = self.state().map(|st| st.uid) else { return false };
        self.lisp.envs.get(&uid).is_some_and(|env| env.dialog.active())
    }

    /// Refuse drawing input while a DCL dialog owns it.
    pub fn require_dialog_closed(&self, command: &str) -> crate::Result<()> {
        if self.dialog_active() {
            return Err(crate::EngineError::Disabled(command.into(), "close the active DCL dialog before editing the drawing".into()));
        }
        Ok(())
    }

    /// `Some(false)` when `mode_tile` disabled the tile or an enclosing cluster. `None` for an unknown key.
    pub fn dialog_tile_enabled(&self, key: &str) -> Option<bool> {
        let uid = self.state().ok()?.uid;
        self.lisp.envs.get(&uid)?.prepared.as_ref()?.tile_enabled(key)
    }

    /// A callback can rebuild a list before the native adapter sends a queued click.
    /// A double-click commit can follow an applied selection while its row survives at the same index.
    pub fn dialog_list_rows_current(&self, key: &str, rows: &Arc<Vec<Arc<str>>>, commit_at: Option<usize>) -> bool {
        let Ok(uid) = self.state().map(|st| st.uid) else { return false };
        self.lisp.envs.get(&uid).and_then(|env| env.prepared.as_ref()).is_some_and(|dialog| dialog.list_rows_current(key, rows, commit_at))
    }

    /// Whether the routine is ready for tile events. A suspended callback owns its own prompt.
    pub fn dialog_waiting_for_tile(&self) -> bool {
        self.dialog_active() && self.lisp.run.as_ref().is_some_and(|run| matches!(run.waiting, Some(Need::Dialog)))
    }

    /// Whether a dialog callback is waiting for a point from the drawing.
    pub fn dialog_waiting_for_point(&self) -> bool {
        self.dialog_active() && self.lisp_prompt().is_some_and(|prompt| prompt.accept.point)
    }

    /// The waiting dialog's title and controls in source order.
    pub fn dialog_view(&self) -> Option<dcl_prepare::DialogView> {
        if !self.dialog_waiting_for_tile() {
            return None;
        }
        let uid = self.state().ok()?.uid;
        let env = self.lisp.envs.get(&uid)?;
        let id = env.dialog.id();
        if id == 0 {
            return None;
        }
        env.prepared.as_ref().map(|dialog| dialog.view(id))
    }

    /// Drain focus, selection, and highlight requests so the adapter applies each once.
    pub fn take_dialog_requests(&mut self) -> Vec<dcl_prepare::DialogRequest> {
        let Ok(uid) = self.state().map(|st| st.uid) else { return Vec::new() };
        self.lisp.envs.get_mut(&uid).and_then(|env| env.prepared.as_mut()).map(|dialog| dialog.take_requests()).unwrap_or_default()
    }

    /// Give an input to a waiting routine. `true` when the routine took it.
    pub(crate) fn lisp_input(&mut self, input: Input) -> bool {
        let Some(req) = self.lisp.run.as_ref().and_then(|r| match &r.waiting {
            Some(Need::Get(req)) => Some(req.clone()),
            _ => None,
        }) else {
            return false;
        };
        if input == Input::Cancel {
            self.echo("*Cancel*");
            self.lisp_abort("Function cancelled");
            return true;
        }
        match req.answer(self, input) {
            input::Answer::Value(v) => {
                if let Some(r) = self.lisp.run.as_mut() {
                    r.waiting = None;
                    r.machine.resume(v);
                }
                self.lisp_drive();
            }
            // Not an answer this prompt takes: say why and ask again.
            input::Answer::Again(msg) => self.echo(msg),
            // The first of two points: the prompt asks for the second.
            input::Answer::Second(p) => {
                if let Some(r) = self.lisp.run.as_mut() {
                    r.waiting = Some(Need::Get(input::with_first(&req, p)));
                }
            }
        }
        true
    }

    /// After the running command took an input: a routine paused for it carries on.
    pub(crate) fn lisp_after_command_input(&mut self) {
        if self.lisp.run.as_ref().is_some_and(|r| matches!(r.waiting, Some(Need::Pause))) {
            if let Some(r) = self.lisp.run.as_mut() {
                r.waiting = None;
                r.machine.resume(Value::Nil);
            }
            self.lisp_drive();
        }
    }

    /// Whether the active drawing owns the running routine's undo group.
    pub(crate) fn lisp_groups_undo(&self) -> bool {
        self.routine_steps().is_some()
    }

    /// A command the running routine ran changed the drawing (`snap` holds it as it was): U inside
    /// the routine takes it back, as AutoCAD's does, while the routine stays one undo step. UNDO
    /// Control One keeps only the last, None none.
    pub(crate) fn lisp_record_step(&mut self, snap: crate::Snapshot) {
        let Some(uid) = self.routine_steps() else { return };
        let mut control = undoctl::Control::All;
        // A command ends what REDO could bring back, the drawing's own steps too.
        if let Some(st) = self.docs.iter_mut().find(|d| d.uid == uid) {
            st.redo.clear();
            control = st.undo_ctl.control;
        }
        let steps = &mut self.lisp.steps;
        steps.redo.clear();
        match control {
            undoctl::Control::None => steps.undo.clear(),
            undoctl::Control::One => {
                steps.undo.clear();
                steps.undo.push_back(snap);
            }
            // As many as the drawing keeps.
            undoctl::Control::All => {
                steps.undo.push_back(snap);
                if steps.undo.len() > undoctl::MAX_UNDO {
                    steps.undo.pop_front();
                }
            }
        }
    }

    /// U (or with `redo`, REDO) inside the running routine: take its last command back (or bring
    /// back what U took). None when it has none left, and the drawing's own steps go on from there.
    pub(crate) fn lisp_take_back(&mut self, redo: bool) -> Option<String> {
        let uid = self.routine_steps()?;
        let st = self.docs.iter_mut().find(|d| d.uid == uid)?;
        let steps = &mut self.lisp.steps;
        let (from, to) = if redo { (&mut steps.redo, &mut steps.undo) } else { (&mut steps.undo, &mut steps.redo) };
        let cur = st.swap_in(from.pop_back()?);
        let label = cur.label.clone();
        to.push_back(cur);
        Some(label)
    }

    /// U or REDO inside the running routine went on to the drawing's steps before it: the
    /// routine's own undo step starts from the drawing as that left it, and REDO brings back that
    /// step before what U took back of the routine's.
    pub(crate) fn lisp_rebase(&mut self) {
        if self.routine_steps().is_none() {
            return;
        }
        if let Ok(st) = self.state() {
            self.lisp.steps.base = Some((st.doc.clone(), st.selection.clone()));
            self.lisp.steps.redo.clear();
        }
    }

    /// The active drawing's uid when a routine runs in it.
    fn routine_steps(&self) -> Option<u64> {
        let uid = self.state().ok()?.uid;
        (self.lisp.active && self.lisp.steps.uid == uid).then_some(uid)
    }

    /// The commands the active drawing's LISP defines (`c:` functions), uppercase and sorted.
    pub fn lisp_commands(&self) -> Vec<String> {
        let Ok(uid) = self.state().map(|s| s.uid) else { return Vec::new() };
        let Some(env) = self.lisp.envs.get(&uid) else { return Vec::new() };
        let mut names: Vec<String> = env
            .globals
            .iter()
            .filter(|(name, f)| name.name().starts_with("C:") && matches!(f, Value::Func(_) | Value::Subr(_)))
            .filter_map(|(name, _)| name.name().strip_prefix("C:").filter(|n| !n.is_empty()).map(str::to_string))
            .collect();
        names.sort();
        names
    }

    /// A `c:` function for a command name typed at the command line.
    pub(crate) fn lisp_command(&self, name: &str) -> Option<Value> {
        if self.lisp.active {
            return None;
        }
        let uid = self.state().ok()?.uid;
        let f = self.lisp.envs.get(&uid)?.get(&format!("C:{name}"));
        matches!(f, Value::Func(_) | Value::Subr(_)).then_some(f)
    }

    /// Run a `c:` command.
    pub(crate) fn lisp_run_command(&mut self, name: &str, f: Value) -> crate::Result<()> {
        self.lisp_call(f, name.to_ascii_uppercase()).map(|_| ())
    }
}

pub use dcl_image::{SlideSource, SlideSourcePrimitive, SlideSourceSolid, SlideSourceVector, set_slide_decoder};
pub use dcl_prepare::{
    DialogControl, DialogEvent, DialogEventOutcome, DialogImage, DialogPicture, DialogPrimitive, DialogRequest, DialogSolid, DialogVector, DialogView,
};

pub(crate) use drawing::from_json as drawing_from_json;
pub(crate) use drawing::load_source;

impl Session {
    /// A file APPLOAD loaded, by its name (with or without `.lsp`, any case).
    pub(crate) fn lisp_file(&self, name: &str) -> Option<String> {
        let key = file_key(name);
        self.lisp.files.get(&key).cloned()
    }

    /// Load a LISP file (APPLOAD, a dropped `.lsp` file): its forms run in the active drawing,
    /// and `(load name)` finds it later even where there is no file system. `blocking`: run to
    /// the end (programmatic calls).
    pub fn lisp_load(&mut self, name: &str, text: &str, blocking: bool) -> crate::Result<Option<std::result::Result<Value, String>>> {
        self.lisp.files.insert(file_key(name), text.to_string());
        let r = self.lisp_eval(text, false, blocking, Vec::new())?;
        if !matches!(r, Some(Err(_))) {
            let shown = std::path::Path::new(name).file_name().and_then(std::ffi::OsStr::to_str).unwrap_or(name).to_string();
            self.echo(format!("{shown} successfully loaded."));
        }
        Ok(r)
    }
}

/// Count the callback source, the new key, and an accepted edit before any of them is allocated.
fn charge_dialog_event(run: &mut Run, budget: u64, key_bytes: usize, value: Option<&str>, action: Option<&str>) -> Result<(), LispError> {
    value::set_string_budget(run.string_bytes, budget);
    let bytes = key_bytes.saturating_add(value.map_or(0, str::len)).saturating_add(action.map_or(0, str::len));
    let charged = value::charge_bytes(bytes);
    run.string_bytes = value::string_bytes_used();
    charged
}

/// Store an accepted edit and the dialog position. Rejected events never call this.
fn apply_dialog_edit(env: &mut Env, key: &str, value: Option<&str>, position: Option<(f64, f64)>) {
    if let Some(position) = position {
        env.dialog.set_position(position);
    }
    if let Some(text) = value
        && let Some(dialog) = env.prepared.as_mut()
    {
        let _ = dialog.set_tile(key, Arc::from(text));
    }
}

/// Bind the callback variables and remember their previous values for the frame that restores them.
fn bind_dialog_action(env: &mut Env, key: &str, value: Arc<str>, reason: i32) -> Vec<(Sym, Option<Value>)> {
    let bindings = [
        (Sym::new("$KEY"), Value::Str(Arc::from(key))),
        (Sym::new("$VALUE"), Value::Str(value)),
        (Sym::new("$REASON"), Value::Int(reason)),
        (Sym::new("$DATA"), Value::Nil),
        (Sym::new("$X"), Value::Nil),
        (Sym::new("$Y"), Value::Nil),
    ];
    let mut saved = Vec::with_capacity(bindings.len());
    for (sym, bound) in bindings {
        saved.push((sym.clone(), env.globals.get(&sym).cloned()));
        env.set(&sym, bound);
    }
    saved
}

/// Why `start_dialog` must fail for this run, before the dialog is activated.
fn dialog_refusal_of(run: &Run) -> Option<&'static str> {
    if run.blocking {
        Some("a dialog cannot start from a blocking call")
    } else if run.scripted {
        Some("a dialog cannot start from a script")
    } else {
        None
    }
}

fn file_key(name: &str) -> String {
    let base = std::path::Path::new(&name.replace('\\', "/")).file_name().and_then(std::ffi::OsStr::to_str).unwrap_or(name).to_ascii_lowercase();
    base.strip_suffix(".lsp").map(str::to_string).unwrap_or(base)
}

#[cfg(test)]
mod tests;
#[cfg(test)]
mod ucs_tests;

#[cfg(test)]
mod block_tests;

#[cfg(test)]
mod dcl_tests;
