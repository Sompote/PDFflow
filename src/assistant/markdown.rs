//! A small Markdown renderer for the chat replies: headings, lines with **bold**, *italic*
//! and `code`, bullet and numbered lists, tables, code blocks, quotes and rules. Lines stay
//! lines (a reply's single line breaks are kept), as the plain text showed them before.

use eframe::egui::{self, text::LayoutJob, Color32, FontFamily, FontId, TextFormat};

use super::chat::Tokens;

/// Shows `text` as Markdown in the theme's colours.
pub fn show(ui: &mut egui::Ui, text: &str, t: &Tokens) {
    let size = ui.style().text_styles.get(&egui::TextStyle::Body).map_or(13.0, |f| f.size);
    let lines: Vec<&str> = text.lines().collect();
    let mut i = 0;
    let mut first = true;
    while i < lines.len() {
        let line = lines[i];
        let trimmed = line.trim();
        // code block
        if trimmed.starts_with("```") {
            let mut code = vec![];
            i += 1;
            while i < lines.len() && !lines[i].trim_start().starts_with("```") {
                code.push(lines[i]);
                i += 1;
            }
            i += 1;
            egui::Frame::new().fill(t.field).stroke(egui::Stroke::new(1.0, t.field_border)).corner_radius(6.0).inner_margin(egui::Margin::same(6)).show(ui, |ui| {
                ui.add(egui::Label::new(egui::RichText::new(code.join("\n")).monospace().size(size - 1.0).color(t.text)).wrap());
            });
            first = false;
            continue;
        }
        // table: consecutive lines that start with '|'
        if trimmed.starts_with('|') && trimmed.matches('|').count() >= 2 {
            let mut rows = vec![];
            while i < lines.len() && lines[i].trim().starts_with('|') {
                rows.push(lines[i].trim());
                i += 1;
            }
            table(ui, &rows, size, t, i);
            first = false;
            continue;
        }
        i += 1;
        if trimmed.is_empty() {
            if !first {
                ui.add_space(size * 0.4);
            }
            continue;
        }
        first = false;
        if matches!(trimmed, "---" | "***" | "___") {
            ui.separator();
            continue;
        }
        let hashes = trimmed.chars().take_while(|&c| c == '#').count();
        if (1..=6).contains(&hashes) && trimmed[hashes..].starts_with(' ') {
            let grow = match hashes {
                1 => 5.0,
                2 => 3.0,
                _ => 1.5,
            };
            ui.add_space(size * 0.3);
            ui.add(egui::Label::new(job(&trimmed[hashes + 1..], size + grow, t.text, true, t)).wrap());
            ui.add_space(size * 0.15);
            continue;
        }
        if let Some(q) = trimmed.strip_prefix("> ").or_else(|| (trimmed == ">").then_some("")) {
            ui.horizontal_top(|ui| {
                let (r, _) = ui.allocate_exact_size(egui::vec2(3.0, size * 1.4), egui::Sense::hover());
                ui.painter().rect_filled(r, 1.0, t.accent);
                ui.add(egui::Label::new(job(q, size, t.text_dim, false, t)).wrap());
            });
            continue;
        }
        // lists, indented by the leading spaces
        let indent = (line.len() - line.trim_start().len()) as f32 / 2.0 * size;
        let bullet = ["- ", "* ", "+ "].iter().find_map(|b| trimmed.strip_prefix(b)).map(|rest| ("•".to_string(), rest));
        let numbered = trimmed.split_once(". ").filter(|(n, _)| !n.is_empty() && n.len() <= 3 && n.chars().all(|c| c.is_ascii_digit())).map(|(n, rest)| (format!("{n}."), rest));
        if let Some((mark, rest)) = bullet.or(numbered) {
            ui.horizontal_top(|ui| {
                ui.spacing_mut().item_spacing.x = 4.0;
                ui.add_space(indent + 2.0);
                ui.add_sized([size * 1.2, size * 1.3], egui::Label::new(egui::RichText::new(mark).size(size).color(t.text_dim)));
                ui.add(egui::Label::new(job(rest, size, t.text, false, t)).wrap());
            });
            continue;
        }
        ui.add(egui::Label::new(job(trimmed, size, t.text, false, t)).wrap());
    }
}

/// A table: the first row is the header, separator rows (`|---|:--:|`) are skipped.
fn table(ui: &mut egui::Ui, rows: &[&str], size: f32, t: &Tokens, id: usize) {
    let cells = |r: &str| -> Vec<String> { split_row(r) };
    let sep = |r: &str| cells(r).iter().all(|c| !c.is_empty() && c.chars().all(|ch| matches!(ch, '-' | ':' | ' ')));
    let rows: Vec<Vec<String>> = rows.iter().filter(|r| !sep(r)).map(|r| cells(r)).collect();
    let cols = rows.iter().map(Vec::len).max().unwrap_or(0);
    if cols == 0 {
        return;
    }
    // right-align a column whose body cells are all numbers
    let numeric: Vec<bool> = (0..cols).map(|c| rows.len() > 1 && rows[1..].iter().all(|r| r.get(c).is_none_or(|s| s.is_empty() || is_number(s)))).collect();
    let width = ((ui.available_width() - 8.0) / cols as f32).max(48.0);
    ui.add_space(size * 0.2);
    egui::Frame::new().stroke(egui::Stroke::new(1.0, t.field_border)).corner_radius(6.0).inner_margin(egui::Margin::same(4)).show(ui, |ui| {
        egui::ScrollArea::horizontal().id_salt(("md_table", id)).show(ui, |ui| {
            egui::Grid::new(("md_grid", id)).striped(true).spacing([10.0, 4.0]).min_col_width(32.0).show(ui, |ui| {
                for (k, row) in rows.iter().enumerate() {
                    for c in 0..cols {
                        let s = row.get(c).map(String::as_str).unwrap_or("");
                        let layout = if numeric[c] && k > 0 { egui::Layout::right_to_left(egui::Align::Center) } else { egui::Layout::left_to_right(egui::Align::Center) };
                        ui.allocate_ui_with_layout(egui::vec2(width, size * 1.4), layout, |ui| {
                            ui.set_max_width(width);
                            ui.add(egui::Label::new(job(s, size - 0.5, if k == 0 { t.accent_text } else { t.text }, k == 0, t)).wrap());
                        });
                    }
                    ui.end_row();
                }
            });
        });
    });
    ui.add_space(size * 0.2);
}

/// The cells of a table row: split at the pipes that are not escaped (`\|` is a pipe in a
/// cell, as in `\|u\|`), without the outer ones.
fn split_row(r: &str) -> Vec<String> {
    let r = r.trim();
    let r = r.strip_prefix('|').unwrap_or(r);
    let r = if r.ends_with('|') && !r.ends_with("\\|") { &r[..r.len() - 1] } else { r };
    let mut cells = vec![String::new()];
    let mut chars = r.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\\' if chars.peek() == Some(&'|') => {
                // keep the escape for the inline pass, which turns it into a plain pipe
                cells.last_mut().unwrap().push_str("\\|");
                chars.next();
            }
            '|' => cells.push(String::new()),
            _ => cells.last_mut().unwrap().push(c),
        }
    }
    cells.into_iter().map(|c| c.trim().to_string()).collect()
}

fn is_number(s: &str) -> bool {
    let s = s.trim().trim_end_matches(['%', ' ']).replace([',', '−'], "");
    let s = s.split_whitespace().next().unwrap_or("");
    !s.is_empty() && s.trim_start_matches(['-', '+', '≈', '~']).parse::<f64>().is_ok()
}

/// One line with inline **bold**, *italic* and `code` (underscores are left alone: names such
/// as Borehole_1 use them).
fn job(s: &str, size: f32, color: Color32, strong: bool, t: &Tokens) -> LayoutJob {
    let mut job = LayoutJob::default();
    let (mut bold, mut italic, mut code) = (strong, false, false);
    let mut buf = String::new();
    let flush = |job: &mut LayoutJob, buf: &mut String, bold: bool, italic: bool, code: bool| {
        if buf.is_empty() {
            return;
        }
        let font = if code {
            FontId::new(size - 1.0, FontFamily::Monospace)
        } else if bold {
            FontId::new(size, crate::theme::doc_family(true))
        } else {
            FontId::new(size, FontFamily::Proportional)
        };
        let mut f = TextFormat::simple(font, color);
        if bold && color == t.text {
            f.color = if t.dark { Color32::WHITE } else { Color32::BLACK };
        }
        f.italics = italic;
        if code {
            f.background = t.field;
        }
        job.append(buf, 0.0, f);
        buf.clear();
    };
    let chars: Vec<char> = s.chars().collect();
    let mut k = 0;
    while k < chars.len() {
        let c = chars[k];
        // a backslash escapes Markdown punctuation: \| \* \` \_ \# and the like
        if c == '\\' && !code && chars.get(k + 1).is_some_and(|n| n.is_ascii_punctuation()) {
            buf.push(chars[k + 1]);
            k += 2;
            continue;
        }
        if c == '`' {
            flush(&mut job, &mut buf, bold, italic, code);
            code = !code;
        } else if !code && c == '*' && chars.get(k + 1) == Some(&'*') {
            flush(&mut job, &mut buf, bold, italic, code);
            bold = strong || !bold;
            k += 1;
        } else if !code && c == '*' && chars.get(k + 1).is_some_and(|n| !n.is_whitespace()) || !code && c == '*' && italic {
            flush(&mut job, &mut buf, bold, italic, code);
            italic = !italic;
        } else {
            buf.push(c);
        }
        k += 1;
    }
    flush(&mut job, &mut buf, bold, italic, code);
    job.wrap.max_width = f32::INFINITY;
    job
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inline_and_numbers() {
        let t = Tokens::get(&egui::Context::default());
        let _ = &t;
        let j = job("Settlement **12.4 mm** at `Phase_1`, *drained*", 13.0, Color32::GRAY, false, &t);
        let text: String = j.text.clone();
        assert_eq!(text, "Settlement 12.4 mm at Phase_1, drained");
        assert_eq!(j.sections.len(), 6);
        assert!(j.sections[3].format.font_id.family == FontFamily::Monospace);
        assert!(j.sections[5].format.italics);
        // a lone star (a multiplication) stays
        assert_eq!(job("2 * 3", 13.0, Color32::GRAY, false, &t).text, "2 * 3");
        assert!(is_number("-12.4") && is_number("1,530") && is_number("8.9 mm") && !is_number("Sand"));
        // escaped pipes stay in their cell and show as pipes
        assert_eq!(split_row("| Drawdown | plastic | max \\|u\\| = 19.5 mm |"), ["Drawdown", "plastic", "max \\|u\\| = 19.5 mm"]);
        assert_eq!(job("max \\|u\\| = 19.5 mm, 2 \\* 3", 13.0, Color32::GRAY, false, &t).text, "max |u| = 19.5 mm, 2 * 3");
    }
}
