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
    pub error: Option<String>,
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
        self.sign_dlg = Some(SignDialog {
            id: self.id_sel.min(self.ids.len() - 1),
            password: String::new(),
            reason: "I approve this document".into(),
            location: String::new(),
            visible,
            error: None,
        });
    }

    /// The signed file for the current document, and the signer's name.
    pub(crate) fn sign_document(&mut self, d: &SignDialog) -> Result<(Vec<u8>, String), String> {
        let entry = self.ids.get(d.id).ok_or("Choose a Digital ID.")?;
        let p12 = std::fs::read(&entry.file).map_err(|e| e.to_string())?;
        let id = dsig::load_p12(&p12, &d.password)?;
        // Untouched documents are signed on their original bytes, which keeps earlier signatures.
        let base = match (&self.doc.source, self.doc.is_pristine()) {
            (Some(src), true) => src.as_ref().clone(),
            _ => export::save(&self.doc)?,
        };
        let (page, rect, extra) = match (d.visible, self.sel) {
            (true, Some((p, i))) => {
                let r = self.doc.pages[p].objects[i].rect;
                (p, Some([r.min.x, r.min.y, r.max.x, r.max.y]), self.doc.pages[p].rotation)
            }
            _ => (self.current.min(self.doc.pages.len() - 1), None, 0),
        };
        let opts = dsig::SignOptions { page, rect, extra_rotation: extra, reason: d.reason.clone(), location: d.location.clone() };
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
        let file = crate::sig::store_id_file(d.name.trim(), &p12)?;
        let detail = [d.org.trim(), d.email.trim()].iter().filter(|s| !s.is_empty()).cloned().collect::<Vec<_>>().join(" · ");
        self.ids.push(IdEntry { file, name: d.name.trim().to_owned(), detail: format!("Self-signed · {detail}").trim_end_matches(" · ").to_owned() });
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
        self.ids.push(IdEntry { file, name: id.name.clone(), detail });
        self.id_sel = self.ids.len() - 1;
        crate::sig::save_ids(&self.ids);
        self.status = format!("Imported the Digital ID of {} from {}.", id.name, d.path.display());
        Ok(())
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
        egui::ScrollArea::vertical().id_salt("ids").max_height(list_h).auto_shrink([false, false]).show(ui, |ui| {
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
                ui.painter().text(pos2(row.left() + 30.0, row.top() + 27.0), egui::Align2::LEFT_CENTER, &e.detail, FontId::proportional(10.5), t.text_faint);
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
        });
        if let Some(i) = remove {
            self.delete_id = Some(i);
        }
        widgets::panel_footer(ui, |ui| {
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
        let (fill, icon, text) = if !ok {
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
                ui.add_space(6.0);
                ui.add_enabled_ui(can_visible, |ui| {
                    widgets::checkbox(ui, &mut d.visible, "Show the signature on the selected object")
                        .on_disabled_hover_text("Select your drawn signature (or any object) first to make the signature visible there.");
                });
                if !can_visible {
                    ui.label(egui::RichText::new("Nothing is selected, so the signature will be invisible (listed in the signature panel).").color(t.text_faint));
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
                        let identity = if r.self_signed {
                            "Self-signed certificate: the signer's identity cannot be confirmed by a certificate authority.".to_owned()
                        } else {
                            format!("Issued by {}. Not checked against trusted root certificates.", r.issuer)
                        };
                        ui.label(egui::RichText::new(format!("    {identity}")).color(t.text_faint));
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
