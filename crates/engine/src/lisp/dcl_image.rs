//! Load one slide into the pending image of a prepared dialog.
//!
//! The file is read with the same cap as other hostile local files. Bytes already read count
//! against the Lisp run even when the slide, the member, or the geometry is rejected. A rejection
//! leaves the published image and any earlier pending picture in place.

use std::path::Path;
#[cfg(not(target_arch = "wasm32"))]
use std::path::PathBuf;
#[cfg(not(target_arch = "wasm32"))]
use std::sync::OnceLock;

#[cfg(not(target_arch = "wasm32"))]
use cadcraft_geom::{Bounds2, Vec2};
#[cfg(not(target_arch = "wasm32"))]
use cadcraft_render::{clip, triangulate_evenodd};

use super::LispError;
#[cfg(not(target_arch = "wasm32"))]
use super::dcl_load::is_readable_file;
use super::dcl_prepare::PreparedDialog;
#[cfg(not(target_arch = "wasm32"))]
use super::dcl_prepare::{DialogPicture, DialogPrimitive, DialogSolid, DialogVector};
#[cfg(not(target_arch = "wasm32"))]
use super::value::charge_bytes;

/// Most vectors and solids accepted from one decoded slide.
#[cfg(not(target_arch = "wasm32"))]
const MAX_SLIDE_PRIMS: usize = 4_096;

/// A level 2 slide after the host's decoder has checked the file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SlideSource {
    pub high_x: u16,
    pub high_y: u16,
    pub primitives: Vec<SlideSourcePrimitive>,
}

/// A decoded vector or solid, retained in file order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SlideSourcePrimitive {
    Vector(SlideSourceVector),
    Solid(SlideSourceSolid),
}

/// One source line. The origin is the lower-left corner of the slide.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SlideSourceVector {
    pub x1: i32,
    pub y1: i32,
    pub x2: i32,
    pub y2: i32,
    pub color: u8,
}

/// One source polygon.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SlideSourceSolid {
    pub points: Vec<(i32, i32)>,
    pub color: u8,
}

#[cfg(not(target_arch = "wasm32"))]
struct SlideHooks {
    decode: fn(&[u8]) -> Option<SlideSource>,
    member: fn(&[u8], &str) -> Option<(usize, usize)>,
}

#[cfg(not(target_arch = "wasm32"))]
static SLIDES: OnceLock<SlideHooks> = OnceLock::new();

/// Install the slide decoder. The engine does not read the file format itself.
/// A second call leaves the first decoder in place.
pub fn set_slide_decoder(decode: fn(&[u8]) -> Option<SlideSource>, member: fn(&[u8], &str) -> Option<(usize, usize)>) {
    #[cfg(not(target_arch = "wasm32"))]
    let _ = SLIDES.set(SlideHooks { decode, member });
    #[cfg(target_arch = "wasm32")]
    let _ = (decode, member);
}

/// Most bytes read from one slide or one library. One extra byte distinguishes "too big".
#[cfg(not(target_arch = "wasm32"))]
pub(crate) const MAX_SLIDE_FILE: usize = 1 << 20;

/// Place `name` into the pending image. `Ok(false)` is a Lisp nil and keeps the previous picture.
#[cfg(target_arch = "wasm32")]
pub(crate) fn show_slide(
    _dialog: &mut PreparedDialog,
    _x1: i32,
    _y1: i32,
    _width: i32,
    _height: i32,
    _name: &str,
    _support: Option<&Path>,
) -> Result<bool, LispError> {
    Ok(false)
}

/// Place `name` into the pending image. `Ok(false)` is a Lisp nil and keeps the previous picture.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn show_slide(
    dialog: &mut PreparedDialog,
    x1: i32,
    y1: i32,
    width: i32,
    height: i32,
    name: &str,
    support: Option<&Path>,
) -> Result<bool, LispError> {
    let Some((dim_x, dim_y)) = dialog.pending_extent() else { return Ok(false) };
    let Some(x2) = x1.checked_add(width) else { return Ok(false) };
    let Some(y2) = y1.checked_add(height) else { return Ok(false) };
    if x1 < 0 || y1 < 0 || width <= 0 || height <= 0 || x2 > dim_x || y2 > dim_y {
        return Ok(false);
    }
    let Some(hooks) = SLIDES.get() else { return Ok(false) };
    let Some((path, member)) = find_slide(name, support) else { return Ok(false) };
    let Some(bytes) = read_file(&path)? else { return Ok(false) };
    let encoded: &[u8] = if let Some(member) = member {
        let Some((start, end)) = (hooks.member)(&bytes, member) else { return Ok(false) };
        let Some(span) = bytes.get(start..end) else { return Ok(false) };
        span
    } else {
        bytes.as_slice()
    };
    let Some(slide) = (hooks.decode)(encoded) else { return Ok(false) };
    let destination = Bounds2::new(Vec2::new(f64::from(x1), f64::from(y1)), Vec2::new(f64::from(x2), f64::from(y2)));
    let tile = Bounds2::new(Vec2::ZERO, Vec2::new(f64::from(dim_x), f64::from(dim_y)));
    let Some(available) = dialog.pending_geometry_space() else { return Ok(false) };
    let Some(picture) = place(slide, destination, &tile, available) else { return Ok(false) };
    Ok(dialog.store_pending(picture))
}

#[cfg(not(target_arch = "wasm32"))]
fn find_slide<'a>(name: &'a str, support: Option<&Path>) -> Option<(PathBuf, Option<&'a str>)> {
    let (file, member) = split_spec(name)?;
    let requested = if Path::new(file).extension().is_some() {
        PathBuf::from(file)
    } else {
        Path::new(file).with_extension(if member.is_some() { "slb" } else { "sld" })
    };
    let path = if is_readable_file(&requested) {
        requested
    } else if requested.is_absolute() {
        return None;
    } else {
        let candidate = support?.join(&requested);
        if !is_readable_file(&candidate) {
            return None;
        }
        candidate
    };
    Some((path, member))
}

#[cfg(not(target_arch = "wasm32"))]
fn split_spec(spec: &str) -> Option<(&str, Option<&str>)> {
    if spec.is_empty() || spec.contains('\0') {
        return None;
    }
    if let Some(open) = spec.rfind('(')
        && spec.ends_with(')')
        && open > 0
    {
        let member = spec.get(open.checked_add(1)?..spec.len().checked_sub(1)?)?;
        if member.is_empty() || member.contains('\0') || member.contains('(') || member.contains(')') {
            return None;
        }
        return Some((spec.get(..open)?, Some(member)));
    }
    Some((spec, None))
}

#[cfg(not(target_arch = "wasm32"))]
fn read_file(path: &Path) -> Result<Option<Vec<u8>>, LispError> {
    if crate::file_access::check_read(path).is_err() {
        return Ok(None);
    }
    let mut buf = Vec::new();
    let read = crate::capped_read::read_capped_into(path, MAX_SLIDE_FILE, &mut buf);
    charge_bytes(buf.len())?;
    Ok(read.is_ok().then_some(buf))
}

#[cfg(not(target_arch = "wasm32"))]
fn place(slide: SlideSource, destination: Bounds2, tile: &Bounds2, mut available: usize) -> Option<DialogPicture> {
    if slide.primitives.len() > MAX_SLIDE_PRIMS {
        return None;
    }
    let x_scale = destination.width() / f64::from(slide.high_x.max(1));
    let y_scale = destination.height() / f64::from(slide.high_y.max(1));
    let project = |x, y| Vec2::new(destination.min.x + f64::from(x) * x_scale, destination.max.y - f64::from(y) * y_scale);
    let tile_point = |p: Vec2| (logical_pixel(p.x, tile.max.x), logical_pixel(p.y, tile.max.y));
    let mut primitives = Vec::new();
    for primitive in slide.primitives {
        let mapped = match primitive {
            SlideSourcePrimitive::Vector(line) => {
                let Some((a, b)) = clip::clip_segment(project(line.x1, line.y1), project(line.x2, line.y2), tile) else { continue };
                let (x1, y1) = tile_point(a);
                let (x2, y2) = tile_point(b);
                DialogPrimitive::Vector(DialogVector { x1, y1, x2, y2, color: line.color })
            }
            SlideSourcePrimitive::Solid(solid) => {
                if !(3..=10).contains(&solid.points.len()) {
                    return None;
                }
                let polygon = solid.points.into_iter().map(|(x, y)| project(x, y)).collect();
                let triangles = clip::clip_triangles(&triangulate_evenodd(&[polygon]), tile);
                if triangles.is_empty() {
                    continue;
                }
                let triangles = triangles.into_iter().map(tile_point).collect();
                DialogPrimitive::Solid(DialogSolid { triangles, color: solid.color })
            }
        };
        available = available.checked_sub(mapped.geometry_bytes())?;
        primitives.push(mapped);
    }
    Some(DialogPicture { primitives })
}

/// Round finite, clipped coordinates to pixels in the tile's bounded range.
#[cfg(not(target_arch = "wasm32"))]
#[allow(clippy::cast_possible_truncation)]
fn logical_pixel(value: f64, max: f64) -> i32 {
    value.round().clamp(0.0, max) as i32
}
