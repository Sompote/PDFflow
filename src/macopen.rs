//! macOS: open the PDF the user double-clicked in Finder (or dropped on the Dock icon).
//!
//! Finder does not pass the file on the command line; it sends an "open documents" event that
//! AppKit forwards to the app delegate's `application:openURLs:`. winit's delegate does not
//! implement it, so AppKit answers "PDFflow cannot open files in the PDF document format".
//! We add the method to winit's delegate class before the event loop starts.

use eframe::egui;
use objc2::runtime::{AnyClass, AnyObject, Sel};
use objc2::{msg_send, sel};
use std::ffi::{CStr, c_char};
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

static PENDING: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());
static CTX: OnceLock<egui::Context> = OnceLock::new();

extern "C" fn open_urls(_this: &AnyObject, _cmd: Sel, _app: &AnyObject, urls: &AnyObject) {
    // SAFETY: AppKit passes an NSArray<NSURL *>; `path` returns an NSString or nil.
    unsafe {
        let n: usize = msg_send![urls, count];
        for i in 0..n {
            let url: *mut AnyObject = msg_send![urls, objectAtIndex: i];
            if url.is_null() {
                continue;
            }
            let path: *mut AnyObject = msg_send![url, path];
            if path.is_null() {
                continue;
            }
            let s: *const c_char = msg_send![path, UTF8String];
            if !s.is_null() {
                if let Ok(mut p) = PENDING.lock() {
                    p.push(PathBuf::from(CStr::from_ptr(s).to_string_lossy().into_owned()));
                }
            }
        }
    }
    if let Some(c) = CTX.get() {
        c.request_repaint();
    }
}

/// Add `application:openURLs:` to winit's app delegate. Call after the event loop is built
/// (which registers the delegate class) and before it runs.
pub fn install() {
    let Some(cls) = AnyClass::get("WinitApplicationDelegate") else { return };
    // SAFETY: the method has the `v@:@@` signature AppKit calls it with.
    unsafe {
        let imp: objc2::ffi::IMP = Some(std::mem::transmute::<extern "C" fn(&AnyObject, Sel, &AnyObject, &AnyObject), unsafe extern "C" fn()>(open_urls));
        objc2::ffi::class_addMethod(
            cls as *const AnyClass as *mut objc2::ffi::objc_class,
            sel!(application:openURLs:).as_ptr(),
            imp,
            c"v@:@@".as_ptr(),
        );
    }
}

/// Let the open handler wake the UI.
pub fn set_context(ctx: &egui::Context) {
    let _ = CTX.set(ctx.clone());
}

/// Files Finder asked to open since the last call.
pub fn take() -> Vec<PathBuf> {
    PENDING.lock().map(|mut p| std::mem::take(&mut *p)).unwrap_or_default()
}
