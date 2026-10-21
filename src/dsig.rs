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
    let mut new_id = || {
        let id = (next, 0u16);
        next += 1;
        id
    };
    let (sig_id, widget_id, ap_id) = (new_id(), new_id(), new_id());
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

    // Appearance: a small label inside the visible box.
    let (w, h) = (user_rect[2] - user_rect[0], user_rect[3] - user_rect[1]);
    let mut ap = String::new();
    if w > 1.0 && h > 1.0 {
        let date = human_date(now);
        let who = if id.name.is_ascii() { format!("Digitally signed by {}", id.name) } else { "Digitally signed".to_owned() };
        let longest = who.len().max(date.len() + 6) as f32;
        let fs = (w / (longest * 0.5)).min(h / 4.0).clamp(3.0, 9.0);
        let _ = write!(
            ap,
            "q BT /Helv {fs:.2} Tf 0.15 0.25 0.55 rg 2 {:.2} Td ({}) Tj 0 {:.2} Td (Date: {}) Tj ET Q",
            fs * 1.4,
            pdf_escape(&who),
            -fs * 1.2,
            pdf_escape(&date)
        );
    }
    let mut ap_obj = Vec::new();
    let _ = write!(
        ap_obj,
        "<< /Type /XObject /Subtype /Form /BBox [0 0 {:.3} {:.3}] /Resources << /Font << /Helv << /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >> >> >> /Length {} >>\nstream\n{}\nendstream",
        w.max(0.0),
        h.max(0.0),
        ap.len() + 0,
        ap
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
            let af_id = new_id();
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
    pub problem: Option<String>,
}

impl SigReport {
    pub fn valid(&self) -> bool {
        self.intact && self.crypto_ok && self.problem.is_none()
    }
}

/// Check every signature in a PDF.
pub fn verify(bytes: &[u8]) -> Vec<SigReport> {
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
            problem: None,
        };
        if let Err(e) = check(bytes, d, &mut r) {
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

fn check(bytes: &[u8], d: &Dictionary, r: &mut SigReport) -> Res<()> {
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
        SignOptions { page: 0, rect, extra_rotation: 0, reason: "Approved".into(), location: "Bangkok".into() }
    }

    #[test]
    fn sign_verify_tamper_and_countersign() {
        let id = id();
        assert_eq!(id.name, "Test Signer");
        // Base saved with a cross-reference stream (lopdf save_modern).
        let base = crate::export::save(&Doc::new_blank()).unwrap();
        let signed = sign_pdf(&base, &id, &opts(Some([100.0, 100.0, 260.0, 160.0]))).unwrap();
        assert!(signed.starts_with(&base), "incremental: the original bytes are untouched");
        let r = verify(&signed);
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
        let r = verify(&bad);
        assert!(!r[0].intact, "tampering is detected");

        // A second signature is appended; the first stays valid but no longer covers the file.
        let twice = sign_pdf(&signed, &id, &opts(None)).unwrap();
        let r = verify(&twice);
        assert_eq!(r.len(), 2, "{r:?}");
        assert_eq!(r.iter().filter(|s| s.valid()).count(), 2, "{r:?}");
        assert_eq!(r.iter().filter(|s| s.whole_file).count(), 1);
        let mut names: Vec<&str> = r.iter().map(|s| s.field.as_str()).collect();
        names.sort();
        assert_eq!(names, ["Signature1", "Signature2"]);
        std::fs::write(std::env::temp_dir().join("pdfflow-signed-twice.pdf"), &twice).unwrap();
    }

    #[test]
    fn signs_a_classic_xref_file() {
        let id = id();
        let base = crate::tests::source_pdf(90);
        let signed = sign_pdf(&base, &id, &opts(Some([20.0, 20.0, 200.0, 80.0]))).unwrap();
        let r = verify(&signed);
        assert!(r.len() == 1 && r[0].valid() && r[0].whole_file, "{r:?}");
        std::fs::write(std::env::temp_dir().join("pdfflow-signed-classic.pdf"), &signed).unwrap();
    }
}
