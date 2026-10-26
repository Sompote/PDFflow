//! Digital ID management, signing and the signature panel.

use super::*;
use crate::dsig::{self, SigReport};
use crate::sig::IdEntry;

pub(crate) struct SignDialog {
    pub id: usize,
    pub password: String,
    pub reason: String,
    pub location: String,
    pub visible: bool,
    /// A box drawn on a page for the signature (page, rect in page points).
    pub place: Option<(usize, Rect)>,
    pub image: ImageChoice,
    pub show_name: bool,
    pub show_date: bool,
    pub show_reason: bool,
    pub error: Option<String>,
}

/// What picture the visible signature shows.
#[derive(Clone, PartialEq)]
pub(crate) enum ImageChoice {
    None,
    /// The selected drawn signature or picture (it moves into the signature box).
    Selected,
    /// A saved drawn signature from the Signatures tab.
    Saved(usize),
    /// A picture file of a handwritten signature.
    Picture(Arc<Vec<u8>>, String),
}

/// A trusted root being confirmed before it is added.
pub(super) struct TrustDialog {
    pub der: Vec<u8>,
    pub subject: String,
    pub issuer: String,
    pub expires: String,
    pub fingerprint: String,
}

#[derive(Default)]
pub(super) struct CreateDialog {
    pub name: String,
    pub org: String,
    pub email: String,
    pub pw: String,
    pub pw2: String,
    pub error: Option<String>,
}

pub(super) struct ImportDialog {
    pub path: PathBuf,
    pub bytes: Vec<u8>,
    pub password: String,
    pub error: Option<String>,
}

fn field(ui: &mut Ui, label: &str, value: &mut String, password: bool) {
    ui.horizontal(|ui| {
        ui.add_sized([110.0, 20.0], egui::Label::new(label));
        ui.add(egui::TextEdit::singleline(value).password(password).desired_width(260.0));
    });
}

fn error_line(ui: &mut Ui, e: &Option<String>) {
    if let Some(e) = e {
        let t = Tokens::get(ui.ctx());
        ui.add_space(4.0);
        ui.label(egui::RichText::new(e).color(t.danger));
    }
}

impl App {
    /// The sign dialog (File › Sign with Digital ID…).
    pub(super) fn open_sign_dialog(&mut self) {
        self.end_edit();
        if self.ids.is_empty() {
            self.create_dlg = Some(CreateDialog::default());
            self.status = "Create or import a Digital ID first.".into();
            return;
        }
        let visible = self.selected().is_some();
        let sel_is_image = self.selected().is_some_and(|o| matches!(o.kind, Kind::Ink { highlighter: false, .. } | Kind::Image(_)));
        let image = if sel_is_image {
            ImageChoice::Selected
        } else if !self.sigs.is_empty() {
            ImageChoice::Saved(self.sig_sel.min(self.sigs.len() - 1))
        } else {
            ImageChoice::None
        };
        self.sign_dlg = Some(SignDialog {
            id: self.id_sel.min(self.ids.len() - 1),
            password: String::new(),
            reason: "I approve this document".into(),
            location: String::new(),
            visible,
            place: None,
            image,
            show_name: true,
            show_date: true,
            show_reason: false,
            error: None,
        });
    }

    /// The signed file for the current document, and the signer's name.
    pub(crate) fn sign_document(&mut self, d: &SignDialog) -> Result<(Vec<u8>, String), String> {
        let entry = self.ids.get(d.id).ok_or("Choose a Digital ID.")?;
        let p12 = std::fs::read(&entry.file).map_err(|e| e.to_string())?;
        let id = dsig::load_p12(&p12, &d.password)?;
        if let Some(cert) = id.chain.first() {
            if dsig::seconds_left(cert) < 0 {
                return Err(format!(
                    "This Digital ID expired on {}. Download a new one from your certificate authority (for KMUTT: mysign.kmutt.ac.th) and import it.",
                    dsig::expiry_date(cert)
                ));
            }
        }
        // Where the signature box goes: a drawn box, else the selected object, else invisible.
        let selected = self.sel.filter(|_| d.visible);
        let (page, rect, extra) = match (d.place, selected) {
            (Some((p, r)), _) => (p, Some([r.min.x, r.min.y, r.max.x, r.max.y]), self.doc.pages[p].rotation),
            (None, Some((p, i))) => {
                let r = self.doc.pages[p].objects[i].rect;
                (p, Some([r.min.x, r.min.y, r.max.x, r.max.y]), self.doc.pages[p].rotation)
            }
            _ => (self.current.min(self.doc.pages.len() - 1), None, 0),
        };
        let mut appearance = dsig::Appearance { show_name: d.show_name, show_date: d.show_date, show_reason: d.show_reason, ..Default::default() };
        // The selected signature drawing or picture moves into the signature box, so it is not
        // also painted on the page.
        let mut lifted = None;
        match &d.image {
            ImageChoice::Selected => {
                if let Some((p, i)) = self.sel {
                    let o = &self.doc.pages[p].objects[i];
                    match &o.kind {
                        Kind::Ink { strokes, color, highlighter: false, .. } => {
                            appearance.strokes = Some((strokes.clone(), o.rect.width() / o.rect.height().max(1e-3), [color.r(), color.g(), color.b()]));
                            lifted = Some((p, i));
                        }
                        Kind::Image(img) => {
                            appearance.picture = Some(match &img.raster {
                                None => img.bytes.clone(),
                                Some(r) => {
                                    let mut png = Vec::new();
                                    image::RgbaImage::from_raw(r.w, r.h, r.to_rgba())
                                        .ok_or("bad picture")?
                                        .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
                                        .map_err(|e| e.to_string())?;
                                    Arc::new(png)
                                }
                            });
                            lifted = Some((p, i));
                        }
                        _ => {}
                    }
                }
            }
            ImageChoice::Saved(i) => {
                if let Some(sg) = self.sigs.get(*i) {
                    let c = self.opts.sig_color;
                    appearance.strokes = Some((sg.strokes.clone(), sg.aspect, [c.r(), c.g(), c.b()]));
                }
            }
            ImageChoice::Picture(bytes, _) => appearance.picture = Some(bytes.clone()),
            ImageChoice::None => {}
        }
        if rect.is_none() {
            lifted = None;
        }
        // Untouched documents are signed on their original bytes, which keeps earlier signatures.
        let base = match (&self.doc.source, self.doc.is_pristine(), lifted) {
            (Some(src), true, None) => src.as_ref().clone(),
            (_, _, Some((p, i))) => {
                let o = self.doc.pages[p].objects.remove(i);
                let r = export::save(&self.doc);
                self.doc.pages[p].objects.insert(i, o);
                r?
            }
            _ => export::save(&self.doc)?,
        };
        let opts = dsig::SignOptions { page, rect, extra_rotation: extra, reason: d.reason.clone(), location: d.location.clone(), appearance };
        Ok((dsig::sign_pdf(&base, &id, &opts)?, id.name))
    }

    /// Sign and save; returns an error message to show in the dialog.
    fn do_sign(&mut self, d: &SignDialog) -> Result<(), String> {
        let (signed, name) = self.sign_document(d)?;
        let stem = self.doc.title().trim_end_matches(".pdf").to_owned();
        let Some(mut path) = rfd::FileDialog::new().add_filter("PDF", &["pdf"]).set_file_name(format!("{stem}-signed.pdf")).save_file() else {
            return Err("Not saved.".into());
        };
        if path.extension().is_none() {
            path.set_extension("pdf");
        }
        std::fs::write(&path, &signed).map_err(|e| e.to_string())?;
        // Reopen the signed file so its signatures are listed and further saves know about them.
        self.doc.dirty = false;
        self.open_path(path.clone());
        self.status = format!("Signed by {name} and saved to {}.", path.display());
        Ok(())
    }

    pub(super) fn create_id(&mut self, d: &CreateDialog) -> Result<(), String> {
        if d.name.trim().is_empty() {
            return Err("Enter your name.".into());
        }
        if d.pw.chars().count() < 6 {
            return Err("Use a password of at least 6 characters.".into());
        }
        if d.pw != d.pw2 {
            return Err("The passwords do not match.".into());
        }
        let p12 = dsig::create_self_signed(d.name.trim(), d.org.trim(), d.email.trim(), &d.pw)?;
        let expires = dsig::load_p12(&p12, &d.pw).ok().and_then(|i| i.chain.first().map(dsig::expiry_date)).unwrap_or_default();
        let file = crate::sig::store_id_file(d.name.trim(), &p12)?;
        let detail = [d.org.trim(), d.email.trim()].iter().filter(|s| !s.is_empty()).cloned().collect::<Vec<_>>().join(" · ");
        self.ids.push(IdEntry { file, name: d.name.trim().to_owned(), detail: format!("Self-signed · {detail}").trim_end_matches(" · ").to_owned(), expires });
        self.id_sel = self.ids.len() - 1;
        crate::sig::save_ids(&self.ids);
        self.status = "Digital ID created. Keep its password safe: it cannot be recovered.".into();
        Ok(())
    }

    pub(super) fn import_id(&mut self, d: &ImportDialog) -> Result<(), String> {
        let id = dsig::load_p12(&d.bytes, &d.password)?;
        let cert = id.chain.first().ok_or("no certificate")?;
        let issuer = dsig::common_name_of(&cert.tbs_certificate.issuer.to_string());
        let file = crate::sig::store_id_file(&id.name, &d.bytes)?;
        let detail = if cert.tbs_certificate.issuer == cert.tbs_certificate.subject { "Self-signed".to_owned() } else { format!("Issued by {issuer}") };
        self.ids.push(IdEntry { file, name: id.name.clone(), detail, expires: dsig::expiry_date(cert) });
        self.id_sel = self.ids.len() - 1;
        crate::sig::save_ids(&self.ids);
        self.status = format!("Imported the Digital ID of {} from {}.", id.name, d.path.display());
        Ok(())
    }

    /// Pick a root certificate (.crt / .cer) to trust, e.g. KMUTT's CA.crt from mysign.
    pub(super) fn pick_trust_root(&mut self) {
        let Some(path) = rfd::FileDialog::new().add_filter("Certificate", &["crt", "cer", "pem", "der"]).pick_file() else { return };
        let bytes = match std::fs::read(&path) {
            Ok(b) => b,
            Err(e) => return self.error = Some(e.to_string()),
        };
        match dsig::parse_cert_file(&bytes) {
            Ok(cert) => {
                use der::Encode;
                let der = cert.to_der().unwrap_or_default();
                let fp: String = <sha2::Sha256 as sha2::Digest>::digest(&der).iter().map(|b| format!("{b:02X}")).collect::<Vec<_>>().chunks(2).map(|c| c.concat()).collect::<Vec<_>>().join(" ");
                self.trust_dlg = Some(TrustDialog {
                    subject: dsig::common_name(&cert),
                    issuer: dsig::common_name_of(&cert.tbs_certificate.issuer.to_string()),
                    expires: dsig::expiry_date(&cert),
                    fingerprint: fp,
                    der,
                });
            }
            Err(e) => self.error = Some(e),
        }
    }

    /// Re-check the open document's signatures (after the trusted roots change).
    pub(super) fn reverify(&mut self) {
        if let Some(src) = &self.doc.source {
            if !self.sig_reports.is_empty() {
                self.sig_reports = dsig::verify(src, &self.trusted_certs());
            }
        }
    }

    pub(super) fn pick_import_id(&mut self) {
        if let Some(path) = rfd::FileDialog::new().add_filter("Digital ID", &["p12", "pfx"]).pick_file() {
            match std::fs::read(&path) {
                Ok(bytes) => self.import_dlg = Some(ImportDialog { path, bytes, password: String::new(), error: None }),
                Err(e) => self.error = Some(e.to_string()),
            }
        }
    }

    /// The "Digital IDs" tab of the Signatures card.
    pub(super) fn ids_panel(&mut self, ui: &mut Ui) {
        let t = Tokens::get(ui.ctx());
        let list_h = widgets::list_height(ui);
        let mut remove = None;
        egui::ScrollArea::vertical().id_salt(self.wid("ids")).max_height(list_h).auto_shrink([false, false]).show(ui, |ui| {
            if !self.sig_reports.is_empty() {
                let ok = self.sig_reports.iter().all(SigReport::valid);
                let (icon, text, c) = if ok {
                    ("check", format!("{} valid signature(s) in this PDF", self.sig_reports.len()), Color32::from_rgb(90, 200, 120))
                } else {
                    ("x", "A signature in this PDF has problems".to_owned(), t.danger)
                };
                let r = ui.horizontal(|ui| {
                    let (rr, _) = ui.allocate_exact_size(vec2(16.0, 16.0), Sense::hover());
                    icons::paint(ui.painter(), rr, icon, c);
                    ui.add(egui::Label::new(egui::RichText::new(text).color(c)).sense(Sense::click()))
                });
                if r.inner.on_hover_text("Open the signature panel").clicked() {
                    self.sig_panel = true;
                }
                ui.add_space(4.0);
            }
            if widgets::primary_button(ui, "Sign Document…", ui.available_width()).clicked() {
                self.open_sign_dialog();
            }
            ui.add_space(6.0);
            if self.ids.is_empty() {
                ui.label(egui::RichText::new("No Digital ID yet. + creates one; the folder button imports a .p12 / .pfx from a certificate authority.").color(t.text_faint));
            }
            for (i, e) in self.ids.iter().enumerate() {
                let (row, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 38.0), Sense::click());
                if i == self.id_sel {
                    ui.painter().rect_filled(row, t.radius_sm, t.row_selected);
                } else if resp.hovered() {
                    ui.painter().rect_filled(row, t.radius_sm, t.hover);
                }
                icons::paint(ui.painter(), Rect::from_center_size(pos2(row.left() + 14.0, row.center().y), vec2(16.0, 16.0)), "signature", t.icon);
                ui.painter().text(pos2(row.left() + 30.0, row.top() + 11.0), egui::Align2::LEFT_CENTER, &e.name, FontId::proportional(12.5), t.text);
                let expired = !e.expires.is_empty() && e.expires.as_str() < today().as_str();
                let detail = match (e.expires.is_empty(), expired) {
                    (true, _) => e.detail.clone(),
                    (false, false) => format!("{} · until {}", e.detail, e.expires),
                    (false, true) => format!("Expired {} · renew it", e.expires),
                };
                ui.painter().text(pos2(row.left() + 30.0, row.top() + 27.0), egui::Align2::LEFT_CENTER, detail, FontId::proportional(10.5), if expired { t.danger } else { t.text_faint });
                if resp.clicked() {
                    self.id_sel = i;
                }
                resp.context_menu(|ui| {
                    if ui.button("Delete Digital ID").clicked() {
                        remove = Some(i);
                        ui.close();
                    }
                });
            }
            ui.add_space(8.0);
            ui.label(egui::RichText::new("Trusted root certificates").color(t.text_dim));
            if self.trusted.is_empty() {
                ui.label(egui::RichText::new("None. Trust your certificate authority's root (✓ below) so its signatures show as verified.").color(t.text_faint).size(11.0));
            }
            let mut untrust = None;
            for (k, (path, cert)) in self.trusted.iter().enumerate() {
                let r = ui.label(egui::RichText::new(format!("✓ {} · until {}", dsig::common_name(cert), dsig::expiry_date(cert))).size(11.5));
                r.context_menu(|ui| {
                    if ui.button("Stop trusting").clicked() {
                        untrust = Some((k, path.clone()));
                        ui.close();
                    }
                });
            }
            if let Some((k, path)) = untrust {
                let _ = std::fs::remove_file(path);
                self.trusted.remove(k);
                self.reverify();
            }
        });
        if let Some(i) = remove {
            self.delete_id = Some(i);
        }
        widgets::panel_footer(ui, |ui| {
            if icons::small(ui, "check", "Trust a root certificate (.crt / .cer), e.g. your university's CA").clicked() {
                self.pick_trust_root();
            }
            if icons::small(ui, "trash", "Delete the selected Digital ID").clicked() && !self.ids.is_empty() {
                self.delete_id = Some(self.id_sel.min(self.ids.len() - 1));
            }
            if icons::small(ui, "file-down", "Export the selected Digital ID (.p12) as a backup").clicked() {
                if let Some(e) = self.ids.get(self.id_sel) {
                    if let Some(to) = rfd::FileDialog::new().add_filter("Digital ID", &["p12"]).set_file_name(format!("{}.p12", e.name)).save_file() {
                        if let Err(err) = std::fs::copy(&e.file, &to) {
                            self.error = Some(err.to_string());
                        }
                    }
                }
            }
            if icons::small(ui, "folder-open", "Import a Digital ID (.p12 / .pfx)").clicked() {
                self.pick_import_id();
            }
            if icons::small(ui, "plus", "Create a self-signed Digital ID").clicked() {
                self.create_dlg = Some(CreateDialog::default());
            }
        });
    }

    /// Green/amber strip over the canvas when the open PDF carries signatures.
    pub(super) fn sig_banner(&mut self, ui: &mut Ui) {
        if self.sig_reports.is_empty() {
            return;
        }
        let t = Tokens::get(ui.ctx());
        let ok = self.sig_reports.iter().all(SigReport::valid);
        let later = self.sig_reports.iter().all(|r| !r.whole_file);
        let trusted = self.sig_reports.iter().all(|r| r.trusted);
        let (fill, icon, text) = if ok && !trusted && !later {
            (Color32::from_rgb(110, 90, 30), "check", "Signatures are intact, but a signer's identity is not verified (the certificate does not chain to a trusted root).".to_owned())
        } else if !ok {
            (Color32::from_rgb(120, 52, 40), "x", "At least one signature is invalid: the document was altered after signing, or the signature is damaged.".to_owned())
        } else if later {
            (Color32::from_rgb(110, 90, 30), "check", "Signatures are valid, but changes were added after the last signature.".to_owned())
        } else {
            let who: Vec<&str> = self.sig_reports.iter().map(|r| r.signer.as_str()).collect();
            (Color32::from_rgb(36, 92, 58), "check", format!("Signed and all signatures are valid · {}", who.join(", ")))
        };
        let edited = !self.doc.is_pristine();
        egui::Frame::NONE.fill(fill).inner_margin(egui::Margin::symmetric(10, 5)).show(ui, |ui| {
            ui.horizontal(|ui| {
                let (r, _) = ui.allocate_exact_size(vec2(16.0, 16.0), Sense::hover());
                icons::paint(ui.painter(), r, icon, Color32::WHITE);
                ui.label(egui::RichText::new(text).color(Color32::WHITE));
                if edited {
                    ui.label(egui::RichText::new("· unsaved edits will remove these signatures when saved").color(Color32::from_rgb(255, 220, 160)));
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if widgets::secondary_button(ui, "Signature Panel", 0.0).clicked() {
                        self.sig_panel = true;
                    }
                });
            });
        });
        let _ = t;
    }

    pub(super) fn sign_dialogs(&mut self, ctx: &egui::Context) {
        let t = Tokens::get(ctx);
        if let Some(mut d) = self.sign_dlg.take() {
            let mut close = false;
            let mut go = false;
            let mut draw_box = false;
            let mut pick_picture = false;
            let can_visible = self.selected().is_some();
            egui::Modal::new(Id::new("sign-dlg")).show(ctx, |ui| {
                ui.set_width(440.0);
                ui.heading("Sign with Digital ID");
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    ui.add_sized([110.0, 20.0], egui::Label::new("Digital ID"));
                    egui::ComboBox::from_id_salt("id-pick").width(260.0).selected_text(self.ids.get(d.id).map(|e| e.name.clone()).unwrap_or_default()).show_ui(ui, |ui| {
                        for (i, e) in self.ids.iter().enumerate() {
                            ui.selectable_value(&mut d.id, i, format!("{}  ({})", e.name, e.detail));
                        }
                    });
                });
                field(ui, "Password", &mut d.password, true);
                field(ui, "Reason", &mut d.reason, false);
                field(ui, "Location", &mut d.location, false);
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    ui.add_sized([110.0, 20.0], egui::Label::new("Place"));
                    let where_ = match (d.place, can_visible && d.visible) {
                        (Some((p, _)), _) => format!("Box on page {}", p + 1),
                        (None, true) => "On the selected object".to_owned(),
                        _ => "Invisible (listed in the signature panel)".to_owned(),
                    };
                    ui.label(where_);
                });
                ui.horizontal(|ui| {
                    ui.add_space(116.0);
                    if widgets::secondary_button(ui, "Draw Box on Page…", 0.0).clicked() {
                        draw_box = true;
                    }
                    if d.place.is_some() && widgets::secondary_button(ui, "Remove Box", 0.0).clicked() {
                        d.place = None;
                    }
                    if d.place.is_none() && can_visible {
                        widgets::checkbox(ui, &mut d.visible, "Use selection");
                    }
                });
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    ui.add_sized([110.0, 20.0], egui::Label::new("Signature image"));
                    let label = |c: &ImageChoice| match c {
                        ImageChoice::None => "None (text only)".to_owned(),
                        ImageChoice::Selected => "The selected drawing / picture".to_owned(),
                        ImageChoice::Saved(i) => format!("Saved signature {}", i + 1),
                        ImageChoice::Picture(_, n) => n.clone(),
                    };
                    egui::ComboBox::from_id_salt("sig-img").width(260.0).selected_text(label(&d.image)).show_ui(ui, |ui| {
                        ui.selectable_value(&mut d.image, ImageChoice::None, label(&ImageChoice::None));
                        if can_visible {
                            ui.selectable_value(&mut d.image, ImageChoice::Selected, label(&ImageChoice::Selected));
                        }
                        for i in 0..self.sigs.len() {
                            ui.selectable_value(&mut d.image, ImageChoice::Saved(i), label(&ImageChoice::Saved(i)));
                        }
                        if ui.button("Picture file…").clicked() {
                            pick_picture = true;
                        }
                    });
                });
                ui.horizontal(|ui| {
                    ui.add_space(116.0);
                    widgets::checkbox(ui, &mut d.show_name, "Name");
                    widgets::checkbox(ui, &mut d.show_date, "Date");
                    widgets::checkbox(ui, &mut d.show_reason, "Reason");
                });
                if let Some(e) = self.ids.get(d.id).filter(|e| !e.expires.is_empty()) {
                    let expired = e.expires.as_str() < today().as_str();
                    let c = if expired { t.danger } else { t.text_faint };
                    ui.label(egui::RichText::new(if expired { format!("This Digital ID expired on {}; renew it (KMUTT: mysign.kmutt.ac.th).", e.expires) } else { format!("Digital ID valid until {}.", e.expires) }).color(c));
                }
                if !self.doc.is_pristine() && !self.sig_reports.is_empty() {
                    ui.label(egui::RichText::new("This PDF has been edited, so its existing signatures will not survive. Sign the unedited file to keep them.").color(t.danger));
                }
                error_line(ui, &d.error);
                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if widgets::primary_button(ui, "Sign and Save As…", 140.0).clicked() {
                            go = true;
                        }
                        if widgets::secondary_button(ui, "Cancel", 80.0).clicked() {
                            close = true;
                        }
                    });
                });
            });
            if pick_picture {
                if let Some(p) = rfd::FileDialog::new().add_filter("Picture", &["png", "jpg", "jpeg"]).pick_file() {
                    if let Ok(b) = std::fs::read(&p) {
                        d.image = ImageChoice::Picture(Arc::new(b), p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default());
                    }
                }
            }
            if draw_box {
                self.status = "Drag a box where the signature should appear (Esc cancels).".into();
                self.sign_placing = Some(d);
                return;
            }
            if go || ctx.input(|i| i.key_pressed(Key::Enter)) {
                d.visible &= can_visible;
                match self.do_sign(&d) {
                    Ok(()) => close = true,
                    Err(e) => d.error = Some(e),
                }
            }
            if !close && !ctx.input(|i| i.key_pressed(Key::Escape)) {
                self.sign_dlg = Some(d);
            }
        }

        if let Some(mut d) = self.create_dlg.take() {
            let mut close = false;
            let mut go = false;
            egui::Modal::new(Id::new("create-id")).show(ctx, |ui| {
                ui.set_width(420.0);
                ui.heading("Create a Digital ID");
                ui.add_space(4.0);
                ui.label(egui::RichText::new("A self-signed ID (RSA-2048, valid for five years). It proves the document was not changed after you signed, but readers cannot confirm who you are unless they trust your certificate; for that, import an ID from a certificate authority.").color(t.text_faint));
                ui.add_space(8.0);
                field(ui, "Name", &mut d.name, false);
                field(ui, "Organisation", &mut d.org, false);
                field(ui, "E-mail", &mut d.email, false);
                field(ui, "Password", &mut d.pw, true);
                field(ui, "Confirm", &mut d.pw2, true);
                error_line(ui, &d.error);
                ui.add_space(10.0);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if widgets::primary_button(ui, "Create", 90.0).clicked() {
                        go = true;
                    }
                    if widgets::secondary_button(ui, "Cancel", 80.0).clicked() {
                        close = true;
                    }
                });
            });
            if go {
                match self.create_id(&d) {
                    Ok(()) => close = true,
                    Err(e) => d.error = Some(e),
                }
            }
            if !close && !ctx.input(|i| i.key_pressed(Key::Escape)) {
                self.create_dlg = Some(d);
            }
        }

        if let Some(mut d) = self.import_dlg.take() {
            let mut close = false;
            let mut go = false;
            egui::Modal::new(Id::new("import-id")).show(ctx, |ui| {
                ui.set_width(420.0);
                ui.heading("Import Digital ID");
                ui.add_space(4.0);
                ui.label(egui::RichText::new(d.path.display().to_string()).color(t.text_faint));
                ui.add_space(8.0);
                field(ui, "Password", &mut d.password, true);
                error_line(ui, &d.error);
                ui.add_space(10.0);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if widgets::primary_button(ui, "Import", 90.0).clicked() {
                        go = true;
                    }
                    if widgets::secondary_button(ui, "Cancel", 80.0).clicked() {
                        close = true;
                    }
                });
            });
            if go || ctx.input(|i| i.key_pressed(Key::Enter)) {
                match self.import_id(&d) {
                    Ok(()) => close = true,
                    Err(e) => d.error = Some(e),
                }
            }
            if !close && !ctx.input(|i| i.key_pressed(Key::Escape)) {
                self.import_dlg = Some(d);
            }
        }

        if let Some(i) = self.delete_id {
            let name = self.ids.get(i).map(|e| e.name.clone()).unwrap_or_default();
            let mut choice = None;
            egui::Modal::new(Id::new("del-id")).show(ctx, |ui| {
                ui.set_width(380.0);
                ui.heading(format!("Delete the Digital ID “{name}”?"));
                ui.add_space(6.0);
                ui.label("This deletes its private key from PDFflow's settings. Export a backup first if you may need it again; documents already signed stay valid.");
                ui.add_space(12.0);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if widgets::primary_button(ui, "Delete", 80.0).clicked() {
                        choice = Some(true);
                    }
                    if widgets::secondary_button(ui, "Cancel", 80.0).clicked() {
                        choice = Some(false);
                    }
                });
            });
            match choice {
                Some(true) => {
                    if i < self.ids.len() {
                        let e = self.ids.remove(i);
                        let _ = std::fs::remove_file(&e.file);
                        crate::sig::save_ids(&self.ids);
                        self.id_sel = self.id_sel.min(self.ids.len().saturating_sub(1));
                    }
                    self.delete_id = None;
                }
                Some(false) => self.delete_id = None,
                None => {}
            }
        }

        if let Some(save_as) = self.confirm_unsign {
            let mut choice = None;
            let n = self.sig_reports.len();
            egui::Modal::new(Id::new("unsign")).show(ctx, |ui| {
                ui.set_width(420.0);
                ui.heading("Saving removes the digital signatures");
                ui.add_space(6.0);
                ui.label(format!(
                    "This PDF carries {n} digital signature(s). Your edits change the signed content, so the saved file will not contain them any more. Sign the edited file again if it needs a signature."
                ));
                ui.add_space(12.0);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if widgets::primary_button(ui, "Save Without Signatures", 0.0).clicked() {
                        choice = Some(true);
                    }
                    if widgets::secondary_button(ui, "Cancel", 80.0).clicked() {
                        choice = Some(false);
                    }
                });
            });
            match choice {
                Some(true) => {
                    self.confirm_unsign = None;
                    self.unsign_ok = true;
                    self.save(save_as);
                    self.unsign_ok = false;
                }
                Some(false) => self.confirm_unsign = None,
                None => {}
            }
        }

        if let Some(td) = &self.trust_dlg {
            let mut choice = None;
            egui::Modal::new(Id::new("trust")).show(ctx, |ui| {
                ui.set_width(460.0);
                ui.heading("Trust this root certificate?");
                ui.add_space(6.0);
                ui.label(format!("Name: {}", td.subject));
                ui.label(format!("Issued by: {}", td.issuer));
                ui.label(format!("Valid until: {}", td.expires));
                ui.label(egui::RichText::new(format!("SHA-256: {}", td.fingerprint)).monospace().size(10.5));
                ui.add_space(6.0);
                ui.label(egui::RichText::new("Signatures whose certificates chain to this root will be shown as verified. Only trust a certificate you got from its owner (for KMUTT: CA.crt from mysign.kmutt.ac.th).").color(t.text_faint));
                ui.add_space(12.0);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if widgets::primary_button(ui, "Trust", 80.0).clicked() {
                        choice = Some(true);
                    }
                    if widgets::secondary_button(ui, "Cancel", 80.0).clicked() {
                        choice = Some(false);
                    }
                });
            });
            match choice {
                Some(true) => {
                    let td = self.trust_dlg.take().expect("trust dialog");
                    match crate::sig::store_trusted(&td.der, &td.subject) {
                        Ok(path) => {
                            if let Ok(c) = <x509_cert::Certificate as der::Decode>::from_der(&td.der) {
                                self.trusted.push((path, c));
                            }
                            self.reverify();
                            self.status = format!("Now trusting {}.", td.subject);
                        }
                        Err(e) => self.error = Some(e),
                    }
                }
                Some(false) => self.trust_dlg = None,
                None => {}
            }
        }

        if self.sig_panel {
            let mut open = true;
            egui::Window::new("Signature Panel").open(&mut open).default_width(520.0).collapsible(false).pivot(egui::Align2::CENTER_CENTER).default_pos(ctx.content_rect().center()).show(ctx, |ui| {
                if self.sig_reports.is_empty() {
                    ui.label("This PDF has no digital signatures.");
                }
                egui::ScrollArea::vertical().max_height(420.0).show(ui, |ui| {
                    for r in &self.sig_reports {
                        let good = Color32::from_rgb(90, 200, 120);
                        let (icon, c) = if r.valid() { ("check", good) } else { ("x", t.danger) };
                        ui.horizontal(|ui| {
                            let (rr, _) = ui.allocate_exact_size(vec2(18.0, 18.0), Sense::hover());
                            icons::paint(ui.painter(), rr, icon, c);
                            ui.label(egui::RichText::new(format!("{} · signed by {}", r.field, r.signer)).strong().color(t.text));
                        });
                        let line = |ui: &mut Ui, ok: bool, s: &str| {
                            ui.label(egui::RichText::new(format!("    {} {s}", if ok { "✓" } else { "✗" })).color(if ok { t.text_dim } else { t.danger }));
                        };
                        if let Some(p) = &r.problem {
                            line(ui, false, p);
                        } else {
                            line(ui, r.intact, if r.intact { "The signed content has not been modified." } else { "The signed content was modified after signing." });
                            line(ui, r.crypto_ok, if r.crypto_ok { "The signature matches the signer's certificate." } else { "The signature does not match the certificate." });
                            line(
                                ui,
                                true,
                                if r.whole_file { "This signature covers the whole document." } else { "The document has later revisions (further changes or signatures were added after this one)." },
                            );
                        }
                        ui.label(egui::RichText::new(format!("    Signed {}{}", r.when, if r.reason.is_empty() { String::new() } else { format!(" · Reason: {}", r.reason) })).color(t.text_faint));
                        if r.trusted {
                            line(ui, true, &format!("Trusted certificate chain: {}.", r.chain.join(" → ")));
                        } else if r.self_signed {
                            line(ui, false, "Self-signed certificate: the signer's identity cannot be confirmed.");
                        } else {
                            line(ui, false, &format!("Identity not verified: issued by {}, which is not a trusted root here. Trust its root certificate in Digital IDs (for KMUTT: CA.crt from mysign.kmutt.ac.th).", r.issuer));
                        }
                        if !r.expires.is_empty() {
                            ui.label(egui::RichText::new(format!("    Certificate valid until {}", r.expires)).color(t.text_faint));
                        }
                        if !r.cert_current {
                            ui.label(egui::RichText::new("    The certificate is outside its validity period today.").color(t.danger));
                        }
                        ui.add_space(8.0);
                    }
                });
            });
            self.sig_panel = open;
        }
    }
}

/// Today's date, "YYYY-MM-DD" (UTC), for comparing with certificate expiry dates.
fn today() -> String {
    let d = dsig::pdf_date(std::time::SystemTime::now());
    let d = d.trim_start_matches("(D:");
    format!("{}-{}-{}", &d[0..4], &d[4..6], &d[6..8])
}
