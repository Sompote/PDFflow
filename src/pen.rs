//! The Pen tool group (Photoshop's P tools): Pen, Freeform Pen, Curvature Pen, Add Anchor Point,
//! Delete Anchor Point and Convert Point, plus the toolbar flyout that holds them.

use super::*;
use crate::path::{self, Node};

pub(super) const PEN_TOOLS: [Tool; 6] = [Tool::PenPath, Tool::FreeformPen, Tool::CurvaturePen, Tool::AddAnchor, Tool::DeleteAnchor, Tool::ConvertPoint];

/// Pick radius for anchors and handles, in screen points.
const PICK: f32 = 7.0;

/// A path being drawn with the Pen or Curvature Pen (page points).
pub(super) struct PenDraft {
    pub page: usize,
    pub nodes: Vec<Node>,
    pub corners: Vec<bool>,
    pub curvature: bool,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Part {
    Anchor,
    In,
    Out,
    /// Convert Point dragged from an anchor: pull out a new pair of symmetric handles.
    Pull,
}

impl Tool {
    pub(super) fn is_pen(self) -> bool {
        PEN_TOOLS.contains(&self)
    }
}

impl App {
    fn sp(&self, rects: &[Rect], page: usize, p: Pos2) -> Pos2 {
        rects[page].min + self.doc.pages[page].to_view(p).to_vec2() * self.zoom
    }

    /// Anchor or handle of path `idx` on `page` under the screen point `s` (handles first).
    fn path_part_at(&self, rects: &[Rect], page: usize, idx: usize, s: Pos2) -> Option<(usize, Part)> {
        let (abs, _) = self.doc.pages[page].objects.get(idx)?.path_abs()?;
        for (i, n) in abs.iter().enumerate() {
            if !n.is_corner() {
                if (self.sp(rects, page, n.cout) - s).length() < PICK {
                    return Some((i, Part::Out));
                }
                if (self.sp(rects, page, n.cin) - s).length() < PICK {
                    return Some((i, Part::In));
                }
            }
        }
        abs.iter().position(|n| (self.sp(rects, page, n.p) - s).length() < PICK).map(|i| (i, Part::Anchor))
    }

    /// The path point under `s`: the selected path first, then any path from the top down.
    fn path_target(&self, rects: &[Rect], page: usize, s: Pos2) -> Option<(usize, usize, Part)> {
        if let Some((sp, si)) = self.sel {
            if sp == page {
                if let Some((n, part)) = self.path_part_at(rects, page, si, s) {
                    return Some((si, n, part));
                }
            }
        }
        let objs = &self.doc.pages[page].objects;
        (0..objs.len()).rev().find_map(|i| self.path_part_at(rects, page, i, s).map(|(n, part)| (i, n, part)))
    }

    fn start_point_drag(&mut self, page: usize, idx: usize, node: usize, part: Part, symmetric: bool) {
        let Some((orig, closed)) = self.doc.pages[page].objects[idx].path_abs() else { return };
        self.sel = Some((page, idx));
        self.drag = Some(Drag::PathPoint { page, idx, node, part, symmetric, before: self.doc.pages.clone(), orig, closed, moved: false });
    }

    /// Handle a press with a pen-group tool; `hit` is the page under the pointer.
    pub(super) fn pen_press(&mut self, rects: &[Rect], s: Pos2, hit: Option<(usize, Pos2)>, alt: bool) {
        let z = self.zoom;
        match self.tool {
            Tool::PenPath | Tool::CurvaturePen => {
                if let Some(d) = &self.pen {
                    let page = d.page;
                    if d.nodes.len() >= 2 && (self.sp(rects, page, d.nodes[0].p) - s).length() < PICK + 1.0 {
                        self.finish_pen(true);
                        return;
                    }
                    if hit.map(|h| h.0) == Some(page) {
                        let pp = self.to_page(rects, page, s);
                        let d = self.pen.as_mut().expect("draft");
                        d.nodes.push(Node::corner(pp));
                        d.corners.push(alt);
                        if d.curvature {
                            path::auto_smooth(&mut d.nodes, &d.corners, false);
                        } else {
                            self.drag = Some(Drag::PenPull { page });
                        }
                        return;
                    }
                    self.finish_pen(false);
                }
                let Some((pi, pp)) = hit else { return };
                if let Some((idx, node, part)) = self.path_target(rects, pi, s) {
                    self.start_point_drag(pi, idx, node, part, true);
                    return;
                }
                self.sel = None;
                let curvature = self.tool == Tool::CurvaturePen;
                self.pen = Some(PenDraft { page: pi, nodes: vec![Node::corner(pp)], corners: vec![alt], curvature });
                if !curvature {
                    self.drag = Some(Drag::PenPull { page: pi });
                }
            }
            Tool::FreeformPen => {
                if let Some((pi, pp)) = hit {
                    self.drag = Some(Drag::Freeform { page: pi, pts: vec![pp] });
                }
            }
            Tool::AddAnchor => {
                let Some((pi, pp)) = hit else { return };
                let objs = &self.doc.pages[pi].objects;
                let target = (0..objs.len()).rev().find_map(|i| {
                    let (abs, closed) = objs[i].path_abs()?;
                    let (seg, t, d) = path::nearest(&abs, closed, pp)?;
                    (d * z < PICK).then_some((i, abs, closed, seg, t))
                });
                if let Some((i, mut abs, closed, seg, t)) = target {
                    self.doc.checkpoint();
                    path::split(&mut abs, closed, seg, t);
                    self.doc.pages[pi].objects[i].set_path_abs(&abs, closed);
                    self.sel = Some((pi, i));
                }
            }
            Tool::DeleteAnchor => {
                let Some((pi, _)) = hit else { return };
                if let Some((i, node, Part::Anchor)) = self.path_target(rects, pi, s) {
                    let Some((mut abs, closed)) = self.doc.pages[pi].objects[i].path_abs() else { return };
                    self.doc.checkpoint();
                    abs.remove(node);
                    if abs.len() < 2 {
                        self.doc.pages[pi].objects.remove(i);
                        self.sel = None;
                    } else {
                        self.doc.pages[pi].objects[i].set_path_abs(&abs, closed && abs.len() > 2);
                        self.sel = Some((pi, i));
                    }
                }
            }
            Tool::ConvertPoint => {
                let Some((pi, _)) = hit else { return };
                match self.path_target(rects, pi, s) {
                    Some((i, node, Part::Anchor)) => self.start_point_drag(pi, i, node, Part::Pull, false),
                    Some((i, node, part)) => self.start_point_drag(pi, i, node, part, false),
                    None => {}
                }
            }
            _ => {}
        }
    }

    /// Pointer moved while a pen drag is active (`pp` in the drag's page points).
    pub(super) fn pen_drag(&mut self, pp: Pos2) {
        let z = self.zoom;
        match &mut self.drag {
            Some(Drag::PenPull { .. }) => {
                if let Some(d) = &mut self.pen {
                    if let Some(n) = d.nodes.last_mut() {
                        let v = pp - n.p;
                        if v.length() * z > 2.0 {
                            n.cout = n.p + v;
                            n.cin = n.p - v;
                        }
                    }
                }
            }
            Some(Drag::Freeform { pts, .. }) => {
                if pts.last().is_none_or(|l| (pp - *l).length() > 0.75 / z) {
                    pts.push(pp);
                }
            }
            Some(Drag::PathPoint { page, idx, node, part, symmetric, orig, closed, moved, .. }) => {
                let mut nodes = orig.clone();
                let o = orig[*node];
                let n = &mut nodes[*node];
                match part {
                    Part::Anchor => {
                        let d = pp - o.p;
                        *n = o.map(|q| q + d);
                    }
                    Part::In | Part::Out => {
                        let (this, other) = if *part == Part::Out { (&mut n.cout, &mut n.cin) } else { (&mut n.cin, &mut n.cout) };
                        *this = pp;
                        let other_len = (*other - o.p).length();
                        if *symmetric && other_len > 1e-3 && (pp - o.p).length() > 1e-3 {
                            *other = o.p - (pp - o.p).normalized() * other_len;
                        }
                    }
                    Part::Pull => {
                        n.cout = pp;
                        n.cin = o.p - (pp - o.p);
                    }
                }
                *moved |= (pp - o.p).length() * z > 2.0 || *part != Part::Pull;
                let (page, idx, closed) = (*page, *idx, *closed);
                self.doc.pages[page].objects[idx].set_path_abs(&nodes, closed);
            }
            _ => {}
        }
    }

    /// Release of a pen drag.
    pub(super) fn pen_release(&mut self, drag: Drag) {
        match drag {
            Drag::Freeform { page, pts } => {
                let nodes = path::fit_freehand(&pts, 1.5 / self.zoom);
                if nodes.len() >= 2 {
                    self.add_path(page, &nodes, false);
                }
            }
            Drag::PathPoint { page, idx, node, part, before, orig, closed, moved, .. } => {
                if moved {
                    self.doc.push_undo(before);
                } else if part == Part::Pull {
                    // A click with Convert Point toggles the anchor between corner and smooth.
                    let mut nodes = orig;
                    let n = nodes.len();
                    if nodes[node].is_corner() {
                        let prev = if node > 0 { Some(nodes[node - 1].p) } else if closed { Some(nodes[n - 1].p) } else { None };
                        let next = if node + 1 < n { Some(nodes[node + 1].p) } else if closed { Some(nodes[0].p) } else { None };
                        let (cin, cout) = path::smooth_handles(prev, nodes[node].p, next);
                        nodes[node].cin = cin;
                        nodes[node].cout = cout;
                    } else {
                        nodes[node] = Node::corner(nodes[node].p);
                    }
                    self.doc.pages[page].objects[idx].set_path_abs(&nodes, closed);
                    self.doc.push_undo(before);
                }
            }
            _ => {}
        }
    }

    fn add_path(&mut self, page: usize, nodes: &[Node], closed: bool) {
        let mut o = Obj {
            rect: Rect::NOTHING,
            kind: Kind::Path { nodes: Arc::new(Vec::new()), closed, stroke: self.opts.color, fill: self.opts.fill, width: self.opts.width },
        };
        o.set_path_abs(nodes, closed);
        self.doc.checkpoint();
        self.doc.pages[page].objects.push(o);
        self.sel = Some((page, self.doc.pages[page].objects.len() - 1));
    }

    /// Turn the path being drawn into an object (needs two anchors).
    pub(super) fn finish_pen(&mut self, closed: bool) {
        let Some(d) = self.pen.take() else { return };
        if d.nodes.len() < 2 {
            return;
        }
        let mut nodes = d.nodes;
        if d.curvature {
            path::auto_smooth(&mut nodes, &d.corners, closed);
        }
        self.add_path(d.page, &nodes, closed);
    }

    /// Remove the last anchor of the path being drawn (Backspace).
    pub(super) fn pen_backspace(&mut self) {
        if let Some(d) = &mut self.pen {
            d.nodes.pop();
            d.corners.pop();
            if d.nodes.is_empty() {
                self.pen = None;
            } else if d.curvature {
                path::auto_smooth(&mut d.nodes, &d.corners, false);
            }
        }
    }

    /// Draft path, rubber band and the anchors/handles of the selected path.
    pub(super) fn pen_overlay(&self, painter: &egui::Painter, rects: &[Rect], pointer: Option<Pos2>) {
        let t = Tokens::get(painter.ctx());
        let z = self.zoom;
        let anchor = |p: Pos2, hot: bool| {
            let r = Rect::from_center_size(p, Vec2::splat(if hot { 9.0 } else { 7.0 }));
            painter.rect_filled(r, 0.0, if hot { t.accent } else { Color32::WHITE });
            painter.rect_stroke(r, 0.0, Stroke::new(1.0, t.accent), StrokeKind::Inside);
        };
        let handle = |a: Pos2, h: Pos2| {
            painter.line_segment([a, h], Stroke::new(1.0, t.accent));
            painter.circle(h, 3.5, Color32::WHITE, Stroke::new(1.0, t.accent));
        };
        if let Some(d) = &self.pen {
            let page = d.page;
            let ts = |p: Pos2| self.sp(rects, page, p);
            let mut nodes = d.nodes.clone();
            // Preview the next segment to the pointer.
            if let (Some(s), None) = (pointer, &self.drag) {
                let pp = self.to_page(rects, page, s);
                nodes.push(Node::corner(pp));
                if d.curvature {
                    let mut corners = d.corners.clone();
                    corners.push(false);
                    path::auto_smooth(&mut nodes, &corners, false);
                }
            }
            paint_path(painter, &nodes, false, self.opts.color, None, self.opts.width, z, ts);
            let near_first = pointer.is_some_and(|s| d.nodes.len() >= 2 && (ts(d.nodes[0].p) - s).length() < PICK + 1.0);
            for (i, n) in d.nodes.iter().enumerate() {
                if !n.is_corner() && i + 1 == d.nodes.len() {
                    handle(ts(n.p), ts(n.cin));
                    handle(ts(n.p), ts(n.cout));
                }
                anchor(ts(n.p), i == 0 && near_first);
            }
        }
        if !self.tool.is_pen() {
            return;
        }
        if let Some((page, idx)) = self.sel {
            if let Some((abs, _)) = self.doc.pages.get(page).and_then(|p| p.objects.get(idx)).and_then(|o| o.path_abs()) {
                let ts = |p: Pos2| self.sp(rects, page, p);
                for n in &abs {
                    if !n.is_corner() {
                        handle(ts(n.p), ts(n.cin));
                        handle(ts(n.p), ts(n.cout));
                    }
                }
                for n in &abs {
                    anchor(ts(n.p), false);
                }
            }
        }
    }

    /// The pen slot in the tool column: the last used pen tool, a corner triangle, and a flyout
    /// on right-click or press-and-hold, as in Photoshop.
    pub(super) fn pen_slot(&mut self, ui: &mut Ui) {
        let t = Tokens::get(ui.ctx());
        let tip = format!("{} (P) · right-click or hold for more pen tools", self.pen_last.name());
        let resp = icons::button(ui, self.pen_last.icon(), self.tool.is_pen(), &tip);
        let r = resp.rect;
        let c = r.right_bottom() + vec2(-3.0, -3.0);
        ui.painter().add(Shape::convex_polygon(vec![c, c - vec2(5.0, 0.0), c - vec2(0.0, 5.0)], t.icon, Stroke::NONE));
        let held = resp.is_pointer_button_down_on() && ui.input(|i| i.pointer.press_start_time().is_some_and(|s| i.time - s > 0.4));
        if resp.secondary_clicked() || (held && self.flyout.is_none()) {
            self.flyout = Some(r.right_top() + vec2(6.0, -4.0));
            self.flyout_frame = ui.ctx().cumulative_pass_nr();
        } else if resp.clicked() && self.flyout.is_none() {
            self.set_tool(self.pen_last);
        }
    }

    pub(super) fn pen_flyout(&mut self, ctx: &egui::Context) {
        let Some(at) = self.flyout else { return };
        let t = Tokens::get(ctx);
        let mut pick = None;
        let area = egui::Area::new(Id::new("pen-flyout")).order(egui::Order::Foreground).fixed_pos(at).show(ctx, |ui| {
            egui::Frame::popup(ui.style()).show(ui, |ui| {
                ui.set_width(250.0);
                ui.spacing_mut().item_spacing.y = 0.0;
                for tool in PEN_TOOLS {
                    let (row, resp) = ui.allocate_exact_size(vec2(250.0, 30.0), Sense::click());
                    if resp.hovered() {
                        ui.painter().rect_filled(row, t.radius_sm, t.accent);
                    }
                    if tool == self.pen_last {
                        ui.painter().rect_filled(Rect::from_center_size(pos2(row.left() + 10.0, row.center().y), vec2(5.0, 5.0)), 0.0, t.text);
                    }
                    icons::paint(ui.painter(), Rect::from_center_size(pos2(row.left() + 32.0, row.center().y), vec2(17.0, 17.0)), tool.icon(), t.text);
                    ui.painter().text(pos2(row.left() + 52.0, row.center().y), egui::Align2::LEFT_CENTER, format!("{} Tool", tool.name()), FontId::proportional(13.0), t.text);
                    ui.painter().text(pos2(row.right() - 12.0, row.center().y), egui::Align2::RIGHT_CENTER, "P", FontId::proportional(12.0), t.text_dim);
                    if resp.clicked() {
                        pick = Some(tool);
                    }
                }
            });
        });
        let fresh = ctx.cumulative_pass_nr() <= self.flyout_frame + 1;
        if let Some(tool) = pick {
            self.flyout = None;
            self.set_tool(tool);
        } else if !fresh && (area.response.clicked_elsewhere() || ctx.input(|i| i.key_pressed(Key::Escape))) {
            self.flyout = None;
        }
    }

    /// P selects the last pen tool; Shift+P steps through the group.
    pub(super) fn pen_key(&mut self, shift: bool) {
        if shift {
            let i = PEN_TOOLS.iter().position(|t| *t == self.pen_last).unwrap_or(0);
            self.set_tool(PEN_TOOLS[(i + 1) % PEN_TOOLS.len()]);
        } else {
            self.set_tool(self.pen_last);
        }
    }
}
