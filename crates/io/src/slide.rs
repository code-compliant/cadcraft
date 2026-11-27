//! Level 2 AutoCAD slide files and slide-library members.
//!
//! The layout is the published slide and slide-library description: a 31-byte slide header,
//! records whose high byte selects the type, and a 32-byte library header followed by 36-byte
//! directory entries. Coordinates stay in the slide's own space. The caller maps them onto a tile.
//!
//! Older slide levels and unknown record types are rejected. Their byte length is not defined, so
//! skipping one could read the rest of the file as geometry. Every offset and count is checked
//! before the bytes it names are used.

/// Most vectors and solids one slide may contain.
///
/// A dialog can keep two of these (the picture on screen and one replacement). The engine applies
/// a separate byte cap, so this count cannot multiply into an unbounded copy.
pub const MAX_PRIMITIVES: usize = 4_096;

/// Most directory entries read from one library, including the blank entry that ends the list.
const MAX_LIBRARY_ENTRIES: usize = 4_096;

const SLIDE_HEADER: &[u8] = b"AutoCAD Slide\r\n\x1a\x00";
const LIBRARY_HEADER: &[u8] = b"AutoCAD Slide Library 1.0\r\n\x1a\x00\x00\x00\x00";
const MAX_ENCODED_BYTES: usize = 1 << 20;
const HEADER_LEN: usize = 31;
const ENTRY_LEN: usize = 36;

/// One endpoint in slide space. The origin is the lower-left corner of the source picture.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SlidePoint {
    pub x: i32,
    pub y: i32,
}

/// A line recorded by the slide. `color` is an AutoCAD color index.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SlideVector {
    pub from: SlidePoint,
    pub to: SlidePoint,
    pub color: u8,
}

/// A filled polygon. The published record allows three to ten source vertices.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SlideSolid {
    pub points: Vec<SlidePoint>,
    pub color: u8,
}

/// Geometry from one level 2 slide, in source order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SlidePrim {
    Vector(SlideVector),
    Solid(SlideSolid),
}

/// A level 2 slide. `high_x` and `high_y` are the largest source coordinates.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Slide {
    pub high_x: u16,
    pub high_y: u16,
    pub prims: Vec<SlidePrim>,
}

/// Decode one level 2 slide. `None` means the bytes are not that format or they exceed the cap.
pub fn decode(bytes: &[u8]) -> Option<Slide> {
    if !(HEADER_LEN..=MAX_ENCODED_BYTES).contains(&bytes.len()) || bytes.get(..SLIDE_HEADER.len()) != Some(SLIDE_HEADER) {
        return None;
    }
    if *bytes.get(17)? != 0x56 || *bytes.get(18)? != 2 {
        return None;
    }
    let little = match bytes.get(29..31) {
        Some([0x34, 0x12]) => true,
        Some([0x12, 0x34]) => false,
        _ => return None,
    };
    let high_x = read_u16(bytes, 19, little)?;
    let high_y = read_u16(bytes, 21, little)?;
    let mut index = HEADER_LEN;
    let mut color = 7u8;
    let mut last: Option<(i32, i32)> = None;
    let mut prims = Vec::new();
    let mut fill: Option<Fill> = None;
    while index < bytes.len() {
        let word = read_u16_at(bytes, &mut index, little)?;
        let kind = u8::try_from(word >> 8).ok()?;
        let low = u8::try_from(word & 0xff).ok()?;
        if kind != 0xfd && fill.is_some() {
            return None;
        }
        match kind {
            0x00..=0x7f => {
                let from_x = i32::from(word);
                let from_y = i32::from(read_u16_at(bytes, &mut index, little)?);
                let to_x = i32::from(read_u16_at(bytes, &mut index, little)?);
                let to_y = i32::from(read_u16_at(bytes, &mut index, little)?);
                push_vector(&mut prims, from_x, from_y, to_x, to_y, color)?;
                last = Some((from_x, from_y));
            }
            0xfb => {
                let from_y = take_i8(bytes, &mut index)?;
                let to_x = take_i8(bytes, &mut index)?;
                let to_y = take_i8(bytes, &mut index)?;
                let (x, y) = last?;
                let from_x = x.checked_add(i32::from(i8::from_le_bytes([low])))?;
                let from_y = y.checked_add(from_y)?;
                let to_x = x.checked_add(to_x)?;
                let to_y = y.checked_add(to_y)?;
                push_vector(&mut prims, from_x, from_y, to_x, to_y, color)?;
                last = Some((from_x, from_y));
            }
            0xfc => {
                if low != 0 {
                    return None;
                }
                return Some(Slide { high_x, high_y, prims });
            }
            0xfd => {
                if low != 0 {
                    return None;
                }
                let x = signed(read_u16_at(bytes, &mut index, little)?);
                let y = signed(read_u16_at(bytes, &mut index, little)?);
                fill = next_fill(fill.take(), x, y, color, &mut prims)?;
            }
            0xfe => {
                let to_y = take_i8(bytes, &mut index)?;
                let (x, y) = last?;
                let to_x = x.checked_add(i32::from(i8::from_le_bytes([low])))?;
                let to_y = y.checked_add(to_y)?;
                push_vector(&mut prims, x, y, to_x, to_y, color)?;
                last = Some((to_x, to_y));
            }
            0xff => color = low,
            _ => return None,
        }
    }
    None
}

/// Byte range of one library member, bounded by the next stored address.
///
/// `None` when the library header, an entry, or any address is not valid. Addresses are checked
/// before the range is returned.
pub fn library_span(bytes: &[u8], name: &str) -> Option<(usize, usize)> {
    if name.is_empty() || bytes.len() > MAX_ENCODED_BYTES || bytes.get(..LIBRARY_HEADER.len()) != Some(LIBRARY_HEADER) {
        return None;
    }
    let mut index = LIBRARY_HEADER.len();
    let mut entries: Vec<(&[u8], usize)> = Vec::new();
    for _ in 0..MAX_LIBRARY_ENTRIES {
        let entry = bytes.get(index..index.checked_add(ENTRY_LEN)?)?;
        index += ENTRY_LEN;
        if entry.first().copied() == Some(0) {
            let directory_end = index;
            let requested = name.as_bytes();
            let (_, start) = entries
                .iter()
                .find(|(found, _)| *found == requested)
                .or_else(|| entries.iter().find(|(found, _)| found.eq_ignore_ascii_case(requested)))?;
            let start = *start;
            let end = entries.iter().map(|(_, offset)| *offset).filter(|offset| *offset > start).min().unwrap_or(bytes.len());
            if start >= end || entries.iter().any(|(_, offset)| *offset < directory_end || *offset >= bytes.len()) {
                return None;
            }
            return Some((start, end));
        }
        let name_end = entry.get(..32)?.iter().position(|byte| *byte == 0)?;
        let member = entry.get(..name_end)?;
        let address = u32::from_le_bytes(entry.get(32..36)?.try_into().ok()?);
        entries.push((member, usize::try_from(address).ok()?));
    }
    None
}

/// The bytes of one library member. See [`library_span`].
pub fn library_member<'a>(bytes: &'a [u8], name: &str) -> Option<&'a [u8]> {
    let (start, end) = library_span(bytes, name)?;
    bytes.get(start..end)
}

struct Fill {
    left: u8,
    points: Vec<SlidePoint>,
    color: u8,
}

fn next_fill(fill: Option<Fill>, x: i32, y: i32, color: u8, prims: &mut Vec<SlidePrim>) -> Option<Option<Fill>> {
    match fill {
        None if y < 0 && (3..=10).contains(&x) => {
            let left = u8::try_from(x).ok()?;
            Some(Some(Fill { left, points: Vec::with_capacity(usize::from(left)), color }))
        }
        Some(mut fill) if y >= 0 && fill.left > 0 => {
            fill.points.push(SlidePoint { x, y });
            fill.left -= 1;
            Some(Some(fill))
        }
        Some(fill) if y < 0 && fill.left == 0 && (3..=10).contains(&fill.points.len()) => {
            push_solid(prims, fill)?;
            Some(None)
        }
        _ => None,
    }
}

fn push_vector(prims: &mut Vec<SlidePrim>, x1: i32, y1: i32, x2: i32, y2: i32, color: u8) -> Option<()> {
    if prims.len() >= MAX_PRIMITIVES {
        return None;
    }
    prims.push(SlidePrim::Vector(SlideVector { from: SlidePoint { x: x1, y: y1 }, to: SlidePoint { x: x2, y: y2 }, color }));
    Some(())
}

fn push_solid(prims: &mut Vec<SlidePrim>, fill: Fill) -> Option<()> {
    if prims.len() >= MAX_PRIMITIVES {
        return None;
    }
    prims.push(SlidePrim::Solid(SlideSolid { points: fill.points, color: fill.color }));
    Some(())
}

fn read_u16(bytes: &[u8], offset: usize, little: bool) -> Option<u16> {
    let pair = bytes.get(offset..offset.checked_add(2)?)?.try_into().ok()?;
    Some(if little { u16::from_le_bytes(pair) } else { u16::from_be_bytes(pair) })
}

fn read_u16_at(bytes: &[u8], index: &mut usize, little: bool) -> Option<u16> {
    let value = read_u16(bytes, *index, little)?;
    *index = index.checked_add(2)?;
    Some(value)
}

fn take_i8(bytes: &[u8], index: &mut usize) -> Option<i32> {
    let byte = *bytes.get(*index)?;
    *index = index.checked_add(1)?;
    Some(i32::from(i8::from_le_bytes([byte])))
}

fn signed(word: u16) -> i32 {
    i32::from(i16::from_le_bytes(word.to_le_bytes()))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    fn word(out: &mut Vec<u8>, value: u16, little: bool) {
        let bytes = if little { value.to_le_bytes() } else { value.to_be_bytes() };
        out.extend_from_slice(&bytes);
    }

    fn slide(little: bool, high_x: u16, high_y: u16, body: &[u8]) -> Vec<u8> {
        let mut out = SLIDE_HEADER.to_vec();
        out.push(0x56);
        out.push(2);
        word(&mut out, high_x, little);
        word(&mut out, high_y, little);
        out.extend_from_slice(&1_460_000u32.to_le_bytes());
        word(&mut out, 2, little);
        word(&mut out, 0x1234, little);
        out.extend_from_slice(body);
        word(&mut out, 0xfc00, little);
        out
    }

    fn vector(little: bool, color: u8, x1: u16, y1: u16, x2: u16, y2: u16) -> Vec<u8> {
        let mut out = Vec::new();
        word(&mut out, 0xff00 | u16::from(color), little);
        word(&mut out, x1, little);
        word(&mut out, y1, little);
        word(&mut out, x2, little);
        word(&mut out, y2, little);
        out
    }

    #[test]
    fn compressed_records_update_the_saved_point_in_both_byte_orders() {
        for little in [true, false] {
            let mut body = vector(little, 5, 18, 11, 27, 16);
            word(&mut body, 0xfb04, little);
            body.extend_from_slice(&[0xfe, 0xfd, 6]);
            word(&mut body, 0xfe05, little);
            body.push(0xfc);
            word(&mut body, 0xfefe, little);
            body.push(6);
            let slide = decode(&slide(little, 38, 26, &body)).expect("compressed vectors");
            let lines: Vec<_> = slide
                .prims
                .iter()
                .map(|prim| match prim {
                    SlidePrim::Vector(line) => (line.color, (line.from.x, line.from.y), (line.to.x, line.to.y)),
                    SlidePrim::Solid(_) => panic!("expected a vector"),
                })
                .collect();
            assert_eq!(lines, vec![(5, (18, 11), (27, 16)), (5, (22, 9), (15, 17)), (5, (22, 9), (27, 5)), (5, (27, 5), (25, 11))]);
        }
    }

    #[test]
    fn both_byte_orders_select_the_same_geometry() {
        let little_body = vector(true, 1, 0, 0, 15, 15);
        let big_body = vector(false, 1, 0, 0, 15, 15);
        let little = decode(&slide(true, 15, 15, &little_body)).expect("little");
        let big = decode(&slide(false, 15, 15, &big_body)).expect("big");
        assert_eq!(little, big);
        match &little.prims[0] {
            SlidePrim::Vector(line) => assert_eq!((line.from, line.to, line.color), (SlidePoint { x: 0, y: 0 }, SlidePoint { x: 15, y: 15 }, 1)),
            SlidePrim::Solid(_) => panic!("vector"),
        }
    }

    #[test]
    fn decodes_a_solid_and_stops_at_the_end_record() {
        let mut body = vector(true, 4, 0, 0, 1, 1);
        word(&mut body, 0xfd00, true);
        word(&mut body, 3, true);
        word(&mut body, u16::from_le_bytes((-1i16).to_le_bytes()), true);
        for (x, y) in [(0u16, 0u16), (10, 0), (10, 10)] {
            word(&mut body, 0xfd00, true);
            word(&mut body, x, true);
            word(&mut body, y, true);
        }
        word(&mut body, 0xfd00, true);
        word(&mut body, 3, true);
        word(&mut body, u16::from_le_bytes((-1i16).to_le_bytes()), true);
        let mut bytes = slide(true, 10, 10, &body);
        bytes.extend_from_slice(&[0, 0, 0]);
        let slide = decode(&bytes).expect("solid");
        assert!(matches!(slide.prims[0], SlidePrim::Vector(_)));
        match &slide.prims[1] {
            SlidePrim::Solid(solid) => {
                assert_eq!(solid.color, 4);
                assert_eq!(solid.points, vec![SlidePoint { x: 0, y: 0 }, SlidePoint { x: 10, y: 0 }, SlidePoint { x: 10, y: 10 }]);
            }
            SlidePrim::Vector(_) => panic!("solid"),
        }
    }

    #[test]
    fn rejects_a_truncated_slide_another_level_and_too_many_vectors() {
        assert!(decode(SLIDE_HEADER).is_none());
        let mut other = slide(true, 1, 1, &vector(true, 7, 0, 0, 1, 1));
        other[18] = 1;
        assert!(decode(&other).is_none());
        let mut unknown = vector(true, 7, 0, 0, 1, 1);
        word(&mut unknown, 0x8000, true);
        assert!(decode(&slide(true, 1, 1, &unknown)).is_none());
        let mut many = Vec::new();
        for _ in 0..=MAX_PRIMITIVES {
            many.extend(vector(true, 7, 0, 0, 1, 0));
        }
        assert!(decode(&slide(true, 1, 1, &many)).is_none());
    }

    #[test]
    fn library_member_uses_only_its_own_span() {
        let red = slide(true, 15, 15, &vector(true, 1, 0, 0, 15, 15));
        let green = slide(true, 15, 15, &vector(true, 3, 0, 15, 15, 0));
        let mut bytes = LIBRARY_HEADER.to_vec();
        let directory = 32 + 36 * 3;
        let mut entry = |name: &str, offset: u32| {
            let mut item = [0u8; 36];
            item[..name.len()].copy_from_slice(name.as_bytes());
            item[32..36].copy_from_slice(&offset.to_le_bytes());
            bytes.extend_from_slice(&item);
        };
        entry("red", directory as u32);
        entry("green", (directory + red.len()) as u32);
        bytes.extend_from_slice(&[0u8; 36]);
        bytes.extend_from_slice(&red);
        bytes.extend_from_slice(&green);
        let red_span = library_member(&bytes, "red").expect("red");
        let green_span = library_member(&bytes, "green").expect("green");
        assert_eq!(red_span.len(), red.len());
        assert!(library_member(&bytes, "blue").is_none());
        assert_eq!(library_member(&bytes, "GREEN"), Some(green_span));
        match &decode(red_span).expect("red slide").prims[0] {
            SlidePrim::Vector(line) => assert_eq!(line.color, 1),
            SlidePrim::Solid(_) => panic!("vector"),
        }
        match &decode(green_span).expect("green slide").prims[0] {
            SlidePrim::Vector(line) => assert_eq!((line.color, line.from.y, line.to.y), (3, 15, 0)),
            SlidePrim::Solid(_) => panic!("vector"),
        }
        let mut bad = bytes.clone();
        bad[32 + 32..32 + 36].copy_from_slice(&10u32.to_le_bytes());
        assert!(library_member(&bad, "red").is_none(), "an address inside the header is rejected");
    }
}
