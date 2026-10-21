//! Bezier paths for the Pen tool group: anchors with in/out handles, flattening, hit testing,
//! splitting, smoothing, freehand fitting and polygon filling.

use eframe::egui::{Pos2, Rect, Vec2, pos2};

/// One anchor. `cin`/`cout` are the handle end points (equal to `p` for a corner).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Node {
    pub p: Pos2,
    pub cin: Pos2,
    pub cout: Pos2,
}

impl Node {
    pub fn corner(p: Pos2) -> Self {
        Node { p, cin: p, cout: p }
    }

    pub fn is_corner(&self) -> bool {
        (self.cin - self.p).length() < 1e-3 && (self.cout - self.p).length() < 1e-3
    }

    pub fn map(&self, f: impl Fn(Pos2) -> Pos2) -> Node {
        Node { p: f(self.p), cin: f(self.cin), cout: f(self.cout) }
    }
}

/// Segment `i` runs from node `i` to node `i + 1` (wrapping when closed).
pub fn segments(nodes: &[Node], closed: bool) -> impl Iterator<Item = (usize, [Pos2; 4])> + '_ {
    let n = nodes.len();
    let count = if n < 2 { 0 } else if closed { n } else { n - 1 };
    (0..count).map(move |i| {
        let (a, b) = (nodes[i], nodes[(i + 1) % n]);
        (i, [a.p, a.cout, b.cin, b.p])
    })
}

pub fn cubic(c: &[Pos2; 4], t: f32) -> Pos2 {
    let u = 1.0 - t;
    let w = [u * u * u, 3.0 * u * u * t, 3.0 * u * t * t, t * t * t];
    pos2(
        w[0] * c[0].x + w[1] * c[1].x + w[2] * c[2].x + w[3] * c[3].x,
        w[0] * c[0].y + w[1] * c[1].y + w[2] * c[2].y + w[3] * c[3].y,
    )
}

fn is_line(c: &[Pos2; 4]) -> bool {
    c[0] == c[1] && c[2] == c[3]
}

/// Points along the path, `steps` per curved segment.
pub fn flatten(nodes: &[Node], closed: bool, steps: usize) -> Vec<Pos2> {
    let mut out = Vec::new();
    if let Some(n) = nodes.first() {
        out.push(n.p);
    }
    for (_, c) in segments(nodes, closed) {
        if is_line(&c) {
            out.push(c[3]);
        } else {
            for k in 1..=steps {
                out.push(cubic(&c, k as f32 / steps as f32));
            }
        }
    }
    out
}

/// Bounding box of anchors and handles (a superset of the curve).
pub fn bounds(nodes: &[Node]) -> Rect {
    let mut r = Rect::NOTHING;
    for n in nodes {
        r.extend_with(n.p);
        r.extend_with(n.cin);
        r.extend_with(n.cout);
    }
    r
}

/// Closest point on the path: (segment, t, distance).
pub fn nearest(nodes: &[Node], closed: bool, q: Pos2) -> Option<(usize, f32, f32)> {
    let mut best: Option<(usize, f32, f32)> = None;
    for (i, c) in segments(nodes, closed) {
        let steps = 48;
        for k in 0..=steps {
            let t = k as f32 / steps as f32;
            let d = (cubic(&c, t) - q).length();
            if best.is_none_or(|b| d < b.2) {
                best = Some((i, t, d));
            }
        }
    }
    best
}

/// Insert an anchor at parameter `t` of segment `seg` without changing the curve's shape.
pub fn split(nodes: &mut Vec<Node>, closed: bool, seg: usize, t: f32) {
    let Some((_, c)) = segments(nodes, closed).find(|(i, _)| *i == seg) else { return };
    let lerp = |a: Pos2, b: Pos2| a + (b - a) * t;
    let (p01, p12, p23) = (lerp(c[0], c[1]), lerp(c[1], c[2]), lerp(c[2], c[3]));
    let (p012, p123) = (lerp(p01, p12), lerp(p12, p23));
    let mid = lerp(p012, p123);
    let n = nodes.len();
    let line = is_line(&c);
    nodes[seg].cout = if line { nodes[seg].p } else { p01 };
    let next = (seg + 1) % n;
    nodes[next].cin = if line { nodes[next].p } else { p23 };
    let node = if line { Node::corner(mid) } else { Node { p: mid, cin: p012, cout: p123 } };
    nodes.insert(seg + 1, node);
}

/// Handles for a smooth anchor between its neighbours (Catmull-Rom tangent).
pub fn smooth_handles(prev: Option<Pos2>, p: Pos2, next: Option<Pos2>) -> (Pos2, Pos2) {
    let (a, b) = match (prev, next) {
        (Some(a), Some(b)) => (a, b),
        (Some(a), None) => (a, p),
        (None, Some(b)) => (p, b),
        (None, None) => return (p, p),
    };
    let tangent = (b - a) / 6.0;
    (p - tangent, p + tangent)
}

/// Recompute the handles of every non-corner anchor (Curvature Pen).
pub fn auto_smooth(nodes: &mut [Node], corners: &[bool], closed: bool) {
    let n = nodes.len();
    let pts: Vec<Pos2> = nodes.iter().map(|x| x.p).collect();
    for i in 0..n {
        if corners.get(i).copied().unwrap_or(false) {
            nodes[i] = Node::corner(pts[i]);
            continue;
        }
        let prev = if i > 0 { Some(pts[i - 1]) } else if closed && n > 2 { Some(pts[n - 1]) } else { None };
        let next = if i + 1 < n { Some(pts[i + 1]) } else if closed && n > 2 { Some(pts[0]) } else { None };
        let (cin, cout) = smooth_handles(prev, pts[i], next);
        nodes[i].cin = cin;
        nodes[i].cout = cout;
    }
}

/// Ramer-Douglas-Peucker simplification.
fn simplify(pts: &[Pos2], tol: f32) -> Vec<Pos2> {
    if pts.len() < 3 {
        return pts.to_vec();
    }
    let (a, b) = (pts[0], pts[pts.len() - 1]);
    let ab = b - a;
    let dist = |p: Pos2| {
        if ab.length_sq() < 1e-9 {
            (p - a).length()
        } else {
            let t = ((p - a).dot(ab) / ab.length_sq()).clamp(0.0, 1.0);
            (a + ab * t - p).length()
        }
    };
    let (mut idx, mut far) = (0, 0.0);
    for (i, p) in pts.iter().enumerate().take(pts.len() - 1).skip(1) {
        let d = dist(*p);
        if d > far {
            far = d;
            idx = i;
        }
    }
    if far <= tol {
        return vec![a, b];
    }
    let mut left = simplify(&pts[..=idx], tol);
    let right = simplify(&pts[idx..], tol);
    left.pop();
    left.extend(right);
    left
}

/// Fit a freehand stroke with a smooth path through its simplified points (Freeform Pen).
pub fn fit_freehand(pts: &[Pos2], tol: f32) -> Vec<Node> {
    let key = simplify(pts, tol);
    let mut nodes: Vec<Node> = key.iter().map(|p| Node::corner(*p)).collect();
    let corners = vec![false; nodes.len()];
    auto_smooth(&mut nodes, &corners, false);
    nodes
}

fn area2(poly: &[Pos2]) -> f32 {
    let n = poly.len();
    (0..n).map(|i| {
        let (a, b) = (poly[i], poly[(i + 1) % n]);
        a.x * b.y - b.x * a.y
    })
    .sum()
}

fn cross(o: Pos2, a: Pos2, b: Pos2) -> f32 {
    (a - o).x * (b - o).y - (a - o).y * (b - o).x
}

/// Triangulate a simple polygon by ear clipping (indices into `poly`).
pub fn triangulate(poly: &[Pos2]) -> Vec<u32> {
    let mut idx: Vec<usize> = (0..poly.len()).collect();
    // Drop consecutive duplicates, which create zero-area ears.
    idx.dedup_by(|a, b| (poly[*a] - poly[*b]).length() < 1e-4);
    let ccw = area2(&idx.iter().map(|i| poly[*i]).collect::<Vec<_>>()) > 0.0;
    let mut tris = Vec::new();
    let mut guard = 0;
    while idx.len() > 3 && guard < 10_000 {
        guard += 1;
        let n = idx.len();
        let mut clipped = false;
        for k in 0..n {
            let (ia, ib, ic) = (idx[(k + n - 1) % n], idx[k], idx[(k + 1) % n]);
            let (a, b, c) = (poly[ia], poly[ib], poly[ic]);
            let c2 = cross(a, b, c);
            if (c2 > 0.0) != ccw || c2.abs() < 1e-9 {
                continue;
            }
            let inside = idx.iter().any(|&j| {
                if j == ia || j == ib || j == ic {
                    return false;
                }
                let p = poly[j];
                let (d1, d2, d3) = (cross(a, b, p), cross(b, c, p), cross(c, a, p));
                if ccw { d1 > 0.0 && d2 > 0.0 && d3 > 0.0 } else { d1 < 0.0 && d2 < 0.0 && d3 < 0.0 }
            });
            if !inside {
                tris.extend_from_slice(&[ia as u32, ib as u32, ic as u32]);
                idx.remove(k);
                clipped = true;
                break;
            }
        }
        if !clipped {
            idx.remove(0); // self-intersecting or degenerate: drop a vertex and carry on
        }
    }
    if idx.len() == 3 {
        tris.extend(idx.iter().map(|i| *i as u32));
    }
    tris
}

pub fn point_in_polygon(poly: &[Pos2], q: Pos2) -> bool {
    let mut inside = false;
    let n = poly.len();
    for i in 0..n {
        let (a, b) = (poly[i], poly[(i + n - 1) % n]);
        if (a.y > q.y) != (b.y > q.y) && q.x < (b.x - a.x) * (q.y - a.y) / (b.y - a.y) + a.x {
            inside = !inside;
        }
    }
    inside
}

/// Absolute nodes to unit coordinates of their bounding rect (as stored in the object).
pub fn to_unit(nodes: &[Node]) -> (Rect, Vec<Node>) {
    let r = bounds(nodes);
    let u = |p: Pos2| {
        pos2(
            if r.width() > 0.0 { (p.x - r.min.x) / r.width() } else { 0.0 },
            if r.height() > 0.0 { (p.y - r.min.y) / r.height() } else { 0.0 },
        )
    };
    (r, nodes.iter().map(|n| n.map(u)).collect())
}

pub fn from_unit(r: Rect, nodes: &[Node]) -> Vec<Node> {
    nodes.iter().map(|n| n.map(|u| r.min + Vec2::new(u.x * r.width(), u.y * r.height()))).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_keeps_the_curve() {
        let mut nodes = vec![Node { p: pos2(0.0, 0.0), cin: pos2(0.0, 0.0), cout: pos2(0.0, 50.0) }, Node { p: pos2(100.0, 0.0), cin: pos2(100.0, 50.0), cout: pos2(100.0, 0.0) }];
        let orig = [nodes[0].p, nodes[0].cout, nodes[1].cin, nodes[1].p];
        split(&mut nodes, false, 0, 0.3);
        assert_eq!(nodes.len(), 3);
        let segs: Vec<[Pos2; 4]> = segments(&nodes, false).map(|(_, c)| c).collect();
        for k in 0..=10 {
            let u = k as f32 / 10.0;
            assert!((cubic(&segs[0], u) - cubic(&orig, 0.3 * u)).length() < 1e-3);
            assert!((cubic(&segs[1], u) - cubic(&orig, 0.3 + 0.7 * u)).length() < 1e-3);
        }
    }

    #[test]
    fn triangulates_a_concave_polygon() {
        let l = [pos2(0.0, 0.0), pos2(10.0, 0.0), pos2(10.0, 4.0), pos2(4.0, 4.0), pos2(4.0, 10.0), pos2(0.0, 10.0)];
        let t = triangulate(&l);
        assert_eq!(t.len(), 12);
        let area: f32 = t.chunks(3).map(|c| cross(l[c[0] as usize], l[c[1] as usize], l[c[2] as usize]).abs() / 2.0).sum();
        assert!((area - 64.0).abs() < 1e-3);
    }

    #[test]
    fn freehand_fit_follows_the_stroke() {
        let pts: Vec<Pos2> = (0..=100).map(|i| pos2(i as f32, (i as f32 / 10.0).sin() * 20.0)).collect();
        let nodes = fit_freehand(&pts, 1.0);
        assert!(nodes.len() < 30);
        for p in pts.iter().step_by(5) {
            assert!(nearest(&nodes, false, *p).unwrap().2 < 2.5);
        }
    }
}
