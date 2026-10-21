//! Certificate-based digital signatures: sign with a Digital ID (PKCS#12, RSA or ECDSA P-256)
//! as a CMS detached signature (`adbe.pkcs7.detached`, SHA-256) written as an incremental update,
//! so existing signatures stay valid, and verify the signatures in an opened PDF.

use cms::builder::{SignedDataBuilder, SignerInfoBuilder, create_signing_time_attribute};
use cms::cert::{CertificateChoices, IssuerAndSerialNumber};
use cms::content_info::ContentInfo;
use cms::signed_data::{EncapsulatedContentInfo, SignedData, SignerIdentifier};
use der::asn1::OctetString;
use der::referenced::OwnedToRef;
use der::{Decode, Encode};
use lopdf::{Dictionary, Document, Object, ObjectId, StringFormat};
use sha2::{Digest, Sha256};
use spki::AlgorithmIdentifierOwned;
use std::fmt::Write as _;
use std::io::Write as _;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use x509_cert::Certificate;

type Res<T> = Result<T, String>;

const OID_RSA: &str = "1.2.840.113549.1.1.1";
const OID_EC: &str = "1.2.840.10045.2.1";

pub enum Key {
    Rsa(rsa::pkcs1v15::SigningKey<Sha256>),
    P256(p256::ecdsa::SigningKey),
}

/// A loaded Digital ID: private key and certificate chain (signer first).
pub struct DigitalId {
    pub key: Key,
    pub chain: Vec<Certificate>,
    pub name: String,
}

/// Common name of a certificate subject (or the whole subject when it has no CN).
pub fn common_name(cert: &Certificate) -> String {
    name_part(&cert.tbs_certificate.subject.to_string(), "CN")
}

fn name_part(dn: &str, key: &str) -> String {
    dn.split(',')
        .find_map(|p| p.trim().strip_prefix(key).and_then(|r| r.strip_prefix('=')))
        .map(|s| s.replace('\\', ""))
        .unwrap_or_else(|| dn.to_owned())
}

pub fn load_p12(data: &[u8], password: &str) -> Res<DigitalId> {
    let ks = p12_keystore::KeyStore::from_pkcs12(data, password).map_err(|_| "Wrong password, or not a PKCS#12 (.p12 / .pfx) Digital ID.".to_owned())?;
    let (_, chain) = ks.private_key_chain().ok_or("The file has no private key.")?;
    let der = chain.key();
    let info = pkcs8::PrivateKeyInfo::try_from(der).map_err(|e| format!("Unreadable private key: {e}"))?;
    let key = match info.algorithm.oid.to_string().as_str() {
        OID_RSA => {
            use rsa::pkcs8::DecodePrivateKey;
            Key::Rsa(rsa::pkcs1v15::SigningKey::new(rsa::RsaPrivateKey::from_pkcs8_der(der).map_err(|e| e.to_string())?))
        }
        OID_EC => {
            use p256::pkcs8::DecodePrivateKey;
            Key::P256(p256::ecdsa::SigningKey::from_pkcs8_der(der).map_err(|_| "Only P-256 elliptic-curve keys are supported.".to_owned())?)
        }
        other => return Err(format!("Unsupported key type ({other}).")),
    };
    let certs: Vec<Certificate> = chain.chain().iter().filter_map(|c| Certificate::from_der(c.as_der()).ok()).collect();
    let first = certs.first().ok_or("The Digital ID has no certificate.")?;
    Ok(DigitalId { name: common_name(first), key, chain: certs })
}

/// Create a self-signed Digital ID (RSA-2048, valid five years) and return it as PKCS#12.
pub fn create_self_signed(name: &str, org: &str, email: &str, password: &str) -> Res<Vec<u8>> {
    use rsa::pkcs8::EncodePrivateKey;
    use std::str::FromStr;
    use x509_cert::builder::{Builder, CertificateBuilder, Profile};
    use x509_cert::ext::pkix::SubjectAltName;
    use x509_cert::ext::pkix::name::GeneralName;
    let mut rng = rand_core::OsRng;
    let private = rsa::RsaPrivateKey::new(&mut rng, 2048).map_err(|e| e.to_string())?;
    let signer = rsa::pkcs1v15::SigningKey::<Sha256>::new(private.clone());
    let clean = |s: &str| s.replace([',', '+', '=', '"', '\\', '<', '>', ';'], " ").trim().to_owned();
    let mut dn = format!("CN={}", clean(name));
    if !org.trim().is_empty() {
        dn.push_str(&format!(",O={}", clean(org)));
    }
    let subject = x509_cert::name::Name::from_str(&dn).map_err(|e| e.to_string())?;
    let mut serial = [0u8; 16];
    rand_core::RngCore::fill_bytes(&mut rng, &mut serial);
    serial[0] &= 0x7f;
    let serial = x509_cert::serial_number::SerialNumber::new(&serial).map_err(|e| e.to_string())?;
    let validity = x509_cert::time::Validity::from_now(Duration::from_secs(5 * 365 * 86_400)).map_err(|e| e.to_string())?;
    let spki = spki::SubjectPublicKeyInfoOwned::from_key(private.to_public_key()).map_err(|e| e.to_string())?;
    let profile = Profile::Leaf { issuer: subject.clone(), enable_key_agreement: false, enable_key_encipherment: false };
    let mut builder = CertificateBuilder::new(profile, serial, validity, subject, spki, &signer).map_err(|e| e.to_string())?;
    if !email.trim().is_empty() {
        let mail = der::asn1::Ia5String::new(email.trim()).map_err(|_| "The e-mail address must be plain ASCII.".to_owned())?;
        builder.add_extension(&SubjectAltName(vec![GeneralName::Rfc822Name(mail)])).map_err(|e| e.to_string())?;
    }
    let cert = builder.build::<rsa::pkcs1v15::Signature>().map_err(|e| e.to_string())?;
    let cert_der = cert.to_der().map_err(|e| e.to_string())?;
    let key_der = private.to_pkcs8_der().map_err(|e| e.to_string())?;
    let local_id = Sha256::digest(&cert_der)[..20].to_vec();
    let mut ks = p12_keystore::KeyStore::new();
    let entry = p12_keystore::PrivateKeyChain::new(key_der.as_bytes(), local_id, [p12_keystore::Certificate::from_der(&cert_der).map_err(|e| e.to_string())?]);
    ks.add_entry(name, p12_keystore::KeyStoreEntry::PrivateKeyChain(entry));
    ks.writer(password).write().map_err(|e| e.to_string())
}

/// CMS SignedData (detached) over a SHA-256 digest.
fn cms_sign(id: &DigitalId, digest: &[u8]) -> Res<Vec<u8>> {
    let e = |x: cms::builder::Error| format!("{x:?}");
    let cert = id.chain.first().ok_or("no certificate")?;
    let econtent = EncapsulatedContentInfo { econtent_type: const_oid::db::rfc5911::ID_DATA, econtent: None };
    let sid = SignerIdentifier::IssuerAndSerialNumber(IssuerAndSerialNumber {
        issuer: cert.tbs_certificate.issuer.clone(),
        serial_number: cert.tbs_certificate.serial_number.clone(),
    });
    let alg = AlgorithmIdentifierOwned { oid: const_oid::db::rfc5912::ID_SHA_256, parameters: None };
    let mut b = SignedDataBuilder::new(&econtent);
    b.add_digest_algorithm(alg.clone()).map_err(e)?;
    for c in &id.chain {
        b.add_certificate(CertificateChoices::Certificate(c.clone())).map_err(e)?;
    }
    match &id.key {
        Key::Rsa(k) => {
            let mut si = SignerInfoBuilder::new(k, sid, alg, &econtent, Some(digest)).map_err(e)?;
            si.add_signed_attribute(create_signing_time_attribute().map_err(e)?).map_err(e)?;
            b.add_signer_info::<_, rsa::pkcs1v15::Signature>(si).map_err(e)?;
        }
        Key::P256(k) => {
            let mut si = SignerInfoBuilder::new(k, sid, alg, &econtent, Some(digest)).map_err(e)?;
            si.add_signed_attribute(create_signing_time_attribute().map_err(e)?).map_err(e)?;
            b.add_signer_info::<_, p256::ecdsa::DerSignature>(si).map_err(e)?;
        }
    }
    b.build().map_err(e)?.to_der().map_err(|x| x.to_string())
}

pub struct SignOptions {
    /// Page index in the document being signed.
    pub page: usize,
    /// Visible signature box in the page's display space (top-left origin, points), if any.
    pub rect: Option<[f32; 4]>,
    /// Rotation the editor added to this page (the display space predates it).
    pub extra_rotation: u16,
    pub reason: String,
    pub location: String,
    pub appearance: Appearance,
}

/// What the visible signature box shows, as in Acrobat's "Customize the Signature Appearance".
#[derive(Clone, Default)]
pub struct Appearance {
    /// A drawn signature: strokes in a unit box, its width/height ratio, and the ink colour.
    pub strokes: Option<(std::sync::Arc<Vec<Vec<eframe::egui::Pos2>>>, f32, [u8; 3])>,
    /// A signature picture (PNG / JPEG file bytes).
    pub picture: Option<std::sync::Arc<Vec<u8>>>,
    pub show_name: bool,
    pub show_date: bool,
    pub show_reason: bool,
}

/// Sign `base` and return the signed file (the original bytes plus one incremental update).
pub fn sign_pdf(base: &[u8], id: &DigitalId, o: &SignOptions) -> Res<Vec<u8>> {
    let doc = Document::load_mem(base).map_err(|e| format!("Cannot read the PDF: {e}"))?;
    if doc.is_encrypted() || doc.trailer.get(b"Encrypt").is_ok() {
        return Err("Encrypted PDFs cannot be signed.".into());
    }
    let page_id = *doc.get_pages().get(&(o.page as u32 + 1)).ok_or("page out of range")?;
    let root_id = doc.trailer.get(b"Root").and_then(Object::as_reference).map_err(|e| e.to_string())?;
    let size = doc.trailer.get(b"Size").and_then(Object::as_i64).unwrap_or(0) as u32;
    let mut next = doc.max_id.max(size.saturating_sub(1)) + 1;
    let take = |next: &mut u32| {
        let id = (*next, 0u16);
        *next += 1;
        id
    };
    let (sig_id, widget_id, ap_id) = (take(&mut next), take(&mut next), take(&mut next));
    let mut objects: Vec<(ObjectId, Vec<u8>)> = Vec::new();

    // Widget rectangle in PDF user space.
    let user_rect = match o.rect {
        Some(r) => display_to_user(&doc, page_id, r, o.extra_rotation),
        None => [0.0; 4],
    };
    let now = SystemTime::now();
    // Field names must be unique: Signature1, Signature2, ...
    let taken: std::collections::HashSet<Vec<u8>> =
        doc.objects.values().filter_map(|o| o.as_dict().ok()).filter_map(|d| d.get(b"T").ok()).filter_map(|t| t.as_str().ok()).map(<[u8]>::to_vec).collect();
    let field_name = (1..).map(|n| format!("Signature{n}")).find(|n| !taken.contains(n.as_bytes())).expect("a free name");

    // Signature value dictionary with placeholders for /ByteRange and /Contents.
    let placeholder_len = 16_384usize;
    let mut sig = Vec::new();
    let _ = write!(sig, "<< /Type /Sig /Filter /Adobe.PPKLite /SubFilter /adbe.pkcs7.detached /ByteRange ");
    let br_rel = sig.len();
    sig.extend_from_slice(BYTE_RANGE_PLACEHOLDER.as_bytes());
    sig.extend_from_slice(b" /Contents ");
    let contents_rel = sig.len();
    sig.push(b'<');
    sig.extend(std::iter::repeat_n(b'0', placeholder_len * 2));
    sig.push(b'>');
    let _ = write!(sig, " /M {} /Name ", pdf_date(now));
    sig.extend(text_string(&id.name));
    if !o.reason.trim().is_empty() {
        sig.extend_from_slice(b" /Reason ");
        sig.extend(text_string(o.reason.trim()));
    }
    if !o.location.trim().is_empty() {
        sig.extend_from_slice(b" /Location ");
        sig.extend(text_string(o.location.trim()));
    }
    sig.extend_from_slice(b" /Prop_Build << /App << /Name /PDFflow >> >> >>");
    objects.push((sig_id, sig));

    // Appearance in the box's upright (display) orientation; /Matrix turns it with the page.
    let (dw, dh) = o.rect.map(|r| (r[2] - r[0], r[3] - r[1])).unwrap_or((0.0, 0.0));
    let m = page_matrix(&doc, page_id, o.extra_rotation);
    let mut extra = Document::with_version("1.7");
    extra.max_id = next - 1;
    let (content, resources) = if dw > 1.0 && dh > 1.0 { appearance(&mut extra, id, o, now, dw, dh)? } else { (String::new(), String::new()) };
    next = extra.max_id + 1;
    for (oid, obj) in extra.objects.iter() {
        objects.push((*oid, ser_object(obj)?));
    }
    let mut ap_obj = Vec::new();
    let _ = write!(
        ap_obj,
        "<< /Type /XObject /Subtype /Form /BBox [0 0 {:.3} {:.3}] /Matrix [{} {} {} {} 0 0] /Resources << {resources} >> /Length {} >>\nstream\n{content}\nendstream",
        dw.max(0.0),
        dh.max(0.0),
        m[0],
        m[1],
        -m[2],
        -m[3],
        content.len()
    );
    objects.push((ap_id, ap_obj));

    let mut widget = Vec::new();
    let _ = write!(
        widget,
        "<< /Type /Annot /Subtype /Widget /FT /Sig /F 132 /T {} /V {} 0 R /P {} {} R /Rect [{:.3} {:.3} {:.3} {:.3}] /AP << /N {} 0 R >> >>",
        String::from_utf8_lossy(&text_string(&field_name)),
        sig_id.0,
        page_id.0,
        page_id.1,
        user_rect[0],
        user_rect[1],
        user_rect[2],
        user_rect[3],
        ap_id.0
    );
    objects.push((widget_id, widget));

    // The page gains the widget in /Annots.
    let page = doc.get_dictionary(page_id).map_err(|e| e.to_string())?.clone();
    match page.get(b"Annots") {
        Ok(Object::Reference(arr_id)) => {
            let mut arr = doc.get_object(*arr_id).and_then(Object::as_array).cloned().unwrap_or_default();
            arr.push(Object::Reference(widget_id));
            objects.push((*arr_id, ser(&Object::Array(arr))?));
        }
        other => {
            let mut arr = match other {
                Ok(Object::Array(a)) => a.clone(),
                _ => Vec::new(),
            };
            arr.push(Object::Reference(widget_id));
            let mut p = page.clone();
            p.set("Annots", arr);
            objects.push((page_id, ser(&Object::Dictionary(p))?));
        }
    }

    // The catalog's AcroForm lists the field and announces signatures (/SigFlags 3).
    let catalog = doc.get_dictionary(root_id).map_err(|e| e.to_string())?.clone();
    let add_field = |af: &mut Dictionary, objects: &mut Vec<(ObjectId, Vec<u8>)>| -> Res<()> {
        match af.get(b"Fields") {
            Ok(Object::Reference(fid)) => {
                let mut arr = doc.get_object(*fid).and_then(Object::as_array).cloned().unwrap_or_default();
                arr.push(Object::Reference(widget_id));
                objects.push((*fid, ser(&Object::Array(arr))?));
            }
            other => {
                let mut arr = match other {
                    Ok(Object::Array(a)) => a.clone(),
                    _ => Vec::new(),
                };
                arr.push(Object::Reference(widget_id));
                af.set("Fields", arr);
            }
        }
        af.set("SigFlags", 3);
        Ok(())
    };
    match catalog.get(b"AcroForm") {
        Ok(Object::Reference(af_id)) => {
            let mut af = doc.get_dictionary(*af_id).map_err(|e| e.to_string())?.clone();
            add_field(&mut af, &mut objects)?;
            objects.push((*af_id, ser(&Object::Dictionary(af))?));
        }
        Ok(Object::Dictionary(d)) => {
            let mut af = d.clone();
            add_field(&mut af, &mut objects)?;
            let mut cat = catalog.clone();
            cat.set("AcroForm", af);
            objects.push((root_id, ser(&Object::Dictionary(cat))?));
        }
        _ => {
            let af_id = take(&mut next);
            let mut af = Dictionary::new();
            add_field(&mut af, &mut objects)?;
            objects.push((af_id, ser(&Object::Dictionary(af))?));
            let mut cat = catalog.clone();
            cat.set("AcroForm", Object::Reference(af_id));
            objects.push((root_id, ser(&Object::Dictionary(cat))?));
        }
    }

    // Append the update.
    let mut out = base.to_vec();
    if !out.ends_with(b"\n") {
        out.push(b'\n');
    }
    let mut offsets: Vec<(ObjectId, usize)> = Vec::new();
    let (mut br_abs, mut contents_abs) = (0, 0);
    for (oid, body) in &objects {
        offsets.push((*oid, out.len()));
        let _ = write!(out, "{} {} obj\n", oid.0, oid.1);
        if *oid == sig_id {
            br_abs = out.len() + br_rel;
            contents_abs = out.len() + contents_rel;
        }
        out.extend_from_slice(body);
        out.extend_from_slice(b"\nendobj\n");
    }
    let prev = last_startxref(base).ok_or("The PDF has no cross-reference table.")?;
    let classic = base.get(prev..).is_some_and(|t| t.starts_with(b"xref"));
    let trailer_extra = trailer_entries(&doc)?;
    write_xref(&mut out, &mut offsets, prev, next, &trailer_extra, classic, new_id_after(next))?;

    // Fill in /ByteRange, hash the covered bytes and embed the CMS signature.
    let contents_end = contents_abs + placeholder_len * 2 + 2;
    let byte_range = format!("[0 {} {} {}]", contents_abs, contents_end, out.len() - contents_end);
    if byte_range.len() > BYTE_RANGE_PLACEHOLDER.len() {
        return Err("File too large to sign.".into());
    }
    let padded = format!("{byte_range:<width$}", width = BYTE_RANGE_PLACEHOLDER.len());
    out[br_abs..br_abs + padded.len()].copy_from_slice(padded.as_bytes());
    let mut hasher = Sha256::new();
    hasher.update(&out[..contents_abs]);
    hasher.update(&out[contents_end..]);
    let digest = hasher.finalize();
    let cms = cms_sign(id, &digest)?;
    if cms.len() > placeholder_len {
        return Err("The signature is larger than the space reserved for it.".into());
    }
    let hex: String = cms.iter().map(|b| format!("{b:02X}")).collect();
    out[contents_abs + 1..contents_abs + 1 + hex.len()].copy_from_slice(hex.as_bytes());
    Ok(out)
}

/// Append serialised replacement objects to `base` as one incremental update.
pub(crate) fn append_update(base: &[u8], doc: &Document, objects: Vec<(ObjectId, Vec<u8>)>) -> Res<Vec<u8>> {
    let size = doc.trailer.get(b"Size").and_then(Object::as_i64).unwrap_or(0) as u32;
    let next = doc.max_id.max(size.saturating_sub(1)) + 1;
    let mut out = base.to_vec();
    if !out.ends_with(b"\n") {
        out.push(b'\n');
    }
    let mut offsets = Vec::new();
    for (oid, body) in &objects {
        offsets.push((*oid, out.len()));
        let _ = write!(out, "{} {} obj\n", oid.0, oid.1);
        out.extend_from_slice(body);
        out.extend_from_slice(b"\nendobj\n");
    }
    let prev = last_startxref(base).ok_or("The PDF has no cross-reference table.")?;
    let classic = base.get(prev..).is_some_and(|t| t.starts_with(b"xref"));
    write_xref(&mut out, &mut offsets, prev, next, &trailer_entries(doc)?, classic, next)?;
    Ok(out)
}

const BYTE_RANGE_PLACEHOLDER: &str = "[0 0000000000 0000000000 0000000000]";

fn new_id_after(next: u32) -> u32 {
    next
}

/// /Root, /Info and /ID carried over from the previous trailer.
fn trailer_entries(doc: &Document) -> Res<Vec<u8>> {
    let mut out = Vec::new();
    for key in [&b"Root"[..], b"Info", b"ID"] {
        if let Ok(v) = doc.trailer.get(key) {
            out.push(b'/');
            out.extend_from_slice(key);
            out.push(b' ');
            out.extend(ser(v)?);
            out.push(b' ');
        }
    }
    Ok(out)
}

fn write_xref(out: &mut Vec<u8>, offsets: &mut Vec<(ObjectId, usize)>, prev: usize, next: u32, extra: &[u8], classic: bool, xref_id: u32) -> Res<()> {
    if classic {
        offsets.sort_by_key(|(id, _)| id.0);
        let start = out.len();
        out.extend_from_slice(b"xref\n");
        for group in groups(offsets) {
            let _ = write!(out, "{} {}\n", group[0].0.0, group.len());
            for (id, off) in group {
                let _ = write!(out, "{off:010} {:05} n\r\n", id.1);
            }
        }
        let _ = write!(out, "trailer\n<< /Size {next} /Prev {prev} ");
        out.extend_from_slice(extra);
        let _ = write!(out, ">>\nstartxref\n{start}\n%%EOF\n");
    } else {
        // The previous section is a cross-reference stream, so this update uses one too.
        let start = out.len();
        offsets.push(((xref_id, 0), start));
        offsets.sort_by_key(|(id, _)| id.0);
        let mut data = Vec::new();
        let mut index = String::new();
        for group in groups(offsets) {
            let _ = write!(index, "{} {} ", group[0].0.0, group.len());
            for (id, off) in group {
                data.push(1u8);
                data.extend_from_slice(&(*off as u32).to_be_bytes());
                data.extend_from_slice(&id.1.to_be_bytes());
            }
        }
        let _ = write!(
            out,
            "{xref_id} 0 obj\n<< /Type /XRef /Size {} /W [1 4 2] /Index [{}] /Prev {prev} ",
            xref_id + 1,
            index.trim_end()
        );
        out.extend_from_slice(extra);
        let _ = write!(out, "/Length {} >>\nstream\n", data.len());
        out.extend_from_slice(&data);
        let _ = write!(out, "\nendstream\nendobj\nstartxref\n{start}\n%%EOF\n");
    }
    Ok(())
}

/// Consecutive object numbers grouped into xref subsections.
fn groups(sorted: &[(ObjectId, usize)]) -> Vec<&[(ObjectId, usize)]> {
    let mut out = Vec::new();
    let mut s = 0;
    for i in 1..=sorted.len() {
        if i == sorted.len() || sorted[i].0.0 != sorted[i - 1].0.0 + 1 {
            out.push(&sorted[s..i]);
            s = i;
        }
    }
    out
}

fn last_startxref(b: &[u8]) -> Option<usize> {
    let tail_start = b.len().saturating_sub(2048);
    let tail = &b[tail_start..];
    let pos = tail.windows(9).rposition(|w| w == b"startxref")?;
    let rest = std::str::from_utf8(&tail[pos + 9..]).ok()?;
    rest.split_whitespace().next()?.parse().ok()
}

/// Content stream and resources for the visible signature: the signature image (drawn strokes
/// or a picture) and the chosen text lines in embedded Sarabun, so Thai names show correctly.
fn appearance(extra: &mut Document, id: &DigitalId, o: &SignOptions, now: SystemTime, w: f32, h: f32) -> Res<(String, String)> {
    let a = &o.appearance;
    let mut lines = Vec::new();
    if a.show_name {
        lines.push("Digitally signed by".to_owned());
        lines.push(id.name.clone());
    }
    if a.show_date {
        lines.push(format!("Date: {}", human_date(now)));
    }
    if a.show_reason && !o.reason.trim().is_empty() {
        lines.push(format!("Reason: {}", o.reason.trim()));
    }
    let has_image = a.strokes.is_some() || a.picture.is_some();
    let pad = (w.min(h) * 0.06).clamp(1.0, 4.0);
    // Image and text side by side in a wide box, stacked in a tall one.
    let (img_box, text_box) = match (has_image, lines.is_empty()) {
        (true, true) => ([pad, pad, w - pad, h - pad], None),
        (false, _) => ([0.0; 4], Some([pad, pad, w - pad, h - pad])),
        (true, false) if w / h >= 1.6 => ([pad, pad, w * 0.48, h - pad], Some([w * 0.5, pad, w - pad, h - pad])),
        (true, false) => ([pad, h * 0.42, w - pad, h - pad], Some([pad, pad, w - pad, h * 0.40])),
    };
    let mut c = String::new();
    let mut res = String::new();
    let fit = |aspect: f32, b: [f32; 4]| {
        let (bw, bh) = (b[2] - b[0], b[3] - b[1]);
        let (fw, fh) = if bw / bh > aspect { (bh * aspect, bh) } else { (bw, bw / aspect) };
        [b[0] + (bw - fw) / 2.0, b[1] + (bh - fh) / 2.0, fw, fh]
    };
    if let Some((strokes, aspect, col)) = &a.strokes {
        let [x, y, fw, fh] = fit(*aspect, img_box);
        let _ = write!(c, "q {:.3} {:.3} {:.3} RG {:.2} w 1 J 1 j ", col[0] as f32 / 255.0, col[1] as f32 / 255.0, col[2] as f32 / 255.0, (fh / 28.0).clamp(0.6, 2.5));
        for st in strokes.iter() {
            for (i, p) in st.iter().enumerate() {
                let _ = write!(c, "{:.2} {:.2} {} ", x + p.x * fw, y + (1.0 - p.y) * fh, if i == 0 { "m" } else { "l" });
            }
            if st.len() == 1 {
                let _ = write!(c, "{:.2} {:.2} l ", x + st[0].x * fw, y + (1.0 - st[0].y) * fh);
            }
            c.push_str("S ");
        }
        c.push_str("Q\n");
    } else if let Some(bytes) = &a.picture {
        let (pw, ph) = image::ImageReader::new(std::io::Cursor::new(bytes.as_slice()))
            .with_guessed_format()
            .ok()
            .and_then(|r| r.into_dimensions().ok())
            .ok_or("The signature picture cannot be read.")?;
        let img = crate::doc::ImageData::new(bytes.clone(), [pw, ph], None);
        let xo = crate::export::image_xobject(extra, &img)?;
        let [x, y, fw, fh] = fit(pw as f32 / ph as f32, img_box);
        let _ = write!(c, "q {fw:.3} 0 0 {fh:.3} {x:.3} {y:.3} cm /SigImg Do Q\n");
        let _ = write!(res, "/XObject << /SigImg {} 0 R >> ", xo.0);
    }
    if let Some(tb) = text_box {
        let (bw, bh) = (tb[2] - tb[0], tb[3] - tb[1]);
        // Size the text to fill the box: widest line across, all lines down.
        let unit: Vec<crate::text::Layout> = lines.iter().map(|l| crate::text::layout(l, 1.0, false)).collect();
        let widest = unit.iter().map(|l| l.width).fold(0.01, f32::max);
        let lh1 = crate::text::face(false).line_height(1.0);
        let fs = (bw / widest).min(bh / (lines.len() as f32 * lh1)).clamp(2.0, 14.0);
        let lh = lh1 * fs;
        let top = tb[1] + bh - (bh - lh * lines.len() as f32) / 2.0;
        let mut glyphs = std::collections::BTreeMap::new();
        let _ = write!(c, "BT /SigFont {fs:.3} Tf 0.12 0.2 0.45 rg\n");
        for (i, line) in lines.iter().enumerate() {
            let l = crate::text::layout(line, fs, false);
            for g in &l.glyphs {
                let _ = writeln!(c, "1 0 0 1 {:.2} {:.2} Tm <{:04X}> Tj", tb[0] + g.x, top - i as f32 * lh - g.y, g.gid);
                glyphs.entry(g.gid).or_insert_with(|| g.text.clone());
            }
        }
        c.push_str("ET\n");
        let fid = extra.new_object_id();
        crate::export::embed_font(extra, fid, false, &glyphs);
        let _ = write!(res, "/Font << /SigFont {} 0 R >> ", fid.0);
    }
    Ok((c, res))
}

/// The display-to-user matrix of a page (as in the editor's export).
fn page_matrix(doc: &Document, page_id: ObjectId, extra_rotation: u16) -> [f32; 6] {
    let (b, rotate) = crate::export::page_frame(doc, page_id);
    crate::export::display_matrix(b, (rotate + 360 - extra_rotation % 360) % 360)
}

/// Serialise an indirect object, streams included.
fn ser_object(o: &Object) -> Res<Vec<u8>> {
    match o {
        Object::Stream(st) => {
            let mut d = st.dict.clone();
            d.set("Length", st.content.len() as i64);
            let mut out = ser(&Object::Dictionary(d))?;
            out.extend_from_slice(b"\nstream\n");
            out.extend_from_slice(&st.content);
            out.extend_from_slice(b"\nendstream");
            Ok(out)
        }
        other => ser(other),
    }
}

/// Map a display-space rectangle (top-left origin, y down) to PDF user space.
fn display_to_user(doc: &Document, page_id: ObjectId, r: [f32; 4], extra_rotation: u16) -> [f32; 4] {
    let media = crate::export::inherited(doc, page_id, b"MediaBox").and_then(|o| crate::export::rect_of(doc, &o)).unwrap_or([0.0, 0.0, 595.0, 842.0]);
    let crop = crate::export::inherited(doc, page_id, b"CropBox").and_then(|o| crate::export::rect_of(doc, &o));
    let b = crop.map(|c| crate::export::intersect(c, media)).filter(|b| b[2] > b[0] && b[3] > b[1]).unwrap_or(media);
    let rotate = crate::export::inherited(doc, page_id, b"Rotate").and_then(|o| o.as_i64().ok()).unwrap_or(0).rem_euclid(360) as u16 / 90 * 90;
    let rot = (rotate + 360 - extra_rotation % 360) % 360;
    let [x0, y0, x1, y1] = b;
    let m = match rot {
        90 => [0.0, 1.0, 1.0, 0.0, x0, y0],
        180 => [-1.0, 0.0, 0.0, 1.0, x1, y0],
        270 => [0.0, -1.0, -1.0, 0.0, x1, y1],
        _ => [1.0, 0.0, 0.0, -1.0, x0, y1],
    };
    let t = |u: f32, v: f32| (m[0] * u + m[2] * v + m[4], m[1] * u + m[3] * v + m[5]);
    let (ax, ay) = t(r[0], r[1]);
    let (bx, by) = t(r[2], r[3]);
    [ax.min(bx), ay.min(by), ax.max(bx), ay.max(by)]
}

/// Serialise a lopdf object (no streams) in PDF syntax.
pub fn ser(o: &Object) -> Res<Vec<u8>> {
    let mut out = Vec::new();
    ser_into(o, &mut out)?;
    Ok(out)
}

fn ser_into(o: &Object, out: &mut Vec<u8>) -> Res<()> {
    match o {
        Object::Null => out.extend_from_slice(b"null"),
        Object::Boolean(b) => out.extend_from_slice(if *b { b"true" } else { b"false" }),
        Object::Integer(i) => {
            let _ = write!(out, "{i}");
        }
        Object::Real(r) => {
            let s = format!("{r:.4}");
            out.extend_from_slice(s.trim_end_matches('0').trim_end_matches('.').as_bytes());
        }
        Object::Name(n) => {
            out.push(b'/');
            for &c in n {
                if c.is_ascii_alphanumeric() || b"-_.+*!@$&'~:;=?,".contains(&c) {
                    out.push(c);
                } else {
                    let _ = write!(out, "#{c:02X}");
                }
            }
        }
        Object::String(s, StringFormat::Hexadecimal) => {
            out.push(b'<');
            for c in s {
                let _ = write!(out, "{c:02X}");
            }
            out.push(b'>');
        }
        Object::String(s, StringFormat::Literal) => {
            out.push(b'(');
            for &c in s {
                match c {
                    b'(' | b')' | b'\\' => {
                        out.push(b'\\');
                        out.push(c);
                    }
                    b'\r' => out.extend_from_slice(b"\\r"),
                    b'\n' => out.extend_from_slice(b"\\n"),
                    _ => out.push(c),
                }
            }
            out.push(b')');
        }
        Object::Array(a) => {
            out.push(b'[');
            for (i, x) in a.iter().enumerate() {
                if i > 0 {
                    out.push(b' ');
                }
                ser_into(x, out)?;
            }
            out.push(b']');
        }
        Object::Dictionary(d) => {
            out.extend_from_slice(b"<<");
            for (k, v) in d.iter() {
                out.push(b' ');
                ser_into(&Object::Name(k.clone()), out)?;
                out.push(b' ');
                ser_into(v, out)?;
            }
            out.extend_from_slice(b" >>");
        }
        Object::Reference(id) => {
            let _ = write!(out, "{} {} R", id.0, id.1);
        }
        Object::Stream(_) => return Err("unexpected stream".into()),
    }
    Ok(())
}

/// A PDF text string: literal for ASCII, UTF-16BE with BOM otherwise.
fn text_string(s: &str) -> Vec<u8> {
    if s.is_ascii() {
        return format!("({})", pdf_escape(s)).into_bytes();
    }
    let mut out = String::from("<FEFF");
    for u in s.encode_utf16() {
        let _ = write!(out, "{u:04X}");
    }
    out.push('>');
    out.into_bytes()
}

fn pdf_escape(s: &str) -> String {
    s.chars().filter(|c| c.is_ascii() && !c.is_ascii_control()).flat_map(|c| if matches!(c, '(' | ')' | '\\') { vec!['\\', c] } else { vec![c] }).collect()
}

fn civil(t: SystemTime) -> (i64, u32, u32, u32, u32, u32) {
    let secs = t.duration_since(UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0);
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    // Howard Hinnant's days-to-civil algorithm.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = yoe + era * 400 + if m <= 2 { 1 } else { 0 };
    (y, m, d, (rem / 3600) as u32, (rem % 3600 / 60) as u32, (rem % 60) as u32)
}

pub(crate) fn pdf_date(t: SystemTime) -> String {
    let (y, m, d, hh, mm, ss) = civil(t);
    format!("(D:{y:04}{m:02}{d:02}{hh:02}{mm:02}{ss:02}Z)")
}

fn human_date(t: SystemTime) -> String {
    let (y, m, d, hh, mm, _) = civil(t);
    format!("{y:04}-{m:02}-{d:02} {hh:02}:{mm:02} UTC")
}

/// "D:YYYYMMDDHHmmSS..." to "YYYY-MM-DD HH:mm".
fn readable_pdf_date(s: &str) -> String {
    let d: String = s.trim_start_matches("D:").chars().take(14).collect();
    if d.len() < 12 || !d.chars().all(|c| c.is_ascii_digit()) {
        return s.to_owned();
    }
    let tz = s.trim_start_matches("D:").get(14..).unwrap_or("").replace('\'', "");
    format!("{}-{}-{} {}:{} {}", &d[0..4], &d[4..6], &d[6..8], &d[8..10], &d[10..12], if tz.is_empty() || tz == "Z" { "UTC".into() } else { tz })
}

#[derive(Clone, Debug)]
pub struct SigReport {
    pub field: String,
    pub signer: String,
    pub issuer: String,
    pub when: String,
    pub reason: String,
    /// The signed bytes hash to the digest inside the signature.
    pub intact: bool,
    /// The signature over that digest verifies with the signer's public key.
    pub crypto_ok: bool,
    /// The signature covers the whole file (no changes were appended later).
    pub whole_file: bool,
    pub self_signed: bool,
    pub cert_current: bool,
    /// The signer's certificate chains to a root the user trusts.
    pub trusted: bool,
    /// Certificate names from the signer up to the trusted root (when trusted).
    pub chain: Vec<String>,
    pub expires: String,
    pub problem: Option<String>,
}

impl SigReport {
    pub fn valid(&self) -> bool {
        self.intact && self.crypto_ok && self.problem.is_none()
    }
}

/// Check every signature in a PDF.
pub fn verify(bytes: &[u8], trusted: &[Certificate]) -> Vec<SigReport> {
    let Ok(doc) = Document::load_mem(bytes) else { return Vec::new() };
    let mut field_names = std::collections::HashMap::new();
    for obj in doc.objects.values() {
        if let Ok(d) = obj.as_dict() {
            if let (Ok(Object::Reference(v)), Ok(t)) = (d.get(b"V"), d.get(b"T")) {
                if let Ok(name) = t.as_str() {
                    field_names.insert(*v, String::from_utf8_lossy(name).into_owned());
                }
            }
        }
    }
    let mut out = Vec::new();
    for (id, obj) in &doc.objects {
        let Ok(d) = obj.as_dict() else { continue };
        if d.get(b"ByteRange").is_err() || d.get(b"Contents").is_err() {
            continue;
        }
        let s = |k: &[u8]| d.get(k).ok().and_then(|o| o.as_str().ok()).map(decode_text).unwrap_or_default();
        let mut r = SigReport {
            field: field_names.get(id).cloned().unwrap_or_else(|| format!("Signature {}", id.0)),
            signer: s(b"Name"),
            issuer: String::new(),
            when: readable_pdf_date(&s(b"M")),
            reason: s(b"Reason"),
            intact: false,
            crypto_ok: false,
            whole_file: false,
            self_signed: false,
            cert_current: false,
            trusted: false,
            chain: Vec::new(),
            expires: String::new(),
            problem: None,
        };
        if let Err(e) = check(bytes, d, trusted, &mut r) {
            r.problem = Some(e);
        }
        out.push(r);
    }
    out.sort_by(|a, b| a.when.cmp(&b.when));
    out
}

pub(crate) fn decode_text(b: &[u8]) -> String {
    if b.starts_with(&[0xFE, 0xFF]) {
        let u: Vec<u16> = b[2..].chunks(2).map(|c| u16::from_be_bytes([c[0], *c.get(1).unwrap_or(&0)])).collect();
        String::from_utf16_lossy(&u)
    } else {
        b.iter().map(|&c| c as char).collect()
    }
}

fn check(bytes: &[u8], d: &Dictionary, trusted: &[Certificate], r: &mut SigReport) -> Res<()> {
    let br: Vec<usize> = d.get(b"ByteRange").and_then(Object::as_array).map_err(|e| e.to_string())?.iter().filter_map(|o| o.as_i64().ok()).map(|v| v as usize).collect();
    if br.len() != 4 || br[0] + br[1] > bytes.len() || br[2] + br[3] > bytes.len() {
        return Err("Malformed byte range.".into());
    }
    let tail = &bytes[br[2] + br[3]..];
    r.whole_file = tail.iter().all(|c| c.is_ascii_whitespace() || *c == b'%' || c.is_ascii_alphabetic());
    let contents = d.get(b"Contents").and_then(Object::as_str).map_err(|e| e.to_string())?;
    let der = der_prefix(contents).ok_or("The signature data is not DER.")?;
    let ci = ContentInfo::from_der(der).map_err(|e| format!("Unreadable signature: {e}"))?;
    let sd: SignedData = ci.content.decode_as().map_err(|e| format!("Not a CMS SignedData signature: {e}"))?;
    let si = sd.signer_infos.0.get(0).ok_or("No signer in the signature.")?;
    let cert = sd
        .certificates
        .as_ref()
        .and_then(|set| {
            set.0.iter().find_map(|c| match (c, &si.sid) {
                (CertificateChoices::Certificate(c), SignerIdentifier::IssuerAndSerialNumber(ias))
                    if c.tbs_certificate.issuer == ias.issuer && c.tbs_certificate.serial_number == ias.serial_number =>
                {
                    Some(c.clone())
                }
                (CertificateChoices::Certificate(c), SignerIdentifier::SubjectKeyIdentifier(_)) => Some(c.clone()),
                _ => None,
            })
        })
        .ok_or("The signer's certificate is not in the signature.")?;
    r.signer = common_name(&cert);
    r.issuer = common_name_of(&cert.tbs_certificate.issuer.to_string());
    r.self_signed = cert.tbs_certificate.issuer == cert.tbs_certificate.subject;
    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default();
    let v = &cert.tbs_certificate.validity;
    r.cert_current = v.not_before.to_unix_duration() <= now && now <= v.not_after.to_unix_duration();
    r.expires = expiry_date(&cert);
    let pool: Vec<Certificate> = sd
        .certificates
        .as_ref()
        .map(|set| set.0.iter().filter_map(|c| if let CertificateChoices::Certificate(c) = c { Some(c.clone()) } else { None }).collect())
        .unwrap_or_default();
    if let Some(chain) = chain_to_trust(&cert, &pool, trusted) {
        r.trusted = true;
        r.chain = chain;
    }

    let digest_oid = si.digest_alg.oid.to_string();
    let mut data = Vec::with_capacity(br[1] + br[3]);
    data.extend_from_slice(&bytes[br[0]..br[0] + br[1]]);
    data.extend_from_slice(&bytes[br[2]..br[2] + br[3]]);
    let hash = digest(&digest_oid, &data).ok_or_else(|| format!("Unsupported digest algorithm {digest_oid}."))?;
    let (msg, intact) = match &si.signed_attrs {
        Some(attrs) => {
            let md = attrs
                .iter()
                .find(|a| a.oid == const_oid::db::rfc5911::ID_MESSAGE_DIGEST)
                .and_then(|a| a.values.get(0))
                .and_then(|v| v.decode_as::<OctetString>().ok())
                .ok_or("The signature has no message digest.")?;
            (attrs.to_der().map_err(|e| e.to_string())?, md.as_bytes() == hash.as_slice())
        }
        None => (data.clone(), true),
    };
    r.intact = intact;
    let sig = si.signature.as_bytes();
    let spki = &cert.tbs_certificate.subject_public_key_info;
    r.crypto_ok = match spki.algorithm.oid.to_string().as_str() {
        OID_RSA => {
            use rsa::signature::Verifier;
            let pk = rsa::RsaPublicKey::try_from(spki.owned_to_ref()).map_err(|e| e.to_string())?;
            let s = rsa::pkcs1v15::Signature::try_from(sig).map_err(|e| e.to_string())?;
            match digest_oid.as_str() {
                "2.16.840.1.101.3.4.2.1" => rsa::pkcs1v15::VerifyingKey::<Sha256>::new(pk).verify(&msg, &s).is_ok(),
                "2.16.840.1.101.3.4.2.2" => rsa::pkcs1v15::VerifyingKey::<sha2::Sha384>::new(pk).verify(&msg, &s).is_ok(),
                "2.16.840.1.101.3.4.2.3" => rsa::pkcs1v15::VerifyingKey::<sha2::Sha512>::new(pk).verify(&msg, &s).is_ok(),
                _ => return Err(format!("Unsupported digest algorithm {digest_oid}.")),
            }
        }
        OID_EC => {
            use p256::ecdsa::signature::Verifier;
            let vk = p256::ecdsa::VerifyingKey::from_sec1_bytes(spki.subject_public_key.raw_bytes()).map_err(|_| "Only P-256 elliptic-curve signatures can be checked.".to_owned())?;
            let s = p256::ecdsa::DerSignature::try_from(sig).map_err(|e| e.to_string())?;
            vk.verify(&msg, &s).is_ok()
        }
        other => return Err(format!("Unsupported signature algorithm {other}.")),
    };
    Ok(())
}

/// Read a certificate from a .crt / .cer file (DER or PEM).
pub fn parse_cert_file(bytes: &[u8]) -> Res<Certificate> {
    use der::DecodePem;
    Certificate::from_der(bytes).or_else(|_| Certificate::from_pem(bytes)).map_err(|_| "Not an X.509 certificate (.crt / .cer).".to_owned())
}

/// The certificate's expiry date, "YYYY-MM-DD".
pub fn expiry_date(cert: &Certificate) -> String {
    let t = UNIX_EPOCH + cert.tbs_certificate.validity.not_after.to_unix_duration();
    let (y, m, d, ..) = civil(t);
    format!("{y:04}-{m:02}-{d:02}")
}

/// Seconds until the certificate expires (negative when expired).
pub fn seconds_left(cert: &Certificate) -> i64 {
    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs() as i64;
    cert.tbs_certificate.validity.not_after.to_unix_duration().as_secs() as i64 - now
}

/// Does `issuer`'s key verify the signature on `cert`?
fn signed_by(cert: &Certificate, issuer: &Certificate) -> bool {
    use rsa::signature::Verifier;
    if cert.tbs_certificate.issuer != issuer.tbs_certificate.subject {
        return false;
    }
    let Ok(tbs) = cert.tbs_certificate.to_der() else { return false };
    let sig = cert.signature.raw_bytes();
    let spki = &issuer.tbs_certificate.subject_public_key_info;
    match cert.signature_algorithm.oid.to_string().as_str() {
        alg @ ("1.2.840.113549.1.1.11" | "1.2.840.113549.1.1.12" | "1.2.840.113549.1.1.13" | "1.2.840.113549.1.1.5") => {
            let Ok(pk) = rsa::RsaPublicKey::try_from(spki.owned_to_ref()) else { return false };
            let Ok(s) = rsa::pkcs1v15::Signature::try_from(sig) else { return false };
            match alg {
                "1.2.840.113549.1.1.11" => rsa::pkcs1v15::VerifyingKey::<Sha256>::new(pk).verify(&tbs, &s).is_ok(),
                "1.2.840.113549.1.1.12" => rsa::pkcs1v15::VerifyingKey::<sha2::Sha384>::new(pk).verify(&tbs, &s).is_ok(),
                "1.2.840.113549.1.1.13" => rsa::pkcs1v15::VerifyingKey::<sha2::Sha512>::new(pk).verify(&tbs, &s).is_ok(),
                _ => false, // SHA-1 certificate signatures are not accepted
            }
        }
        "1.2.840.10045.4.3.2" => {
            use p256::ecdsa::signature::Verifier as _;
            let Ok(vk) = p256::ecdsa::VerifyingKey::from_sec1_bytes(spki.subject_public_key.raw_bytes()) else { return false };
            let Ok(s) = p256::ecdsa::DerSignature::try_from(sig) else { return false };
            vk.verify(&tbs, &s).is_ok()
        }
        _ => false,
    }
}

/// Follow issuers from `leaf` through `pool` (certificates shipped in the signature) until a
/// trusted certificate signs one; returns the names along the chain.
pub fn chain_to_trust(leaf: &Certificate, pool: &[Certificate], trusted: &[Certificate]) -> Option<Vec<String>> {
    let mut chain = vec![common_name(leaf)];
    if trusted.iter().any(|t| t == leaf) {
        return Some(chain);
    }
    let mut cur = leaf.clone();
    for _ in 0..8 {
        if let Some(root) = trusted.iter().find(|t| signed_by(&cur, t)) {
            chain.push(common_name(root));
            return Some(chain);
        }
        let next = pool.iter().find(|c| **c != cur && signed_by(&cur, c))?.clone();
        chain.push(common_name(&next));
        cur = next;
    }
    None
}

pub fn common_name_of(dn: &str) -> String {
    name_part(dn, "CN")
}

fn digest(oid: &str, data: &[u8]) -> Option<Vec<u8>> {
    Some(match oid {
        "2.16.840.1.101.3.4.2.1" => Sha256::digest(data).to_vec(),
        "2.16.840.1.101.3.4.2.2" => sha2::Sha384::digest(data).to_vec(),
        "2.16.840.1.101.3.4.2.3" => sha2::Sha512::digest(data).to_vec(),
        _ => return None,
    })
}

/// The DER object at the start of a zero-padded /Contents string.
fn der_prefix(b: &[u8]) -> Option<&[u8]> {
    if b.len() < 2 || b[0] != 0x30 {
        return None;
    }
    let (len, hdr) = if b[1] < 0x80 {
        (b[1] as usize, 2)
    } else {
        let n = (b[1] & 0x7f) as usize;
        if n == 0 || n > 4 || b.len() < 2 + n {
            return None;
        }
        (b[2..2 + n].iter().fold(0usize, |a, x| (a << 8) | *x as usize), 2 + n)
    };
    b.get(..hdr + len)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::doc::Doc;

    fn id() -> DigitalId {
        let p12 = create_self_signed("Test Signer", "PDFflow Lab", "signer@example.com", "pw").unwrap();
        assert!(load_p12(&p12, "wrong").is_err(), "wrong password must fail");
        load_p12(&p12, "pw").unwrap()
    }

    fn opts(rect: Option<[f32; 4]>) -> SignOptions {
        SignOptions { page: 0, rect, extra_rotation: 0, reason: "Approved".into(), location: "Bangkok".into(), appearance: Appearance { show_name: true, show_date: true, ..Default::default() } }
    }

    #[test]
    fn sign_verify_tamper_and_countersign() {
        let id = id();
        assert_eq!(id.name, "Test Signer");
        // Base saved with a cross-reference stream (lopdf save_modern).
        let base = crate::export::save(&Doc::new_blank()).unwrap();
        let signed = sign_pdf(&base, &id, &opts(Some([100.0, 100.0, 260.0, 160.0]))).unwrap();
        assert!(signed.starts_with(&base), "incremental: the original bytes are untouched");
        let r = verify(&signed, &[]);
        assert_eq!(r.len(), 1);
        assert!(r[0].valid() && r[0].whole_file && r[0].self_signed && r[0].cert_current, "{:?}", r[0]);
        assert_eq!(r[0].signer, "Test Signer");
        assert_eq!(r[0].reason, "Approved");
        // Still a readable PDF for both parsers.
        assert!(hayro::hayro_syntax::Pdf::new(std::sync::Arc::new(signed.clone())).is_ok());
        assert_eq!(Document::load_mem(&signed).unwrap().get_pages().len(), 1);

        // Changing one signed byte breaks the signature.
        let mut bad = signed.clone();
        let i = base.len() / 2;
        bad[i] = bad[i].wrapping_add(1);
        let r = verify(&bad, &[]);
        assert!(!r[0].intact, "tampering is detected");

        // A second signature is appended; the first stays valid but no longer covers the file.
        let twice = sign_pdf(&signed, &id, &opts(None)).unwrap();
        let r = verify(&twice, &[]);
        assert_eq!(r.len(), 2, "{r:?}");
        assert_eq!(r.iter().filter(|s| s.valid()).count(), 2, "{r:?}");
        assert_eq!(r.iter().filter(|s| s.whole_file).count(), 1);
        let mut names: Vec<&str> = r.iter().map(|s| s.field.as_str()).collect();
        names.sort();
        assert_eq!(names, ["Signature1", "Signature2"]);
        std::fs::write(std::env::temp_dir().join("pdfflow-signed-twice.pdf"), &twice).unwrap();
    }

    /// A university-style setup: a root CA, and a personal certificate it issued (Thai name),
    /// packed as PKCS#12 without the root, as a CA's download portal would hand it out.
    pub(crate) fn university_id(cn: &str) -> (Certificate, Vec<u8>) {
        use rsa::pkcs8::EncodePrivateKey;
        use std::str::FromStr;
        use x509_cert::builder::{Builder, CertificateBuilder, Profile};
        let mut rng = rand_core::OsRng;
        let ca_key = rsa::RsaPrivateKey::new(&mut rng, 2048).unwrap();
        let ca_signer = rsa::pkcs1v15::SigningKey::<Sha256>::new(ca_key.clone());
        let ca_name = x509_cert::name::Name::from_str("CN=Test University Consortium Certification Authority,O=Test Universities").unwrap();
        let validity = x509_cert::time::Validity::from_now(Duration::from_secs(10 * 365 * 86_400)).unwrap();
        let ca_spki = spki::SubjectPublicKeyInfoOwned::from_key(ca_key.to_public_key()).unwrap();
        let ca = CertificateBuilder::new(Profile::Root, x509_cert::serial_number::SerialNumber::from(1u32), validity.clone(), ca_name.clone(), ca_spki, &ca_signer)
            .unwrap()
            .build::<rsa::pkcs1v15::Signature>()
            .unwrap();
        let key = rsa::RsaPrivateKey::new(&mut rng, 2048).unwrap();
        let subject = x509_cert::name::Name::from_str(&format!("CN={cn},O=Test University")).unwrap();
        let spki = spki::SubjectPublicKeyInfoOwned::from_key(key.to_public_key()).unwrap();
        let profile = Profile::Leaf { issuer: ca_name, enable_key_agreement: false, enable_key_encipherment: false };
        let leaf = CertificateBuilder::new(profile, x509_cert::serial_number::SerialNumber::from(42u32), validity, subject, spki, &ca_signer)
            .unwrap()
            .build::<rsa::pkcs1v15::Signature>()
            .unwrap();
        let mut ks = p12_keystore::KeyStore::new();
        let entry = p12_keystore::PrivateKeyChain::new(key.to_pkcs8_der().unwrap().as_bytes(), [1u8; 20], [p12_keystore::Certificate::from_der(&leaf.to_der().unwrap()).unwrap()]);
        ks.add_entry("me", p12_keystore::KeyStoreEntry::PrivateKeyChain(entry));
        (ca, ks.writer("pw").write().unwrap())
    }

    #[test]
    fn university_certificate_chain_and_thai_appearance() {
        let (root, p12) = university_id("สมชาย ใจดี");
        let id = load_p12(&p12, "pw").unwrap();
        assert_eq!(id.name, "สมชาย ใจดี");
        let base = crate::export::save(&Doc::new_blank()).unwrap();
        let strokes = std::sync::Arc::new(vec![vec![eframe::egui::pos2(0.0, 0.8), eframe::egui::pos2(0.5, 0.1), eframe::egui::pos2(1.0, 0.8)]]);
        let mut o = opts(Some([100.0, 100.0, 340.0, 180.0]));
        o.appearance = Appearance { strokes: Some((strokes, 3.0, [20, 50, 140])), picture: None, show_name: true, show_date: true, show_reason: true };
        let signed = sign_pdf(&base, &id, &o).unwrap();
        // Without the root the signature is intact but the identity is unverified; trusting the
        // root (as importing KMUTT's CA.crt does) makes the chain verified.
        let r = verify(&signed, &[]);
        assert!(r[0].valid() && !r[0].trusted, "{r:?}");
        let r = verify(&signed, std::slice::from_ref(&root));
        assert!(r[0].trusted, "{r:?}");
        assert_eq!(r[0].chain, ["สมชาย ใจดี", "Test University Consortium Certification Authority"]);
        // A root that did not issue the certificate is not accepted.
        let (other_root, _) = university_id("x");
        assert!(!verify(&signed, &[other_root])[0].trusted);
        // The visible box is drawn: render the page and look for ink inside it.
        let pdf = hayro::hayro_syntax::Pdf::new(std::sync::Arc::new(signed.clone())).unwrap();
        let pix = hayro::render(&pdf.pages()[0], &hayro::RenderCache::new(), &Default::default(), &hayro::RenderSettings { x_scale: 1.0, y_scale: 1.0, bg_color: hayro::vello_cpu::color::palette::css::WHITE, ..Default::default() });
        let data = pix.data_as_u8_slice();
        let w = pix.width() as usize;
        let ink = (100..180).flat_map(|y| (100..340).map(move |x| (x, y))).filter(|(x, y)| data[(y * w + x) * 4] < 150).count();
        assert!(ink > 200, "signature appearance not drawn ({ink} dark pixels)");
        // The Thai name is embedded in Sarabun with a ToUnicode map.
        let doc = Document::load_mem(&signed).unwrap();
        assert!(doc.objects.values().any(|o| o.as_dict().ok().and_then(|d| d.get(b"BaseFont").ok()).and_then(|b| b.as_name().ok()) == Some(b"Sarabun-Regular".as_slice())));
        std::fs::write(std::env::temp_dir().join("pdfflow-signed-thai.pdf"), &signed).unwrap();
    }

    #[test]
    fn signs_a_classic_xref_file() {
        let id = id();
        let base = crate::tests::source_pdf(90);
        let signed = sign_pdf(&base, &id, &opts(Some([20.0, 20.0, 200.0, 80.0]))).unwrap();
        let r = verify(&signed, &[]);
        assert!(r.len() == 1 && r[0].valid() && r[0].whole_file, "{r:?}");
        std::fs::write(std::env::temp_dir().join("pdfflow-signed-classic.pdf"), &signed).unwrap();
    }
}
