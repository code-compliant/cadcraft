//! `load_dialog` and `unload_dialog` through the Lisp entry point.
//!
//! Fixtures are synthetic DCL. A load failure must leave the drawing's handle store as it was.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use super::value::to_text;
use crate::Session;

fn eval(s: &mut Session, code: &str) -> Result<String, String> {
    match s.lisp_eval(code, false, true, Vec::new()) {
        Ok(Some(Ok(v))) => Ok(to_text(&v, true)),
        Ok(Some(Err(e))) => Err(e),
        Ok(None) => Err("did not finish".into()),
        Err(e) => Err(e.to_string()),
    }
}

fn ok(s: &mut Session, code: &str) -> String {
    match eval(s, code) {
        Ok(v) => v,
        Err(e) => panic!("{code}: {e}"),
    }
}

fn negative(text: &str) -> i32 {
    let id: i32 = text.parse().unwrap_or_else(|_| panic!("expected an integer, got {text}"));
    assert!(id < 0, "expected a negative integer, got {id}");
    id
}

fn loaded_ids(s: &Session) -> Vec<i32> {
    let uid = s.state().unwrap().uid;
    let mut ids: Vec<i32> = s.lisp.envs.get(&uid).map(|env| env.dialogs.keys().copied().collect()).unwrap_or_default();
    ids.sort_unstable();
    ids
}

#[cfg(not(target_arch = "wasm32"))]
mod native {
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::{eval, loaded_ids, negative, ok, to_text};
    use crate::Session;
    use crate::lisp::{DialogEvent, DialogEventOutcome, DialogRequest, dcl, dcl_load, tests::session};
    use cadcraft_doc::EntityKind;
    use serde_json::json;

    fn positive(text: &str) -> i32 {
        let id: i32 = text.parse().unwrap_or_else(|_| panic!("expected an integer handle, got {text}"));
        assert!(id > 0, "expected a positive handle, got {id}");
        id
    }

    fn scratch(label: &str) -> PathBuf {
        static N: AtomicU64 = AtomicU64::new(0);
        let n = N.fetch_add(1, Ordering::Relaxed);
        let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        std::env::temp_dir().join(format!("cadcraft-dcl-{label}-{}-{nanos}-{n}", std::process::id()))
    }

    struct TempDir(PathBuf);

    impl TempDir {
        fn new(label: &str) -> Self {
            let path = scratch(label);
            std::fs::create_dir_all(&path).unwrap();
            TempDir(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.0).ok();
        }
    }

    fn write(dir: &TempDir, name: &str, bytes: &[u8]) -> PathBuf {
        let path = dir.path().join(name);
        std::fs::write(&path, bytes).unwrap();
        path
    }

    /// A quoted Lisp string for `path`.
    fn lisp_path(path: &Path) -> String {
        let text = path.to_string_lossy();
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

    const TILE: &str = "probe : dialog {\n  label = \"Probe\";\n}\n";

    /// Valid DCL of exactly `bytes` length. The padding sits in one block comment so it stays inside
    /// the parser's string and token caps.
    fn valid_dcl_of_len(bytes: usize) -> Vec<u8> {
        let head = b"/*";
        let tail = b"*/\nprobe : dialog {\n  label = \"ok\";\n}\n";
        assert!(bytes >= head.len() + tail.len());
        let mut out = vec![b'x'; bytes];
        out[..head.len()].copy_from_slice(head);
        out[bytes - tail.len()..].copy_from_slice(tail);
        out
    }

    fn definition_names(s: &Session, id: i32) -> Vec<String> {
        let uid = s.state().unwrap().uid;
        let Some(dialog) = s.lisp.envs.get(&uid).and_then(|env| env.dialogs.get(&id)) else { return Vec::new() };
        let mut names = Vec::new();
        for file in &dialog.files {
            for item in &file.parsed.items {
                if let dcl::Item::Definition(definition) = item {
                    names.push(definition.name.clone());
                }
            }
        }
        names
    }

    #[test]
    fn dcl_file_load_and_unload_return_the_documented_values() {
        let mut s = session();
        let dir = TempDir::new("life");
        let first = write(&dir, "first.dcl", TILE.as_bytes());
        let second = write(&dir, "second.dcl", b"other : dialog {\n  label = \"Other\";\n}\n");
        let a = positive(&ok(&mut s, &format!("(load_dialog {})", lisp_path(&first))));
        let b = positive(&ok(&mut s, &format!("(load_dialog {})", lisp_path(&second))));
        assert_ne!(a, b);
        let mut expect = vec![a, b];
        expect.sort_unstable();
        assert_eq!(loaded_ids(&s), expect);
        assert_eq!(ok(&mut s, &format!("(unload_dialog {a})")), "nil");
        assert_eq!(loaded_ids(&s), vec![b], "unload removes only the selected handle");
        assert_eq!(ok(&mut s, &format!("(unload_dialog {a})")), "nil");
        assert_eq!(loaded_ids(&s), vec![b]);
        assert_eq!(ok(&mut s, &format!("(unload_dialog {b})")), "nil");
        assert!(loaded_ids(&s).is_empty());
    }

    #[test]
    fn dcl_file_load_finds_a_name_without_an_extension() {
        let mut s = session();
        let dir = TempDir::new("support");
        write(&dir, "cadcraft_probe_widget.dcl", TILE.as_bytes());
        s.set_lisp_support_dir(Some(dir.path().to_path_buf()));
        let id = positive(&ok(&mut s, "(load_dialog \"cadcraft_probe_widget\")"));
        assert_eq!(loaded_ids(&s), vec![id]);
        assert_eq!(ok(&mut s, &format!("(unload_dialog {id})")), "nil");

        let bare = TempDir::new("bare");
        write(&bare, "cadcraft_probe_bare", TILE.as_bytes());
        s.set_lisp_support_dir(Some(bare.path().to_path_buf()));
        negative(&ok(&mut s, "(load_dialog \"cadcraft_probe_bare\")"));
        assert!(loaded_ids(&s).is_empty());
    }

    #[test]
    fn dcl_file_load_honors_automation_read_root() {
        let mut s = session();
        let root = TempDir::new("automation-root");
        let other = TempDir::new("automation-other");
        let allowed = write(&root, "allowed.dcl", TILE.as_bytes());
        let refused = write(&other, "refused.dcl", TILE.as_bytes());
        let source = format!("@include {}\nhost : dialog {{ ok_only; }}\n", lisp_path(&refused));
        let including = write(&root, "including.dcl", source.as_bytes());
        let roots = crate::file_access::AutomationRoots::new(Some(root.path()), None).unwrap();

        crate::file_access::confine(roots.as_ref(), || {
            let id = positive(&ok(&mut s, &format!("(load_dialog {})", lisp_path(&allowed))));
            negative(&ok(&mut s, &format!("(load_dialog {})", lisp_path(&refused))));
            negative(&ok(&mut s, &format!("(load_dialog {})", lisp_path(&including))));
            assert_eq!(loaded_ids(&s), vec![id]);
        });
    }

    #[test]
    fn dcl_file_load_resolves_a_relative_include() {
        let mut s = session();
        let beside = TempDir::new("beside");
        let child_name = "cadcraft_probe_child.dcl";
        write(&beside, child_name, TILE.as_bytes());
        let parent = format!("@include \"{child_name}\"\nhost : dialog {{\n  label = \"Host\";\n}}\n");
        let parent_path = write(&beside, "host.dcl", parent.as_bytes());
        let id = positive(&ok(&mut s, &format!("(load_dialog {})", lisp_path(&parent_path))));
        assert_eq!(definition_names(&s, id), vec!["host".to_string(), "probe".to_string()]);

        let abs_dir = TempDir::new("abs");
        let abs_child = write(&abs_dir, "abs-child.dcl", TILE.as_bytes());
        let quoted = abs_child.display().to_string().replace('\\', "\\\\").replace('"', "\\\"");
        let abs_src = format!("@include \"{quoted}\"\nabs : dialog {{\n  label = \"Abs\";\n}}\n");
        let abs_parent = write(&beside, "abs-host.dcl", abs_src.as_bytes());
        let abs_id = positive(&ok(&mut s, &format!("(load_dialog {})", lisp_path(&abs_parent))));

        let mut expect = vec![id, abs_id];
        expect.sort_unstable();
        assert_eq!(loaded_ids(&s), expect);
    }

    #[test]
    fn dcl_file_load_includes_the_exact_file_name() {
        let mut s = session();
        let dir = TempDir::new("include-name");
        let exact = write(&dir, "child", TILE.as_bytes());
        write(&dir, "child.dcl", b"wrong : dialog { ok_only; }\n");
        let source = format!("@include {}\nhost : dialog {{ ok_only; }}\n", lisp_path(&exact));
        let root = write(&dir, "host.dcl", source.as_bytes());

        let id = positive(&ok(&mut s, &format!("(load_dialog {})", lisp_path(&root))));
        assert_eq!(definition_names(&s, id), ["host", "probe"]);
    }

    #[test]
    fn dcl_file_load_counts_repeated_include_requests() {
        let mut s = session();
        let dir = TempDir::new("request-cap");
        write(&dir, "child.dcl", TILE.as_bytes());
        let includes = "@include \"child.dcl\"\n";
        let at_limit = includes.repeat(dcl_load::MAX_FILES - 1);
        let root = write(&dir, "limit.dcl", at_limit.as_bytes());
        let id = positive(&ok(&mut s, &format!("(load_dialog {})", lisp_path(&root))));

        let above_limit = at_limit + includes;
        let excess = write(&dir, "excess.dcl", above_limit.as_bytes());
        negative(&ok(&mut s, &format!("(load_dialog {})", lisp_path(&excess))));
        assert_eq!(loaded_ids(&s), [id]);
    }

    #[test]
    fn dcl_file_load_rejects_bad_files_without_storing_a_handle() {
        let mut s = session();
        assert!(loaded_ids(&s).is_empty());
        let missing = scratch("missing").join("no-such.dcl");
        negative(&ok(&mut s, &format!("(load_dialog {})", lisp_path(&missing))));
        assert!(loaded_ids(&s).is_empty());

        let dir = TempDir::new("bad");
        let broken = write(&dir, "broken.dcl", b"box : dialog {\n  /*");
        negative(&ok(&mut s, &format!("(load_dialog {})", lisp_path(&broken))));
        assert!(loaded_ids(&s).is_empty());

        let huge = vec![b'x'; dcl::MAX_INPUT_BYTES + 1];
        let oversized = write(&dir, "huge.dcl", &huge);
        negative(&ok(&mut s, &format!("(load_dialog {})", lisp_path(&oversized))));
        assert!(loaded_ids(&s).is_empty());

        let left = write(&dir, "left.dcl", b"@include \"right.dcl\"\nleft : dialog {\n  label = \"L\";\n}\n");
        write(&dir, "right.dcl", b"@include \"left.dcl\"\nright : dialog {\n  label = \"R\";\n}\n");
        negative(&ok(&mut s, &format!("(load_dialog {})", lisp_path(&left))));
        assert!(loaded_ids(&s).is_empty(), "a cycle must not leave a live DCL file");

        let big = valid_dcl_of_len(dcl::MAX_INPUT_BYTES);
        assert!(dcl::parse(std::str::from_utf8(&big).unwrap()).is_ok(), "the oversized-include fixture must be valid DCL");
        write(&dir, "big.dcl", &big);
        let host = write(&dir, "with-big.dcl", b"@include \"big.dcl\"\nhost : dialog {\n  label = \"H\";\n}\n");
        negative(&ok(&mut s, &format!("(load_dialog {})", lisp_path(&host))));
        assert!(loaded_ids(&s).is_empty());

        let good = write(&dir, "good.dcl", TILE.as_bytes());
        let id = positive(&ok(&mut s, &format!("(load_dialog {})", lisp_path(&good))));
        assert_eq!(loaded_ids(&s), vec![id], "earlier failures must leave no live DCL file");
    }

    #[test]
    fn dcl_file_load_charges_failed_reads_to_the_run() {
        let mut s = session();
        let dir = TempDir::new("run-budget");
        let mut unclosed_comment = vec![b' '; 768];
        unclosed_comment[..2].copy_from_slice(b"/*");
        let malformed = write(&dir, "malformed.dcl", &unclosed_comment);
        let invalid_utf8 = write(&dir, "invalid-utf8.dcl", &[0xff; 768]);
        let mut missing_include = valid_dcl_of_len(768);
        missing_include.extend_from_slice(b"@include \"./missing-budget-child.dcl\"\n");
        let parent = write(&dir, "missing-child.dcl", &missing_include);
        let oversize = write(&dir, "oversize.dcl", &vec![b' '; dcl_load::MAX_LOAD_BYTES + 1]);

        s.lisp.limits.string_bytes = 1024;
        for path in [malformed, invalid_utf8, parent] {
            let load = format!("(load_dialog {})", lisp_path(&path));
            negative(&ok(&mut s, &load));
            let twice = format!("(progn {load} {load})");
            assert_eq!(eval(&mut s, &twice), Err("out of memory: the routine made too much text".into()));
            assert!(loaded_ids(&s).is_empty());
        }
        let load = format!("(load_dialog {})", lisp_path(&oversize));
        assert_eq!(eval(&mut s, &load), Err("out of memory: the routine made too much text".into()));
        assert!(loaded_ids(&s).is_empty());

        let good = write(&dir, "good.dcl", TILE.as_bytes());
        let id = positive(&ok(&mut s, &format!("(load_dialog {})", lisp_path(&good))));
        assert_eq!(loaded_ids(&s), [id]);
    }

    #[test]
    fn dcl_file_load_checks_depth_of_each_include_path() {
        let mut s = session();
        let dir = TempDir::new("depth-path");
        let leaf = dcl_load::MAX_INCLUDE_DEPTH - 1;
        for step in 1..leaf {
            let include = format!("@include \"depth-chain-{}.dcl\"\n", step + 1);
            write(&dir, &format!("depth-chain-{step}.dcl"), include.as_bytes());
        }
        write(&dir, &format!("depth-chain-{leaf}.dcl"), TILE.as_bytes());

        let short = "@include \"depth-chain-1.dcl\"\n";
        let long = "@include \"depth-wrapper.dcl\"\n";
        write(&dir, "depth-wrapper.dcl", short.as_bytes());
        let at_limit = write(&dir, "at-limit.dcl", short.as_bytes());
        let short_first = write(&dir, "short-first.dcl", format!("{short}{long}").as_bytes());
        let long_first = write(&dir, "long-first.dcl", format!("{long}{short}").as_bytes());

        let id = positive(&ok(&mut s, &format!("(load_dialog {})", lisp_path(&at_limit))));
        assert_eq!(definition_names(&s, id), ["probe"]);
        let short_first_result = ok(&mut s, &format!("(load_dialog {})", lisp_path(&short_first)));
        let long_first_result = ok(&mut s, &format!("(load_dialog {})", lisp_path(&long_first)));
        negative(&short_first_result);
        negative(&long_first_result);
        assert_eq!(loaded_ids(&s), [id]);
    }

    #[test]
    fn dcl_file_load_checks_argument_types() {
        let mut s = session();
        assert_eq!(eval(&mut s, "(load_dialog)"), Err("too few arguments".into()));
        assert_eq!(eval(&mut s, "(load_dialog \"a\" \"b\")"), Err("too many arguments".into()));
        assert_eq!(eval(&mut s, "(load_dialog 1)"), Err("bad argument type: stringp: 1".into()));
        assert_eq!(eval(&mut s, "(unload_dialog)"), Err("too few arguments".into()));
        assert_eq!(eval(&mut s, "(unload_dialog \"x\")"), Err("bad argument type: fixnump: \"x\"".into()));
        assert_eq!(ok(&mut s, "(unload_dialog 1)"), "nil");
        assert!(loaded_ids(&s).is_empty());
    }

    #[test]
    fn dcl_file_handles_are_bounded_per_drawing() {
        let mut s = session();
        let first_drawing = s.active;
        let dir = TempDir::new("drawing-cap");
        let path = write(&dir, "probe.dcl", TILE.as_bytes());
        let load = format!("(load_dialog {})", lisp_path(&path));
        for _ in 0..dcl_load::MAX_DIALOGS {
            positive(&ok(&mut s, &load));
        }
        let handles = loaded_ids(&s);
        assert_eq!(handles.len(), dcl_load::MAX_DIALOGS);
        negative(&ok(&mut s, &load));
        assert_eq!(loaded_ids(&s), handles);

        s.new_drawing(false);
        assert!(loaded_ids(&s).is_empty());
        positive(&ok(&mut s, &load));
        assert_eq!(loaded_ids(&s).len(), 1);

        s.active = first_drawing;
        assert_eq!(loaded_ids(&s), handles);
        assert_eq!(ok(&mut s, &format!("(unload_dialog {})", handles[0])), "nil");
        positive(&ok(&mut s, &load));
        assert_eq!(loaded_ids(&s).len(), dcl_load::MAX_DIALOGS);
    }

    fn load_source(s: &mut Session, dir: &TempDir, name: &str, source: &str) -> i32 {
        let path = write(dir, name, source.as_bytes());
        positive(&ok(s, &format!("(load_dialog {})", lisp_path(&path))))
    }

    /// Tile references in a full binary cluster of `levels` (`t0` twice under each parent).
    ///
    /// `t0` is one reference. Each added level is one reference plus two copies of the level
    /// below, so level `n` expands to `2^(n+1) - 1` references.
    fn cluster_tree(levels: usize) -> String {
        let mut out = String::from("t0 : spacer {\n}\n");
        for level in 1..=levels {
            out.push_str(&format!("t{level} : column {{\n  t{};\n  t{};\n}}\n", level - 1, level - 1));
        }
        out
    }

    fn proto_chain(levels: usize) -> String {
        let mut out = String::from("p1 : edit_box {\n  key = \"k\";\n  value = \"v\";\n}\n");
        for level in 2..=levels {
            out.push_str(&format!("p{level} : p{} {{\n}}\n", level - 1));
        }
        out
    }

    fn wide_bases(levels: usize) -> String {
        let mut out = String::from("e0 : edit_box {\n  key = \"ek\";\n  value = \"ev\";\n}\n");
        for level in 1..=levels {
            let prev = level - 1;
            out.push_str(&format!("e{level} : e{prev} : e{prev} {{\n}}\n"));
        }
        out
    }

    #[test]
    fn dcl_prepare_reads_a_dialog_from_an_include_and_sets_tiles() {
        let mut s = session();
        let dir = TempDir::new("prepare");
        let widgets = "cadcraft_prepare_widgets.dcl";
        write(
            &dir,
            widgets,
            br#"named : edit_box {
  key = "Name";
  value = "proto";
}
left : edit_box {
  key = "order";
  value = "left";
}
right : edit_box {
  value = "right";
}
merged : left : right {
}
local : left : right {
  value = "local";
}
part : column {
  : named { value = "local-name"; }
  : edit_box { key = "name"; value = "other"; }
  : edit_box { key = "plain"; }
  merged;
  : local { key = "Local"; }
}
probe : dialog {
  label = "Probe";
  part;
}
"#,
        );
        let root = format!("@include \"{widgets}\"\n");
        let id = load_source(&mut s, &dir, "cadcraft_prepare_root.dcl", &root);

        assert_eq!(ok(&mut s, &format!("(new_dialog \"probe\" {id} \"(done)\" '(8 21))")), "T");
        let prepared = prepared_dialog(&s);
        assert_eq!(prepared.name.as_ref(), "probe");
        assert_eq!(prepared.handle, id);
        assert_eq!(prepared.action.as_deref(), Some("(done)"));
        assert_eq!(prepared.position, Some((8.0, 21.0)));
        assert_eq!(ok(&mut s, "(get_tile \"Name\")"), "\"local-name\"");
        assert_eq!(ok(&mut s, "(get_tile \"name\")"), "\"other\"");
        assert_eq!(ok(&mut s, "(get_tile \"plain\")"), "\"\"");
        assert_eq!(ok(&mut s, "(get_tile \"order\")"), "\"left\"");
        assert_eq!(ok(&mut s, "(get_tile \"Local\")"), "\"local\"");
        assert_eq!(ok(&mut s, "(get_tile \"local\")"), "nil");
        assert_eq!(ok(&mut s, "(get_tile \"missing\")"), "nil");
        assert_eq!(ok(&mut s, "(set_tile \"Name\" \"changed\")"), "\"changed\"");
        assert_eq!(ok(&mut s, "(get_tile \"Name\")"), "\"changed\"");
        assert_eq!(ok(&mut s, "(get_tile \"name\")"), "\"other\"");
    }

    #[test]
    fn dcl_prepare_keeps_state_on_bad_input_and_clears_it_on_unload() {
        let mut s = session();
        let dir = TempDir::new("prepare-keep");
        let id = load_source(&mut s, &dir, "anchor.dcl", "anchor : dialog {\n  : edit_box { key = \"hold\"; value = \"start\"; }\n}\n");
        let other = load_source(&mut s, &dir, "other.dcl", "other : dialog {\n  : edit_box { key = \"side\"; value = \"s\"; }\n}\n");
        assert_eq!(ok(&mut s, &format!("(new_dialog \"anchor\" {id})")), "T");
        assert_eq!(ok(&mut s, "(set_tile \"hold\" \"kept\")"), "\"kept\"");

        assert_eq!(ok(&mut s, "(new_dialog \"anchor\" 99)"), "nil");
        assert_eq!(ok(&mut s, &format!("(new_dialog \"missing\" {id})")), "nil");
        assert_eq!(ok(&mut s, &format!("(new_dialog \"Anchor\" {id})")), "nil");
        assert_eq!(ok(&mut s, "(get_tile \"hold\")"), "\"kept\"");

        assert_eq!(ok(&mut s, &format!("(new_dialog \"anchor\" {id})")), "T");
        assert!(prepared_dialog(&s).action.is_none());
        assert!(prepared_dialog(&s).position.is_none());
        assert_eq!(ok(&mut s, "(get_tile \"hold\")"), "\"start\"");
        assert_eq!(ok(&mut s, "(set_tile \"hold\" \"kept\")"), "\"kept\"");

        assert_eq!(ok(&mut s, &format!("(unload_dialog {other})")), "nil");
        assert_eq!(ok(&mut s, "(get_tile \"hold\")"), "\"kept\"");
        assert_eq!(ok(&mut s, &format!("(unload_dialog {id})")), "nil");
        assert_eq!(ok(&mut s, "(get_tile \"hold\")"), "nil");
        assert_eq!(ok(&mut s, "(set_tile \"hold\" \"again\")"), "nil");
    }

    #[test]
    fn dcl_prepare_is_isolated_by_drawing() {
        let mut s = session();
        let first = s.active;
        let dir = TempDir::new("prepare-drawing");
        let source = "probe : dialog {\n  : edit_box { key = \"Name\"; value = \"one\"; }\n}\n";
        let id = load_source(&mut s, &dir, "drawing.dcl", source);
        assert_eq!(ok(&mut s, &format!("(new_dialog \"probe\" {id})")), "T");
        assert_eq!(ok(&mut s, "(set_tile \"Name\" \"drawing-1\")"), "\"drawing-1\"");

        let second = s.new_drawing(false);
        assert_eq!(ok(&mut s, "(get_tile \"Name\")"), "nil");
        assert_eq!(ok(&mut s, "(set_tile \"Name\" \"other\")"), "nil");
        let second_id = load_source(&mut s, &dir, "drawing.dcl", source);
        assert_eq!(ok(&mut s, &format!("(new_dialog \"probe\" {second_id})")), "T");
        assert_eq!(ok(&mut s, "(set_tile \"Name\" \"drawing-2\")"), "\"drawing-2\"");

        s.active = first;
        assert_eq!(ok(&mut s, "(get_tile \"Name\")"), "\"drawing-1\"");
        s.active = second;
        assert_eq!(ok(&mut s, "(get_tile \"Name\")"), "\"drawing-2\"");
    }

    #[test]
    fn dcl_prepare_rejects_cycles_duplicates_and_excess() {
        let mut s = session();
        let dir = TempDir::new("prepare-limits");
        let anchor_id = load_source(&mut s, &dir, "anchor.dcl", "anchor : dialog {\n  : edit_box { key = \"hold\"; value = \"start\"; }\n}\n");
        let hold = |s: &mut Session| {
            assert_eq!(ok(s, &format!("(new_dialog \"anchor\" {anchor_id})")), "T");
            assert_eq!(ok(s, "(set_tile \"hold\" \"kept\")"), "\"kept\"");
        };
        let kept = |s: &mut Session| assert_eq!(ok(s, "(get_tile \"hold\")"), "\"kept\"");
        hold(&mut s);

        let dup_id = load_source(
            &mut s,
            &dir,
            "dup.dcl",
            "dup : dialog {\n  : edit_box { key = \"same\"; value = \"1\"; }\n  : edit_box { key = \"Same\"; value = \"2\"; }\n  : edit_box { key = \"same\"; value = \"3\"; }\n}\n",
        );
        assert_eq!(ok(&mut s, &format!("(new_dialog \"dup\" {dup_id})")), "nil");
        kept(&mut s);

        let cycle_id = load_source(&mut s, &dir, "cycle.dcl", "a : b {\n  key = \"k\";\n  value = \"1\";\n}\nb : a {\n}\ncyc : dialog {\n  a;\n}\n");
        assert_eq!(ok(&mut s, &format!("(new_dialog \"cyc\" {cycle_id})")), "nil");
        kept(&mut s);

        let brace_id = load_source(
            &mut s,
            &dir,
            "brace.dcl",
            "part : column {\n  : edit_box { key = \"a\"; value = \"1\"; }\n}\nbadref : dialog {\n  : part { label = \"no\"; }\n}\n",
        );
        assert_eq!(ok(&mut s, &format!("(new_dialog \"badref\" {brace_id})")), "nil");
        kept(&mut s);

        let mut depth = proto_chain(33);
        depth.push_str("deep_ok : dialog {\n  p32;\n}\ndeep_bad : dialog {\n  p33;\n}\n");
        let depth_id = load_source(&mut s, &dir, "depth.dcl", &depth);
        assert_eq!(ok(&mut s, &format!("(new_dialog \"deep_bad\" {depth_id})")), "nil");
        kept(&mut s);
        assert_eq!(ok(&mut s, &format!("(new_dialog \"deep_ok\" {depth_id})")), "T");
        assert_eq!(ok(&mut s, "(get_tile \"k\")"), "\"v\"");
        hold(&mut s);

        let mut wide = wide_bases(20);
        wide.push_str("wide : dialog {\n  e20;\n}\n");
        let wide_id = load_source(&mut s, &dir, "wide.dcl", &wide);
        assert_eq!(ok(&mut s, &format!("(new_dialog \"wide\" {wide_id})")), "T");
        assert_eq!(ok(&mut s, "(get_tile \"ek\")"), "\"ev\"");
        hold(&mut s);

        // 4096 is the expanded-reference cap: level 11 is 4095 references, plus one keyed tile.
        let mut tiles = cluster_tree(12);
        tiles.push_str("fit : dialog {\n  t11;\n  : edit_box { key = \"only\"; value = \"z\"; }\n}\nover : dialog {\n  t12;\n}\n");
        let tile_id = load_source(&mut s, &dir, "tiles.dcl", &tiles);
        assert_eq!(ok(&mut s, &format!("(new_dialog \"over\" {tile_id})")), "nil");
        kept(&mut s);
        assert_eq!(ok(&mut s, &format!("(new_dialog \"fit\" {tile_id})")), "T");
        assert_eq!(ok(&mut s, "(get_tile \"only\")"), "\"z\"");
        hold(&mut s);

        // Retained key and value text is capped at 1 MiB. Each copy is a 1024-byte value and a 5-byte key.
        let value_len = 1024;
        let per = value_len + 5;
        let fit_count = (1 << 20) / per;
        let value = "v".repeat(value_len);
        let mut text = format!("proto : edit_box {{\n  value = \"{value}\";\n}}\nfit_text : dialog {{\n");
        for i in 0..fit_count {
            text.push_str(&format!("  : proto {{ key = \"k{i:04}\"; }}\n"));
        }
        text.push_str("}\nover_text : dialog {\n");
        for i in 0..=fit_count {
            text.push_str(&format!("  : proto {{ key = \"k{i:04}\"; }}\n"));
        }
        text.push_str("}\n");
        let text_id = load_source(&mut s, &dir, "text.dcl", &text);
        assert_eq!(ok(&mut s, &format!("(new_dialog \"over_text\" {text_id})")), "nil");
        kept(&mut s);
        assert_eq!(ok(&mut s, &format!("(new_dialog \"fit_text\" {text_id})")), "T");
        assert_eq!(ok(&mut s, "(get_tile \"k0000\")"), format!("\"{value}\""));
        assert_eq!(ok(&mut s, &format!("(get_tile \"k{last:04}\")", last = fit_count - 1)), format!("\"{value}\""));
        let bigger = "v".repeat(value_len + 26);
        assert_eq!(ok(&mut s, &format!("(set_tile \"k0000\" \"{bigger}\")")), "nil");
        assert_eq!(ok(&mut s, "(get_tile \"k0000\")"), format!("\"{value}\""));
    }

    #[test]
    fn dcl_prepare_depth_check_includes_cached_prototypes() {
        let mut s = session();
        let dir = TempDir::new("prepare-shared-depth");
        let mut source = proto_chain(33);
        source.push_str(
            "at_limit : dialog { p32; }\n\
             short_first : dialog { p32; : p33 { key = \"long\"; } }\n\
             long_first : dialog { : p33 { key = \"long\"; } p32; }\n",
        );
        let id = load_source(&mut s, &dir, "shared-depth.dcl", &source);
        assert_eq!(ok(&mut s, &format!("(new_dialog \"at_limit\" {id})")), "T");
        assert_eq!(ok(&mut s, "(set_tile \"k\" \"keep\")"), "\"keep\"");
        let short_first = ok(&mut s, &format!("(new_dialog \"short_first\" {id})"));
        let long_first = ok(&mut s, &format!("(new_dialog \"long_first\" {id})"));
        assert_eq!([short_first, long_first], ["nil", "nil"]);
        assert_eq!(ok(&mut s, "(get_tile \"k\")"), "\"keep\"");
    }

    #[test]
    fn dcl_prepare_counts_cached_prototype_edges_toward_work_limit() {
        let mut s = session();
        let dir = TempDir::new("prepare-edge-budget");
        let bases = " : proto".repeat(34_000);
        write(&dir, "left.dcl", format!("left{bases} {{}}\n").as_bytes());
        write(&dir, "right.dcl", format!("right{bases} {{}}\n").as_bytes());
        let source = "proto : edit_box { key = \"field\"; value = \"initial\"; }\n\
                      @include \"left.dcl\"\n\
                      @include \"right.dcl\"\n\
                      both : left : right {}\n\
                      within_limit : dialog { left; }\n\
                      over_limit : dialog { both; }\n";
        let id = load_source(&mut s, &dir, "edge-budget.dcl", source);
        assert_eq!(ok(&mut s, &format!("(new_dialog \"within_limit\" {id})")), "T");
        assert_eq!(ok(&mut s, "(set_tile \"field\" \"keep\")"), "\"keep\"");
        assert_eq!(ok(&mut s, &format!("(new_dialog \"over_limit\" {id})")), "nil");
        assert_eq!(ok(&mut s, "(get_tile \"field\")"), "\"keep\"");
    }

    #[test]
    fn dcl_prepare_charges_new_tile_text_to_the_run() {
        let mut s = session();
        let dir = TempDir::new("prepare-text-budget");
        let value = "x".repeat(768);
        let source = format!("probe : dialog {{ : edit_box {{ key = \"entry\"; value = \"{value}\"; }} }}\n");
        let id = load_source(&mut s, &dir, "text-budget.dcl", &source);
        let prepare = format!("(new_dialog \"probe\" {id})");
        let twice = format!("(progn {prepare} (set_tile \"entry\" \"keep\") {prepare})");
        s.lisp.limits.string_bytes = 1024;
        assert_eq!(eval(&mut s, &twice), Err("out of memory: the routine made too much text".into()));
        assert_eq!(ok(&mut s, "(get_tile \"entry\")"), "\"keep\"");
        assert_eq!(ok(&mut s, &prepare), "T");
        assert_eq!(ok(&mut s, "(get_tile \"entry\")"), format!("\"{value}\""));
    }

    #[test]
    fn dcl_prepare_rejects_unknown_tile_names_and_keeps_state() {
        let mut s = session();
        let dir = TempDir::new("prepare-known-tiles");
        let source = r#"
entry : edit_box { key = "keep"; value = "initial"; }
probe : dialog { entry; ok_cancel; }
unknown : edit_bxo { key = "wrong"; value = "bad"; }
bad_base : dialog { unknown; ok_cancel; }
bad_tile : dialog { edittile; ok_cancel; }
bad_case : dialog { Edit_Box; ok_cancel; }
"#;
        let id = load_source(&mut s, &dir, "known-tiles.dcl", source);
        let mut outcomes = Vec::new();
        for invalid in ["bad_base", "bad_tile", "bad_case"] {
            assert_eq!(ok(&mut s, &format!("(new_dialog \"probe\" {id})")), "T");
            assert_eq!(ok(&mut s, "(set_tile \"keep\" \"current\")"), "\"current\"");
            let result = ok(&mut s, &format!("(new_dialog \"{invalid}\" {id})"));
            outcomes.push((result, ok(&mut s, "(get_tile \"keep\")")));
        }
        assert_eq!(outcomes, vec![("nil".into(), "\"current\"".into()); 3]);
    }

    #[test]
    fn dcl_prepare_accepts_documented_exit_button_prototypes() {
        let mut s = session();
        let dir = TempDir::new("prepare-exit-buttons");
        let source = r#"
dismiss : cancel_button { label = "Dismiss"; }
confirm : dialog { ok_button; }
go_back : dialog { dismiss; }
with_help : dialog { ok_only; help_button; }
"#;
        let id = load_source(&mut s, &dir, "exit-buttons.dcl", source);
        let outcomes = ["confirm", "go_back", "with_help"].map(|name| ok(&mut s, &format!("(new_dialog \"{name}\" {id})")));
        assert_eq!(outcomes, ["T", "T", "T"]);
    }

    #[test]
    fn dcl_prepare_keeps_source_order_labels_and_standard_button_text() {
        let mut s = session();
        let dir = TempDir::new("prepare-layout");
        let source = r#"part_field : edit_box { label = "Part"; value = "bolt"; }
stock_field : toggle { label = "In stock"; value = "0"; }
parts : dialog {
  label = "Parts";
  : text { label = "Name"; }
  : column {
    : part_field { key = "part"; }
    : stock_field { key = "stock"; }
  }
  : row {
    : button { key = "apply"; label = "Apply"; }
    ok_cancel;
  }
}
"#;
        let id = load_source(&mut s, &dir, "parts.dcl", source);
        assert_eq!(ok(&mut s, &format!("(new_dialog \"parts\" {id})")), "T");
        let prepared = prepared_dialog(&s);
        let view = prepared.view(9);
        assert_eq!(view.title.as_ref(), "Parts");
        assert_eq!(view.id, 9);
        let mut order = Vec::new();
        layout_outline(&view.controls, &mut order);
        assert_eq!(
            order,
            vec![
                "text::column:Name",
                "column::column:",
                "edit_box:part:column:Part=bolt",
                "toggle:stock:column:In stock=0",
                "row::row:",
                "button:apply:column:Apply=",
                "ok_cancel::row:",
                "ok_button:accept:column:OK=",
                "cancel_button:cancel:column:Cancel=",
            ]
        );
        assert_eq!(ok(&mut s, "(set_tile \"part\" \"clamp\")"), "\"clamp\"");
        let updated = prepared_dialog(&s).view(9);
        let part = find_control(&updated.controls, "part").expect("part");
        assert_eq!(part.value.as_ref(), "clamp");
        assert!(part.enabled);
    }

    #[test]
    fn dcl_prepare_keeps_keyless_text_inside_the_retained_text_limit() {
        let mut s = session();
        let dir = TempDir::new("prepare-keyless-text");
        let repeated_tiles = "notice;".repeat(dcl::MAX_INPUT_BYTES / dcl::MAX_STRING_BYTES);
        let source = format!(
            "notice : text {{ value = \"{}\"; }}\n\
             fits : dialog {{ {repeated_tiles} }}\n\
             too_much : dialog {{ {repeated_tiles} : text {{ value = \"x\"; }} }}\n\
             plain : dialog {{ : text {{ value = \"Ready\"; }} }}",
            "x".repeat(dcl::MAX_STRING_BYTES)
        );
        let id = load_source(&mut s, &dir, "messages.dcl", &source);
        assert_eq!(ok(&mut s, &format!("(new_dialog \"fits\" {id})")), "T");
        assert_eq!(ok(&mut s, &format!("(new_dialog \"too_much\" {id})")), "nil");
        assert_eq!(prepared_dialog(&s).name.as_ref(), "fits");
        assert_eq!(ok(&mut s, &format!("(new_dialog \"plain\" {id})")), "T");
        let view = prepared_dialog(&s).view(9);
        let message = view.controls.first().expect("the text tile");
        assert!(message.key.is_none());
        assert_eq!(message.value.as_ref(), "Ready");
    }

    #[test]
    fn dcl_prepare_counts_default_labels_and_formatted_values() {
        let mut s = session();
        let dir = TempDir::new("prepare-display-budget");
        let full_chunks = dcl::MAX_INPUT_BYTES / dcl::MAX_STRING_BYTES - 1;
        let filler = format!("{} tail;", "notice;".repeat(full_chunks));
        let source = format!(
            "notice : text {{ value = \"{}\"; }}\n\
             tail : text {{ value = \"{}\"; }}\n\
             button_fits : dialog {{ {filler} ok_only; }}\n\
             button_over : dialog {{ {filler} : text {{ value = \"xx\"; }} ok_only; }}\n\
             numbers_fits : dialog {{ {filler} : text {{ value = -10; }} : text {{ value = 0.125; }} }}\n\
             numbers_over : dialog {{ {filler} : text {{ value = -10; }} : text {{ value = 0.125; }} : text {{ value = 1; }} }}",
            "x".repeat(dcl::MAX_STRING_BYTES),
            "x".repeat(dcl::MAX_STRING_BYTES - "accept".len() - "OK".len()),
        );
        let id = load_source(&mut s, &dir, "display-budget.dcl", &source);
        assert_eq!(ok(&mut s, &format!("(new_dialog \"numbers_fits\" {id})")), "T");
        let view = prepared_dialog(&s).view(9);
        let tail = view.controls.get(full_chunks + 1..).expect("numeric text tiles");
        assert_eq!(tail.iter().map(|tile| tile.value.as_ref()).collect::<Vec<_>>(), ["-10", "0.125"]);
        assert_eq!(ok(&mut s, &format!("(new_dialog \"numbers_over\" {id})")), "nil");
        assert_eq!(prepared_dialog(&s).name.as_ref(), "numbers_fits");

        assert_eq!(ok(&mut s, &format!("(new_dialog \"button_fits\" {id})")), "T");
        let view = prepared_dialog(&s).view(9);
        assert_eq!(find_control(&view.controls, "accept").expect("the OK button").label.as_ref(), "OK");
        assert_eq!(ok(&mut s, &format!("(new_dialog \"button_over\" {id})")), "nil");
        assert_eq!(prepared_dialog(&s).name.as_ref(), "button_fits");
    }

    fn layout_outline(controls: &[crate::lisp::DialogControl], out: &mut Vec<String>) {
        for control in controls {
            let key = control.key.as_deref().unwrap_or("");
            let label = control.label.as_ref();
            let value = if control.key.is_some() { format!("={}", control.value) } else { String::new() };
            out.push(format!("{}:{key}:{}:{label}{value}", control.kind, if control.row { "row" } else { "column" }));
            layout_outline(&control.children, out);
        }
    }

    fn find_control<'a>(controls: &'a [crate::lisp::DialogControl], key: &str) -> Option<&'a crate::lisp::DialogControl> {
        for control in controls {
            if control.key.as_deref() == Some(key) {
                return Some(control);
            }
            if let Some(found) = find_control(&control.children, key) {
                return Some(found);
            }
        }
        None
    }

    fn prepared_dialog(s: &Session) -> &crate::lisp::dcl_prepare::PreparedDialog {
        let uid = s.state().unwrap().uid;
        s.lisp.envs.get(&uid).and_then(|env| env.prepared.as_ref()).unwrap()
    }

    #[test]
    fn dcl_prepare_checks_argument_types() {
        let mut s = session();
        assert_eq!(eval(&mut s, "(new_dialog)"), Err("too few arguments".into()));
        assert_eq!(eval(&mut s, "(new_dialog \"a\")"), Err("too few arguments".into()));
        assert_eq!(eval(&mut s, "(new_dialog \"a\" 1 \"\" '(0 0) \"extra\")"), Err("too many arguments".into()));
        assert_eq!(eval(&mut s, "(new_dialog 1 1)"), Err("bad argument type: stringp: 1".into()));
        assert_eq!(eval(&mut s, "(new_dialog \"a\" \"b\")"), Err("bad argument type: fixnump: \"b\"".into()));
        assert_eq!(eval(&mut s, "(new_dialog \"a\" 1 2)"), Err("bad argument type: stringp: 2".into()));
        assert_eq!(eval(&mut s, "(new_dialog \"a\" 1 \"\" \"pt\")"), Err("bad argument type: 2D/3D point: \"pt\"".into()));
        assert_eq!(eval(&mut s, "(get_tile)"), Err("too few arguments".into()));
        assert_eq!(eval(&mut s, "(get_tile \"a\" \"b\")"), Err("too many arguments".into()));
        assert_eq!(eval(&mut s, "(get_tile 1)"), Err("bad argument type: stringp: 1".into()));
        assert_eq!(eval(&mut s, "(set_tile)"), Err("too few arguments".into()));
        assert_eq!(eval(&mut s, "(set_tile \"a\")"), Err("too few arguments".into()));
        assert_eq!(eval(&mut s, "(set_tile \"a\" \"b\" \"c\")"), Err("too many arguments".into()));
        assert_eq!(eval(&mut s, "(set_tile 1 \"b\")"), Err("bad argument type: stringp: 1".into()));
        assert_eq!(eval(&mut s, "(set_tile \"a\" 1)"), Err("bad argument type: stringp: 1".into()));
        assert_eq!(ok(&mut s, "(new_dialog \"missing\" 1)"), "nil");
        assert_eq!(ok(&mut s, "(get_tile \"missing\")"), "nil");
        assert_eq!(ok(&mut s, "(set_tile \"missing\" \"x\")"), "nil");
    }

    #[test]
    fn dcl_dialog_checks_argument_types() {
        let mut s = session();
        assert_eq!(eval(&mut s, "(action_tile)"), Err("too few arguments".into()));
        assert_eq!(eval(&mut s, "(action_tile \"a\")"), Err("too few arguments".into()));
        assert_eq!(eval(&mut s, "(action_tile \"a\" \"b\" \"c\")"), Err("too many arguments".into()));
        assert_eq!(eval(&mut s, "(action_tile 1 \"b\")"), Err("bad argument type: stringp: 1".into()));
        assert_eq!(eval(&mut s, "(action_tile \"a\" 1)"), Err("bad argument type: stringp: 1".into()));
        assert_eq!(eval(&mut s, "(mode_tile)"), Err("too few arguments".into()));
        assert_eq!(eval(&mut s, "(mode_tile \"a\")"), Err("too few arguments".into()));
        assert_eq!(eval(&mut s, "(mode_tile \"a\" 0 \"b\")"), Err("too many arguments".into()));
        assert_eq!(eval(&mut s, "(mode_tile 1 0)"), Err("bad argument type: stringp: 1".into()));
        assert_eq!(eval(&mut s, "(mode_tile \"a\" \"b\")"), Err("bad argument type: fixnump: \"b\"".into()));
        assert_eq!(eval(&mut s, "(start_dialog 1)"), Err("too many arguments".into()));
        assert_eq!(eval(&mut s, "(done_dialog \"x\")"), Err("bad argument type: fixnump: \"x\"".into()));
        assert_eq!(eval(&mut s, "(done_dialog 1 2)"), Err("too many arguments".into()));
        assert_eq!(eval(&mut s, "(term_dialog 1)"), Err("too many arguments".into()));
        assert_eq!(ok(&mut s, "(action_tile \"missing\" \"(done)\")"), "nil");
        assert_eq!(ok(&mut s, "(mode_tile \"missing\" 0)"), "nil");
        assert_eq!(ok(&mut s, "(start_dialog)"), "nil");
        assert_eq!(ok(&mut s, "(done_dialog 1)"), "nil");
        assert_eq!(ok(&mut s, "(term_dialog)"), "nil");
    }

    const PROBE_TILES: &str = r#"probe : dialog {
  label = "Probe";
  : edit_box { key = "name"; value = "old"; action = "(setq which (strcat which \"dcl\"))"; }
  : button { key = "plain"; }
  : button { key = "accept"; }
  : button { key = "cancel"; }
  : button { key = "go"; }
"#;

    fn probe_source(extra_keys: usize) -> String {
        let mut source = String::from(PROBE_TILES);
        for index in 0..extra_keys {
            source.push_str(&format!("  : button {{ key = \"k{index:02}\"; }}\n"));
        }
        source.push_str("}\n");
        source
    }

    fn shown(s: &Session, name: &str) -> String {
        to_text(&s.lisp_var(name), true)
    }

    fn logged(s: &Session, text: &str) -> bool {
        s.log.iter().any(|line| line.contains(text))
    }

    fn kind_count(s: &Session, kind: fn(&EntityKind) -> bool) -> usize {
        s.doc().unwrap().model.iter().filter(|entity| kind(&entity.kind)).count()
    }

    fn is_line(kind: &EntityKind) -> bool {
        matches!(kind, EntityKind::Line(_))
    }

    fn is_circle(kind: &EntityKind) -> bool {
        matches!(kind, EntityKind::Circle(_))
    }

    /// Start one interactive routine. The form after `start_dialog` is the continuation.
    fn start_interactive(s: &mut Session, code: &str) {
        s.cmdline(code).unwrap();
        assert!(s.lisp_running(), "start_dialog waits on the interactive entry: {:?}", s.log);
        assert!(s.dialog_active());
    }

    /// Send one event to the dialog that is waiting. An idle session ignores id 0.
    fn tile_event(s: &mut Session, key: &str, reason: i32) -> crate::Result<DialogEventOutcome> {
        let id = s.dialog_id().unwrap_or(0);
        s.dialog_event(DialogEvent { id, key, reason, value: None, position: None })
    }

    fn edit_event(s: &mut Session, key: &str, value: Option<&str>, position: Option<(f64, f64)>) -> crate::Result<DialogEventOutcome> {
        let id = s.dialog_id().unwrap_or(0);
        s.dialog_event(DialogEvent { id, key, reason: 1, value, position })
    }

    #[test]
    fn dcl_dialog_registers_actions_and_modes_for_a_keyed_tile() {
        let mut s = session();
        let dir = TempDir::new("lifecycle-mode");
        let extra = (dcl::MAX_INPUT_BYTES / dcl::MAX_STRING_BYTES) + 1;
        let id = load_source(&mut s, &dir, "probe.dcl", &probe_source(extra));
        assert_eq!(ok(&mut s, &format!("(new_dialog \"probe\" {id})")), "T");
        assert_eq!(ok(&mut s, "(action_tile \"name\" \"(setq saw $value)\")"), "T");
        assert_eq!(ok(&mut s, "(action_tile \"missing\" \"(setq saw 1)\")"), "nil");
        assert_eq!(ok(&mut s, "(action_tile \"Name\" \"(setq saw 1)\")"), "nil");
        assert_eq!(ok(&mut s, "(mode_tile \"name\" 1)"), "nil");
        assert_eq!(s.dialog_tile_enabled("name"), Some(false));
        assert_eq!(ok(&mut s, "(mode_tile \"name\" 0)"), "nil");
        assert_eq!(s.dialog_tile_enabled("name"), Some(true));
        assert_eq!(ok(&mut s, "(mode_tile \"missing\" 1)"), "nil");
        assert_eq!(s.dialog_tile_enabled("name"), Some(true));
        assert_eq!(ok(&mut s, "(mode_tile \"name\" 9)"), "nil");
        assert_eq!(ok(&mut s, "(mode_tile \"name\" 2)"), "nil");
        assert_eq!(ok(&mut s, "(mode_tile \"name\" 3)"), "nil");
        assert_eq!(ok(&mut s, "(mode_tile \"name\" 4)"), "nil");
        assert_eq!(
            s.take_dialog_requests(),
            vec![DialogRequest::Focus("name".into()), DialogRequest::Select("name".into()), DialogRequest::Highlight("name".into()),]
        );
        assert_eq!(ok(&mut s, "(mode_tile \"name\" 2)"), "nil");

        assert_eq!(ok(&mut s, &format!("(new_dialog \"probe\" {id})")), "T");
        assert!(s.take_dialog_requests().is_empty(), "a new preparation drops adapter requests");
        for _ in 0..80 {
            assert_eq!(ok(&mut s, "(mode_tile \"name\" 2)"), "nil");
        }
        let carried = s.take_dialog_requests().len();
        assert!(carried > 0 && carried <= 64, "focus requests stay bounded, carried {carried}");
        assert!(s.take_dialog_requests().is_empty());
        assert_eq!(ok(&mut s, "(mode_tile \"name\" 3)"), "nil");
        assert_eq!(s.take_dialog_requests(), vec![DialogRequest::Select("name".into())]);

        let chunk = "y".repeat(dcl::MAX_STRING_BYTES);
        let over = "y".repeat(dcl::MAX_STRING_BYTES + 1);
        assert_eq!(ok(&mut s, &format!("(action_tile \"name\" \"{over}\")")), "nil");
        let fit = dcl::MAX_INPUT_BYTES / dcl::MAX_STRING_BYTES;
        for index in 0..fit {
            assert_eq!(ok(&mut s, &format!("(action_tile \"k{index:02}\" \"{chunk}\")")), "T");
        }
        assert_eq!(ok(&mut s, &format!("(action_tile \"k{fit:02}\" \"{chunk}\")")), "nil");
        assert_eq!(ok(&mut s, "(action_tile \"k00\" \"(setq kept T)\")"), "T");
    }

    #[test]
    fn dcl_dialog_runs_a_callback_and_resumes_once() {
        let mut s = session();
        let dir = TempDir::new("lifecycle-run");
        let id = load_source(&mut s, &dir, "probe.dcl", &probe_source(0));
        let code = format!(
            "(progn \
                (command \"_.LINE\" \"0,0\" \"10,0\" \"\") \
                (setq n 0) \
                (new_dialog \"probe\" {id} \"(setq which \\\"default\\\")\" '(8 21)) \
                (action_tile \"go\" \"(set_tile \\\"name\\\" \\\"traced\\\") (setq seen (get_tile \\\"name\\\")) (setq pos (done_dialog 7))\") \
                (setq st (start_dialog)) \
                (setq n (1+ n)) \
                (setq after (get_tile \"name\")) \
                (unload_dialog {id}))"
        );
        start_interactive(&mut s, &code);
        assert_eq!(kind_count(&s, is_line), 1, "the drawing stays while the dialog waits");
        assert_eq!(prepared_dialog(&s).get_tile("name").as_deref(), Some("old"));
        tile_event(&mut s, "go", 1).unwrap();
        assert!(!s.lisp_running());
        assert!(!s.dialog_active());
        assert_eq!(shown(&s, "N"), "1");
        assert_eq!(shown(&s, "ST"), "7");
        assert_eq!(shown(&s, "SEEN"), "\"traced\"");
        assert_eq!(shown(&s, "AFTER"), "\"traced\"");
        assert_eq!(shown(&s, "POS"), "(8.0 21.0)");
        assert_eq!(kind_count(&s, is_line), 1);
        assert_eq!(ok(&mut s, "(get_tile \"name\")"), "nil");
        tile_event(&mut s, "go", 1).unwrap();
        assert_eq!(shown(&s, "N"), "1", "an event from the expired dialog does not resume again");
        assert_eq!(ok(&mut s, "(setq next 4)"), "4");
        assert_eq!(shown(&s, "NEXT"), "4");
        assert!(!s.lisp_running());
    }

    #[test]
    fn dcl_dialog_prefers_an_action_tile_and_restores_callback_bindings() {
        let mut s = session();
        let dir = TempDir::new("lifecycle-order");
        let id = load_source(&mut s, &dir, "probe.dcl", &probe_source(0));
        let code = format!(
            "(progn \
                (setq which \"\") \
                (setq $key \"outer\") \
                (new_dialog \"probe\" {id} \"(setq which (strcat which \\\"default\\\"))\") \
                (action_tile \"go\" \"(setq k $key v $value r $reason)\") \
                (setq st (start_dialog)) \
                (setq after $key))"
        );
        start_interactive(&mut s, &code);
        tile_event(&mut s, "go", 4).unwrap();
        assert_eq!(shown(&s, "K"), "\"go\"");
        assert_eq!(shown(&s, "V"), "\"\"");
        assert_eq!(shown(&s, "R"), "4");
        assert_eq!(shown(&s, "$KEY"), "\"outer\"");
        tile_event(&mut s, "plain", 1).unwrap();
        assert_eq!(shown(&s, "WHICH"), "\"default\"");
        tile_event(&mut s, "name", 1).unwrap();
        assert_eq!(shown(&s, "WHICH"), "\"defaultdcl\"");
        tile_event(&mut s, "accept", 1).unwrap();
        assert!(!s.lisp_running());
        assert_eq!(shown(&s, "ST"), "1");
        assert_eq!(shown(&s, "AFTER"), "\"outer\"");
        assert_eq!(shown(&s, "WHICH"), "\"defaultdcl\"");

        let tile_action = format!(
            "(progn \
                (setq which \"\") \
                (new_dialog \"probe\" {id} \"(setq which (strcat which \\\"default\\\"))\") \
                (action_tile \"name\" \"(setq which (strcat which \\\"tile\\\"))\") \
                (setq st (start_dialog)))"
        );
        start_interactive(&mut s, &tile_action);
        tile_event(&mut s, "name", 1).unwrap();
        assert_eq!(shown(&s, "WHICH"), "\"tile\"");
        tile_event(&mut s, "cancel", 1).unwrap();
        assert!(!s.lisp_running());
        assert_eq!(shown(&s, "ST"), "0");
        assert_eq!(shown(&s, "WHICH"), "\"tile\"");
    }

    #[test]
    fn dcl_dialog_closes_from_done_term_and_exit_tiles() {
        let mut s = session();
        let dir = TempDir::new("lifecycle-close");
        let id = load_source(&mut s, &dir, "probe.dcl", &probe_source(0));
        let accept = format!("(progn (setq ran nil) (new_dialog \"probe\" {id} \"(setq ran T)\") (setq st (start_dialog)))");
        start_interactive(&mut s, &accept);
        tile_event(&mut s, "missing", 1).unwrap();
        assert!(s.lisp_running(), "an unknown tile does not end the dialog");
        assert_eq!(shown(&s, "RAN"), "nil");
        tile_event(&mut s, "accept", 1).unwrap();
        assert!(!s.lisp_running());
        assert_eq!(shown(&s, "ST"), "1");
        assert_eq!(shown(&s, "RAN"), "nil");

        let cancel = format!("(progn (setq ran nil) (new_dialog \"probe\" {id} \"(setq ran T)\") (setq st (start_dialog)))");
        start_interactive(&mut s, &cancel);
        tile_event(&mut s, "cancel", 1).unwrap();
        assert_eq!(shown(&s, "ST"), "0");
        assert_eq!(shown(&s, "RAN"), "nil");

        let explicit = format!(
            "(progn \
                (new_dialog \"probe\" {id} \"\" '(3 9)) \
                (action_tile \"accept\" \"(setq saw $key)\") \
                (action_tile \"go\" \"(setq pos (done_dialog))\") \
                (setq st (start_dialog)))"
        );
        start_interactive(&mut s, &explicit);
        tile_event(&mut s, "accept", 1).unwrap();
        assert!(s.lisp_running(), "an explicit exit action controls closure");
        assert_eq!(shown(&s, "SAW"), "\"accept\"");
        tile_event(&mut s, "go", 1).unwrap();
        assert!(!s.lisp_running());
        assert_eq!(shown(&s, "ST"), "0");
        assert_eq!(shown(&s, "POS"), "(3.0 9.0)");

        let term = format!("(progn (new_dialog \"probe\" {id}) (action_tile \"go\" \"(setq tr (term_dialog))\") (setq st (start_dialog)))");
        start_interactive(&mut s, &term);
        tile_event(&mut s, "go", 1).unwrap();
        assert!(!s.lisp_running());
        assert_eq!(shown(&s, "ST"), "-1");
        assert_eq!(shown(&s, "TR"), "nil");
    }

    #[test]
    fn dcl_dialog_ignores_disabled_actions_and_exit_buttons() {
        let mut s = session();
        let dir = TempDir::new("lifecycle-disabled");
        let id = load_source(&mut s, &dir, "probe.dcl", &probe_source(0));
        let code = format!(
            "(progn (new_dialog \"probe\" {id}) (setq ran nil)
             (action_tile \"go\" \"(setq ran T)\")
             (mode_tile \"go\" 1) (mode_tile \"accept\" 1) (setq st (start_dialog)))"
        );
        start_interactive(&mut s, &code);
        tile_event(&mut s, "go", 1).unwrap();
        assert_eq!(shown(&s, "RAN"), "nil");
        tile_event(&mut s, "accept", 1).unwrap();
        assert!(s.dialog_active());
        tile_event(&mut s, "cancel", 1).unwrap();
        assert!(!s.dialog_active());
        assert_eq!(shown(&s, "ST"), "0");
    }

    #[test]
    fn dcl_dialog_ends_when_an_action_unloads_its_file() {
        let mut s = session();
        let dir = TempDir::new("lifecycle-unload");
        let id = load_source(&mut s, &dir, "probe.dcl", &probe_source(0));
        let code = format!("(progn (new_dialog \"probe\" {id}) (action_tile \"go\" \"(unload_dialog {id})\") (setq st (start_dialog)))");
        start_interactive(&mut s, &code);
        tile_event(&mut s, "go", 1).unwrap();
        assert!(!s.dialog_active());
        assert_eq!(shown(&s, "ST"), "-1");
        assert_eq!(ok(&mut s, &format!("(new_dialog \"probe\" {id})")), "nil");
    }

    #[test]
    fn dcl_dialog_keeps_the_engine_usable_after_failure() {
        let mut s = session();
        let dir = TempDir::new("lifecycle-fail");
        let id = load_source(&mut s, &dir, "probe.dcl", &probe_source(0));
        assert_eq!(
            eval(&mut s, &format!("(progn (new_dialog \"probe\" {id}) (start_dialog))")),
            Err("a dialog cannot start from a blocking call".into())
        );
        assert!(!s.lisp_running());
        assert!(!s.dialog_active());
        assert_eq!(ok(&mut s, "(get_tile \"name\")"), "\"old\"");

        let malformed = format!("(progn (new_dialog \"probe\" {id}) (action_tile \"go\" \"(\") (setq st (start_dialog)))");
        start_interactive(&mut s, &malformed);
        tile_event(&mut s, "go", 1).unwrap();
        assert!(!s.lisp_running());
        assert!(!s.dialog_active());
        assert!(logged(&s, "malformed list on input"));
        assert_eq!(ok(&mut s, "(+ 1 2)"), "3");

        let command = format!(
            "(progn (new_dialog \"probe\" {id}) (action_tile \"go\" \"(command \\\"_.CIRCLE\\\" \\\"0,0\\\" \\\"2\\\")\") (setq st (start_dialog)))"
        );
        start_interactive(&mut s, &command);
        tile_event(&mut s, "go", 1).unwrap();
        assert!(!s.lisp_running());
        assert!(!s.dialog_active());
        assert!(logged(&s, "command cannot be called from a dialog action"));
        assert_eq!(kind_count(&s, is_circle), 0);
        tile_event(&mut s, "go", 1).unwrap();
        assert_eq!(ok(&mut s, "(setq next 5)"), "5");

        let waiting = format!("(progn (new_dialog \"probe\" {id}) (setq st (start_dialog)))");
        start_interactive(&mut s, &waiting);
        s.cancel();
        assert!(!s.lisp_running());
        assert!(!s.dialog_active());
        assert!(logged(&s, "Function cancelled"));
        tile_event(&mut s, "accept", 1).unwrap();
        assert_eq!(ok(&mut s, "(+ 4 5)"), "9");
        assert!(!s.lisp_running());
    }

    #[test]
    fn dcl_dialog_ignores_a_stale_event_for_the_previous_id() {
        let mut s = session();
        let dir = TempDir::new("lifecycle-stale");
        let id = load_source(&mut s, &dir, "probe.dcl", &probe_source(0));
        let code = format!(
            "(progn (setq n 0) (new_dialog \"probe\" {id}) (setq a (start_dialog)) \
             (new_dialog \"probe\" {id}) (setq b (start_dialog)) (setq n (1+ n)))"
        );
        start_interactive(&mut s, &code);
        let first = s.dialog_id().unwrap();
        tile_event(&mut s, "accept", 1).unwrap();
        let second = s.dialog_id().unwrap();
        assert_ne!(first, second);
        s.dialog_event(DialogEvent { id: first, key: "accept", reason: 1, value: Some("stale"), position: Some((9.0, 9.0)) }).unwrap();
        assert!(s.dialog_active());
        assert_eq!(shown(&s, "N"), "0");
        assert_eq!(shown(&s, "B"), "nil");
        assert_eq!(prepared_dialog(&s).get_tile("name").as_deref(), Some("old"));
        tile_event(&mut s, "cancel", 1).unwrap();
        assert!(!s.lisp_running());
        assert_eq!(shown(&s, "A"), "1");
        assert_eq!(shown(&s, "B"), "0");
        assert_eq!(shown(&s, "N"), "1");
    }

    #[test]
    fn dcl_dialog_applies_the_edit_and_rejects_a_bad_event() {
        let mut s = session();
        let dir = TempDir::new("lifecycle-edit");
        let id = load_source(&mut s, &dir, "probe.dcl", &probe_source(0));
        let code = format!(
            "(progn (new_dialog \"probe\" {id}) \
             (action_tile \"name\" \"(setq saw $value) (setq got (get_tile \\\"name\\\")) (setq pos (done_dialog 4))\") \
             (setq st (start_dialog)))"
        );
        start_interactive(&mut s, &code);
        let huge = "z".repeat(dcl::MAX_STRING_BYTES + 1);
        assert_eq!(edit_event(&mut s, "name", Some(&huge), Some((1.0, 2.0))).unwrap(), DialogEventOutcome::TextLimit);
        assert_eq!(edit_event(&mut s, "name", Some("nope"), Some((f64::NAN, 1.0))).unwrap(), DialogEventOutcome::Ignored);
        assert!(s.dialog_active());
        assert_eq!(prepared_dialog(&s).get_tile("name").as_deref(), Some("old"));
        assert_eq!(edit_event(&mut s, "name", Some("edited"), Some((12.5, 3.5))).unwrap(), DialogEventOutcome::Applied);
        assert!(!s.lisp_running());
        assert_eq!(shown(&s, "SAW"), "\"edited\"");
        assert_eq!(shown(&s, "GOT"), "\"edited\"");
        assert_eq!(shown(&s, "POS"), "(12.5 3.5)");
        assert_eq!(shown(&s, "ST"), "4");
    }

    #[test]
    fn dcl_dialog_keeps_an_edit_without_an_action() {
        let mut s = session();
        let dir = TempDir::new("lifecycle-edit-without-action");
        let source = r#"probe : dialog { : edit_box { key = "field"; value = "old"; } ok_cancel; }"#;
        let id = load_source(&mut s, &dir, "probe.dcl", source);
        let code = format!(
            "(progn (new_dialog \"probe\" {id})
             (action_tile \"accept\" \"(setq pos (done_dialog 7))\")
             (setq status (start_dialog) edited (get_tile \"field\")))"
        );
        start_interactive(&mut s, &code);
        edit_event(&mut s, "field", Some("new"), Some((3.0, 4.0))).unwrap();
        assert!(s.dialog_active());
        tile_event(&mut s, "accept", 1).unwrap();
        assert_eq!(shown(&s, "EDITED"), "\"new\"");
        assert_eq!(shown(&s, "POS"), "(3.0 4.0)");
        assert_eq!(shown(&s, "STATUS"), "7");
    }

    #[test]
    fn dcl_dialog_supplies_standard_exit_keys() {
        let mut s = session();
        let dir = TempDir::new("lifecycle-exit-keys");
        let source = r#"
dismiss : cancel_button { label = "Dismiss"; }
only : dialog { ok_only; }
pair : dialog { ok_cancel; }
helped : dialog { ok_cancel_help; }
infoed : dialog { ok_cancel_help_info; }
noted : dialog { ok_cancel_help_errtile; }
custom : dialog { dismiss; }
renamed : dialog { : ok_button { key = "go"; } }
live : dialog { ok_button; cancel_button; }
"#;
        let id = load_source(&mut s, &dir, "exits.dcl", source);
        for (name, keys) in [
            ("only", vec!["accept"]),
            ("pair", vec!["accept", "cancel"]),
            ("helped", vec!["accept", "cancel", "help"]),
            ("infoed", vec!["accept", "cancel", "help", "info"]),
            ("noted", vec!["accept", "cancel", "help", "error"]),
            ("custom", vec!["cancel"]),
            ("renamed", vec!["go"]),
        ] {
            assert_eq!(ok(&mut s, &format!("(new_dialog \"{name}\" {id})")), "T", "{name}");
            for key in keys {
                assert_eq!(ok(&mut s, &format!("(get_tile \"{key}\")")), "\"\"", "{name} {key}");
            }
        }
        assert_eq!(ok(&mut s, "(get_tile \"accept\")"), "nil");

        let accept = format!("(progn (new_dialog \"only\" {id}) (setq st (start_dialog)))");
        start_interactive(&mut s, &accept);
        tile_event(&mut s, "accept", 1).unwrap();
        assert_eq!(shown(&s, "ST"), "1");

        let cancel = format!("(progn (new_dialog \"pair\" {id}) (setq st (start_dialog)))");
        start_interactive(&mut s, &cancel);
        tile_event(&mut s, "cancel", 1).unwrap();
        assert_eq!(shown(&s, "ST"), "0");

        let explicit = format!("(progn (new_dialog \"live\" {id}) (action_tile \"accept\" \"(setq saw $key)\") (setq st (start_dialog)))");
        start_interactive(&mut s, &explicit);
        tile_event(&mut s, "accept", 1).unwrap();
        assert!(s.dialog_active(), "an explicit exit action controls closure");
        assert_eq!(shown(&s, "SAW"), "\"accept\"");
        tile_event(&mut s, "cancel", 1).unwrap();
        assert_eq!(shown(&s, "ST"), "0");
    }

    #[test]
    fn dcl_dialog_unwinds_a_callback_error_and_escape() {
        let mut s = session();
        let dir = TempDir::new("lifecycle-unwind");
        let id = load_source(&mut s, &dir, "probe.dcl", &probe_source(0));
        let caught = format!(
            "(progn (new_dialog \"probe\" {id}) \
             (setq err (vl-catch-all-apply '(lambda () (start_dialog)) nil)) \
             (setq msg (vl-catch-all-error-message err)) (setq after 1))"
        );
        assert_eq!(ok(&mut s, &caught), "1");
        assert_eq!(shown(&s, "MSG"), "\"a dialog cannot start from a blocking call\"");
        assert!(!s.dialog_active());
        assert_eq!(ok(&mut s, "(get_tile \"name\")"), "\"old\"");

        let malformed = format!(
            "(progn (setq $key \"outer\") (new_dialog \"probe\" {id}) (action_tile \"go\" \"(\") \
             (setq err (vl-catch-all-apply '(lambda () (setq st (start_dialog))) nil)) (setq after $key))"
        );
        start_interactive(&mut s, &malformed);
        tile_event(&mut s, "go", 1).unwrap();
        assert!(!s.lisp_running());
        assert!(!s.dialog_active());
        assert_eq!(ok(&mut s, "(vl-catch-all-error-p err)"), "T");
        assert_eq!(shown(&s, "AFTER"), "\"outer\"");
        assert_eq!(ok(&mut s, "(+ 1 2)"), "3");

        let suspended = format!(
            "(progn (setq $key \"outer\") (new_dialog \"probe\" {id}) \
             (action_tile \"go\" \"(getpoint \\\"Pick: \\\")\") (setq st (start_dialog)))"
        );
        start_interactive(&mut s, &suspended);
        tile_event(&mut s, "go", 1).unwrap();
        assert!(s.lisp_running());
        assert_eq!(shown(&s, "$KEY"), "\"go\"");
        s.cancel();
        assert!(!s.lisp_running());
        assert!(!s.dialog_active());
        assert_eq!(shown(&s, "$KEY"), "\"outer\"");
        assert_eq!(ok(&mut s, "(+ 4 5)"), "9");

        let yielded = format!(
            "(progn (setq $key \"outer\") (new_dialog \"probe\" {id}) \
             (action_tile \"go\" \"(repeat 200000 (setq i 1))\") (setq st (start_dialog)))"
        );
        start_interactive(&mut s, &yielded);
        tile_event(&mut s, "go", 1).unwrap();
        assert!(s.lisp_busy(), "the callback yields with its bindings still saved");
        assert_eq!(shown(&s, "$KEY"), "\"go\"");
        s.cancel();
        assert!(!s.lisp_running());
        assert!(!s.dialog_active());
        assert_eq!(shown(&s, "$KEY"), "\"outer\"");
        assert_eq!(ok(&mut s, "(+ 6 1)"), "7");
    }

    #[test]
    fn dcl_dialog_rejects_a_script_and_keeps_later_script_input() {
        let mut s = session();
        let dir = TempDir::new("lifecycle-script");
        let id = load_source(&mut s, &dir, "probe.dcl", &probe_source(0));
        s.script("LINE 0,0 10,0 \n").unwrap();
        assert_eq!(kind_count(&s, is_line), 1);
        let script = format!("(progn (new_dialog \"probe\" {id}) (start_dialog))\n(+ 2 3)\n");
        s.script(&script).unwrap();
        assert!(!s.lisp_running());
        assert!(!s.dialog_active());
        assert!(logged(&s, "a dialog cannot start from a script"));
        assert!(logged(&s, "5"));
        let command = format!("(defun c:SHOW () (new_dialog \"probe\" {id}) (start_dialog) (princ))\n");
        assert_eq!(ok(&mut s, &command), "C:SHOW");
        s.script("SHOW\n(+ 8 1)\n").unwrap();
        assert!(!s.lisp_running());
        assert!(!s.dialog_active());
        assert!(logged(&s, "9"));
        assert_eq!(kind_count(&s, is_line), 1);
    }

    #[test]
    fn dcl_dialog_blocks_drawing_input_until_escape() {
        let mut s = session();
        let dir = TempDir::new("lifecycle-modal");
        let id = load_source(&mut s, &dir, "probe.dcl", &probe_source(0));
        let code = format!("(progn (command \"_.LINE\" \"0,0\" \"10,0\" \"\") (new_dialog \"probe\" {id}) (setq st (start_dialog)))");
        start_interactive(&mut s, &code);
        assert_eq!(kind_count(&s, is_line), 1);
        s.cmdline("LINE 1,1 2,2 ").unwrap();
        let json_line = r#"line {"points":[[1,1],[2,2]]}"#;
        s.cmdline(json_line).unwrap();
        s.script(json_line).unwrap();
        let undo_before = s.state().unwrap().undo.len();
        let space_before = s.layout_space();
        for (id, params) in
            [("line", json!({ "points": [[1, 1], [2, 2]] })), ("u", json!({})), ("redo", json!({})), ("layout.set", json!({ "name": "Layout1" }))]
        {
            let error = s.execute(id, &params).unwrap_err();
            assert!(error.to_string().contains("active DCL dialog"), "{error}");
        }
        assert_eq!(s.state().unwrap().undo.len(), undo_before);
        assert_eq!(s.layout_space(), space_before);
        s.input(crate::Input::Enter).unwrap();
        s.idle_click(cadcraft_geom::Vec2::new(5.0, 0.0), false).unwrap();
        assert_eq!((kind_count(&s, is_line), s.prompting()), (1, true));
        assert!(s.selection().is_empty());
        assert!(s.dialog_active());
        s.cancel();
        assert!(!s.lisp_running());
        assert!(!s.dialog_active());
        assert!(!s.prompting());
        s.idle_click(cadcraft_geom::Vec2::new(5.0, 0.0), false).unwrap();
        assert!(!s.selection().is_empty(), "the same click selects once the dialog is gone");
        s.cmdline("LINE 1,1 2,2 ").unwrap();
        assert_eq!(kind_count(&s, is_line), 2);
    }

    #[test]
    fn dcl_dialog_keeps_block_editor_context_and_undo() {
        let mut s = session();
        let dir = TempDir::new("block-editor-modal");
        let id = load_source(&mut s, &dir, "probe.dcl", &probe_source(0));
        let code = format!("(progn (new_dialog \"probe\" {id}) (setq st (start_dialog)))");
        let inspect = json!({ "entities": true });
        let edit_block = json!({ "name": "Scratch", "create": true });
        s.execute("line", &json!({ "points": [[0, 0], [10, 0]] })).unwrap();
        let host = s.execute("drawing.inspect", &inspect).unwrap();

        start_interactive(&mut s, &code);
        let error = s.execute("bedit", &edit_block).unwrap_err();
        assert!(error.to_string().contains("active DCL dialog"), "{error}");
        assert_eq!(tile_event(&mut s, "accept", 1).unwrap(), DialogEventOutcome::Applied);
        assert_eq!(shown(&s, "ST"), "1");
        assert_eq!(s.execute("drawing.inspect", &inspect).unwrap(), host);

        s.execute("bedit", &edit_block).unwrap();
        s.execute("line", &json!({ "points": [[0, 0], [0, 6]] })).unwrap();
        let editing = s.execute("drawing.inspect", &inspect).unwrap();
        assert_eq!(editing["blockEditor"], "Scratch");
        assert!(s.state().unwrap().block_unsaved());
        start_interactive(&mut s, &code);
        for (command, params) in [
            ("bedit", json!({ "name": "Other", "create": true, "save": true })),
            ("bsave", json!({})),
            ("bsaveas", json!({ "name": "Other" })),
            ("bclose", json!({ "save": true })),
        ] {
            let error = s.execute(command, &params).unwrap_err();
            assert!(error.to_string().contains("active DCL dialog"), "{error}");
        }
        assert_eq!(tile_event(&mut s, "accept", 1).unwrap(), DialogEventOutcome::Applied);
        assert_eq!(shown(&s, "ST"), "1");
        assert!(s.state().unwrap().block_unsaved());
        assert_eq!(s.execute("drawing.inspect", &inspect).unwrap(), editing);

        s.execute("bclose", &json!({ "save": false })).unwrap();
        assert_eq!(s.execute("drawing.inspect", &inspect).unwrap(), host);
    }

    #[test]
    fn dcl_dialog_keeps_an_unlocked_viewport_and_undo() {
        let mut s = session();
        let dir = TempDir::new("viewport-modal");
        let id = load_source(&mut s, &dir, "probe.dcl", &probe_source(0));
        let code = format!("(progn (new_dialog \"probe\" {id}) (setq st (start_dialog)))");
        s.execute("layout.set", &json!({ "name": "Layout1" })).unwrap();
        let created = s.execute("mview", &json!({ "p1": [20, 20], "p2": [120, 100] })).unwrap();
        s.execute("mspace", &json!({ "handle": created["viewports"][0] })).unwrap();
        let viewport = s.state().unwrap().active_viewport().unwrap();
        assert!(!viewport.1.locked);
        let drawing = s.execute("drawing.inspect", &json!({})).unwrap();
        let view = s.execute("view.get", &json!({})).unwrap();

        start_interactive(&mut s, &code);
        for (command, params) in [
            ("dview", json!({ "twist": 45 })),
            ("view.set", json!({ "center": [10, 20], "height": 60 })),
            ("zoom.in", json!({})),
            ("pan", json!({ "delta": [15, 5] })),
        ] {
            let error = s.execute(command, &params).unwrap_err();
            assert!(error.to_string().contains("active DCL dialog"), "{error}");
        }
        assert!(s.zoom_about(2.0, cadcraft_geom::Vec2::ZERO).is_err());
        assert!(s.zoom_extents().is_err());
        assert_eq!(s.state().unwrap().active_viewport().unwrap(), viewport);
        assert_eq!(s.execute("view.get", &json!({})).unwrap(), view);
        assert_eq!(s.execute("drawing.inspect", &json!({})).unwrap(), drawing);
        assert_eq!(tile_event(&mut s, "accept", 1).unwrap(), DialogEventOutcome::Applied);
        assert_eq!(shown(&s, "ST"), "1");
        assert_eq!(s.execute("drawing.inspect", &json!({})).unwrap(), drawing);

        s.execute("dview", &json!({ "twist": 45 })).unwrap();
        assert_ne!(s.state().unwrap().active_viewport().unwrap(), viewport);
        assert_eq!(s.undo().unwrap().as_deref(), Some("DView"));
        assert_eq!(s.state().unwrap().active_viewport().unwrap(), viewport);
        assert_eq!(s.execute("drawing.inspect", &json!({})).unwrap()["undo"], drawing["undo"]);
    }

    #[test]
    fn dcl_dialog_refuses_interactive_table_paste() {
        let mut s = session();
        let dir = TempDir::new("paste-table-modal");
        let id = load_source(&mut s, &dir, "probe.dcl", &probe_source(0));
        let code = format!("(progn (new_dialog \"probe\" {id}) (setq st (start_dialog)))");
        s.execute("line", &json!({ "points": [[0, 0], [10, 0]] })).unwrap();
        let before = s.execute("drawing.inspect", &json!({})).unwrap();

        start_interactive(&mut s, &code);
        let error = s.start_paste_table("A\tB", None).unwrap_err();
        assert!(error.to_string().contains("active DCL dialog"), "{error}");
        assert!(s.dialog_waiting_for_tile());
        assert!(s.running.is_none());
        assert_eq!(tile_event(&mut s, "accept", 1).unwrap(), DialogEventOutcome::Applied);
        assert_eq!(shown(&s, "ST"), "1");
        assert!(!s.prompting());
        assert_eq!(s.execute("drawing.inspect", &json!({})).unwrap(), before);

        s.start_paste_table("A\tB", None).unwrap();
        s.input(crate::Input::Point(cadcraft_geom::Vec2::new(2.0, 2.0))).unwrap();
        assert_ne!(s.execute("drawing.inspect", &json!({})).unwrap(), before);
        assert!(s.undo().unwrap().is_some());
        assert_eq!(s.execute("drawing.inspect", &json!({})).unwrap(), before);
    }

    #[test]
    fn dcl_dialog_refuses_unsupported_interactive_controls() {
        let mut s = session();
        let dir = TempDir::new("unsupported-control");
        for kind in ["radio_button", "slider", "image_button"] {
            let source = format!("probe : dialog {{ : column {{ : {kind} {{ key = \"unsupported\"; }} }} ok_cancel; }}");
            let id = load_source(&mut s, &dir, "probe.dcl", &source);
            assert_eq!(ok(&mut s, &format!("(new_dialog \"probe\" {id})")), "T");
            let result = s.lisp_eval("(start_dialog)", false, false, Vec::new()).unwrap();
            assert!(matches!(result, Some(Err(message)) if message == format!("unsupported dialog control: {kind}")));
            assert!(!s.dialog_active());
            assert_eq!(ok(&mut s, "(+ 1 2)"), "3");
        }
    }

    #[test]
    fn dcl_dialog_new_and_open_cancel_the_modal_run() {
        let mut s = session();
        let dir = TempDir::new("lifecycle-docs");
        let id = load_source(&mut s, &dir, "probe.dcl", &probe_source(0));
        let code = format!("(progn (new_dialog \"probe\" {id}) (setq st (start_dialog)))");
        start_interactive(&mut s, &code);
        let drawings = s.docs.len();
        s.start("NEW").unwrap();
        assert_eq!(s.docs.len(), drawings + 1, "New from a file tab starts a drawing");
        assert!(!s.lisp_running());
        assert!(!s.dialog_active());
        assert_eq!(ok(&mut s, "(+ 1 2)"), "3");

        let reloaded = load_source(&mut s, &dir, "probe.dcl", &probe_source(0));
        let again = format!("(progn (new_dialog \"probe\" {reloaded}) (setq st (start_dialog)))");
        start_interactive(&mut s, &again);
        s.open_drawing(cadcraft_doc::Drawing::new_metric(), "Opened", None);
        assert!(!s.lisp_running());
        assert!(!s.dialog_active());
        assert_eq!(ok(&mut s, "(+ 4 5)"), "9");

        let third = load_source(&mut s, &dir, "probe.dcl", &probe_source(0));
        let waiting = format!("(progn (new_dialog \"probe\" {third}) (setq st (start_dialog)))");
        start_interactive(&mut s, &waiting);
        let other = if s.active == 0 { 1 } else { 0 };
        s.execute("document.switch", &serde_json::json!({ "index": other })).unwrap();
        assert!(!s.dialog_active());
        assert_eq!(ok(&mut s, "(+ 6 1)"), "7");

        let fourth = load_source(&mut s, &dir, "probe.dcl", &probe_source(0));
        let closing = format!("(progn (new_dialog \"probe\" {fourth}) (setq st (start_dialog)))");
        start_interactive(&mut s, &closing);
        s.execute("close", &serde_json::json!({})).unwrap();
        assert!(!s.lisp_running());
        s.execute("new", &serde_json::json!({})).unwrap();
        assert_eq!(ok(&mut s, "(+ 8 1)"), "9");
    }

    #[test]
    fn dcl_dialog_charges_mode_tile_key_copies() {
        let mut s = session();
        let dir = TempDir::new("lifecycle-mode-budget");
        let id = load_source(&mut s, &dir, "probe.dcl", &probe_source(0));
        assert_eq!(ok(&mut s, &format!("(new_dialog \"probe\" {id})")), "T");
        let error = "out of memory: the routine made too much text";

        s.lisp.limits.string_bytes = 8;
        let focus = "(progn (mode_tile \"name\" 2) (mode_tile \"name\" 3) (mode_tile \"name\" 4))";
        assert_eq!(eval(&mut s, focus), Err(error.into()));
        assert_eq!(s.take_dialog_requests(), vec![DialogRequest::Focus("name".into()), DialogRequest::Select("name".into())]);

        s.lisp.limits.string_bytes = 3;
        assert_eq!(eval(&mut s, "(mode_tile \"name\" 1)"), Err(error.into()));
        assert_eq!(s.dialog_tile_enabled("name"), Some(true));
        s.lisp.limits.string_bytes = 4;
        assert_eq!(ok(&mut s, "(progn (mode_tile \"name\" 1) (mode_tile \"name\" 1))"), "nil");
        assert_eq!(s.dialog_tile_enabled("name"), Some(false));
    }

    #[test]
    fn dcl_dialog_charges_actions_and_events_before_allocation() {
        let mut s = session();
        let dir = TempDir::new("lifecycle-budget");
        let wide_action = "y".repeat(400);
        let source = format!(
            "small : dialog {{ : button {{ key = \"k\"; }} }}\nwide : dialog {{ : button {{ key = \"k\"; action = \"{wide_action}\"; }} }}\n"
        );
        let id = load_source(&mut s, &dir, "budget.dcl", &source);
        assert_eq!(ok(&mut s, &format!("(new_dialog \"small\" {id})")), "T");
        assert_eq!(ok(&mut s, "(set_tile \"k\" \"keep\")"), "\"keep\"");
        s.lisp.limits.string_bytes = 80;
        assert_eq!(eval(&mut s, &format!("(new_dialog \"wide\" {id})")), Err("out of memory: the routine made too much text".into()));
        assert_eq!(ok(&mut s, "(get_tile \"k\")"), "\"keep\"");
        assert_eq!(ok(&mut s, "(+ 1 1)"), "2");
        s.lisp.limits.string_bytes = 1 << 20;

        let probe = load_source(&mut s, &dir, "probe.dcl", "probe : dialog { : button { key = \"go\"; } }\n");
        let code = format!("(progn (new_dialog \"probe\" {probe}) (action_tile \"go\" \"(setq n (1+ n))\") (setq n 0) (setq st (start_dialog)))");
        start_interactive(&mut s, &code);
        let used = s.lisp.run.as_ref().unwrap().string_bytes;
        s.lisp.limits.string_bytes = used + 40;
        let mut sent = 0;
        while s.dialog_active() && sent < 8 {
            tile_event(&mut s, "go", 1).unwrap();
            sent += 1;
        }
        assert!(sent >= 2, "repeated events spend the parent budget, sent {sent}");
        assert!(!s.dialog_active());
        assert!(logged(&s, "out of memory: the routine made too much text"));
        assert_eq!(ok(&mut s, "(+ 3 4)"), "7");
        assert!(!s.lisp_running());
    }

    /// Caught `start_dialog` refusals stay in one blocking run. A later command still runs.
    #[test]
    fn dcl_dialog_rejects_repeated_starts_in_one_run() {
        let mut s = session();
        let dir = TempDir::new("lifecycle-reject-repeat");
        let id = load_source(&mut s, &dir, "probe.dcl", &probe_source(0));
        let code = format!(
            "(progn (new_dialog \"probe\" {id}) \
             (repeat 48 (vl-catch-all-apply '(lambda () (start_dialog)) nil)) \
             (setq n 7))"
        );
        assert_eq!(ok(&mut s, &code), "7");
        assert!(!s.dialog_active());
        assert!(!s.lisp_running());
        assert_eq!(ok(&mut s, "(+ 1 2)"), "3");
    }

    /// A script that yields before `start_dialog` still refuses the dialog, then keeps later input.
    #[test]
    fn dcl_dialog_keeps_script_rejection_across_a_yield() {
        let mut s = session();
        let dir = TempDir::new("lifecycle-script-yield");
        let id = load_source(&mut s, &dir, "probe.dcl", &probe_source(0));
        s.script("LINE 0,0 10,0 \n").unwrap();
        assert_eq!(kind_count(&s, is_line), 1);
        let script = format!("(progn (repeat 200000 (setq i 1)) (new_dialog \"probe\" {id}) (start_dialog))\n(+ 2 3)\nLINE 1,1 2,2 \n");
        s.script(&script).unwrap();
        assert!(!s.lisp_running());
        assert!(!s.dialog_active());
        assert!(logged(&s, "a dialog cannot start from a script"));
        assert!(logged(&s, "5"));
        assert_eq!(kind_count(&s, is_line), 2, "a point on a later script line still draws");
        let command = format!("(defun c:SHOW () (repeat 200000 (setq i 1)) (new_dialog \"probe\" {id}) (start_dialog) (princ))");
        assert_eq!(ok(&mut s, &command), "C:SHOW");
        s.script("SHOW\n(+ 8 1)\n").unwrap();
        assert!(!s.lisp_running());
        assert!(!s.dialog_active());
        assert!(logged(&s, "9"));
        assert_eq!(kind_count(&s, is_line), 2);
    }

    /// Cancellation restores the environment of the run's drawing after another drawing is active.
    #[test]
    fn dcl_cancel_keeps_the_run_drawing_environment() {
        let mut s = session();
        let first = s.active;
        let dir = TempDir::new("lifecycle-env");
        let id = load_source(&mut s, &dir, "probe.dcl", &probe_source(0));
        ok(&mut s, "(setq marker \"from-a\")");
        let uid_a = s.state().unwrap().uid;
        assert_eq!(loaded_ids(&s), vec![id]);
        s.cmdline("(progn (command \"_.NEW\") (repeat 200000 (setq i 1)) (setq marker \"finished\"))").unwrap();
        assert!(!s.lisp_running(), "the routine cannot wait after changing drawings");
        let second = s.active;
        let uid_b = s.state().unwrap().uid;
        assert_ne!(uid_a, uid_b);
        assert_eq!(shown(&s, "MARKER"), "nil");
        assert!(loaded_ids(&s).is_empty(), "the new drawing has no dialog handles");
        s.active = first;
        assert_eq!(shown(&s, "MARKER"), "\"from-a\"");
        assert_eq!(loaded_ids(&s), vec![id]);
        s.active = second;
        assert_eq!(shown(&s, "MARKER"), "nil");
        assert!(loaded_ids(&s).is_empty());
        assert_eq!(ok(&mut s, "(+ 4 5)"), "9");
    }

    #[test]
    fn dcl_start_after_lisp_new_leaves_the_new_drawing_ready() {
        let mut s = session();
        let first = s.active;
        let dir = TempDir::new("lifecycle-new-before-start");
        let id = load_source(&mut s, &dir, "probe.dcl", &probe_source(0));
        let code = format!("(progn (new_dialog \"probe\" {id}) (command \"_.NEW\") (start_dialog) (setq finished T))");
        s.cmdline(&code).unwrap();

        assert_ne!(s.active, first);
        assert!(!s.lisp_running());
        assert_eq!(s.dialog_id(), None);
        assert!(loaded_ids(&s).is_empty());
        assert_eq!(ok(&mut s, "(+ 1 2)"), "3");
        s.active = first;
        assert_eq!(loaded_ids(&s), vec![id]);
        assert_eq!(s.dialog_id(), None);
        assert_eq!(shown(&s, "FINISHED"), "nil");
    }

    #[test]
    fn dcl_drawing_change_unwinds_a_handler_that_requests_input() {
        for open in [false, true] {
            let mut s = session();
            let first = s.active;
            let dir = TempDir::new("lifecycle-new-with-handler");
            let id = load_source(&mut s, &dir, "probe.dcl", &probe_source(0));
            ok(&mut s, "(setq x 2) (defun *error* (msg) (setq outer-saw msg) (princ))");
            let code = format!(
                "((lambda (/ *error* x)
                    (setq x 9 *error* (lambda (msg) (setq handler-saw x) (getpoint \"Cleanup point: \") (setq cleanup-ended T)))
                    (new_dialog \"probe\" {id}) (start_dialog)))"
            );
            start_interactive(&mut s, &code);
            if open {
                s.open_drawing(cadcraft_doc::Drawing::new_metric(), "Opened", None);
            } else {
                s.start("NEW").unwrap();
            }

            assert_ne!(s.active, first);
            assert!(!s.lisp_running(), "the cancellation handler stays in the original drawing");
            assert!(s.lisp_prompt().is_none());
            assert_eq!(ok(&mut s, "(+ 4 5)"), "9");
            assert_eq!(shown(&s, "HANDLER-SAW"), "nil");
            s.active = first;
            assert_eq!(shown(&s, "HANDLER-SAW"), "9");
            assert_eq!(shown(&s, "X"), "2");
            assert_eq!(shown(&s, "CLEANUP-ENDED"), "nil");
            assert!(eval(&mut s, "(/ 1 0)").is_err());
            assert_eq!(shown(&s, "OUTER-SAW"), "\"divide by zero\"");
        }
    }

    #[test]
    fn dcl_close_keeps_other_drawings_when_handler_closes_target() {
        for has_other_drawing in [true, false] {
            let mut s = session();
            let first = s.active;
            let remaining = if has_other_drawing {
                s.new_drawing(true);
                s.cmdline("LINE 0,0 10,0 ").unwrap();
                let st = s.state().unwrap();
                let remaining = (st.uid, st.doc.clone(), st.undo.len());
                s.execute("document.switch", &json!({ "index": first })).unwrap();
                Some(remaining)
            } else {
                None
            };
            let dir = TempDir::new("lifecycle-reentrant-close");
            let id = load_source(&mut s, &dir, "probe.dcl", &probe_source(0));
            let edit = if has_other_drawing { r#"(command "_.LINE" '(0 5) '(10 5) "")"# } else { "" };
            let code = format!(
                r#"((lambda (/ *error*)
                     (setq *error* (lambda (msg) (command "_.CLOSE") {edit}))
                     (new_dialog "probe" {id}) (start_dialog)))"#
            );
            start_interactive(&mut s, &code);

            s.execute("close", &json!({ "index": first })).unwrap();

            assert_eq!(s.active, 0);
            assert!(!s.lisp_running());
            if let Some((uid, before, undo_depth)) = remaining {
                assert_eq!(s.docs.len(), 1);
                assert_eq!(s.state().unwrap().uid, uid);
                assert_eq!(kind_count(&s, is_line), 2);
                assert_eq!(s.state().unwrap().undo.len(), undo_depth + 1);
                assert!(s.undo().unwrap().is_some());
                assert!(std::sync::Arc::ptr_eq(&s.state().unwrap().doc, &before));
                assert_eq!(s.state().unwrap().undo.len(), undo_depth);
            } else {
                assert!(s.docs.is_empty());
            }
        }
    }

    #[test]
    fn dcl_switch_keeps_target_identity_when_handler_closes_a_drawing() {
        for closes_target in [false, true] {
            let mut s = session();
            let first = s.active;
            let second = s.new_drawing(true);
            let remaining_uid = s.state().unwrap().uid;
            s.execute("document.switch", &json!({ "index": first })).unwrap();
            let dir = TempDir::new("lifecycle-reentrant-switch");
            let id = load_source(&mut s, &dir, "probe.dcl", &probe_source(0));
            let code = format!(
                r#"((lambda (/ *error*)
                     (setq *error* (lambda (msg) (command "_.CLOSE")))
                     (new_dialog "probe" {id}) (start_dialog)))"#
            );
            start_interactive(&mut s, &code);

            let index = if closes_target { first } else { second };
            let result = s.execute("document.switch", &json!({ "index": index }));
            if closes_target {
                assert!(result.unwrap_err().to_string().contains("no such drawing"));
            } else {
                result.unwrap();
            }
            assert_eq!(s.state().unwrap().uid, remaining_uid);
            assert!(!s.lisp_running());
        }
    }

    #[test]
    fn dcl_drawing_change_ends_a_native_command_started_by_handler() {
        for open in [false, true] {
            let mut s = session();
            let first = s.active;
            let undo_depth = s.state().unwrap().undo.len();
            let dir = TempDir::new("lifecycle-new-with-native-command");
            let id = load_source(&mut s, &dir, "probe.dcl", &probe_source(0));
            let code = format!(
                r#"((lambda (/ *error*)
                     (command "_.LINE" '(0 0) '(10 0))
                     (setq *error* (lambda (msg) (command "_.LINE" '(0 5) '(10 5))))
                     (new_dialog "probe" {id}) (start_dialog)))"#
            );
            start_interactive(&mut s, &code);
            assert!(s.running.is_some(), "LINE is still waiting for a point");

            if open {
                s.open_drawing(cadcraft_doc::Drawing::new_metric(), "Opened", None);
            } else {
                s.start("NEW").unwrap();
            }

            assert_ne!(s.active, first);
            assert!(!s.lisp_running());
            assert!(s.running.is_none(), "the cancellation handler's LINE belongs to the original drawing");
            s.input(crate::Input::Point(cadcraft_geom::Vec2::new(15.0, 0.0))).unwrap();
            assert_eq!(kind_count(&s, is_line), 0);
            assert!(s.undo().unwrap().is_none());

            s.execute("document.switch", &json!({ "index": first })).unwrap();
            assert_eq!(kind_count(&s, is_line), 2);
            assert_eq!(s.state().unwrap().undo.len(), undo_depth + 1);
            assert!(s.undo().unwrap().is_some());
            assert_eq!(kind_count(&s, is_line), 0);
            assert_eq!(s.state().unwrap().undo.len(), undo_depth);
        }
    }

    /// Escape calls the local `*error*` handler with its locals, then restores the outer handler.
    #[test]
    fn dcl_escape_keeps_the_local_error_handler() {
        let mut s = session();
        ok(&mut s, "(defun *error* (msg) (setq outer-saw msg) (princ))");
        ok(
            &mut s,
            "(defun c:HOLD ( / *error* x) (setq *error* (lambda (msg) (setq local-saw msg) (setq local-x x) (princ))) (setq x 9) (getpoint \"Pick: \"))",
        );
        s.cmdline("HOLD").unwrap();
        assert!(s.lisp_prompt().is_some());
        s.cancel();
        assert!(!s.lisp_running());
        assert_eq!(shown(&s, "LOCAL-SAW"), "\"Function cancelled\"");
        assert_eq!(shown(&s, "LOCAL-X"), "9");
        assert_eq!(shown(&s, "OUTER-SAW"), "nil");
        assert_eq!(shown(&s, "X"), "nil");
        assert!(eval(&mut s, "(/ 1 0)").is_err());
        assert_eq!(shown(&s, "OUTER-SAW"), "\"divide by zero\"");

        let dir = TempDir::new("lifecycle-error");
        let id = load_source(&mut s, &dir, "probe.dcl", &probe_source(0));
        ok(&mut s, "(setq outer-saw nil local-saw nil local-x nil)");
        let code = format!(
            "(progn (setq $key \"outer\") (new_dialog \"probe\" {id}) \
             (action_tile \"go\" \"((lambda ( / *error*) (setq *error* (lambda (msg) (setq local-saw msg) (setq local-x $key) (princ))) (getpoint \\\"Pick: \\\")))\") \
             (setq st (start_dialog)))"
        );
        start_interactive(&mut s, &code);
        tile_event(&mut s, "go", 1).unwrap();
        assert!(s.lisp_prompt().is_some());
        s.cancel();
        assert!(!s.lisp_running());
        assert!(!s.dialog_active());
        assert_eq!(shown(&s, "LOCAL-SAW"), "\"Function cancelled\"");
        assert_eq!(shown(&s, "LOCAL-X"), "\"go\"");
        assert_eq!(shown(&s, "$KEY"), "\"outer\"");
        assert_eq!(shown(&s, "OUTER-SAW"), "nil");
        assert_eq!(ok(&mut s, "(+ 6 1)"), "7");
    }

    #[test]
    fn dcl_callback_preserves_text_containing_only_spaces() {
        let mut s = session();
        let dir = TempDir::new("lifecycle-spaces");
        let id = load_source(&mut s, &dir, "probe.dcl", &probe_source(0));
        let code = format!(
            r#"(progn (new_dialog "probe" {id})
                      (action_tile "go" "(setq answer (getstring T)) (done_dialog 1)")
                      (setq status (start_dialog)))"#
        );
        start_interactive(&mut s, &code);
        tile_event(&mut s, "go", 1).unwrap();
        assert!(s.lisp_prompt().is_some());

        s.cmdline("   ").unwrap();

        assert_eq!(shown(&s, "ANSWER"), "\"   \"");
        assert_eq!(shown(&s, "STATUS"), "1");
        assert!(!s.lisp_running());
    }

    /// A callback prompt takes a point and text. Commands, selection, and JSON stay blocked.
    #[test]
    fn dcl_callback_prompt_accepts_an_answer() {
        let mut s = session();
        let dir = TempDir::new("lifecycle-prompt");
        let id = load_source(&mut s, &dir, "probe.dcl", &probe_source(0));
        let code = format!(
            "(progn (command \"_.LINE\" \"0,0\" \"10,0\" \"\") (new_dialog \"probe\" {id}) \
             (action_tile \"go\" \"(setq pt (getpoint \\\"Pick: \\\")) (setq tx (getstring T \\\"Text: \\\")) (done_dialog 3)\") \
             (setq st (start_dialog)))"
        );
        start_interactive(&mut s, &code);
        assert_eq!(kind_count(&s, is_line), 1);
        tile_event(&mut s, "go", 1).unwrap();
        assert!(s.lisp_prompt().is_some());
        s.cmdline("LINE").unwrap();
        s.cmdline(r#"line {"points":[[9,9],[8,8]]}"#).unwrap();
        s.idle_click(cadcraft_geom::Vec2::new(5.0, 0.0), false).unwrap();
        assert_eq!(kind_count(&s, is_line), 1);
        assert!(s.selection().is_empty());
        assert!(s.dialog_active());
        s.cmdline("4,5").unwrap();
        assert_ne!(shown(&s, "PT"), "nil", "the point reaches the callback prompt");
        assert!(s.lisp_prompt().is_some(), "getstring is still waiting");
        let drawings = s.docs.len();
        s.cmdline("'NEW").unwrap();
        assert_eq!(s.docs.len(), drawings, "literal text is the callback's answer");
        assert!(!s.lisp_running());
        assert!(!s.dialog_active());
        assert_eq!(shown(&s, "ST"), "3");
        assert_eq!(shown(&s, "TX"), "\"'NEW\"");
        assert!(shown(&s, "PT").contains('4') && shown(&s, "PT").contains('5'), "{}", shown(&s, "PT"));
        assert_eq!(kind_count(&s, is_line), 1);
    }

    const LIST_BOX: &str = "box : dialog {\n  : list_box { key = \"k\"; }\n  : popup_list { key = \"p\"; }\n  : edit_box { key = \"name\"; }\n  : button { key = \"apply\"; label = \"Apply\"; }\n  ok_cancel;\n}\n";

    fn tile_value(s: &Session, key: &str) -> String {
        fn walk(controls: &[crate::lisp::DialogControl], key: &str) -> Option<String> {
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
        walk(&s.dialog_view().expect("dialog is showing").controls, key).unwrap_or_else(|| panic!("missing tile {key}"))
    }

    /// Inherited and local rows keep their text, and the list calls report the key, the row, and nil.
    #[test]
    fn dcl_list_prepares_values_and_populates_in_order() {
        let mut s = session();
        let dir = TempDir::new("list-populate");
        let source = r#"catalog : list_box { list = "Small\nMedium\nLarge"; value = "1"; }
wood : popup_list { list = "Oak\nPine"; }
box : dialog {
  label = "Choices";
  : catalog { key = "sizes"; }
  : wood { key = "finish"; }
  : catalog { key = "local"; list = "Red\n\nBlue"; value = "0"; }
  : list_box { key = "many"; multiple_select = true; list = "One\nTwo\nThree"; }
  : popup_list { key = "plain"; }
  ok_cancel;
}
"#;
        let id = load_source(&mut s, &dir, "lists.dcl", source);
        let prepared = format!(
            "(progn (new_dialog \"box\" {id}) \
             (setq sizes (get_tile \"sizes\")) \
             (setq finish (get_tile \"finish\")) \
             (setq localv (get_tile \"local\")) \
             (setq manyv (get_tile \"many\")) \
             (setq plain (get_tile \"plain\")) \
             (setq started (start_list \"local\" 1 1)) \
             (setq first (add_list \"Mid\")) \
             (setq second (add_list \"No\")) \
             (setq ended (end_list)) \
             (start_list \"sizes\" 2) (add_list \"X\") (add_list \"Y\") (end_list) \
             (start_list \"plain\") (add_list \"First\") (add_list \"Second\") (end_list) \
             (list sizes finish localv manyv plain started first second ended))"
        );
        assert_eq!(ok(&mut s, &prepared), "(\"1\" \"0\" \"0\" \"\" \"0\" \"local\" \"Mid\" \"No\" nil)");
        assert_eq!(ok(&mut s, "(start_list \"sizes\" 1 5)"), "nil");
        assert_eq!(ok(&mut s, "(progn (start_list \"sizes\" 1 4) (add_list \"Y\") (add_list \"Z\") (end_list))"), "nil");
        assert_eq!(ok(&mut s, "(progn (start_list \"sizes\") (add_list \"Only\") (end_list) (start_list \"sizes\" 1 1))"), "nil");
        assert_eq!(ok(&mut s, "(progn (start_list \"sizes\" 1 0) (add_list \"Only\"))"), "\"Only\"");
    }

    /// A bad call leaves the committed rows and a valid pending update in place.
    #[test]
    fn dcl_list_rejects_bad_calls_and_discards_pending_on_reset() {
        let mut s = session();
        let dir = TempDir::new("list-reject");
        let id = load_source(&mut s, &dir, "box.dcl", LIST_BOX);
        assert_eq!(ok(&mut s, &format!("(new_dialog \"box\" {id})")), "T");
        assert_eq!(ok(&mut s, "(add_list \"early\")"), "nil");
        assert_eq!(ok(&mut s, "(end_list)"), "nil");
        assert_eq!(ok(&mut s, "(start_list \"missing\")"), "nil");
        assert_eq!(ok(&mut s, "(start_list \"name\")"), "nil");
        assert_eq!(ok(&mut s, "(start_list \"k\" 4)"), "nil");
        assert_eq!(ok(&mut s, "(start_list \"k\" 9 0)"), "nil");
        assert_eq!(ok(&mut s, "(start_list \"k\" 1 -1)"), "nil");
        assert_eq!(ok(&mut s, "(start_list \"k\" 1 0)"), "nil");
        assert_eq!(
            eval(&mut s, "(progn (start_list \"k\") (add_list \"keep\") (start_list \"k\" \"no\"))"),
            Err("bad argument type: fixnump: \"no\"".into())
        );
        assert_eq!(ok(&mut s, "(progn (setq kept (add_list \"more\")) (end_list) (start_list \"k\" 1 1))"), "\"k\"");
        assert_eq!(ok(&mut s, "(end_list)"), "nil");
        assert_eq!(shown(&s, "KEPT"), "\"more\"");
        assert_eq!(eval(&mut s, "(start_list)"), Err("too few arguments".into()));
        assert_eq!(eval(&mut s, "(start_list \"k\" 3 0 1)"), Err("too many arguments".into()));
        assert_eq!(eval(&mut s, "(start_list 1)"), Err("bad argument type: stringp: 1".into()));
        assert_eq!(eval(&mut s, "(add_list)"), Err("too few arguments".into()));
        assert_eq!(eval(&mut s, "(add_list \"a\" \"b\")"), Err("too many arguments".into()));
        assert_eq!(eval(&mut s, "(add_list 1)"), Err("bad argument type: stringp: 1".into()));
        assert_eq!(eval(&mut s, "(end_list 1)"), Err("too many arguments".into()));
        assert_eq!(ok(&mut s, "(progn (start_list \"k\" 1 0) (add_list \"keep\"))"), "\"keep\"");

        assert_eq!(ok(&mut s, &format!("(new_dialog \"box\" {id})")), "T");
        assert_eq!(ok(&mut s, "(end_list)"), "nil");
        assert_eq!(ok(&mut s, "(start_list \"k\" 1 0)"), "nil");
        assert_eq!(ok(&mut s, &format!("(progn (start_list \"k\") (add_list \"temp\") (unload_dialog {id}) (end_list))")), "nil");
        assert_eq!(ok(&mut s, &format!("(new_dialog \"box\" {id})")), "nil");
        assert_eq!(ok(&mut s, "(+ 2 3)"), "5");
    }

    /// Row, text, and run limits stop before the extra row is stored.
    #[test]
    fn dcl_list_stops_at_the_row_text_and_run_limits() {
        let mut s = session();
        let dir = TempDir::new("list-limits");
        let id = load_source(&mut s, &dir, "box.dcl", "tight : dialog { : list_box { key = \"k\"; } }\n");
        let empty_rows = "\\n".repeat(32_768);
        let over = format!("over : dialog {{ : list_box {{ key = \"k\"; list = \"{empty_rows}\"; }} }}\n");
        assert_eq!(ok(&mut s, &format!("(new_dialog \"tight\" {id})")), "T");
        assert_eq!(ok(&mut s, "(set_tile \"k\" \"keep\")"), "\"keep\"");
        let over_id = load_source(&mut s, &dir, "over.dcl", &over);
        assert_eq!(ok(&mut s, &format!("(new_dialog \"over\" {over_id})")), "nil");
        assert_eq!(ok(&mut s, "(get_tile \"k\")"), "\"keep\"");

        let fit_id = load_source(&mut s, &dir, "fit.dcl", "fit : dialog { : list_box { key = \"k\"; } }\n");
        assert_eq!(ok(&mut s, &format!("(new_dialog \"fit\" {fit_id})")), "T");
        let counted = "(progn (setq n 0) (start_list \"k\") (repeat 32768 (if (add_list \"x\") (setq n (1+ n)))) (setq extra (add_list \"y\")) (end_list) (setq last (start_list \"k\" 1 32767)) (end_list) (setq past (start_list \"k\" 1 32768)) n)";
        assert_eq!(ok(&mut s, counted), "32768");
        assert_eq!(shown(&s, "EXTRA"), "nil");
        assert_eq!(shown(&s, "LAST"), "\"k\"");
        assert_eq!(shown(&s, "PAST"), "nil");

        assert_eq!(ok(&mut s, &format!("(new_dialog \"tight\" {id})")), "T");
        assert_eq!(ok(&mut s, "(start_list \"k\")"), "\"k\"");
        let row = "x".repeat(dcl::MAX_STRING_BYTES);
        let mut full = 0;
        for _ in 0..20 {
            match eval(&mut s, &format!("(add_list \"{row}\")")) {
                Ok(value) if value.starts_with('"') => full += 1,
                Ok(value) if value == "nil" => break,
                other => panic!("full row: {other:?}"),
            }
        }
        assert_eq!(full, 15);
        let tail = "y".repeat(dcl::MAX_STRING_BYTES - 1);
        let added = ok(&mut s, &format!("(add_list \"{tail}\")"));
        assert!(added.starts_with('"') && added.ends_with('"') && added != "nil");
        assert_eq!(ok(&mut s, "(add_list \"z\")"), "nil");
        assert_eq!(ok(&mut s, "(end_list)"), "nil");
        assert_eq!(ok(&mut s, "(progn (start_list \"k\" 2) (add_list \"z\"))"), "nil");
        assert_eq!(ok(&mut s, "(end_list)"), "nil");
        // The retained rows already fill the text cap, so a pending replacement cannot be stored.
        assert_eq!(ok(&mut s, "(progn (start_list \"k\" 1 0) (add_list \"q\"))"), "nil");
        assert_eq!(ok(&mut s, "(start_list \"k\" 1 0)"), "\"k\"");

        assert_eq!(ok(&mut s, &format!("(new_dialog \"fit\" {fit_id})")), "T");
        assert_eq!(ok(&mut s, "(progn (start_list \"k\") (add_list \"old\") (end_list))"), "nil");
        s.lisp.limits.string_bytes = 3;
        assert_eq!(eval(&mut s, "(progn (start_list \"k\" 2) (add_list \"abcd\"))"), Err("out of memory: the routine made too much text".into()));
        assert_eq!(ok(&mut s, "(+ 4 5)"), "9");
        s.lisp.limits.string_bytes = 1 << 20;
        assert_eq!(ok(&mut s, "(progn (start_list \"k\" 1 0) (add_list \"old\"))"), "\"old\"");
        assert_eq!(ok(&mut s, "(progn (start_list \"k\" 2) (add_list \"abcd\") (end_list) (start_list \"k\" 1 1))"), "\"k\"");
    }

    #[test]
    fn dcl_list_caps_initial_rows_across_controls() {
        let mut s = session();
        let dir = TempDir::new("list-initial-total");
        let rows = "\\n".repeat(crate::lisp::dcl_prepare::MAX_LIST_ROWS - 1);
        let source = format!(
            r#"full : list_box {{ list = "{rows}"; }}
                fit : dialog {{ : full {{ key = "one"; }} : full {{ key = "two"; }} }}
                over : dialog {{ : full {{ key = "one"; }} : full {{ key = "two"; }}
                    : list_box {{ key = "over"; list = ""; }} }}"#
        );
        let id = load_source(&mut s, &dir, "total.dcl", &source);
        assert_eq!(ok(&mut s, &format!("(new_dialog \"fit\" {id})")), "T");
        assert_eq!(ok(&mut s, &format!("(new_dialog \"over\" {id})")), "nil");
        assert_eq!(ok(&mut s, "(start_list \"two\" 1 32767)"), "\"two\"");
    }

    #[test]
    fn dcl_list_caps_pending_rows_across_controls() {
        let mut s = session();
        let dir = TempDir::new("list-pending-total");
        let rows = "\\n".repeat(crate::lisp::dcl_prepare::MAX_LIST_ROWS - 1);
        let source = format!(
            r#"full : list_box {{ list = "{rows}"; }}
                fit : dialog {{ : full {{ key = "one"; }} : full {{ key = "two"; }}
                    : list_box {{ key = "extra"; }} }}"#
        );
        let id = load_source(&mut s, &dir, "pending.dcl", &source);
        assert_eq!(ok(&mut s, &format!("(new_dialog \"fit\" {id})")), "T");
        assert_eq!(ok(&mut s, "(progn (start_list \"extra\") (add_list \"\"))"), "nil");
        assert_eq!(ok(&mut s, "(progn (end_list) (start_list \"extra\" 1 0))"), "nil");
        assert_eq!(ok(&mut s, "(progn (start_list \"one\") (end_list) (start_list \"extra\") (add_list \"\"))"), "\"\"");
        assert_eq!(ok(&mut s, "(progn (end_list) (start_list \"extra\" 1 0))"), "\"extra\"");
    }

    #[test]
    fn dcl_list_ignores_out_of_range_event_values() {
        let mut s = session();
        let dir = TempDir::new("list-event-range");
        let source = r#"box : dialog {
            : list_box { key = "rows"; list = "North\nSouth"; value = "1"; }
            : popup_list { key = "finish"; list = "Bare"; }
            ok_cancel;
        }"#;
        let id = load_source(&mut s, &dir, "range.dcl", source);
        start_interactive(&mut s, &format!("(progn (new_dialog \"box\" {id}) (action_tile \"rows\" \"(setq saw $value)\") (start_dialog))"));
        assert_eq!(edit_event(&mut s, "rows", Some("2"), None).unwrap(), DialogEventOutcome::Ignored);
        assert_eq!(shown(&s, "SAW"), "nil");
        assert_eq!(tile_value(&s, "rows"), "1");
        assert_eq!(edit_event(&mut s, "finish", Some("1"), None).unwrap(), DialogEventOutcome::Ignored);
        assert_eq!(tile_value(&s, "finish"), "0");
        assert_eq!(edit_event(&mut s, "rows", Some("0"), None).unwrap(), DialogEventOutcome::Applied);
        assert_eq!(shown(&s, "SAW"), "\"0\"");
        tile_event(&mut s, "cancel", 1).unwrap();
    }

    /// A choice updates the tile and the action, and OK and Cancel leave with that choice.
    #[test]
    fn dcl_list_reports_choices_and_exits() {
        let mut s = session();
        let dir = TempDir::new("list-choice");
        let source = "box : dialog {\n  : list_box { key = \"k\"; multiple_select = true; }\n  : popup_list { key = \"p\"; }\n  : list_box { key = \"locked\"; list = \"Keep\\nDrop\"; value = \"0\"; }\n  : button { key = \"apply\"; label = \"Apply\"; }\n  ok_cancel;\n}\n";
        let id = load_source(&mut s, &dir, "box.dcl", source);
        let accept = format!(
            "(progn (new_dialog \"box\" {id}) \
             (start_list \"k\") (add_list \"Small\") (add_list \"Medium\") (add_list \"Large\") (end_list) \
             (start_list \"p\") (add_list \"Oak\") (add_list \"Pine\") (end_list) \
             (set_tile \"k\" \"1\") \
             (action_tile \"k\" \"(setq saw (strcat $key \\\"=\\\" $value \\\"/\\\" (itoa $reason)))\") \
             (action_tile \"apply\" \"(progn (start_list \\\"k\\\" 2) (setq added (add_list \\\"Fresh\\\")) (end_list))\") \
             (mode_tile \"locked\" 1) \
             (setq st (start_dialog)) \
             (setq chosen (get_tile \"k\")) \
             (if (= st 1) (progn (setq layer (strcat \"L\" (vl-string-translate \" \" \"_\" chosen))) (command \"-LAYER\" \"New\" layer \"Set\" layer \"\") (command \"_.LINE\" \"0,0\" \"10,0\" \"\"))))"
        );
        start_interactive(&mut s, &accept);
        assert_eq!(tile_value(&s, "k"), "1");
        assert_eq!(tile_value(&s, "p"), "0");
        assert_eq!(edit_event(&mut s, "k", Some("0"), None).unwrap(), DialogEventOutcome::Applied);
        assert_eq!(shown(&s, "SAW"), "\"k=0/1\"");
        assert_eq!(tile_value(&s, "k"), "0");
        assert_eq!(edit_event(&mut s, "k", Some("0 2"), None).unwrap(), DialogEventOutcome::Applied);
        assert_eq!(shown(&s, "SAW"), "\"k=0 2/1\"");
        assert_eq!(tile_value(&s, "k"), "0 2");
        assert_eq!(edit_event(&mut s, "locked", Some("1"), None).unwrap(), DialogEventOutcome::Ignored);
        assert_eq!(tile_value(&s, "locked"), "0");
        assert_eq!(tile_event(&mut s, "apply", 1).unwrap(), DialogEventOutcome::Applied);
        assert_eq!(shown(&s, "ADDED"), "\"Fresh\"");
        assert_eq!(eval(&mut s, "(+ 4 5)"), Err("a LISP routine is already running".into()));
        let stale = s.dialog_id().expect("dialog id");
        tile_event(&mut s, "accept", 1).unwrap();
        assert!(!s.dialog_active());
        assert_eq!(shown(&s, "ST"), "1");
        assert_eq!(shown(&s, "CHOSEN"), "\"0 2\"");
        assert_eq!(kind_count(&s, is_line), 1);
        assert!(s.doc().unwrap().layers.iter().any(|layer| layer.name == "L0_2"));
        assert_eq!(
            s.dialog_event(DialogEvent { id: stale, key: "k", reason: 1, value: Some("1"), position: None }).unwrap(),
            DialogEventOutcome::Ignored
        );
        assert_eq!(kind_count(&s, is_line), 1);

        let cancel = format!(
            "(progn (new_dialog \"box\" {id}) (start_list \"k\") (add_list \"Small\") (end_list) (set_tile \"k\" \"0\") (setq st (start_dialog)) (if (= st 1) (command \"_.LINE\" \"0,0\" \"4,0\" \"\")))"
        );
        start_interactive(&mut s, &cancel);
        edit_event(&mut s, "k", Some("0"), None).unwrap();
        tile_event(&mut s, "cancel", 1).unwrap();
        assert!(!s.dialog_active());
        assert_eq!(shown(&s, "ST"), "0");
        assert_eq!(kind_count(&s, is_line), 1);
        assert_eq!(ok(&mut s, "(+ 4 5)"), "9");
    }

    const IMAGE_DCL: &str = r#"show : dialog {
  : image { key = "img"; width = 2; height = 1; aspect_ratio = 4; }
  : image { key = "wide"; width = 4; aspect_ratio = 2; }
  : image_button { key = "tall"; height = 2; aspect_ratio = 0.5; }
  : image { key = "bad"; width = -1; }
  : image { key = "other"; width = 2; height = 1; }
  : button { key = "show"; label = "Show"; }
  ok_cancel;
}
"#;

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

    fn test_slide(body: &[u8]) -> Vec<u8> {
        let mut out = b"AutoCAD Slide\r\n\x1a\x00".to_vec();
        out.extend_from_slice(&[0x56, 2]);
        slide_word(&mut out, 15);
        slide_word(&mut out, 15);
        out.extend_from_slice(&1_000_000u32.to_le_bytes());
        slide_word(&mut out, 0);
        slide_word(&mut out, 0x1234);
        out.extend_from_slice(body);
        slide_word(&mut out, 0xfc00);
        out
    }

    fn vector_body(color: u8, x1: u16, y1: u16, x2: u16, y2: u16) -> Vec<u8> {
        let mut out = Vec::new();
        slide_word(&mut out, 0xff00 | u16::from(color));
        slide_word(&mut out, x1);
        slide_word(&mut out, y1);
        slide_word(&mut out, x2);
        slide_word(&mut out, y2);
        out
    }

    fn red_slide() -> Vec<u8> {
        let mut body = vector_body(1, 0, 0, 15, 15);
        slide_word(&mut body, 0xfd00);
        slide_word(&mut body, 3);
        slide_word(&mut body, 0xffff);
        for (x, y) in [(0u16, 0), (15, 0), (15, 15)] {
            slide_word(&mut body, 0xfd00);
            slide_word(&mut body, x);
            slide_word(&mut body, y);
        }
        slide_word(&mut body, 0xfd00);
        slide_word(&mut body, 3);
        slide_word(&mut body, 0xffff);
        test_slide(&body)
    }

    fn green_slide() -> Vec<u8> {
        test_slide(&vector_body(3, 0, 15, 15, 0))
    }

    fn library_bytes(members: &[(&str, &[u8])]) -> Vec<u8> {
        let mut bytes = b"AutoCAD Slide Library 1.0\r\n\x1a\x00\x00\x00\x00".to_vec();
        let directory = 32 + 36 * (members.len() + 1);
        let mut slides = Vec::new();
        for (name, slide) in members {
            let mut item = [0u8; 36];
            assert!(name.len() < 32, "{name}");
            item[..name.len()].copy_from_slice(name.as_bytes());
            let offset = u32::try_from(directory + slides.len()).unwrap();
            item[32..36].copy_from_slice(&offset.to_le_bytes());
            bytes.extend_from_slice(&item);
            slides.extend_from_slice(slide);
        }
        bytes.extend_from_slice(&[0u8; 36]);
        bytes.extend(slides);
        bytes
    }

    fn many_solids(count: usize) -> Vec<u8> {
        let mut body = Vec::new();
        slide_word(&mut body, 0xff02);
        for _ in 0..count {
            slide_word(&mut body, 0xfd00);
            slide_word(&mut body, 10);
            slide_word(&mut body, 0xffff);
            for (x, y) in [(0, 0), (3, 0), (6, 0), (9, 0), (9, 3), (9, 6), (9, 9), (6, 9), (3, 9), (0, 9)] {
                slide_word(&mut body, 0xfd00);
                slide_word(&mut body, x);
                slide_word(&mut body, y);
            }
            slide_word(&mut body, 0xfd00);
            slide_word(&mut body, 10);
            slide_word(&mut body, 0xffff);
        }
        test_slide(&body)
    }

    struct CwdSlide(PathBuf);

    impl CwdSlide {
        fn new(bytes: &[u8]) -> Self {
            let name = format!(
                "cadcraft-image-{}-{}.sld",
                std::process::id(),
                std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
            );
            let path = std::env::current_dir().unwrap().join(name);
            std::fs::write(&path, bytes).unwrap();
            CwdSlide(path)
        }

        fn file_name(&self) -> String {
            self.0.file_name().unwrap().to_string_lossy().into_owned()
        }
    }

    impl Drop for CwdSlide {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }

    fn image_lines(s: &Session, key: &str) -> Vec<(u8, i32, i32, i32, i32)> {
        let Some(picture) = prepared_dialog(s).image_picture(key) else { return Vec::new() };
        picture
            .primitives
            .iter()
            .filter_map(|primitive| match primitive {
                crate::lisp::DialogPrimitive::Vector(line) => Some((line.color, line.x1, line.y1, line.x2, line.y2)),
                crate::lisp::DialogPrimitive::Solid(_) => None,
            })
            .collect()
    }

    fn image_solids(s: &Session, key: &str) -> Vec<(u8, Vec<(i32, i32)>)> {
        let Some(picture) = prepared_dialog(s).image_picture(key) else { return Vec::new() };
        picture
            .primitives
            .iter()
            .filter_map(|primitive| match primitive {
                crate::lisp::DialogPrimitive::Solid(solid) => Some((solid.color, solid.triangles.clone())),
                crate::lisp::DialogPrimitive::Vector(_) => None,
            })
            .collect()
    }

    fn install_slides() {
        use crate::lisp::{SlideSource, SlideSourcePrimitive, SlideSourceSolid, SlideSourceVector};
        use cadcraft_io::slide::SlidePrim;

        crate::lisp::set_slide_decoder(
            |bytes| {
                let slide = cadcraft_io::slide::decode(bytes)?;
                let primitives = slide
                    .prims
                    .into_iter()
                    .map(|prim| match prim {
                        SlidePrim::Vector(line) => SlideSourcePrimitive::Vector(SlideSourceVector {
                            x1: line.from.x,
                            y1: line.from.y,
                            x2: line.to.x,
                            y2: line.to.y,
                            color: line.color,
                        }),
                        SlidePrim::Solid(solid) => SlideSourcePrimitive::Solid(SlideSourceSolid {
                            points: solid.points.into_iter().map(|point| (point.x, point.y)).collect(),
                            color: solid.color,
                        }),
                    })
                    .collect();
                Some(SlideSource { high_x: slide.high_x, high_y: slide.high_y, primitives })
            },
            cadcraft_io::slide::library_span,
        );
    }

    fn show_slide(s: &mut Session, key: &str, name: &str) {
        install_slides();
        let call = format!("(progn (start_image \"{key}\") (setq named (slide_image 0 0 15 15 {})) (end_image))", quote_lisp(name));
        assert_eq!(ok(s, &call), "nil");
        assert_eq!(shown(s, "NAMED"), quote_lisp(name));
    }

    /// Image tiles use 8 by 16 character cells. Explicit cells win over the aspect ratio.
    #[test]
    fn dcl_image_tile_uses_character_cells_and_places_a_slide() {
        let mut s = session();
        let dir = TempDir::new("image-dim");
        let id = load_source(&mut s, &dir, "show.dcl", IMAGE_DCL);
        assert_eq!(ok(&mut s, "(dimx_tile \"img\")"), "nil");
        assert_eq!(ok(&mut s, &format!("(new_dialog \"show\" {id})")), "T");
        assert_eq!(ok(&mut s, "(dimx_tile \"img\")"), "15");
        assert_eq!(ok(&mut s, "(dimy_tile \"img\")"), "15");
        assert_eq!(ok(&mut s, "(dimx_tile \"wide\")"), "31");
        assert_eq!(ok(&mut s, "(dimy_tile \"wide\")"), "15");
        assert_eq!(ok(&mut s, "(dimx_tile \"tall\")"), "15");
        assert_eq!(ok(&mut s, "(dimy_tile \"tall\")"), "31");
        assert_eq!(ok(&mut s, "(dimx_tile \"bad\")"), "nil");
        assert_eq!(ok(&mut s, "(dimy_tile \"missing\")"), "nil");
        assert_eq!(ok(&mut s, "(dimx_tile \"show\")"), "nil");
        assert_eq!(ok(&mut s, "(start_image \"bad\")"), "nil");
        assert_eq!(ok(&mut s, "(end_image)"), "nil");
    }

    /// A slide and a named library member keep their own geometry and the supplied name.
    #[test]
    fn dcl_image_places_a_slide_and_a_library_member() {
        let mut s = session();
        let dir = TempDir::new("image-place");
        let id = load_source(&mut s, &dir, "show.dcl", IMAGE_DCL);
        let red = write(&dir, "red.sld", &red_slide());
        write(&dir, "my slide.sld", &green_slide());
        let library = library_bytes(&[("red", &red_slide()), ("green", &green_slide())]);
        write(&dir, "widgets.slb", &library);
        s.set_lisp_support_dir(Some(dir.path().to_path_buf()));
        assert_eq!(ok(&mut s, &format!("(new_dialog \"show\" {id})")), "T");

        let red_name = red.display().to_string();
        show_slide(&mut s, "img", &red_name);
        assert_eq!(image_lines(&s, "img"), vec![(1, 0, 15, 15, 0)]);
        let solids = image_solids(&s, "img");
        assert_eq!(solids.len(), 1);
        let (color, triangles) = &solids[0];
        assert_eq!(*color, 1);
        assert_eq!(triangles.len() % 3, 0);
        assert!(triangles.contains(&(0, 15)) && triangles.contains(&(15, 15)) && triangles.contains(&(15, 0)));

        assert_eq!(ok(&mut s, "(progn (start_image \"img\") (setq missed (slide_image 0 0 15 15 \"missing-slide\")) (end_image))"), "nil");
        assert_eq!(shown(&s, "MISSED"), "nil");
        assert_eq!(image_lines(&s, "img"), vec![(1, 0, 15, 15, 0)], "a missing file keeps the last slide");

        show_slide(&mut s, "img", "widgets(green)");
        assert_eq!(image_lines(&s, "img"), vec![(3, 0, 0, 15, 15)]);
        assert!(image_solids(&s, "img").is_empty(), "the green member does not include the red solid");
        show_slide(&mut s, "other", "widgets.slb(red)");
        assert_eq!(image_lines(&s, "other"), vec![(1, 0, 15, 15, 0)]);
        show_slide(&mut s, "tall", "my slide");
        assert_eq!(image_lines(&s, "tall"), vec![(3, 0, 0, 15, 15)]);

        let cwd = CwdSlide::new(&red_slide());
        show_slide(&mut s, "img", &cwd.file_name());
        assert_eq!(image_lines(&s, "img"), vec![(1, 0, 15, 15, 0)]);
    }

    #[test]
    fn dcl_image_honors_automation_read_root() {
        let mut s = session();
        let root = TempDir::new("slide-root");
        let other = TempDir::new("slide-other");
        let id = load_source(&mut s, &root, "show.dcl", IMAGE_DCL);
        let allowed = write(&root, "allowed.sld", &red_slide());
        let refused = write(&other, "refused.sld", &green_slide());
        write(&other, "widgets.slb", &library_bytes(&[("green", &green_slide())]));
        s.set_lisp_support_dir(Some(other.path().to_path_buf()));
        assert_eq!(ok(&mut s, &format!("(new_dialog \"show\" {id})")), "T");
        let roots = crate::file_access::AutomationRoots::new(Some(root.path()), None).unwrap();

        crate::file_access::confine(roots.as_ref(), || {
            show_slide(&mut s, "img", &allowed.display().to_string());
            let picture = image_lines(&s, "img");
            assert!(!picture.is_empty());
            assert_eq!(ok(&mut s, "(start_image \"img\")"), "\"img\"");
            assert_eq!(ok(&mut s, &format!("(slide_image 0 0 15 15 {})", lisp_path(&refused))), "nil");
            assert_eq!(ok(&mut s, "(slide_image 0 0 15 15 \"widgets(green)\")"), "nil");
            assert_eq!(ok(&mut s, "(end_image)"), "nil");
            assert_eq!(image_lines(&s, "img"), picture);
        });
    }

    #[test]
    fn dcl_image_uses_relative_dimensions_and_keeps_pending_placements() {
        let mut s = session();
        let dir = TempDir::new("image-offsets");
        let id = load_source(&mut s, &dir, "show.dcl", IMAGE_DCL);
        write(&dir, "diagonal.sld", &red_slide());
        write(&dir, "other.sld", &green_slide());
        install_slides();
        s.set_lisp_support_dir(Some(dir.path().to_path_buf()));
        assert_eq!(ok(&mut s, &format!("(new_dialog \"show\" {id})")), "T");
        assert_eq!(ok(&mut s, "(start_image \"img\")"), "\"img\"");
        assert_eq!(ok(&mut s, "(slide_image 9 4 5 6 \"diagonal\")"), "\"diagonal\"");
        assert_eq!(ok(&mut s, "(slide_image 1 2 4 5 \"other\")"), "\"other\"");
        assert_eq!(ok(&mut s, "(slide_image 1 2 4 5 \"missing\")"), "nil");
        assert!(image_lines(&s, "img").is_empty(), "the image is still pending");
        assert_eq!(ok(&mut s, "(end_image)"), "nil");
        assert_eq!(image_lines(&s, "img"), vec![(1, 9, 10, 14, 4), (3, 1, 2, 5, 7)]);
    }

    /// A bad read, a bad extent, and geometry past the cap keep the last good slide.
    #[test]
    fn dcl_image_rejects_bad_slides_without_dropping_the_last_good_one() {
        let mut s = session();
        let dir = TempDir::new("image-reject");
        let id = load_source(&mut s, &dir, "show.dcl", IMAGE_DCL);
        let red = write(&dir, "red.sld", &red_slide());
        write(&dir, "cut.sld", b"AutoCAD Slide");
        let mut bad_library = library_bytes(&[("red", &red_slide())]);
        bad_library[32 + 32..32 + 36].copy_from_slice(&4u32.to_le_bytes());
        let bad_lib = write(&dir, "bad.slb", &bad_library);
        let huge = write(&dir, "huge.sld", &vec![0u8; super::super::dcl_image::MAX_SLIDE_FILE + 1]);
        let heavy = write(&dir, "heavy.sld", &many_solids(3_600));
        let mut overflow = Vec::new();
        slide_word(&mut overflow, 0xff07);
        for _ in 0..=cadcraft_io::slide::MAX_PRIMITIVES {
            slide_word(&mut overflow, 0);
            slide_word(&mut overflow, 0);
            slide_word(&mut overflow, 1);
            slide_word(&mut overflow, 0);
        }
        let overflow = write(&dir, "overflow.sld", &test_slide(&overflow));
        assert_eq!(ok(&mut s, &format!("(new_dialog \"show\" {id})")), "T");
        show_slide(&mut s, "img", &red.display().to_string());

        let keep = |s: &Session| image_lines(s, "img") == vec![(1, 0, 15, 15, 0)];
        assert_eq!(ok(&mut s, "(start_image \"img\")"), "\"img\"");
        assert_eq!(ok(&mut s, "(slide_image -1 0 15 15 \"red.sld\")"), "nil");
        assert_eq!(ok(&mut s, "(slide_image 0 0 0 15 \"red.sld\")"), "nil");
        assert_eq!(ok(&mut s, "(slide_image 0 0 16 15 \"red.sld\")"), "nil");
        assert_eq!(ok(&mut s, "(end_image)"), "nil");
        assert!(keep(&s));
        assert_eq!(
            ok(&mut s, &format!("(progn (start_image \"img\") (slide_image 0 0 15 15 {}) (end_image))", lisp_path(&dir.path().join("cut.sld")))),
            "nil"
        );
        assert!(keep(&s));
        assert_eq!(
            ok(
                &mut s,
                &format!("(progn (start_image \"img\") (slide_image 0 0 15 15 {}) (end_image))", quote_lisp(&format!("{}(red)", bad_lib.display())))
            ),
            "nil"
        );
        assert!(keep(&s));
        assert_eq!(ok(&mut s, &format!("(progn (start_image \"img\") (slide_image 0 0 15 15 {}) (end_image))", lisp_path(&heavy))), "nil");
        assert!(keep(&s), "geometry past the dialog cap is not published");
        assert_eq!(ok(&mut s, &format!("(progn (start_image \"img\") (slide_image 0 0 15 15 {}) (end_image))", lisp_path(&overflow))), "nil");
        assert!(keep(&s), "a slide past the primitive cap is not published");

        s.lisp.limits.string_bytes = 20_000;
        assert_eq!(
            eval(&mut s, &format!("(progn (start_image \"img\") (slide_image 0 0 15 15 {}))", lisp_path(&huge))),
            Err("out of memory: the routine made too much text".into())
        );
        assert!(keep(&s), "a read past the byte budget keeps the last slide");
        assert_eq!(ok(&mut s, "(+ 2 2)"), "4");
    }

    /// A button can replace the slide. OK draws. Cancel leaves the drawing alone.
    #[test]
    fn dcl_image_refresh_follows_the_button_and_the_exit() {
        install_slides();
        let mut s = session();
        let dir = TempDir::new("image-refresh");
        let id = load_source(&mut s, &dir, "show.dcl", &IMAGE_DCL.replace("image_button", "image"));
        let red = write(&dir, "red.sld", &red_slide());
        let green = write(&dir, "green.sld", &green_slide());
        let action = format!("(start_image \"img\") (slide_image 0 0 15 15 {}) (end_image)", quote_lisp(&green.display().to_string()));
        let code = format!(
            "(progn (new_dialog \"show\" {id}) (start_image \"img\") (slide_image 0 0 15 15 {}) (end_image) (action_tile \"show\" {}) (setq st (start_dialog)) (if (= st 1) (command \"_.LINE\" \"0,0\" \"4,0\" \"\")))",
            quote_lisp(&red.display().to_string()),
            quote_lisp(&action)
        );
        start_interactive(&mut s, &code);
        assert_eq!(image_lines(&s, "img"), vec![(1, 0, 15, 15, 0)]);
        tile_event(&mut s, "show", 1).unwrap();
        assert_eq!(image_lines(&s, "img"), vec![(3, 0, 0, 15, 15)]);
        tile_event(&mut s, "accept", 1).unwrap();
        assert!(!s.dialog_active());
        assert_eq!(shown(&s, "ST"), "1");
        assert_eq!(kind_count(&s, is_line), 1);

        let cancel = format!("(progn (new_dialog \"show\" {id}) (setq st (start_dialog)) (if (= st 1) (command \"_.LINE\" \"0,0\" \"8,0\" \"\")))");
        start_interactive(&mut s, &cancel);
        tile_event(&mut s, "cancel", 1).unwrap();
        assert_eq!(shown(&s, "ST"), "0");
        assert_eq!(kind_count(&s, is_line), 1);
    }

    /// Image state belongs to the prepared dialog of one drawing.
    #[test]
    fn dcl_image_state_stays_with_its_drawing_until_a_new_preparation() {
        let mut s = session();
        let dir = TempDir::new("image-iso");
        let id = load_source(&mut s, &dir, "show.dcl", IMAGE_DCL);
        let red = write(&dir, "red.sld", &red_slide());
        assert_eq!(ok(&mut s, &format!("(new_dialog \"show\" {id})")), "T");
        show_slide(&mut s, "img", &red.display().to_string());
        let first = s.active;
        s.execute("new", &serde_json::json!({})).unwrap();
        assert_eq!(ok(&mut s, "(dimx_tile \"img\")"), "nil");
        s.execute("document.switch", &serde_json::json!({ "index": first })).unwrap();
        assert_eq!(ok(&mut s, "(dimx_tile \"img\")"), "15");
        assert_eq!(image_lines(&s, "img"), vec![(1, 0, 15, 15, 0)]);

        assert_eq!(ok(&mut s, &format!("(new_dialog \"show\" {id})")), "T");
        assert_eq!(ok(&mut s, "(dimx_tile \"img\")"), "15");
        assert!(image_lines(&s, "img").is_empty(), "a new preparation drops the old slide");
        assert_eq!(ok(&mut s, &format!("(unload_dialog {id})")), "nil");
        assert_eq!(ok(&mut s, "(dimx_tile \"img\")"), "nil");
    }

    /// A blocking run can prepare an image. `start_dialog` still refuses that run.
    #[test]
    fn dcl_image_prepare_works_from_a_blocking_call() {
        install_slides();
        let mut s = session();
        let dir = TempDir::new("image-block");
        let id = load_source(&mut s, &dir, "show.dcl", IMAGE_DCL);
        let red = write(&dir, "red.sld", &red_slide());
        let code = format!(
            "(progn (new_dialog \"show\" {id}) (setq dx (dimx_tile \"img\")) (setq started (start_image \"img\")) (setq named (slide_image 0 0 15 15 {})) (setq ended (end_image)) (start_dialog))",
            quote_lisp(&red.display().to_string())
        );
        assert_eq!(eval(&mut s, &code), Err("a dialog cannot start from a blocking call".into()));
        assert!(!s.dialog_active());
        assert_eq!(shown(&s, "DX"), "15");
        assert_eq!(shown(&s, "STARTED"), "\"img\"");
        assert_eq!(shown(&s, "NAMED"), quote_lisp(&red.display().to_string()));
        assert_eq!(shown(&s, "ENDED"), "nil");
        assert_eq!(image_lines(&s, "img"), vec![(1, 0, 15, 15, 0)]);
        assert_eq!(ok(&mut s, "(+ 1 2)"), "3");
    }
}

#[cfg(target_arch = "wasm32")]
#[test]
fn dcl_image_file_lookup_on_the_web_returns_nil() {
    let mut s = Session::new();
    assert_eq!(ok(&mut s, "(dimx_tile \"img\")"), "nil");
    assert_eq!(ok(&mut s, "(start_image \"img\")"), "nil");
    assert_eq!(ok(&mut s, "(slide_image 0 0 1 1 \"missing.sld\")"), "nil");
    assert_eq!(ok(&mut s, "(end_image)"), "nil");
}

#[cfg(target_arch = "wasm32")]
#[test]
fn dcl_file_native_lookup_on_the_web_returns_a_negative_integer() {
    let mut s = Session::new();
    negative(&ok(&mut s, "(load_dialog \"/any/dialog.dcl\")"));
    assert!(loaded_ids(&s).is_empty());
    assert_eq!(ok(&mut s, "(unload_dialog 1)"), "nil");
}
