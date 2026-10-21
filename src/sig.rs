//! Saved signatures: drawn once in a pad, stored as unit-square strokes, reused on any page.

use crate::theme::Tokens;
use eframe::egui::{self, Color32, Pos2, Rect, Sense, Shape, Stroke, Ui, Vec2, pos2, vec2};
use std::path::PathBuf;
use std::sync::Arc;

#[derive(Clone)]
pub struct Signature {
    /// Width / height of the drawn box.
    pub aspect: f32,
    pub strokes: Arc<Vec<Vec<Pos2>>>,
}

/// The app's settings folder (signatures, Digital IDs).
pub fn config_dir() -> Option<PathBuf> {
    store_path().and_then(|p| p.parent().map(|d| d.to_path_buf()))
}

/// A Digital ID kept in the settings folder: its PKCS#12 file and the name shown in lists.
#[derive(Clone)]
pub struct IdEntry {
    pub file: PathBuf,
    pub name: String,
    pub detail: String,
    /// Certificate expiry, "YYYY-MM-DD" (empty if unknown).
    pub expires: String,
}

fn ids_index() -> Option<PathBuf> {
    config_dir().map(|d| d.join("ids").join("index.txt"))
}

/// Saved Digital IDs (`ids/index.txt`: `file|name|detail` per line).
pub fn load_ids() -> Vec<IdEntry> {
    let (Some(index), Some(dir)) = (ids_index(), config_dir()) else { return Vec::new() };
    let Ok(text) = std::fs::read_to_string(index) else { return Vec::new() };
    text.lines()
        .filter_map(|l| {
            let mut it = l.splitn(4, '|');
            let file = dir.join("ids").join(it.next()?);
            let name = it.next().unwrap_or("").to_owned();
            let detail = it.next().unwrap_or("").to_owned();
            let expires = it.next().unwrap_or("").to_owned();
            file.exists().then_some(IdEntry { file, name, detail, expires })
        })
        .collect()
}

pub fn save_ids(ids: &[IdEntry]) {
    let Some(index) = ids_index() else { return };
    let text: String = ids
        .iter()
        .map(|e| format!("{}|{}|{}|{}\n", e.file.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(), e.name.replace('|', " "), e.detail.replace('|', " "), e.expires))
        .collect();
    let _ = std::fs::write(index, text);
}

/// Store a PKCS#12 file in the settings folder and return its path.
pub fn store_id_file(name: &str, p12: &[u8]) -> Result<PathBuf, String> {
    let dir = config_dir().ok_or("No settings folder.")?.join("ids");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let stem: String = name.chars().map(|c| if c.is_alphanumeric() { c } else { '_' }).collect();
    let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let path = dir.join(format!("{stem}-{stamp}.p12"));
    std::fs::write(&path, p12).map_err(|e| e.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
    }
    Ok(path)
}

/// Root certificates the user chose to trust (e.g. a university CA), as DER files.
pub fn load_trusted() -> Vec<(PathBuf, Vec<u8>)> {
    let Some(dir) = config_dir().map(|d| d.join("trusted")) else { return Vec::new() };
    let Ok(rd) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut v: Vec<(PathBuf, Vec<u8>)> = rd.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|e| e == "der")).filter_map(|p| std::fs::read(&p).ok().map(|b| (p, b))).collect();
    v.sort();
    v
}

pub fn store_trusted(der: &[u8], name: &str) -> Result<PathBuf, String> {
    let dir = config_dir().ok_or("No settings folder.")?.join("trusted");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let stem: String = name.chars().map(|c| if c.is_alphanumeric() { c } else { '_' }).take(60).collect();
    let path = dir.join(format!("{stem}.der"));
    std::fs::write(&path, der).map_err(|e| e.to_string())?;
    Ok(path)
}

fn store_path() -> Option<PathBuf> {
    let base = if cfg!(target_os = "macos") {
        std::env::var_os("HOME").map(|h| PathBuf::from(h).join("Library/Application Support/PDFEditor"))
    } else if cfg!(windows) {
        std::env::var_os("APPDATA").map(|a| PathBuf::from(a).join("PDFEditor"))
    } else {
        std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
            .map(|c| c.join("pdfeditor"))
    };
    base.map(|b| b.join("signatures.txt"))
}

/// One signature per line: `aspect|x,y x,y;x,y ...` with strokes separated by `;`.
pub fn load() -> Vec<Signature> {
    let Some(text) = store_path().and_then(|p| std::fs::read_to_string(p).ok()) else { return Vec::new() };
    text.lines()
        .filter_map(|line| {
            let (aspect, rest) = line.split_once('|')?;
            let strokes = rest
                .split(';')
                .map(|s| {
                    s.split_whitespace()
                        .filter_map(|p| {
                            let (x, y) = p.split_once(',')?;
                            Some(pos2(x.parse().ok()?, y.parse().ok()?))
                        })
                        .collect::<Vec<_>>()
                })
                .filter(|s| !s.is_empty())
                .collect::<Vec<_>>();
            (!strokes.is_empty()).then(|| Signature { aspect: aspect.parse().unwrap_or(3.0), strokes: Arc::new(strokes) })
        })
        .collect()
}

pub fn save(sigs: &[Signature]) {
    let Some(path) = store_path() else { return };
    let mut out = String::new();
    for s in sigs {
        out.push_str(&format!("{:.4}|", s.aspect));
        let parts: Vec<String> =
            s.strokes.iter().map(|st| st.iter().map(|p| format!("{:.4},{:.4}", p.x, p.y)).collect::<Vec<_>>().join(" ")).collect();
        out.push_str(&parts.join(";"));
        out.push('\n');
    }
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(path, out);
}

/// Paint a signature's strokes into a screen rect, keeping its aspect ratio.
pub fn paint(painter: &egui::Painter, rect: Rect, sig: &Signature, color: Color32, width: f32) {
    let r = fit(rect, sig.aspect);
    for st in sig.strokes.iter() {
        let pts: Vec<Pos2> = st.iter().map(|p| pos2(r.min.x + p.x * r.width(), r.min.y + p.y * r.height())).collect();
        if pts.len() == 1 {
            painter.circle_filled(pts[0], width * 0.5, color);
        } else {
            painter.add(Shape::line(pts, Stroke::new(width, color)));
        }
    }
}

fn fit(rect: Rect, aspect: f32) -> Rect {
    let w = rect.width().min(rect.height() * aspect);
    Rect::from_center_size(rect.center(), vec2(w, w / aspect))
}

/// The drawing pad shown in the "New Signature" dialog.
#[derive(Default)]
pub struct Pad {
    strokes: Vec<Vec<Pos2>>,
    current: Vec<Pos2>,
}

impl Pad {
    pub fn ui(&mut self, ui: &mut Ui, color: Color32) {
        let t = Tokens::get(ui.ctx());
        let (rect, resp) = ui.allocate_exact_size(vec2(520.0, 200.0), Sense::drag());
        let p = ui.painter_at(rect);
        p.rect_filled(rect, t.radius, Color32::WHITE);
        let base = rect.bottom() - 48.0;
        p.line_segment([pos2(rect.left() + 24.0, base), pos2(rect.right() - 24.0, base)], Stroke::new(1.0, Color32::from_gray(200)));
        p.text(pos2(rect.left() + 24.0, base + 6.0), egui::Align2::LEFT_TOP, "Sign above the line", egui::FontId::proportional(11.0), Color32::from_gray(150));
        if let Some(pos) = resp.interact_pointer_pos() {
            if resp.dragged() || resp.drag_started() {
                let local = pos - rect.min;
                if self.current.last().is_none_or(|l| (local - l.to_vec2()).length() > 0.8) {
                    self.current.push(local.to_pos2());
                }
            }
        }
        if resp.drag_stopped() && !self.current.is_empty() {
            self.strokes.push(std::mem::take(&mut self.current));
        }
        for st in self.strokes.iter().chain(std::iter::once(&self.current)) {
            let pts: Vec<Pos2> = st.iter().map(|q| rect.min + q.to_vec2()).collect();
            if pts.len() == 1 {
                p.circle_filled(pts[0], 1.4, color);
            } else if pts.len() > 1 {
                p.add(Shape::line(pts, Stroke::new(2.6, color)));
            }
        }
        if self.is_empty() {
            p.text(rect.center() - vec2(0.0, 20.0), egui::Align2::CENTER_CENTER, "Draw your signature here", egui::FontId::proportional(15.0), Color32::from_gray(170));
        }
        resp.on_hover_cursor(egui::CursorIcon::Crosshair);
    }

    pub fn is_empty(&self) -> bool {
        self.strokes.is_empty() && self.current.is_empty()
    }

    pub fn clear(&mut self) {
        self.strokes.clear();
        self.current.clear();
    }

    /// Crop to the drawn strokes and normalise them to a unit square.
    pub fn finish(&self) -> Option<Signature> {
        let (r, strokes) = crate::doc::ink_from_points(&self.strokes, 2.0);
        if self.strokes.is_empty() || !r.is_finite() {
            return None;
        }
        let size: Vec2 = r.size().max(vec2(1.0, 1.0));
        Some(Signature { aspect: size.x / size.y, strokes: Arc::new(strokes) })
    }
}
