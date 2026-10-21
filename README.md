# PDFflow

![PDFflow with a journal paper open: tool column, options bar, Pages, Layers and Signatures panels](docs/screenshot.png)

PDFflow is a small PDF editor written in Rust, with a Photoshop-style interface ported from
[PhotoCraft](https://github.com/storytold/photocraft): the Pro Medium Gray / Pro Dark palettes,
tab-strip panels, panel footers, Spectrum buttons and check boxes, and Lucide icons.

## What it does

- **Read** any PDF: pages render on a background thread with [hayro](https://github.com/LaurenzV/hayro)
  (pure Rust, no native libraries), only for pages on screen.
- **Edit like Photoshop**: everything you add is an object on the page that you can select, move,
  resize with handles, restack (Layers panel), copy, paste, duplicate and delete, with undo/redo.
- **Pages**: insert blank pages, duplicate, rotate, reorder and delete (Pages panel, Page menu,
  or right-click a thumbnail).
- **Pictures**: drag PNG/JPEG/GIF/BMP/WebP files from Finder onto a page, or File › Place Picture.
  Corner handles keep the proportions (hold Shift to free them).
- **Text tool**: real, selectable text in an embedded font (Sarabun, so Thai and Latin both work),
  shaped with the same engine egui uses on screen, so the saved PDF matches what you see.
- **Drawing tools**: pen, highlighter, line, arrow, rectangle, ellipse, and whiteout (cover
  existing content with white, then type on top).
- **Signatures**: draw a signature once (Signatures panel › +), it is saved for reuse; click a page
  with the Signature tool to place it, then move and resize it.

Saving rebuilds the page tree from the untouched source file and adds one content stream per
edited page, so the original content, links and fonts are kept. JPEGs are embedded unchanged.

## Build and run

```sh
cargo build --release
./target/release/pdfeditor [file.pdf]
```

## Shortcuts

| Key | Tool | Key | Action |
|---|---|---|---|
| V | Move | ⌘O / ⌘S / ⇧⌘S | Open / Save / Save As |
| H (or hold Space) | Hand | ⌘Z / ⇧⌘Z | Undo / Redo |
| T | Text | ⌘C / ⌘V / ⌘D | Copy / Paste / Duplicate |
| P | Pen | ⌫ | Delete selection |
| K | Highlighter | Arrows (⇧ ×10) | Nudge selection |
| L / A | Line / Arrow | ⌘+ / ⌘− | Zoom (also ⌘-scroll, pinch) |
| R / E | Rectangle / Ellipse | ⌘0 / ⌘1 | Fit page / 100 % |
| W | Whiteout | | |
| S | Signature | | |

## Small and light

- egui on the glow (OpenGL) renderer, no web view; repaints only on input.
- Rendered page textures are capped at 4096 px per side and 12 MP, kept only for visible pages;
  thumbnails are capped at 40. Placed pictures keep a ≤1024 px preview on screen and their
  original file bytes for the PDF.
- Icons are Lucide SVGs flattened to polylines at build time (`tools/icons_to_rust.py`), so no
  SVG renderer is linked. Fonts: Inter (UI) and Sarabun (document text), both OFL.

## Limits

- Existing PDF text is not edited in place; cover it with Whiteout and type over it.
- Encrypted (password-protected) PDFs cannot be opened.
- Text is set without automatic line wrapping (press Return for a new line).

## Tests

`cargo test` runs round trips (save, re-render with hayro, check pixels for every page rotation and
crop box, embedded text, pictures, ink, page reordering) and interaction tests that drive the real
window headlessly with egui_kittest (drawing, typing, moving, resizing, signatures, undo).

## Licences

Code: MIT OR Apache-2.0. Theme tokens and widgets adapted from PhotoCraft (MIT OR Apache-2.0).
Icons: Lucide (ISC). Fonts: Inter and Sarabun (SIL OFL 1.1, see `assets/fonts`).
