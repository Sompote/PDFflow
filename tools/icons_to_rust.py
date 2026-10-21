"""Flatten Lucide SVG icons (24x24, stroke-only) into polylines for src/icon_data.rs.

Run: python3 tools/icons_to_rust.py > src/icon_data.rs
"""
import math, os, re, sys, xml.etree.ElementTree as ET

ICON_DIR = os.path.join(os.path.dirname(__file__), "..", "assets", "icons")
STEPS = 10  # segments per curve

def tokens(d):
    for m in re.finditer(r"[MmLlHhVvCcSsQqTtAaZz]|-?(?:\d+\.?\d*|\.\d+)(?:e-?\d+)?", d):
        yield m.group(0)

def arc_points(x1, y1, rx, ry, phi, fa, fs, x2, y2):
    if rx == 0 or ry == 0:
        return [(x2, y2)]
    phi = math.radians(phi)
    cp, sp = math.cos(phi), math.sin(phi)
    dx, dy = (x1 - x2) / 2, (y1 - y2) / 2
    x1p, y1p = cp * dx + sp * dy, -sp * dx + cp * dy
    rx, ry = abs(rx), abs(ry)
    lam = x1p**2 / rx**2 + y1p**2 / ry**2
    if lam > 1:
        rx *= math.sqrt(lam); ry *= math.sqrt(lam)
    num = rx**2 * ry**2 - rx**2 * y1p**2 - ry**2 * x1p**2
    den = rx**2 * y1p**2 + ry**2 * x1p**2
    co = math.sqrt(max(0, num / den)) if den else 0
    if fa == fs:
        co = -co
    cxp, cyp = co * rx * y1p / ry, -co * ry * x1p / rx
    cx = cp * cxp - sp * cyp + (x1 + x2) / 2
    cy = sp * cxp + cp * cyp + (y1 + y2) / 2
    ang = lambda ux, uy, vx, vy: math.atan2(ux * vy - uy * vx, ux * vx + uy * vy)
    t1 = ang(1, 0, (x1p - cxp) / rx, (y1p - cyp) / ry)
    dt = ang((x1p - cxp) / rx, (y1p - cyp) / ry, (-x1p - cxp) / rx, (-y1p - cyp) / ry)
    if not fs and dt > 0: dt -= 2 * math.pi
    if fs and dt < 0: dt += 2 * math.pi
    n = max(2, int(abs(dt) / (math.pi / 8)) + 1)
    out = []
    for i in range(1, n + 1):
        t = t1 + dt * i / n
        x, y = rx * math.cos(t), ry * math.sin(t)
        out.append((cp * x - sp * y + cx, sp * x + cp * y + cy))
    return out

def path_polys(d):
    toks = list(tokens(d)); i = 0
    polys, cur = [], []
    x = y = sx = sy = 0.0; cmd = None; lc = None  # last control point
    def num():
        nonlocal i
        v = float(toks[i]); i += 1; return v
    while i < len(toks):
        if re.match(r"[A-Za-z]", toks[i]):
            cmd = toks[i]; i += 1
        rel = cmd.islower(); c = cmd.upper()
        ox, oy = (x, y) if rel else (0.0, 0.0)
        if c == "M":
            if len(cur) > 1: polys.append((cur, False))
            x, y = num() + ox, num() + oy; sx, sy = x, y; cur = [(x, y)]
            cmd = "l" if rel else "L"; lc = None
        elif c == "L":
            x, y = num() + ox, num() + oy; cur.append((x, y)); lc = None
        elif c == "H":
            x = num() + (x if rel else 0); cur.append((x, y)); lc = None
        elif c == "V":
            y = num() + (y if rel else 0); cur.append((x, y)); lc = None
        elif c in "CS":
            if c == "C":
                x1, y1 = num() + ox, num() + oy
            else:
                x1, y1 = (2 * x - lc[0], 2 * y - lc[1]) if lc else (x, y)
            x2, y2 = num() + ox, num() + oy; ex, ey = num() + ox, num() + oy
            for k in range(1, STEPS + 1):
                t = k / STEPS; u = 1 - t
                cur.append((u**3*x + 3*u*u*t*x1 + 3*u*t*t*x2 + t**3*ex,
                            u**3*y + 3*u*u*t*y1 + 3*u*t*t*y2 + t**3*ey))
            lc = (x2, y2); x, y = ex, ey
        elif c in "QT":
            if c == "Q":
                x1, y1 = num() + ox, num() + oy
            else:
                x1, y1 = (2 * x - lc[0], 2 * y - lc[1]) if lc else (x, y)
            ex, ey = num() + ox, num() + oy
            for k in range(1, STEPS + 1):
                t = k / STEPS; u = 1 - t
                cur.append((u*u*x + 2*u*t*x1 + t*t*ex, u*u*y + 2*u*t*y1 + t*t*ey))
            lc = (x1, y1); x, y = ex, ey
        elif c == "A":
            rx, ry, phi = num(), num(), num()
            fa, fs = int(num()), int(num())
            ex, ey = num() + ox, num() + oy
            cur.extend(arc_points(x, y, rx, ry, phi, fa, fs, ex, ey)); x, y = ex, ey; lc = None
        elif c == "Z":
            if len(cur) > 1: polys.append((cur, True))
            x, y = sx, sy; cur = [(x, y)]; lc = None
    if len(cur) > 1: polys.append((cur, False))
    return polys

def ellipse(cx, cy, rx, ry):
    n = 24
    return ([(cx + rx * math.cos(2 * math.pi * k / n), cy + ry * math.sin(2 * math.pi * k / n)) for k in range(n)], True)

def element_polys(e):
    tag = e.tag.split("}")[-1]; a = lambda k, dflt=0: float(e.get(k, dflt))
    if tag == "path": return path_polys(e.get("d"))
    if tag == "line": return [([(a("x1"), a("y1")), (a("x2"), a("y2"))], False)]
    if tag == "circle": return [ellipse(a("cx"), a("cy"), a("r"), a("r"))]
    if tag == "ellipse": return [ellipse(a("cx"), a("cy"), a("rx"), a("ry"))]
    if tag in ("polyline", "polygon"):
        v = [float(t) for t in re.split(r"[ ,]+", e.get("points").strip())]
        return [(list(zip(v[0::2], v[1::2])), tag == "polygon")]
    if tag == "rect":
        x, y, w, h = a("x"), a("y"), a("width"), a("height")
        r = min(a("rx", e.get("ry", 0)), w / 2, h / 2)
        if r <= 0: return [([(x, y), (x + w, y), (x + w, y + h), (x, y + h)], True)]
        d = (f"M{x+r} {y}H{x+w-r}A{r} {r} 0 0 1 {x+w} {y+r}V{y+h-r}A{r} {r} 0 0 1 {x+w-r} {y+h}"
             f"H{x+r}A{r} {r} 0 0 1 {x} {y+h-r}V{y+r}A{r} {r} 0 0 1 {x+r} {y}Z")
        return path_polys(d)
    return []

print("//! Lucide icons (ISC licence, https://lucide.dev), flattened to polylines in a 24x24 box")
print("//! by tools/icons_to_rust.py. Do not edit by hand.\n")
print("pub type Poly = (&'static [[f32; 2]], bool);\n")
print("pub static ICONS: &[(&str, &[Poly])] = &[")
for f in sorted(os.listdir(ICON_DIR)):
    if not f.endswith(".svg"): continue
    root = ET.parse(os.path.join(ICON_DIR, f)).getroot()
    polys = [p for el in root.iter() for p in element_polys(el)]
    print(f'    ("{f[:-4]}", &[')
    for pts, closed in polys:
        s = ", ".join(f"[{px:.2f}, {py:.2f}]" for px, py in pts)
        print(f"        (&[{s}], {'true' if closed else 'false'}),")
    print("    ]),")
print("];")
