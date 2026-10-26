//! Copying pages between documents. A page copied from another file is appended to this
//! document's source PDF with lopdf, so it stays a real PDF page (text, fonts, vector
//! graphics and pictures intact) and the editor refers to it as one more source page.

use crate::export::inherited;
use lopdf::{Dictionary, Document, Object, ObjectId, dictionary};

/// Append pages `idx` (0-based) of `extra` to `base`, in that order, and return the merged
/// PDF with the index its first appended page has. Without a base PDF the result holds only
/// the appended pages. Inherited page attributes (media box, crop box, rotation, resources)
/// are copied into each page, since it leaves its old page tree.
pub fn merge_pages(base: Option<&[u8]>, extra: &[u8], idx: &[usize]) -> Result<(Vec<u8>, usize), String> {
    let mut extra = Document::load_mem(extra).map_err(|e| format!("Cannot read the other PDF: {e}"))?;
    if extra.is_encrypted() {
        return Err("Pages of an encrypted PDF cannot be copied.".into());
    }
    let mut pdf = match base {
        Some(b) => Document::load_mem(b).map_err(|e| format!("Cannot re-read the source PDF: {e}"))?,
        None => {
            let mut d = Document::with_version("1.7");
            let pages = d.add_object(dictionary! { "Type" => "Pages", "Kids" => Vec::<Object>::new(), "Count" => 0 });
            let cat = d.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages });
            d.trailer.set("Root", cat);
            d
        }
    };
    if pdf.is_encrypted() {
        return Err("Pages cannot be added to an encrypted PDF.".into());
    }
    let root_pages = pdf.catalog().map_err(|e| e.to_string())?.get(b"Pages").and_then(Object::as_reference).map_err(|e| e.to_string())?;
    let base_n = pdf.get_pages().len();

    // Flatten what the pages inherit while their old page tree is still around them.
    let extra_pages: Vec<ObjectId> = extra.get_pages().values().copied().collect();
    for &i in idx {
        let id = *extra_pages.get(i).ok_or("page index out of range")?;
        let mut flat = Dictionary::new();
        for key in [b"MediaBox".as_slice(), b"CropBox", b"Rotate", b"Resources"] {
            if let Some(v) = inherited(&extra, id, key) {
                flat.set(key.to_vec(), v);
            }
        }
        let d = extra.get_object_mut(id).and_then(Object::as_dict_mut).map_err(|e| e.to_string())?;
        for (k, v) in flat.iter() {
            if !d.has(k) {
                d.set(k.to_vec(), v.clone());
            }
        }
        // Explicit defaults, so nothing is inherited from this document's page tree instead.
        if !d.has(b"Rotate") {
            d.set("Rotate", 0);
        }
        if !d.has(b"Resources") {
            d.set("Resources", Dictionary::new());
        }
        if !d.has(b"CropBox") {
            if let Ok(m) = d.get(b"MediaBox").cloned() {
                d.set("CropBox", m);
            }
        }
    }

    // Give the other file's objects numbers above ours, then move them over.
    let first_free = pdf.objects.keys().map(|k| k.0).max().unwrap_or(0).max(pdf.max_id) + 1;
    extra.renumber_objects_with(first_free);
    let extra_pages: Vec<ObjectId> = extra.get_pages().values().copied().collect();
    pdf.max_id = extra.objects.keys().map(|k| k.0).max().unwrap_or(first_free).max(extra.max_id);
    pdf.objects.extend(std::mem::take(&mut extra.objects));

    let mut kids = Vec::with_capacity(idx.len());
    for &i in idx {
        let id = extra_pages[i];
        let d = pdf.get_object_mut(id).and_then(Object::as_dict_mut).map_err(|e| e.to_string())?;
        d.set("Parent", root_pages);
        kids.push(Object::Reference(id));
    }
    let root = pdf.get_dictionary_mut(root_pages).map_err(|e| e.to_string())?;
    let mut all = match root.get(b"Kids") {
        Ok(Object::Array(a)) => a.clone(),
        _ => Vec::new(),
    };
    all.extend(kids);
    let count = all.len() as i64;
    root.set("Kids", all);
    root.set("Count", count);
    pdf.prune_objects();
    pdf.reference_table.cross_reference_type = lopdf::xref::XrefType::CrossReferenceStream;
    let mut out = Vec::new();
    pdf.save_to(&mut out).map_err(|e| e.to_string())?;
    Ok((out, base_n))
}
