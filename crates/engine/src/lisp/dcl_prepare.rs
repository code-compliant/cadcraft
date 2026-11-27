//! Resolve a loaded DCL dialog and its initial tile values.
//!
//! Definitions and attributes are borrowed from the loader. Each file is already size-checked.
//! Preparing a dialog also bounds prototype depth, expanded tiles, traversal work, and retained
//! text. The Lisp run pays for the key and value text before the new state replaces the old one.
//! Tile keys are case-sensitive. Unspecified values start empty. A list starts unselected.
//! A popup starts at index zero. `list` keeps every row, including an empty row.

use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::fmt::{self, Write as _};
use std::sync::Arc;

use super::LispError;
use super::dcl::{self, AttributeValue, Definition, Element, Item, Tile};
use super::dcl_load::LoadedDialog;
use super::value::charge_bytes;

/// Most tile references one preparation expands, including nested clusters.
const MAX_EXPANDED_TILES: usize = 4_096;
/// Most user definitions on one prototype path. Primitive tiles have no definition in the file.
const MAX_PROTO_DEPTH: usize = 32;
/// Most prototype lookups and body elements one preparation visits, including cache hits.
const MAX_WALK_STEPS: usize = 65_536;
/// Most bytes of key, label, value, and row text one prepared dialog retains.
const MAX_RETAINED_TEXT: usize = dcl::MAX_INPUT_BYTES;
/// Most rows in one version of a list or popup.
pub(crate) const MAX_LIST_ROWS: usize = 32_768;
/// Count empty rows and open updates too. A full list can keep a full replacement pending.
const MAX_DIALOG_LIST_ROWS: usize = 2 * MAX_LIST_ROWS;
/// Focus, selection, and highlight requests kept for the dialog adapter.
const MAX_DIALOG_REQUESTS: usize = 64;
/// Most geometry bytes one dialog keeps, including a pending replacement.
///
/// A rejected slide does not take this space. Its file bytes still count against the Lisp run.
pub(crate) const MAX_IMAGE_BYTES: usize = 256 * 1024;
/// Character cell used for image width, in logical pixels.
const IMAGE_CHAR_WIDTH: f64 = 8.0;
/// Character cell used for image height, in logical pixels.
const IMAGE_CHAR_HEIGHT: f64 = 16.0;
/// Largest image edge, in logical pixels. The tile coordinate is one less.
const MAX_IMAGE_EDGE: i32 = 4_096;

/// A tile change the dialog adapter applies when it draws the active dialog.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DialogRequest {
    Focus(String),
    Select(String),
    Highlight(String),
}

/// One tile in source order. A container keeps its children in that same order.
///
/// The desktop draws this tree. Layout text stays inside the preparation limits, and the tree
/// does not name a toolkit.
#[derive(Clone, Debug, PartialEq, Eq)]
struct DialogNode {
    kind: Arc<str>,
    key: Option<Arc<str>>,
    label: Arc<str>,
    /// Display text on a tile without a key lives in the layout instead of the value table.
    unkeyed_value: Option<Arc<str>>,
    /// Children sit in a row. Otherwise they stack.
    row: bool,
    /// Rows of a keyless list or popup. A keyed control reads the live rows instead.
    rows: Arc<Vec<Arc<str>>>,
    multiple: bool,
    children: Vec<DialogNode>,
}

/// One control with the value and enabled state the dialog holds now.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DialogControl {
    pub kind: Arc<str>,
    pub key: Option<Arc<str>>,
    /// Text displayed beside an input, on a button, or in a text tile.
    pub label: Arc<str>,
    pub value: Arc<str>,
    /// Rows of a list box or popup, in source order. Empty for every other tile.
    pub rows: Arc<Vec<Arc<str>>>,
    /// A list box can keep more than one selected index. A popup has one.
    pub multiple: bool,
    pub enabled: bool,
    pub row: bool,
    pub children: Vec<DialogControl>,
    /// Logical size and published slide for an image tile. Absent for every other tile.
    pub image: Option<DialogImage>,
}

/// Logical size of one image tile and the slide `end_image` last published there.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DialogImage {
    /// Largest X coordinate. The pixel count is one more.
    pub dim_x: i32,
    /// Largest Y coordinate. The origin is the top-left corner.
    pub dim_y: i32,
    pub picture: Option<Arc<DialogPicture>>,
}

/// Vectors and solids already mapped into the tile. Shared so a frame does not copy them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DialogPicture {
    pub primitives: Vec<DialogPrimitive>,
}

/// Slide geometry in drawing order, including lines that a later fill covers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DialogPrimitive {
    Vector(DialogVector),
    Solid(DialogSolid),
}

impl DialogPrimitive {
    pub(crate) fn geometry_bytes(&self) -> usize {
        let points = match self {
            Self::Vector(_) => 0,
            Self::Solid(solid) => solid.triangles.len(),
        };
        std::mem::size_of::<Self>().saturating_add(points.saturating_mul(std::mem::size_of::<(i32, i32)>()))
    }
}

impl DialogPicture {
    fn geometry_bytes(&self) -> usize {
        self.primitives.iter().map(DialogPrimitive::geometry_bytes).sum()
    }
}

/// One slide line in tile coordinates.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DialogVector {
    pub x1: i32,
    pub y1: i32,
    pub x2: i32,
    pub y2: i32,
    pub color: u8,
}

/// One filled polygon tessellated into triangles in tile coordinates.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DialogSolid {
    pub triangles: Vec<(i32, i32)>,
    pub color: u8,
}

/// Title and controls for the dialog `id` identifies.
#[derive(Clone, Debug, PartialEq)]
pub struct DialogView {
    pub id: u64,
    pub title: Arc<str>,
    pub position: Option<(f64, f64)>,
    pub controls: Vec<DialogControl>,
}

/// One tile event for the dialog `id` identifies. A late event keeps its old id, so it cannot
/// reach a successor that reused the same key.
#[derive(Clone, Copy, Debug)]
pub struct DialogEvent<'a> {
    pub id: u64,
    pub key: &'a str,
    pub reason: i32,
    pub value: Option<&'a str>,
    pub position: Option<(f64, f64)>,
}

/// Whether the dialog took an adapter event. Acceptance can leave a callback running.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DialogEventOutcome {
    Applied,
    Ignored,
    TextLimit,
}

/// What one tile event does. An explicit action is an `action_tile` override or the tile's own
/// DCL action. A dialog default does not take an exit tile away from its standard status.
pub(crate) enum DialogDispatch {
    Missing,
    Ignore,
    Update,
    Close(i32),
    Run(Arc<str>),
}

/// The one dialog `start_dialog` is waiting on. It stays separate from the prepared tiles so
/// unloading the file during a callback can still finish the wait.
#[derive(Clone, Debug)]
pub(crate) struct DialogRun {
    active: bool,
    /// Session-unique id assigned when this wait starts. Zero while no dialog is active.
    id: u64,
    in_action: bool,
    close_status: Option<i32>,
    position: (f64, f64),
}

impl Default for DialogRun {
    fn default() -> Self {
        Self { active: false, id: 0, in_action: false, close_status: None, position: (0.0, 0.0) }
    }
}

impl DialogRun {
    pub(crate) fn active(&self) -> bool {
        self.active
    }

    pub(crate) fn in_action(&self) -> bool {
        self.in_action
    }

    pub(crate) fn id(&self) -> u64 {
        self.id
    }

    pub(crate) fn activate(&mut self, id: u64, position: Option<(f64, f64)>) {
        self.active = true;
        self.id = id;
        self.in_action = false;
        self.close_status = None;
        self.position = position.unwrap_or((0.0, 0.0));
    }

    pub(crate) fn set_position(&mut self, position: (f64, f64)) {
        self.position = position;
    }

    pub(crate) fn deactivate(&mut self) {
        *self = DialogRun::default();
    }

    pub(crate) fn begin_action(&mut self) {
        self.in_action = true;
    }

    /// The callback finished. `Some` is the status `start_dialog` returns.
    pub(crate) fn finish_action(&mut self) -> Option<i32> {
        self.in_action = false;
        let status = self.close_status.take();
        if status.is_some() {
            self.active = false;
        }
        status
    }

    /// Record `(done_dialog)`. The first status wins. `None` when no callback is running.
    pub(crate) fn done(&mut self, status: i32) -> Option<(f64, f64)> {
        if !self.in_action {
            return None;
        }
        if self.close_status.is_none() {
            self.close_status = Some(status);
        }
        Some(self.position)
    }

    /// Record `(term_dialog)` as status -1. Later calls do not replace a status already chosen.
    pub(crate) fn term(&mut self) {
        if self.in_action && self.close_status.is_none() {
            self.close_status = Some(-1);
        }
    }
}

pub struct PreparedDialog {
    /// Used when the prepared dialog starts and when the file sets no title.
    pub name: Arc<str>,
    pub handle: i32,
    pub action: Option<Arc<str>>,
    pub position: Option<(f64, f64)>,
    root: DialogNode,
    /// A missing value keeps the static label. An explicit empty value can clear it.
    tiles: HashMap<Arc<str>, Option<Arc<str>>>,
    lists: HashMap<Arc<str>, ListTile>,
    /// Rows accepted by `add_list` and not yet committed by `end_list`.
    pending: Option<ListUpdate>,
    retained: usize,
    retained_rows: usize,
    /// Action attributes from the DCL source, including an empty string when the attribute is present.
    dcl_actions: HashMap<String, Arc<str>>,
    overrides: HashMap<String, Arc<str>>,
    override_bytes: usize,
    disabled: HashSet<String>,
    requests: Vec<DialogRequest>,
    extents: HashMap<Arc<str>, ImageExtent>,
    images: HashMap<Arc<str>, Arc<DialogPicture>>,
    image_bytes: usize,
    image_pending: Option<PendingImage>,
}

impl PreparedDialog {
    pub fn get_tile(&self, key: &str) -> Option<Arc<str>> {
        Some(self.tiles.get(key)?.clone().unwrap_or_default())
    }

    /// Replace the value for an existing key. A rejected update leaves the value intact.
    pub fn set_tile(&mut self, key: &str, value: Arc<str>) -> Option<Arc<str>> {
        if value.len() > dcl::MAX_STRING_BYTES {
            return None;
        }
        let tile = self.tiles.get_mut(key)?;
        let retained = self.retained.checked_sub(tile.as_ref().map_or(0, |text| text.len()))?.checked_add(value.len())?;
        if retained > MAX_RETAINED_TEXT {
            return None;
        }
        self.retained = retained;
        *tile = Some(value.clone());
        Some(value)
    }

    /// Whether `set_tile` can store `len` bytes for `key` without changing the dialog.
    pub(crate) fn can_set_tile(&self, key: &str, len: usize) -> bool {
        if len > dcl::MAX_STRING_BYTES {
            return false;
        }
        let Some(tile) = self.tiles.get(key) else { return false };
        let Some(without) = self.retained.checked_sub(tile.as_ref().map_or(0, |text| text.len())) else { return false };
        let Some(next) = without.checked_add(len) else { return false };
        next <= MAX_RETAINED_TEXT
    }

    pub(crate) fn list_rows_current(&self, key: &str, rows: &Arc<Vec<Arc<str>>>, commit_at: Option<usize>) -> bool {
        let Some(list) = self.lists.get(key) else { return false };
        match commit_at {
            Some(index) => list.rows.get(index).zip(rows.get(index)).is_some_and(|(current, clicked)| Arc::ptr_eq(current, clicked)),
            None => Arc::ptr_eq(&list.rows, rows),
        }
    }

    /// User input can select only rows in the current list. SET_TILE can stage a future selection.
    pub(crate) fn valid_event_value(&self, key: &str, value: &str) -> bool {
        let Some(list) = self.lists.get(key) else { return true };
        let valid = |item| parse_row(item).is_some_and(|index| index < list.rows.len());
        if list.multiple { value.split_whitespace().all(valid) } else { (!list.popup && value.is_empty()) || valid(value.trim()) }
    }

    /// Store an `action_tile` override. `Ok(false)` leaves the previous action in place.
    /// A budget error allocates nothing.
    pub(crate) fn set_action(&mut self, key: &str, action: Arc<str>) -> Result<bool, LispError> {
        if !self.tiles.contains_key(key) || action.len() > dcl::MAX_STRING_BYTES {
            return Ok(false);
        }
        let old = self.overrides.get(key).map_or(0, |text| text.len());
        let Some(without) = self.override_bytes.checked_sub(old) else { return Ok(false) };
        let Some(next) = without.checked_add(action.len()) else { return Ok(false) };
        if next > MAX_RETAINED_TEXT {
            return Ok(false);
        }
        // The key copy and the retained action are new text. Pay before either allocation.
        charge_bytes(key.len().saturating_add(action.len()))?;
        self.override_bytes = next;
        self.overrides.insert(key.to_string(), action);
        Ok(true)
    }

    /// Apply one `mode_tile` value. Modes 2, 3, and 4 are carried for the adapter until the
    /// request list is full. An unknown mode or key changes nothing.
    pub(crate) fn set_mode(&mut self, key: &str, mode: i32) -> Result<(), LispError> {
        if !self.tiles.contains_key(key) {
            return Ok(());
        }
        match mode {
            0 => {
                self.disabled.remove(key);
            }
            1 if !self.disabled.contains(key) => {
                charge_bytes(key.len())?;
                self.disabled.insert(key.to_string());
            }
            2..=4 if self.requests.len() < MAX_DIALOG_REQUESTS => {
                charge_bytes(key.len())?;
                let owned = key.to_string();
                self.requests.push(match mode {
                    2 => DialogRequest::Focus(owned),
                    3 => DialogRequest::Select(owned),
                    _ => DialogRequest::Highlight(owned),
                });
            }
            _ => {}
        }
        Ok(())
    }

    pub(crate) fn tile_enabled(&self, key: &str) -> Option<bool> {
        if !self.tiles.contains_key(key) {
            return None;
        }
        self.enabled_for_key(&self.root, key, true)
    }

    /// Only the prepared, depth-limited tree participates in event dispatch.
    fn enabled_for_key(&self, node: &DialogNode, key: &str, parent_enabled: bool) -> Option<bool> {
        let enabled = self.node_enabled(node, parent_enabled);
        if node.key.as_deref() == Some(key) {
            return Some(enabled);
        }
        node.children.iter().find_map(|child| self.enabled_for_key(child, key, enabled))
    }

    fn node_enabled(&self, node: &DialogNode, parent_enabled: bool) -> bool {
        parent_enabled && node.key.as_deref().is_none_or(|key| !self.disabled.contains(key))
    }

    pub(crate) fn take_requests(&mut self) -> Vec<DialogRequest> {
        std::mem::take(&mut self.requests)
    }

    /// Find a tile that can be prepared but has no interactive desktop implementation.
    pub(crate) fn unsupported_control(&self) -> Option<&str> {
        fn first(node: &DialogNode) -> Option<&str> {
            if matches!(node.kind.as_ref(), "radio_button" | "slider" | "image_button") {
                return Some(node.kind.as_ref());
            }
            node.children.iter().find_map(first)
        }
        first(&self.root)
    }

    /// The layout with the values and enabled state held now.
    ///
    /// Preparation already capped the tile count and the nesting, so this walk stays inside that tree.
    pub fn view(&self, id: u64) -> DialogView {
        let title = if self.root.label.is_empty() { self.name.clone() } else { self.root.label.clone() };
        let enabled = self.node_enabled(&self.root, true);
        DialogView { id, title, position: self.position, controls: self.root.children.iter().map(|node| self.control(node, enabled)).collect() }
    }

    fn control(&self, node: &DialogNode, parent_enabled: bool) -> DialogControl {
        let value = node.key.as_ref().and_then(|key| self.tiles.get(key)).and_then(Option::as_ref).or(node.unkeyed_value.as_ref());
        let label = if is_text_tile(&node.kind) { value.unwrap_or(&node.label) } else { &node.label };
        let listed = node.key.as_ref().and_then(|key| self.lists.get(key));
        let rows = listed.map(|list| list.rows.clone()).unwrap_or_else(|| node.rows.clone());
        let multiple = listed.map(|list| list.multiple).unwrap_or(node.multiple);
        let enabled = self.node_enabled(node, parent_enabled);
        DialogControl {
            kind: node.kind.clone(),
            key: node.key.clone(),
            label: label.clone(),
            value: value.cloned().unwrap_or_default(),
            rows,
            multiple,
            enabled,
            row: node.row,
            children: node.children.iter().map(|child| self.control(child, enabled)).collect(),
            image: node.key.as_ref().and_then(|key| self.image_for(key)),
        }
    }

    fn image_for(&self, key: &Arc<str>) -> Option<DialogImage> {
        let extent = self.extents.get(key)?;
        Some(DialogImage { dim_x: extent.dim_x, dim_y: extent.dim_y, picture: self.images.get(key).cloned() })
    }

    /// Largest zero-based X of an image tile. A missing key or a tile without an extent is absent.
    pub(crate) fn dim_x(&self, key: &str) -> Option<i32> {
        self.extents.get(key).map(|extent| extent.dim_x)
    }

    /// Largest zero-based Y of an image tile.
    pub(crate) fn dim_y(&self, key: &str) -> Option<i32> {
        self.extents.get(key).map(|extent| extent.dim_y)
    }

    /// Begin a pending image. A rejected key leaves the previous pending image in place.
    pub(crate) fn start_image(&mut self, key: &str) -> Option<Arc<str>> {
        let stored = self.extents.get_key_value(key)?.0.clone();
        self.image_pending = Some(PendingImage { key: stored.clone(), picture: None, bytes: 0 });
        Some(stored)
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn pending_extent(&self) -> Option<(i32, i32)> {
        let pending = self.image_pending.as_ref()?;
        let extent = self.extents.get(&pending.key)?;
        Some((extent.dim_x, extent.dim_y))
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn pending_geometry_space(&self) -> Option<usize> {
        let pending = self.image_pending.as_ref()?;
        MAX_IMAGE_BYTES.checked_sub(self.image_bytes.checked_add(pending.bytes)?)
    }

    /// Draw into the pending picture. Keep its earlier contents if the next slide does not fit.
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn store_pending(&mut self, mut picture: DialogPicture) -> bool {
        let bytes = picture.geometry_bytes();
        if self.pending_geometry_space().is_none_or(|space| bytes > space) {
            return false;
        }
        let Some(pending) = self.image_pending.as_mut() else { return false };
        if let Some(existing) = pending.picture.as_mut() {
            existing.primitives.append(&mut picture.primitives);
        } else {
            pending.picture = Some(picture);
        }
        pending.bytes += bytes;
        true
    }

    /// Publish a successful pending image. A failed draw leaves the last published image alone.
    pub(crate) fn end_image(&mut self) {
        let Some(PendingImage { key, picture: Some(picture), bytes }) = self.image_pending.take() else { return };
        let old = self.images.get(&key).map_or(0, |picture| picture.geometry_bytes());
        let Some(without) = self.image_bytes.checked_sub(old) else { return };
        let Some(next) = without.checked_add(bytes).filter(|next| *next <= MAX_IMAGE_BYTES) else { return };
        self.image_bytes = next;
        self.images.insert(key, Arc::new(picture));
    }

    #[cfg(test)]
    pub(crate) fn image_picture(&self, key: &str) -> Option<&DialogPicture> {
        self.images.get(key).map(Arc::as_ref)
    }

    pub(crate) fn action_for(&self, key: &str) -> DialogDispatch {
        match self.tile_enabled(key) {
            None => return DialogDispatch::Missing,
            Some(false) => return DialogDispatch::Ignore,
            Some(true) => {}
        }
        if let Some(text) = self.overrides.get(key).or_else(|| self.dcl_actions.get(key)) {
            return DialogDispatch::Run(text.clone());
        }
        // Standard exit keys close by themselves until an explicit action takes over.
        match key {
            "accept" => return DialogDispatch::Close(1),
            "cancel" => return DialogDispatch::Close(0),
            _ => {}
        }
        match &self.action {
            Some(text) => DialogDispatch::Run(text.clone()),
            None => DialogDispatch::Update,
        }
    }

    /// Open a list or popup for `end_list`. `None` leaves the rows and any pending update alone.
    pub(crate) fn start_list(&mut self, key: &str, operation: i32, index: i32) -> Option<Arc<str>> {
        if !matches!(operation, 1..=3) {
            return None;
        }
        let (stored_key, list) = self.lists.get_key_value(key)?;
        let stored_key = stored_key.clone();
        let len = list.rows.len();
        let row_index = if operation == 1 {
            if index < 0 {
                return None;
            }
            let row_index = usize::try_from(index).ok()?;
            if row_index >= len {
                return None;
            }
            row_index
        } else {
            0
        };
        if !self.discard_pending() {
            return None;
        }
        self.pending = Some(ListUpdate { key: stored_key.clone(), op: operation, index: row_index, rows: Vec::new(), bytes: 0 });
        Some(stored_key)
    }

    /// Add one row to the open update. `None` keeps the rows already accepted.
    pub(crate) fn add_list(&mut self, text: &str) -> Result<Option<Arc<str>>, LispError> {
        if text.len() > dcl::MAX_STRING_BYTES {
            return Ok(None);
        }
        let Some(update) = self.pending.as_ref() else { return Ok(None) };
        let key = update.key.clone();
        let op = update.op;
        let pending_rows = update.rows.len();
        let pending_bytes = update.bytes;
        let new_row = usize::from(op != 1 || pending_rows == 0);
        let Some(retained_rows) = self.retained_rows.checked_add(new_row).filter(|count| *count <= MAX_DIALOG_LIST_ROWS) else {
            return Ok(None);
        };
        let Some(list) = self.lists.get(&key) else { return Ok(None) };
        if op != 1 {
            let next_rows = if op == 3 {
                let Some(count) = pending_rows.checked_add(1) else { return Ok(None) };
                count
            } else {
                let Some(count) = list.rows.len().checked_add(pending_rows).and_then(|count| count.checked_add(1)) else { return Ok(None) };
                count
            };
            if next_rows > MAX_LIST_ROWS {
                return Ok(None);
            }
        }
        let replaced = if op == 1 { pending_bytes } else { 0 };
        let Some(new_bytes) = pending_bytes.checked_sub(replaced).and_then(|count| count.checked_add(text.len())) else { return Ok(None) };
        let Some(without) = self.retained.checked_sub(replaced) else { return Ok(None) };
        let Some(retained) = without.checked_add(text.len()) else { return Ok(None) };
        if retained > MAX_RETAINED_TEXT {
            return Ok(None);
        }
        // The dialog keeps its own copy. Pay for it before the copy exists.
        charge_bytes(text.len())?;
        let stored: Arc<str> = Arc::from(text);
        let Some(update) = self.pending.as_mut() else { return Ok(None) };
        self.retained = retained;
        self.retained_rows = retained_rows;
        if op == 1 {
            update.rows.clear();
            update.rows.push(stored.clone());
        } else {
            update.rows.push(stored.clone());
        }
        update.bytes = new_bytes;
        Ok(Some(stored))
    }

    /// Commit the open update. Nothing is open when this leaves the rows unchanged.
    pub(crate) fn end_list(&mut self) -> Result<(), LispError> {
        let Some(update) = self.pending.as_ref() else { return Ok(()) };
        let key = update.key.clone();
        let op = update.op;
        let index = update.index;
        let pending_bytes = update.bytes;
        let pending_count = update.rows.len();
        let Some(list) = self.lists.get(&key) else {
            let _ = self.discard_pending();
            return Ok(());
        };
        if (op == 1 || op == 2) && pending_count == 0 {
            self.pending = None;
            return Ok(());
        }
        let old_list_bytes = list.bytes;
        let old_len = list.rows.len();
        let old_row_bytes = list.rows.get(index).map(|row| row.len());
        let popup = list.popup;
        let multiple = list.multiple;
        let current = self.tiles.get(&key).and_then(Option::as_deref).unwrap_or("");
        if op == 1 && old_row_bytes.is_none() {
            return Ok(());
        }
        let new_len = match op {
            3 => pending_count,
            2 => {
                let Some(count) = old_len.checked_add(pending_count) else { return Ok(()) };
                count
            }
            1 => old_len,
            _ => return Ok(()),
        };
        if new_len > MAX_LIST_ROWS {
            return Ok(());
        }
        let Some(new_list_bytes) = (match op {
            3 => Some(pending_bytes),
            2 => old_list_bytes.checked_add(pending_bytes),
            1 => old_row_bytes.and_then(|old_row| old_list_bytes.checked_sub(old_row)).and_then(|count| count.checked_add(pending_bytes)),
            _ => None,
        }) else {
            return Ok(());
        };
        let Some(row_retained) = (match op {
            3 => self.retained.checked_sub(old_list_bytes),
            2 => Some(self.retained),
            1 => old_row_bytes.and_then(|old_row| self.retained.checked_sub(old_row)),
            _ => None,
        }) else {
            return Ok(());
        };
        let removed_rows = match op {
            3 => old_len,
            1 => 1,
            _ => 0,
        };
        let Some(retained_rows) = self.retained_rows.checked_sub(removed_rows) else { return Ok(()) };
        let next = selection_after_resize(current, new_len, popup, multiple);
        let next_len =
            next.as_ref().map_or(current.len(), |parts| parts.iter().map(|part| part.len()).sum::<usize>() + parts.len().saturating_sub(1));
        let Some(without_value) = row_retained.checked_sub(current.len()) else { return Ok(()) };
        let Some(retained) = without_value.checked_add(next_len) else { return Ok(()) };
        if retained > MAX_RETAINED_TEXT {
            return Ok(());
        }
        let replacement = match next {
            Some(parts) if !parts.is_empty() => {
                charge_bytes(next_len)?;
                Some(Some(Arc::from(parts.join(" "))))
            }
            Some(_) => Some(None),
            None => None,
        };
        let Some(update) = self.pending.take() else { return Ok(()) };
        self.retained = retained;
        self.retained_rows = retained_rows;
        if let Some(list) = self.lists.get_mut(&key) {
            match op {
                3 => list.rows = Arc::new(update.rows),
                2 => Arc::make_mut(&mut list.rows).extend(update.rows),
                1 => {
                    if let Some(row) = update.rows.into_iter().next()
                        && let Some(slot) = Arc::make_mut(&mut list.rows).get_mut(index)
                    {
                        *slot = row;
                    }
                }
                _ => {}
            }
            list.bytes = new_list_bytes;
        }
        if let Some(value) = replacement
            && let Some(tile) = self.tiles.get_mut(&key)
        {
            *tile = value;
        }
        Ok(())
    }

    /// Drop an uncommitted update and the bytes it was holding.
    fn discard_pending(&mut self) -> bool {
        let Some(update) = self.pending.as_ref() else { return true };
        let Some(retained) = self.retained.checked_sub(update.bytes) else { return false };
        let Some(retained_rows) = self.retained_rows.checked_sub(update.rows.len()) else { return false };
        self.retained = retained;
        self.retained_rows = retained_rows;
        self.pending = None;
        true
    }
}

/// Rows committed for one list box or popup.
struct ListTile {
    rows: Arc<Vec<Arc<str>>>,
    bytes: usize,
    multiple: bool,
    popup: bool,
}

struct ImageExtent {
    dim_x: i32,
    dim_y: i32,
}

struct PendingImage {
    key: Arc<str>,
    picture: Option<DialogPicture>,
    bytes: usize,
}

/// One `start_list` that `end_list` has not committed.
struct ListUpdate {
    key: Arc<str>,
    /// 1 changes one row, 2 appends, and 3 replaces the whole list.
    op: i32,
    index: usize,
    rows: Vec<Arc<str>>,
    bytes: usize,
}

/// Borrow the surviving parts so the run can pay for the new value before it is copied.
/// None keeps the current selection string.
fn selection_after_resize(current: &str, len: usize, popup: bool, multiple: bool) -> Option<Vec<&str>> {
    let valid = |token| parse_row(token).is_some_and(|index| index < len);
    if popup {
        return (!valid(current.trim()) && current != "0").then(|| vec!["0"]);
    }
    if !multiple {
        return (!current.is_empty() && !valid(current.trim())).then(Vec::new);
    }
    let tokens = current.split_whitespace();
    if tokens.clone().all(valid) {
        return None;
    }
    let mut kept = HashSet::new();
    Some(tokens.filter(|token| parse_row(token).is_some_and(|index| index < len && kept.insert(index))).collect())
}

/// A decimal row index. A sign, a space, or any other character is not an index.
fn parse_row(text: &str) -> Option<usize> {
    if text.is_empty() || !text.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    text.parse().ok()
}

/// Prepare the named dialog. A missing or invalid definition returns nil to Lisp. A run budget
/// error stops the routine before any prepared state is replaced.
pub fn prepare(
    loaded: &LoadedDialog,
    name: Arc<str>,
    handle: i32,
    action: Option<Arc<str>>,
    position: Option<(f64, f64)>,
) -> Result<Option<PreparedDialog>, LispError> {
    let mut walk = Walk::new(loaded);
    let Some(definition) = walk.definitions.get(name.as_ref()).copied().flatten() else { return Ok(None) };
    let Some(layout) = walk.prepare(definition) else { return Ok(None) };
    // Action text and its key copies are retained with the tile values. Pay before those copies.
    let action_charge = walk.actions.iter().fold(0usize, |sum, (key, text)| sum.saturating_add(key.len()).saturating_add(text.len()));
    charge_bytes(walk.retained.saturating_add(action_charge))?;
    let tiles: HashMap<Arc<str>, Option<Arc<str>>> =
        walk.tiles.into_iter().map(|(key, value)| (Arc::from(key.as_ref()), value.map(|text| Arc::from(text.as_ref())))).collect();
    let dcl_actions = walk.actions.into_iter().map(|(key, text)| (key.into_owned(), Arc::from(text.as_ref()))).collect();
    let mut lists = HashMap::new();
    for (key, spec) in walk.lists {
        let Some((key, _)) = tiles.get_key_value(key.as_ref()) else { return Ok(None) };
        let rows = copy_list_rows(spec.rows);
        lists.insert(key.clone(), ListTile { rows, bytes: spec.bytes, multiple: spec.multiple, popup: spec.popup });
    }
    let root = own_node(layout, &tiles);
    let mut extents = HashMap::new();
    for (key, extent) in walk.extents {
        if let Some((key, _)) = tiles.get_key_value(key.as_ref()) {
            extents.insert(key.clone(), extent);
        }
    }
    Ok(Some(PreparedDialog {
        name,
        handle,
        action,
        position,
        root,
        tiles,
        lists,
        pending: None,
        retained: walk.retained,
        retained_rows: walk.retained_rows,
        dcl_actions,
        overrides: HashMap::new(),
        override_bytes: 0,
        disabled: HashSet::new(),
        requests: Vec::new(),
        extents,
        images: HashMap::new(),
        image_bytes: 0,
        image_pending: None,
    }))
}

#[derive(Clone, Copy, Default)]
struct TileValues<'a> {
    key: Option<&'a AttributeValue>,
    value: Option<&'a AttributeValue>,
    action: Option<&'a AttributeValue>,
    label: Option<&'a AttributeValue>,
    list: Option<&'a AttributeValue>,
    multiple: Option<bool>,
    width: Option<&'a AttributeValue>,
    height: Option<&'a AttributeValue>,
    aspect: Option<&'a AttributeValue>,
}

/// Documented key and child tiles of a standard exit button or cluster.
#[derive(Clone, Copy, Default)]
struct StandardPart {
    key: Option<&'static str>,
    children: &'static [&'static str],
}

#[derive(Clone, Copy, Default)]
struct Resolved<'a> {
    values: TileValues<'a>,
    is_dialog: bool,
    /// Predefined tile that gives a prototype its control and layout.
    kind: Option<&'a str>,
    /// A subassembly's child tiles come from its definition.
    children: Option<&'a [Element]>,
    /// Longest path from this definition through its user-defined bases.
    height: usize,
    /// Keys and children supplied by a standard exit prototype when the file does not.
    standard: StandardPart,
}

struct LayoutNode<'a> {
    kind: &'a str,
    text: Kept<'a>,
    row: bool,
    children: Vec<LayoutNode<'a>>,
}

struct Kept<'a> {
    key: Option<Cow<'a, str>>,
    label: Option<Cow<'a, str>>,
    unkeyed_value: Option<&'a AttributeValue>,
    rows: Option<&'a str>,
    multiple: bool,
}

struct ListSpec<'a> {
    rows: Option<&'a str>,
    bytes: usize,
    multiple: bool,
    popup: bool,
}

struct Walk<'a> {
    /// None marks a name that is defined more than once.
    definitions: HashMap<&'a str, Option<&'a Definition>>,
    /// None marks a definition still on the prototype path.
    resolved: HashMap<&'a str, Option<Resolved<'a>>>,
    tiles: HashMap<Cow<'a, str>, Option<Cow<'a, str>>>,
    lists: HashMap<Cow<'a, str>, ListSpec<'a>>,
    retained: usize,
    retained_rows: usize,
    actions: HashMap<Cow<'a, str>, Cow<'a, str>>,
    action_bytes: usize,
    steps_left: usize,
    tiles_left: usize,
    extents: HashMap<Cow<'a, str>, ImageExtent>,
}

impl<'a> Walk<'a> {
    fn new(loaded: &'a LoadedDialog) -> Self {
        let mut definitions = HashMap::new();
        for item in loaded.files.iter().flat_map(|file| &file.parsed.items) {
            if let Item::Definition(def) = item {
                definitions.entry(def.name.as_str()).and_modify(|entry| *entry = None).or_insert(Some(def));
            }
        }
        Self {
            definitions,
            resolved: HashMap::new(),
            tiles: HashMap::new(),
            lists: HashMap::new(),
            retained: 0,
            retained_rows: 0,
            actions: HashMap::new(),
            action_bytes: 0,
            steps_left: MAX_WALK_STEPS,
            tiles_left: MAX_EXPANDED_TILES,
            extents: HashMap::new(),
        }
    }

    fn prepare(&mut self, definition: &'a Definition) -> Option<LayoutNode<'a>> {
        let root = self.resolve(&definition.name, MAX_PROTO_DEPTH)?;
        if !root.is_dialog {
            return None;
        }
        let text = self.record(root.values, root.standard.key, "dialog")?;
        let children = self.expand_nodes(&definition.body, 1)?;
        Some(LayoutNode { kind: "dialog", text, row: false, children })
    }

    fn step(&mut self) -> Option<()> {
        self.steps_left = self.steps_left.checked_sub(1)?;
        Some(())
    }

    fn resolve(&mut self, name: &'a str, height_limit: usize) -> Option<Resolved<'a>> {
        self.step()?;
        let Some(definition) = self.definitions.get(name).copied() else {
            // Predefined tiles contribute no user definition depth. Exit prototypes carry their
            // documented keys so a cluster can expose them without attributes in the file.
            return predefined_tile(name).then_some(Resolved {
                is_dialog: name == "dialog",
                kind: Some(name),
                standard: standard_exit(name),
                ..Resolved::default()
            });
        };
        let definition = definition?;
        let child_limit = height_limit.checked_sub(1)?;
        if let Some(entry) = self.resolved.get(name) {
            return entry.filter(|resolved| resolved.height <= height_limit);
        }
        self.resolved.insert(&definition.name, None);
        let mut resolved = Resolved { height: 1, is_dialog: name == "dialog", ..Resolved::default() };
        for (index, name) in definition.bases.iter().enumerate() {
            let base = self.resolve(name, child_limit)?;
            resolved.height = resolved.height.max(1 + base.height);
            if index == 0 {
                resolved.is_dialog |= base.is_dialog;
                resolved.kind = base.kind;
            }
            // For each attribute the first base that supplies a value wins.
            resolved.values.key = resolved.values.key.or(base.values.key);
            resolved.values.value = resolved.values.value.or(base.values.value);
            resolved.values.action = resolved.values.action.or(base.values.action);
            resolved.values.label = resolved.values.label.or(base.values.label);
            resolved.values.list = resolved.values.list.or(base.values.list);
            resolved.values.multiple = resolved.values.multiple.or(base.values.multiple);
            resolved.values.width = resolved.values.width.or(base.values.width);
            resolved.values.height = resolved.values.height.or(base.values.height);
            resolved.values.aspect = resolved.values.aspect.or(base.values.aspect);
            if resolved.standard.key.is_none() {
                resolved.standard.key = base.standard.key;
            }
            if resolved.standard.children.is_empty() {
                resolved.standard.children = base.standard.children;
            }
        }
        if self.scan_body(&definition.body, &mut resolved.values)? {
            resolved.children = Some(&definition.body);
        }
        self.resolved.insert(&definition.name, Some(resolved));
        Some(resolved)
    }

    /// Apply the local values and report whether this body defines children.
    fn scan_body(&mut self, body: &'a [Element], values: &mut TileValues<'a>) -> Option<bool> {
        let mut has_children = false;
        for element in body {
            self.step()?;
            match element {
                Element::Attribute { name, value, .. } if name == "key" => values.key = Some(value),
                Element::Attribute { name, value, .. } if name == "value" => values.value = Some(value),
                Element::Attribute { name, value, .. } if name == "action" => values.action = Some(value),
                Element::Attribute { name, value, .. } if name == "label" => values.label = Some(value),
                Element::Attribute { name, value, .. } if name == "list" => values.list = Some(value),
                Element::Attribute { name, value, .. } if name == "multiple_select" => values.multiple = Some(attribute_is_true(value)),
                Element::Attribute { name, value, .. } if name == "width" => values.width = Some(value),
                Element::Attribute { name, value, .. } if name == "height" => values.height = Some(value),
                Element::Attribute { name, value, .. } if name == "aspect_ratio" => values.aspect = Some(value),
                Element::Tile(_) => has_children = true,
                _ => {}
            }
        }
        Some(has_children)
    }

    fn expand_nodes(&mut self, body: &'a [Element], depth: usize) -> Option<Vec<LayoutNode<'a>>> {
        let mut nodes = Vec::new();
        for element in body {
            self.step()?;
            if let Element::Tile(tile) = element {
                nodes.push(self.expand(tile, depth)?);
            }
        }
        Some(nodes)
    }

    fn expand(&mut self, tile: &'a Tile, depth: usize) -> Option<LayoutNode<'a>> {
        self.tiles_left = self.tiles_left.checked_sub(1)?;
        if depth > dcl::MAX_DEPTH {
            return None;
        }
        let resolved = self.resolve(&tile.name, MAX_PROTO_DEPTH)?;
        let body = match resolved.children {
            Some(_) if tile.braced => return None,
            Some(children) => children,
            None => &tile.body,
        };
        let mut values = resolved.values;
        self.scan_body(&tile.body, &mut values)?;
        let standard_key = resolved.standard.key;
        let standard_children = resolved.standard.children;
        let kind = resolved.kind.unwrap_or("column");
        let text = self.record(values, standard_key, kind)?;
        let reference_children = tile.body.iter().any(|element| matches!(element, Element::Tile(_)));
        let children = if resolved.children.is_none() && !reference_children {
            let mut nodes = Vec::new();
            for child in standard_children {
                nodes.push(self.expand_named(child, depth + 1)?);
            }
            nodes
        } else {
            self.expand_nodes(body, depth + 1)?
        };
        Some(LayoutNode { kind, text, row: is_row(kind), children })
    }

    /// Expand one standard exit child. Its key comes from the public exit-button reference.
    fn expand_named(&mut self, name: &'a str, depth: usize) -> Option<LayoutNode<'a>> {
        self.tiles_left = self.tiles_left.checked_sub(1)?;
        if depth > dcl::MAX_DEPTH {
            return None;
        }
        let resolved = self.resolve(name, MAX_PROTO_DEPTH)?;
        let standard_key = resolved.standard.key;
        let standard_children = resolved.standard.children;
        let kind = resolved.kind.unwrap_or("column");
        let text = self.record(resolved.values, standard_key, kind)?;
        let mut children = Vec::new();
        for child in standard_children {
            children.push(self.expand_named(child, depth + 1)?);
        }
        if let Some(body) = resolved.children {
            children.extend(self.expand_nodes(body, depth + 1)?);
        }
        Some(LayoutNode { kind, text, row: is_row(kind), children })
    }

    /// Store one tile and charge all its retained text, including text on keyless display tiles.
    fn record(&mut self, values: TileValues<'a>, standard_key: Option<&'a str>, kind: &str) -> Option<Kept<'a>> {
        let keyed = values.key.is_some() || standard_key.is_some();
        let default_label = standard_label(kind);
        let value = values.value.filter(|_| keyed || is_text_tile(kind));
        let action = values.action.filter(|_| keyed);
        let is_popup = kind == "popup_list";
        let is_list = kind == "list_box" || is_popup;
        let row_text = match values.list.filter(|_| is_list) {
            Some(AttributeValue::Text(text)) => Some(text.as_str()),
            Some(_) => return None,
            None => None,
        };
        // Borrow the source until preparation checks the dialog and Lisp run budgets.
        let (row_count, row_bytes) = if let Some(text) = row_text {
            let separators = text.bytes().filter(|byte| *byte == b'\n').count();
            (separators.checked_add(1)?, text.len().checked_sub(separators)?)
        } else {
            (0, 0)
        };
        let retained_rows = self.retained_rows.checked_add(row_count)?;
        if row_count > MAX_LIST_ROWS || retained_rows > MAX_DIALOG_LIST_ROWS {
            return None;
        }
        let value_bytes = if is_popup && value.is_none() { 1 } else { attribute_bytes(value, None)? };
        // A prototype can expand the same text many times. Count defaults and formatted values
        // before allocating their strings, including values originally written as numbers.
        let retained = self
            .retained
            .checked_add(attribute_bytes(values.key, standard_key)?)?
            .checked_add(attribute_bytes(values.label, default_label)?)?
            .checked_add(value_bytes)?
            .checked_add(row_bytes)?;
        let action_bytes = self.action_bytes.checked_add(attribute_bytes(action, None)?)?;
        if retained > MAX_RETAINED_TEXT || action_bytes > MAX_RETAINED_TEXT {
            return None;
        }
        let key = values.key.map(attribute_text).or_else(|| standard_key.map(Cow::Borrowed));
        if key.as_ref().is_some_and(|key| self.tiles.contains_key(key.as_ref())) {
            return None;
        }
        self.retained = retained;
        self.retained_rows = retained_rows;
        self.action_bytes = action_bytes;
        let label = values.label.map(attribute_text).or_else(|| default_label.map(Cow::Borrowed));
        let multiple = is_list && kind == "list_box" && values.multiple == Some(true);
        let Some(key) = key else {
            // The display value stays borrowed until preparation pays the Lisp run's budget.
            return Some(Kept { key: None, label, unkeyed_value: value, rows: row_text, multiple });
        };
        if let Some(action) = action {
            self.actions.insert(key.clone(), attribute_text(action));
        }
        let stored = if is_popup && value.is_none() { Some(Cow::Borrowed("0")) } else { value.map(attribute_text) };
        self.tiles.insert(key.clone(), stored);
        if matches!(kind, "image" | "image_button")
            && let Some(extent) = image_extent(&values)
        {
            self.extents.insert(key.clone(), extent);
        }
        if is_list {
            self.lists.insert(key.clone(), ListSpec { rows: row_text, bytes: row_bytes, multiple, popup: is_popup });
        }
        Some(Kept { key: Some(key), label, unkeyed_value: None, rows: None, multiple })
    }
}

fn own_node(node: LayoutNode<'_>, tiles: &HashMap<Arc<str>, Option<Arc<str>>>) -> DialogNode {
    let Kept { key, label, unkeyed_value, rows, multiple } = node.text;
    DialogNode {
        kind: Arc::from(node.kind),
        key: key.as_deref().and_then(|key| tiles.get_key_value(key)).map(|(key, _)| key.clone()),
        label: Arc::from(label.as_deref().unwrap_or("")),
        unkeyed_value: unkeyed_value.map(|value| Arc::from(attribute_text(value).as_ref())),
        row: node.row,
        rows: copy_list_rows(rows),
        multiple,
        children: node.children.into_iter().map(|child| own_node(child, tiles)).collect(),
    }
}

fn is_text_tile(kind: &str) -> bool {
    matches!(kind, "text" | "text_part" | "errtile")
}

/// Clusters whose children are published in a row. Everything else stacks.
fn is_row(name: &str) -> bool {
    matches!(
        name,
        "boxed_radio_row"
            | "boxed_row"
            | "concatenation"
            | "ok_cancel"
            | "ok_cancel_help"
            | "ok_cancel_help_errtile"
            | "ok_cancel_help_info"
            | "ok_only"
            | "radio_row"
            | "row"
    )
}

/// Published labels of the standard exit buttons when the file does not set `label`.
fn standard_label(kind: &str) -> Option<&'static str> {
    match kind {
        "cancel_button" => Some("Cancel"),
        "help_button" => Some("Help"),
        "info_button" => Some("Info"),
        "ok_button" => Some("OK"),
        _ => None,
    }
}

/// Public DCL tile and prototype names, including the documented base settings and button
/// prototypes. Their default attributes and children belong to dialog control support.
///
/// [DCL tiles](https://help.autodesk.com/cloudhelp/2026/ENU/AutoCAD-AutoLISP-Reference/files/GUID-F8F5A79B-9A05-4E25-A6FC-9720216BA3E7.htm)
/// and [exit button prototypes](https://help.autodesk.com/cloudhelp/2026/ENU/AutoCAD-AutoLISP/files/GUID-71616D0C-9B91-4182-9D14-85D0145C2358.htm).
fn predefined_tile(name: &str) -> bool {
    matches!(
        name,
        "boxed_column"
            | "boxed_radio_column"
            | "boxed_radio_row"
            | "boxed_row"
            | "button"
            | "cancel_button"
            | "column"
            | "concatenation"
            | "default_button"
            | "default_dcl_settings"
            | "dialog"
            | "edit_box"
            | "errtile"
            | "help_button"
            | "image"
            | "image_button"
            | "info_button"
            | "list_box"
            | "ok_button"
            | "ok_cancel"
            | "ok_cancel_help"
            | "ok_cancel_help_errtile"
            | "ok_cancel_help_info"
            | "ok_only"
            | "paragraph"
            | "popup_list"
            | "radio_button"
            | "radio_column"
            | "radio_row"
            | "retirement_button"
            | "row"
            | "slider"
            | "spacer"
            | "spacer_0"
            | "spacer_1"
            | "text"
            | "text_part"
            | "tile"
            | "toggle"
    )
}

/// Keys published for the standard exit buttons and the clusters that contain them.
///
/// [ok_only](https://help.autodesk.com/cloudhelp/2026/ENU/AutoCAD-AutoLISP-Reference/files/GUID-0376DCD0-ACF6-4269-982E-E396E81C4D93.htm),
/// [ok_cancel](https://help.autodesk.com/cloudhelp/2026/ENU/AutoCAD-AutoLISP-Reference/files/GUID-BA3EADFA-E2D7-4D85-9282-3CBB0D83499E.htm),
/// [ok_cancel_help](https://help.autodesk.com/cloudhelp/2026/ENU/AutoCAD-AutoLISP-Reference/files/GUID-DF8DA7C3-7F7E-471C-A6B9-1A5D986CD6E9.htm),
/// [ok_cancel_help_info](https://help.autodesk.com/cloudhelp/2026/ENU/AutoCAD-AutoLISP-Reference/files/GUID-9FB4A683-23CA-4B6C-A5EF-5E43AE7C8D4B.htm),
/// and [errtile](https://help.autodesk.com/cloudhelp/2026/ENU/AutoCAD-AutoLISP-Reference/files/GUID-38298C0C-1F1C-479E-A72D-F432341EA958.htm).
fn standard_exit(name: &str) -> StandardPart {
    let part = |key: Option<&'static str>, children: &'static [&'static str]| StandardPart { key, children };
    match name {
        "ok_button" => part(Some("accept"), &[]),
        "cancel_button" => part(Some("cancel"), &[]),
        "help_button" => part(Some("help"), &[]),
        "info_button" => part(Some("info"), &[]),
        "errtile" => part(Some("error"), &[]),
        "ok_only" => part(None, &["ok_button"]),
        "ok_cancel" => part(None, &["ok_button", "cancel_button"]),
        "ok_cancel_help" => part(None, &["ok_button", "cancel_button", "help_button"]),
        "ok_cancel_help_info" => part(None, &["ok_button", "cancel_button", "help_button", "info_button"]),
        "ok_cancel_help_errtile" => part(None, &["ok_button", "cancel_button", "help_button", "errtile"]),
        _ => StandardPart::default(),
    }
}

/// Count bytes emitted by numeric `Display` without buffering the formatted string.
struct TextLength(usize);

impl fmt::Write for TextLength {
    fn write_str(&mut self, text: &str) -> fmt::Result {
        self.0 = self.0.checked_add(text.len()).ok_or(fmt::Error)?;
        Ok(())
    }
}

fn attribute_bytes(value: Option<&AttributeValue>, default: Option<&str>) -> Option<usize> {
    let mut length = TextLength(0);
    match value {
        Some(AttributeValue::Text(text) | AttributeValue::Word(text)) => return Some(text.len()),
        Some(AttributeValue::Int(value)) => write!(length, "{value}"),
        Some(AttributeValue::Real(value)) => write!(length, "{value}"),
        None => return Some(default.map_or(0, str::len)),
    }
    .ok()?;
    Some(length.0)
}

fn attribute_is_true(value: &AttributeValue) -> bool {
    match value {
        AttributeValue::Word(text) | AttributeValue::Text(text) => text == "true",
        AttributeValue::Int(value) => *value == 1,
        AttributeValue::Real(_) => false,
    }
}

/// Allocate row storage only after the whole dialog passes its limits.
fn copy_list_rows(text: Option<&str>) -> Arc<Vec<Arc<str>>> {
    Arc::new(text.into_iter().flat_map(|text| text.split('\n')).map(Arc::from).collect())
}

enum Measure {
    Missing,
    Bad,
    Ok(f64),
}

fn measure(value: Option<&AttributeValue>) -> Measure {
    match value {
        None => Measure::Missing,
        Some(value) => match positive_dimension(value) {
            Some(number) => Measure::Ok(number),
            None => Measure::Bad,
        },
    }
}

fn positive_dimension(value: &AttributeValue) -> Option<f64> {
    let number = match value {
        AttributeValue::Int(value) => f64::from(*value),
        AttributeValue::Real(value) => *value,
        AttributeValue::Text(text) | AttributeValue::Word(text) => text.parse().ok()?,
    };
    (number.is_finite() && number > 0.0).then_some(number)
}

fn edge_px(pixels: f64) -> Option<i32> {
    if !pixels.is_finite() || pixels < 1.0 || pixels > f64::from(MAX_IMAGE_EDGE) {
        return None;
    }
    let rounded = pixels.round();
    if !rounded.is_finite() || rounded < 1.0 || rounded > f64::from(MAX_IMAGE_EDGE) {
        return None;
    }
    let edge = finite_i32(rounded)?;
    (1..=MAX_IMAGE_EDGE).contains(&edge).then_some(edge)
}

fn finite_i32(value: f64) -> Option<i32> {
    if !value.is_finite() || value < f64::from(i32::MIN) || value > f64::from(i32::MAX) {
        return None;
    }
    #[allow(clippy::cast_possible_truncation)]
    let edge = value as i32;
    Some(edge)
}

fn scaled_edge(value: f64, scale: f64) -> Option<i32> {
    edge_px(value * scale)
}

/// Explicit character cells first. One missing cell comes from the width-to-height ratio.
fn image_extent(values: &TileValues<'_>) -> Option<ImageExtent> {
    let (width_px, height_px) = match (measure(values.width), measure(values.height), measure(values.aspect)) {
        (Measure::Ok(width), Measure::Ok(height), _) => (scaled_edge(width, IMAGE_CHAR_WIDTH)?, scaled_edge(height, IMAGE_CHAR_HEIGHT)?),
        (Measure::Ok(width), Measure::Missing, Measure::Ok(aspect)) => {
            let width_px = scaled_edge(width, IMAGE_CHAR_WIDTH)?;
            (width_px, edge_px(f64::from(width_px) / aspect)?)
        }
        (Measure::Missing, Measure::Ok(height), Measure::Ok(aspect)) => {
            let height_px = scaled_edge(height, IMAGE_CHAR_HEIGHT)?;
            (edge_px(f64::from(height_px) * aspect)?, height_px)
        }
        _ => return None,
    };
    Some(ImageExtent { dim_x: width_px.checked_sub(1)?, dim_y: height_px.checked_sub(1)? })
}

fn attribute_text(value: &AttributeValue) -> Cow<'_, str> {
    match value {
        AttributeValue::Text(text) | AttributeValue::Word(text) => Cow::Borrowed(text),
        AttributeValue::Int(value) => Cow::Owned(value.to_string()),
        AttributeValue::Real(value) => Cow::Owned(value.to_string()),
    }
}
