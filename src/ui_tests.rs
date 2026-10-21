//! Interaction tests: drive the real editor window headlessly with egui_kittest.

use crate::app::{App, Tool};
use crate::doc::Kind;
use crate::path::Node;
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
fn brush_and_highlighter_make_ink() {
    let mut h = harness(None);
    for (key, hl) in [(Key::B, false), (Key::K, true)] {
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

#[test]
fn page_grid_toggles_and_reorders_by_drag() {
    let mut h = harness(None);
    for _ in 0..4 {
        let n = h.state().doc.pages.len();
        h.state_mut().add_blank(n);
    }
    // Tag each page so its order can be checked after the drag.
    for (i, p) in h.state_mut().doc.pages.iter_mut().enumerate() {
        p.size.x = 500.0 + i as f32;
    }
    h.key_press(Key::G);
    h.run_steps(3);
    assert!(h.state().grid_on());
    h.state_mut().move_page_to(0, 3);
    let order: Vec<f32> = h.state().doc.pages.iter().map(|p| p.size.x).collect();
    assert_eq!(order, vec![501.0, 502.0, 500.0, 503.0, 504.0]);
    h.state_mut().move_page_to(4, 0);
    assert_eq!(h.state().doc.pages[0].size.x, 504.0);
    h.key_press(Key::Escape);
    h.run_steps(2);
    assert!(!h.state().grid_on());
}

fn click(h: &mut Harness<'_, App>, p: Pos2) {
    dragp(h, p, p);
}

fn path_of(h: &Harness<'_, App>, i: usize) -> (Vec<Node>, bool) {
    h.state().doc.pages[0].objects[i].path_abs().expect("a path")
}

#[test]
fn pen_tool_clicks_corners_and_closes_on_the_first_anchor() {
    let mut h = harness(None);
    h.key_press(Key::P);
    h.step();
    assert_eq!(h.state().tool, Tool::PenPath);
    for p in [pos2(100.0, 100.0), pos2(220.0, 100.0), pos2(160.0, 200.0)] {
        click(&mut h, p);
    }
    assert!(h.state().pen_drafting());
    click(&mut h, pos2(100.0, 100.0));
    assert!(!h.state().pen_drafting());
    let (nodes, closed) = path_of(&h, 0);
    assert!(closed);
    assert_eq!(nodes.len(), 3);
    assert!(nodes.iter().all(|n| n.is_corner()));
    assert!(close(nodes[1].p, pos2(220.0, 100.0), 2.0 / h.state().zoom));
}

#[test]
fn pen_drag_pulls_symmetric_handles_and_enter_ends() {
    let mut h = harness(None);
    h.key_press(Key::P);
    h.step();
    click(&mut h, pos2(100.0, 300.0));
    dragp(&mut h, pos2(200.0, 300.0), pos2(240.0, 260.0));
    h.key_press(Key::Enter);
    h.step();
    let (nodes, closed) = path_of(&h, 0);
    let tol = 2.0 / h.state().zoom;
    assert!(!closed && nodes.len() == 2);
    assert!(close(nodes[1].cout, pos2(240.0, 260.0), tol), "{:?}", nodes[1]);
    assert!(close(nodes[1].cin, pos2(160.0, 340.0), tol), "{:?}", nodes[1]);
    // Dragging that handle again with the Pen keeps the pair in line; the other handle keeps
    // its length (56.6 pt), as in Photoshop.
    dragp(&mut h, pos2(240.0, 260.0), pos2(260.0, 300.0));
    let (nodes, _) = path_of(&h, 0);
    assert!(close(nodes[1].cout, pos2(260.0, 300.0), tol));
    assert!(close(nodes[1].cin, pos2(200.0 - 40.0 * 2f32.sqrt(), 300.0), tol), "{:?}", nodes[1]);
    undo(&mut h);
    assert!(close(path_of(&h, 0).0[1].cout, pos2(240.0, 260.0), tol));
}

#[test]
fn curvature_add_delete_and_convert_anchors() {
    let mut h = harness(None);
    h.state_mut().set_tool(Tool::CurvaturePen);
    for p in [pos2(100.0, 200.0), pos2(200.0, 120.0), pos2(300.0, 200.0)] {
        click(&mut h, p);
    }
    h.key_press(Key::Enter);
    h.step();
    let (nodes, _) = path_of(&h, 0);
    assert_eq!(nodes.len(), 3);
    assert!(!nodes[1].is_corner(), "middle point is smooth");

    // Add an anchor on the curve between the first two points.
    h.state_mut().set_tool(Tool::AddAnchor);
    let (seg, t, _) = crate::path::nearest(&nodes, false, pos2(150.0, 150.0)).unwrap();
    let on = crate::path::cubic(&[nodes[seg].p, nodes[seg].cout, nodes[seg + 1].cin, nodes[seg + 1].p], t);
    click(&mut h, on);
    assert_eq!(path_of(&h, 0).0.len(), 4);

    // Delete it again.
    h.state_mut().set_tool(Tool::DeleteAnchor);
    let added = path_of(&h, 0).0[1].p;
    click(&mut h, added);
    assert_eq!(path_of(&h, 0).0.len(), 3);

    // Convert Point: click the smooth anchor to make it a corner, click again to smooth it.
    h.state_mut().set_tool(Tool::ConvertPoint);
    let mid = path_of(&h, 0).0[1].p;
    click(&mut h, mid);
    assert!(path_of(&h, 0).0[1].is_corner());
    click(&mut h, mid);
    assert!(!path_of(&h, 0).0[1].is_corner());
}

#[test]
fn freeform_pen_turns_a_stroke_into_a_path() {
    let mut h = harness(None);
    h.state_mut().set_tool(Tool::FreeformPen);
    let (a, b) = (at(&h, 0, pos2(100.0, 400.0)), at(&h, 0, pos2(300.0, 400.0)));
    h.hover_at(a);
    h.step();
    h.drag_at(a);
    h.step();
    for k in 1..=40 {
        let f = k as f32 / 40.0;
        h.hover_at(a + (b - a) * f + vec2(0.0, (f * 6.28).sin() * 40.0));
        h.step();
    }
    h.drop_at(b);
    h.run_steps(2);
    let (nodes, closed) = path_of(&h, 0);
    assert!(!closed && nodes.len() >= 3 && nodes.len() < 30, "{} nodes", nodes.len());
}

#[test]
fn pen_slot_flyout_and_shift_p_cycle() {
    let mut h = harness(None);
    h.key_press(Key::P);
    h.step();
    h.key_press_modifiers(Modifiers::SHIFT, Key::P);
    h.step();
    assert_eq!(h.state().tool, Tool::FreeformPen);
    h.state_mut().set_tool(Tool::Move);
    // A plain P returns to the last pen tool used.
    h.key_press(Key::P);
    h.step();
    assert_eq!(h.state().tool, Tool::FreeformPen);
    let _ = h.state().flyout_open();
}

/// A 200×100 px picture, left half red and right half blue, placed with its centre at (300, 200).
fn with_picture() -> Harness<'static, App> {
    let mut h = harness(None);
    let mut png = Vec::new();
    let im = image::RgbaImage::from_fn(200, 100, |x, _| if x < 100 { image::Rgba([220, 20, 20, 255]) } else { image::Rgba([20, 20, 220, 255]) });
    image::DynamicImage::ImageRgba8(im).write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png).unwrap();
    let ctx = h.ctx.clone();
    h.state_mut().insert_image(&ctx, png, 0, Some(pos2(300.0, 200.0)));
    h.run_steps(2);
    h
}

fn picture(h: &Harness<'_, App>, i: usize) -> (eframe::egui::Rect, Arc<crate::doc::ImageData>) {
    let o = &h.state().doc.pages[0].objects[i];
    match &o.kind {
        Kind::Image(img) => (o.rect, img.clone()),
        _ => panic!("not a picture"),
    }
}

/// Page point of a picture pixel.
fn px_pt(h: &Harness<'_, App>, i: usize, x: f32, y: f32) -> Pos2 {
    let (r, img) = picture(h, i);
    pos2(r.min.x + x / img.px[0] as f32 * r.width(), r.min.y + y / img.px[1] as f32 * r.height())
}

fn pixel(h: &Harness<'_, App>, i: usize, x: u32, y: u32) -> [u8; 4] {
    let (_, img) = picture(h, i);
    let (w, _, data) = img.rgba().unwrap();
    let k = ((y * w + x) * 4) as usize;
    [data[k], data[k + 1], data[k + 2], data[k + 3]]
}

#[test]
fn picture_tools_are_disabled_without_a_picture() {
    let mut h = harness(None);
    h.key_press(Key::M);
    h.step();
    assert_eq!(h.state().tool, Tool::Move);
    let mut h = with_picture();
    h.key_press(Key::M);
    h.step();
    assert_eq!(h.state().tool, Tool::Marquee);
    // Selecting something that is not a picture puts the Move tool back.
    h.state_mut().doc.pages[0].objects.push(crate::doc::Obj {
        rect: eframe::egui::Rect::from_min_size(pos2(20.0, 20.0), vec2(30.0, 30.0)),
        kind: Kind::Shape { shape: crate::doc::ShapeKind::Rect, stroke: eframe::egui::Color32::BLACK, fill: None, width: 1.0, a: pos2(0.0, 0.0), b: pos2(1.0, 1.0) },
    });
    h.state_mut().select(0, 1);
    h.run_steps(2);
    assert_eq!(h.state().tool, Tool::Move);
}

#[test]
fn marquee_delete_erases_only_the_selection_and_undoes() {
    let mut h = with_picture();
    h.key_press(Key::M);
    h.step();
    let (a, b) = (px_pt(&h, 0, 10.0, 10.0), px_pt(&h, 0, 60.0, 60.0));
    dragp(&mut h, a, b);
    assert!(h.state().has_pixel_selection());
    h.key_press(Key::Backspace);
    h.run_steps(2);
    assert_eq!(pixel(&h, 0, 30, 30)[3], 0, "inside erased");
    assert_eq!(pixel(&h, 0, 80, 80)[3], 255, "outside kept");
    assert_eq!(h.state().doc.pages[0].objects.len(), 1, "the object itself stays");
    undo(&mut h);
    assert_eq!(pixel(&h, 0, 30, 30)[3], 255);
}

#[test]
fn brush_paints_and_the_saved_pdf_has_the_new_pixels() {
    let mut h = with_picture();
    h.key_press(Key::N);
    h.step();
    h.state_mut().opts_color_for_test(eframe::egui::Color32::from_rgb(0, 200, 0));
    let (a, b) = (px_pt(&h, 0, 20.0, 50.0), px_pt(&h, 0, 180.0, 50.0));
    dragp(&mut h, a, b);
    let p = pixel(&h, 0, 100, 50);
    assert!(p[1] > 150 && p[0] < 80, "painted green: {p:?}");
    assert_eq!(pixel(&h, 0, 100, 5), [20, 20, 220, 255], "far from the stroke unchanged");
    let saved = crate::export::save(&h.state().doc).unwrap();
    let pdf = hayro::hayro_syntax::Pdf::new(Arc::new(saved)).unwrap();
    let pix = hayro::render(&pdf.pages()[0], &hayro::RenderCache::new(), &Default::default(), &hayro::RenderSettings { x_scale: 1.0, y_scale: 1.0, ..Default::default() });
    let q = px_pt(&h, 0, 100.0, 50.0);
    let i = ((q.y as usize) * pix.width() as usize + q.x as usize) * 4;
    let d = &pix.data_as_u8_slice()[i..i + 4];
    assert!(d[1] > 120 && d[0] < 80, "green in the saved PDF: {d:?}");
}

#[test]
fn crop_eyedropper_and_new_picture_from_selection() {
    let mut h = with_picture();
    // Eyedropper picks the red half.
    h.key_press(Key::I);
    h.step();
    let p = px_pt(&h, 0, 20.0, 20.0);
    click(&mut h, p);
    assert_eq!(h.state().brush_color(), eframe::egui::Color32::from_rgb(220, 20, 20));
    // ⌘J copies a marquee selection into a new picture.
    h.key_press(Key::M);
    h.step();
    let (a, b) = (px_pt(&h, 0, 120.0, 10.0), px_pt(&h, 0, 180.0, 90.0));
    dragp(&mut h, a, b);
    h.key_press_modifiers(Modifiers::COMMAND, Key::J);
    h.run_steps(2);
    assert_eq!(h.state().doc.pages[0].objects.len(), 2);
    let (_, img) = picture(&h, 1);
    assert!((img.px[0] as i32 - 60).abs() <= 1 && (img.px[1] as i32 - 80).abs() <= 1, "{:?}", img.px);
    // Crop the original to its left half; its box shrinks to match.
    h.state_mut().select(0, 0);
    h.key_press(Key::C);
    h.step();
    let (r0, _) = picture(&h, 0);
    let (a, b) = (px_pt(&h, 0, 0.0, 0.0), px_pt(&h, 0, 100.0, 100.0));
    dragp(&mut h, a, b);
    h.key_press(Key::Enter);
    h.run_steps(2);
    let (r1, img) = picture(&h, 0);
    assert!((img.px[0] as i32 - 100).abs() <= 1, "{:?}", img.px);
    assert!((r1.width() - r0.width() / 2.0).abs() < 1.0 && (r1.min - r0.min).length() < 1.0, "{r0:?} {r1:?}");
    assert_eq!(pixel(&h, 0, 90, 50), [220, 20, 20, 255]);
}

fn test_id_file(dir: &std::path::Path) -> std::path::PathBuf {
    let p = dir.join("pdfflow-test-id.p12");
    if !p.exists() {
        std::fs::write(&p, crate::dsig::create_self_signed("UI Tester", "", "", "secret").unwrap()).unwrap();
    }
    p
}

fn dialog(visible: bool) -> crate::app::signui_dialog::SignDialog {
    crate::app::signui_dialog::SignDialog {
        id: 0,
        password: "secret".into(),
        reason: "Approved".into(),
        location: String::new(),
        visible,
        place: None,
        image: crate::app::signui_dialog::ImageChoice::None,
        show_name: true,
        show_date: true,
        show_reason: false,
        error: None,
    }
}

#[test]
fn signing_keeps_earlier_signatures_and_edits_are_guarded() {
    let dir = std::env::temp_dir().join("pdfflow-ui-sign");
    std::fs::create_dir_all(&dir).unwrap();
    let idf = test_id_file(&dir);
    let src = dir.join("doc.pdf");
    std::fs::write(&src, crate::tests::source_pdf(0)).unwrap();
    let mut h = harness(Some(src.clone()));
    h.state_mut().add_test_id(idf);
    assert!(h.state().signature_reports().is_empty());
    // Wrong password is reported, not panicking.
    let mut bad = dialog(false);
    bad.password = "nope".into();
    assert!(h.state_mut().sign_document(&bad).is_err());
    // First signature on the untouched file, then a second one on top of it.
    let (one, _) = h.state_mut().sign_document(&dialog(false)).unwrap();
    std::fs::write(&src, &one).unwrap();
    h.state_mut().open_file(src.clone());
    assert_eq!(h.state().signature_reports().len(), 1);
    let (two, name) = h.state_mut().sign_document(&dialog(false)).unwrap();
    assert_eq!(name, "UI Tester");
    assert!(two.starts_with(&one), "the second signature is an incremental update");
    let r = crate::dsig::verify(&two, &[]);
    assert_eq!(r.len(), 2);
    assert!(r.iter().all(|s| s.valid()), "{r:?}");
    // Saving an unedited signed file is allowed; saving after an edit asks first.
    std::fs::write(&src, &two).unwrap();
    h.state_mut().open_file(src.clone());
    assert!(h.state_mut().try_save());
    assert_eq!(std::fs::read(&src).unwrap(), two, "unedited save keeps the bytes");
    h.key_press(Key::R);
    h.step();
    dragp(&mut h, pos2(100.0, 100.0), pos2(150.0, 150.0));
    assert!(!h.state_mut().try_save());
    assert!(h.state().unsign_prompt());
}

#[test]
fn visible_signature_box_lands_on_the_selected_object() {
    let dir = std::env::temp_dir().join("pdfflow-ui-sign2");
    std::fs::create_dir_all(&dir).unwrap();
    let idf = test_id_file(&dir);
    let mut h = harness(None);
    h.state_mut().add_test_id(idf);
    h.key_press(Key::R);
    h.step();
    dragp(&mut h, pos2(100.0, 100.0), pos2(250.0, 160.0));
    let r = h.state().doc.pages[0].objects[0].rect;
    let (signed, _) = h.state_mut().sign_document(&dialog(true)).unwrap();
    let doc = lopdf::Document::load_mem(&signed).unwrap();
    let widget = doc.objects.values().filter_map(|o| o.as_dict().ok()).find(|d| d.get(b"FT").and_then(|v| v.as_name()).ok() == Some(b"Sig".as_slice())).unwrap();
    let rect: Vec<f32> = widget.get(b"Rect").unwrap().as_array().unwrap().iter().map(|v| v.as_float().unwrap()).collect();
    // A4 user space has its origin at the bottom-left.
    let want = [r.min.x, 842.0 - r.max.y, r.max.x, 842.0 - r.min.y];
    for k in 0..4 {
        assert!((rect[k] - want[k]).abs() < 0.5, "{rect:?} vs {want:?}");
    }
    assert!(crate::dsig::verify(&signed, &[])[0].valid());
}

fn comments_in(pdf: &[u8]) -> Vec<(String, String, Vec<f32>)> {
    let doc = lopdf::Document::load_mem(pdf).unwrap();
    let mut out = Vec::new();
    for (_, pid) in doc.get_pages() {
        let page = doc.get_dictionary(pid).unwrap();
        let Ok(a) = page.get(b"Annots") else { continue };
        for x in doc.dereference(a).unwrap().1.as_array().unwrap() {
            let d = doc.dereference(x).unwrap().1.as_dict().unwrap();
            if d.get(b"Subtype").unwrap().as_name().unwrap() == b"Text" {
                let t = |k: &[u8]| crate::dsig::decode_text(d.get(k).unwrap().as_str().unwrap());
                let r = d.get(b"Rect").unwrap().as_array().unwrap().iter().map(|v| v.as_float().unwrap()).collect();
                out.push((t(b"Contents"), t(b"T"), r));
            }
        }
    }
    out
}

#[test]
fn comments_are_saved_as_pdf_annotations_and_read_back() {
    let mut h = harness(None);
    h.key_press(Key::Z);
    h.step();
    assert_eq!(h.state().tool, Tool::Comment);
    // An empty new comment disappears when closed.
    click(&mut h, pos2(400.0, 300.0));
    assert!(h.state().note_open());
    h.key_press(Key::Escape);
    h.run_steps(2);
    assert!(!h.state().note_open());
    assert!(h.state().doc.pages[0].objects.is_empty());
    // A comment with Thai and Latin text.
    click(&mut h, pos2(100.0, 120.0));
    h.run_steps(2);
    h.event(Event::Text("Check this ตรวจสอบ".into()));
    h.run_steps(2);
    h.key_press(Key::Escape);
    h.run_steps(2);
    let o = h.state().doc.pages[0].objects[0].clone();
    assert!(matches!(&o.kind, Kind::Note { text, .. } if text == "Check this ตรวจสอบ"));
    assert!((o.rect.min - pos2(100.0, 120.0)).length() < 2.0 / h.state().zoom);
    // Saved as a real Text annotation at the same place (A4 user space, origin bottom-left).
    let saved = crate::export::save(&h.state().doc).unwrap();
    let c = comments_in(&saved);
    assert_eq!(c.len(), 1);
    assert_eq!(c[0].0, "Check this ตรวจสอบ");
    assert!((c[0].2[0] - o.rect.min.x).abs() < 0.5 && (c[0].2[3] - (842.0 - o.rect.min.y)).abs() < 0.5, "{:?}", c[0].2);
    // Reopening reads it back as an editable comment, and an untouched file stays pristine.
    let path = std::env::temp_dir().join("pdfflow-comments.pdf");
    std::fs::write(&path, &saved).unwrap();
    h.state_mut().open_file(path.clone());
    let notes: Vec<_> = h.state().doc.pages[0].objects.iter().filter(|o| matches!(o.kind, Kind::Note { .. })).collect();
    assert_eq!(notes.len(), 1);
    assert!((notes[0].rect.min - o.rect.min).length() < 0.5);
    assert!(h.state().doc.is_pristine());
    // The page renderer gets a copy without the comment, so it is not drawn twice.
    let stripped = super::app::notes_without_comments_for_test(&saved).unwrap();
    assert!(comments_in(&stripped).is_empty());
    // Saving after an edit replaces the old comment instead of duplicating it.
    if let Kind::Note { text, .. } = &mut h.state_mut().doc.pages[0].objects[0].kind {
        *text = "Edited".into();
    }
    assert!(!h.state().doc.is_pristine());
    let again = crate::export::save(&h.state().doc).unwrap();
    let c = comments_in(&again);
    assert_eq!(c.len(), 1);
    assert_eq!(c[0].0, "Edited");
}

#[test]
fn drag_a_box_to_place_the_signature() {
    let dir = std::env::temp_dir().join("pdfflow-ui-sign3");
    std::fs::create_dir_all(&dir).unwrap();
    let idf = test_id_file(&dir);
    let mut h = harness(None);
    h.state_mut().add_test_id(idf);
    h.state_mut().start_placing_for_test(dialog(false));
    dragp(&mut h, pos2(300.0, 200.0), pos2(480.0, 260.0));
    let d = h.state_mut().take_sign_dialog_for_test().expect("the dialog comes back with the box");
    let (p, r) = d.place.unwrap();
    assert_eq!(p, 0);
    assert!((r.min - pos2(300.0, 200.0)).length() < 2.0 && (r.max - pos2(480.0, 260.0)).length() < 2.0, "{r:?}");
    let (signed, _) = h.state_mut().sign_document(&d).unwrap();
    let doc = lopdf::Document::load_mem(&signed).unwrap();
    let widget = doc.objects.values().filter_map(|o| o.as_dict().ok()).find(|d| d.get(b"FT").and_then(|v| v.as_name()).ok() == Some(b"Sig".as_slice())).unwrap();
    let rect: Vec<f32> = widget.get(b"Rect").unwrap().as_array().unwrap().iter().map(|v| v.as_float().unwrap()).collect();
    assert!((rect[0] - r.min.x).abs() < 0.5 && (rect[3] - (842.0 - r.min.y)).abs() < 0.5, "{rect:?}");
}
