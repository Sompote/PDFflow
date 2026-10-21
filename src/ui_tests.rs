//! Interaction tests: drive the real editor window headlessly with egui_kittest.

use crate::app::{App, Tool};
use crate::doc::Kind;
use crate::sig::Signature;
use eframe::egui::{Event, Key, Modifiers, Pos2, Vec2, pos2, vec2};
use egui_kittest::Harness;
use std::sync::Arc;

fn harness(path: Option<std::path::PathBuf>) -> Harness<'static, App> {
    let mut h = Harness::builder().with_size(vec2(1280.0, 820.0)).build_eframe(move |cc| App::new(cc, path));
    h.run_steps(3);
    h
}

/// Screen position of a page point.
fn at(h: &Harness<'_, App>, page: usize, p: Pos2) -> Pos2 {
    h.state().screen_of(page, p)
}

fn drag(h: &mut Harness<'_, App>, a: Pos2, b: Pos2) {
    h.hover_at(a);
    h.step();
    h.drag_at(a);
    h.step();
    for k in 1..=6 {
        h.hover_at(a + (b - a) * (k as f32 / 6.0));
        h.step();
    }
    h.drop_at(b);
    h.run_steps(2);
}

/// Drag between two page-0 points.
fn dragp(h: &mut Harness<'_, App>, a: Pos2, b: Pos2) {
    let (a, b) = (at(h, 0, a), at(h, 0, b));
    drag(h, a, b);
}

fn undo(h: &mut Harness<'_, App>) {
    h.key_press_modifiers(Modifiers::COMMAND, Key::Z);
    h.step();
}

fn close(a: Pos2, b: Pos2, tol: f32) -> bool {
    (a - b).length() <= tol
}

#[test]
fn rectangle_tool_draws_where_dragged_and_undoes() {
    let mut h = harness(None);
    h.key_press(Key::R);
    h.step();
    assert_eq!(h.state().tool, Tool::Rect);
    let (a, b) = (at(&h, 0, pos2(100.0, 100.0)), at(&h, 0, pos2(220.0, 180.0)));
    drag(&mut h, a, b);
    let tol = 2.0 / h.state().zoom;
    let objs = &h.state().doc.pages[0].objects;
    assert_eq!(objs.len(), 1);
    assert!(close(objs[0].rect.min, pos2(100.0, 100.0), tol) && close(objs[0].rect.max, pos2(220.0, 180.0), tol), "{:?}", objs[0].rect);
    undo(&mut h);
    assert!(h.state().doc.pages[0].objects.is_empty());
    h.key_press_modifiers(Modifiers::COMMAND | Modifiers::SHIFT, Key::Z);
    h.step();
    assert_eq!(h.state().doc.pages[0].objects.len(), 1);
}

#[test]
fn move_tool_moves_and_resizes_with_one_undo_step_each() {
    let mut h = harness(None);
    h.key_press(Key::R);
    h.step();
    dragp(&mut h, pos2(100.0, 100.0), pos2(200.0, 200.0));
    h.key_press(Key::V);
    h.step();
    dragp(&mut h, pos2(150.0, 150.0), pos2(200.0, 170.0));
    let tol = 2.0 / h.state().zoom;
    let r = h.state().doc.pages[0].objects[0].rect;
    assert!(close(r.min, pos2(150.0, 120.0), tol), "moved to {r:?}");
    // Bottom-right handle: shapes resize freely.
    dragp(&mut h, pos2(250.0, 220.0), pos2(300.0, 240.0));
    let r = h.state().doc.pages[0].objects[0].rect;
    assert!(close(r.max, pos2(300.0, 240.0), tol), "resized to {r:?}");
    undo(&mut h);
    let r = h.state().doc.pages[0].objects[0].rect;
    assert!(close(r.max, pos2(250.0, 220.0), tol), "undo resize: {r:?}");
    undo(&mut h);
    let r = h.state().doc.pages[0].objects[0].rect;
    assert!(close(r.min, pos2(100.0, 100.0), tol), "undo move: {r:?}");
}

#[test]
fn text_tool_types_into_a_new_box() {
    let mut h = harness(None);
    h.key_press(Key::T);
    h.step();
    let p = at(&h, 0, pos2(80.0, 120.0));
    drag(&mut h, p, p);
    assert!(h.state().editing_text());
    h.event(Event::Text("Hello สวัสดี".into()));
    h.run_steps(2);
    h.key_press(Key::Escape);
    h.run_steps(2);
    assert!(!h.state().editing_text());
    let objs = &h.state().doc.pages[0].objects;
    assert_eq!(objs.len(), 1);
    match &objs[0].kind {
        Kind::Text { text, .. } => assert_eq!(text, "Hello สวัสดี"),
        _ => panic!("not text"),
    }
    assert!(objs[0].rect.width() > 40.0);
    undo(&mut h);
    assert!(h.state().doc.pages[0].objects.is_empty());
    // Clicking and leaving without typing leaves nothing behind.
    drag(&mut h, p, p);
    h.key_press(Key::Escape);
    h.run_steps(2);
    assert!(h.state().doc.pages[0].objects.is_empty());
}

#[test]
fn pen_and_highlighter_make_ink() {
    let mut h = harness(None);
    for (key, hl) in [(Key::P, false), (Key::K, true)] {
        h.key_press(key);
        h.step();
        dragp(&mut h, pos2(100.0, 300.0), pos2(300.0, 340.0));
        match &h.state().doc.pages[0].objects.last().unwrap().kind {
            Kind::Ink { strokes, highlighter, .. } => {
                assert_eq!(*highlighter, hl);
                assert!(strokes[0].len() >= 3);
            }
            _ => panic!("not ink"),
        }
    }
}

#[test]
fn signature_is_placed_and_resized_in_proportion() {
    let mut h = harness(None);
    h.state_mut().set_signatures(vec![Signature { aspect: 3.0, strokes: Arc::new(vec![vec![pos2(0.0, 1.0), pos2(0.5, 0.0), pos2(1.0, 1.0)]]) }]);
    h.key_press(Key::S);
    h.step();
    let p = at(&h, 0, pos2(300.0, 300.0));
    drag(&mut h, p, p);
    assert_eq!(h.state().tool, Tool::Move);
    let r = h.state().doc.pages[0].objects[0].rect;
    assert!((r.width() / r.height() - 3.0).abs() < 0.01);
    assert!(close(r.center(), pos2(300.0, 300.0), 0.5));
    // Drag the bottom-right corner: the signature keeps its proportions.
    dragp(&mut h, r.max, r.max + vec2(150.0, 10.0));
    let r2 = h.state().doc.pages[0].objects[0].rect;
    assert!(r2.width() > r.width() * 1.5, "{r2:?}");
    assert!((r2.width() / r2.height() - 3.0).abs() < 0.02, "{r2:?}");
    assert_eq!(r2.min, r.min);
}

#[test]
fn opened_pdf_renders_pages_and_thumbnails() {
    let path = std::env::temp_dir().join("pdfeditor-ui-test.pdf");
    std::fs::write(&path, crate::tests::source_pdf(90)).unwrap();
    let mut h = harness(Some(path));
    assert_eq!(h.state().doc.pages.len(), 1);
    assert_eq!(h.state().doc.pages[0].size, vec2(700.0, 500.0));
    for _ in 0..300 {
        h.step();
        if h.state().rendered_counts() == (1, 1) {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert_eq!(h.state().rendered_counts(), (1, 1), "page texture and thumbnail");
}

#[test]
fn page_operations_and_close_guard() {
    let mut h = harness(None);
    let app = h.state_mut();
    app.add_blank(1);
    app.duplicate_page(1);
    app.rotate_page(2, 90);
    assert_eq!(app.doc.pages.len(), 3);
    assert_eq!(app.doc.pages[2].rotation, 90);
    app.delete_page(0);
    assert_eq!(app.doc.pages.len(), 2);
    for _ in 0..4 {
        app.doc.undo();
    }
    assert_eq!(app.doc.pages.len(), 1);
    assert!(app.doc.dirty);
    let _ = Vec2::ZERO;
}
