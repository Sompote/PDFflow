//! The tools PDFflow offers over MCP: names, descriptions and input schemas. The editor runs
//! them (`app/agent.rs`); the stdio bridge only needs this list.

use serde_json::{Map, Value, json};

/// Argument types, written compactly.
enum T {
    Str,
    Num,
    Int,
    Bool,
    /// [x0, y0, x1, y1] in points.
    Rect,
    /// [x, y] in points.
    Point,
    Pages,
    Strokes,
    Points,
    Enum(&'static [&'static str]),
}

struct Tool {
    name: &'static str,
    desc: &'static str,
    args: &'static [(&'static str, T, &'static str)],
    required: &'static [&'static str],
}

const PAGE: (&str, T, &str) = ("page", T::Int, "Page number, from 1.");
const INDEX: (&str, T, &str) = ("index", T::Int, "Object number on the page, from 0, as list_objects shows it.");
const COLORS: &str = "Colour: '#rrggbb', '#rrggbbaa' or a name (black, white, red, green, blue, yellow, orange, purple, gray).";

const TOOLS: &[Tool] = &[
    // ------------------------------------------------------------------------- document
    Tool { name: "open_pdf", desc: "Open a PDF file in PDFflow (replaces the open document). Returns document_info.", args: &[("path", T::Str, "Absolute path of the PDF (~ is the home folder)."), ("discard_changes", T::Bool, "Open even if the current document has unsaved changes (they are lost).")], required: &["path"] },
    Tool { name: "new_document", desc: "Start a new document with one blank page.", args: &[("width", T::Num, "Page width in points (default 595, A4)."), ("height", T::Num, "Page height in points (default 842, A4)."), ("discard_changes", T::Bool, "Discard unsaved changes of the current document.")], required: &[] },
    Tool { name: "save_pdf", desc: "Save the document as a PDF. Without a path it saves over the opened file. Original page content, links and fonts are kept; edits are added on top.", args: &[("path", T::Str, "Where to save (absolute path); required for a new document."), ("remove_signatures", T::Bool, "Allow saving an edited, digitally signed file, which removes its signatures.")], required: &[] },
    Tool { name: "export_pages", desc: "Write some pages (with their edits) to a new PDF file, e.g. to split a document. The open document is not changed.", args: &[("pages", T::Pages, "Page numbers in the order wanted, e.g. [3, 1, 2]."), ("path", T::Str, "Absolute path of the new PDF.")], required: &["pages", "path"] },
    Tool { name: "document_info", desc: "The open document: file, page count and sizes, current page, unsaved changes, objects per page, digital signatures, undo state.", args: &[], required: &[] },
    Tool { name: "undo", desc: "Undo the last change (as Edit › Undo).", args: &[], required: &[] },
    Tool { name: "redo", desc: "Redo the last undone change.", args: &[], required: &[] },
    // ----------------------------------------------------------------------------- view
    Tool { name: "go_to_page", desc: "Show a page in the window and make it the current page.", args: &[PAGE], required: &["page"] },
    Tool { name: "set_view", desc: "Change the window view: zoom, fit, the Page Grid or the theme.", args: &[("zoom_percent", T::Num, "Zoom in percent (10-800)."), ("fit", T::Enum(&["page", "width"]), "Fit the current page or the page width."), ("grid", T::Bool, "Show the Page Grid (all pages as thumbnails)."), ("theme", T::Enum(&["medium", "dark"]), "Interface theme.")], required: &[] },
    Tool { name: "set_tool", desc: "Choose the active tool in the window, for the user to continue with. Picture tools need a selected picture.", args: &[("tool", T::Enum(&["move", "hand", "select_text", "snapshot", "text", "brush", "highlighter", "pen", "freeform_pen", "curvature_pen", "add_anchor", "delete_anchor", "convert_point", "line", "arrow", "rectangle", "ellipse", "whiteout", "signature", "comment", "marquee", "lasso", "crop", "eyedropper", "healing", "paint_brush", "clone_stamp", "eraser", "gradient", "blur", "dodge"]), "Tool name.")], required: &["tool"] },
    Tool { name: "render_page", desc: "Look at a page: a PNG picture of it as PDFflow shows it, edits included. Use it to check layout before and after editing. Pictures are at most 1600 px for the reply; save_path keeps full size.", args: &[PAGE, ("dpi", T::Num, "Resolution (default 96)."), ("rect", T::Rect, "Only this area of the page, [x0, y0, x1, y1] in points."), ("save_path", T::Str, "Also save the picture here (.png or .jpg).")], required: &["page"] },
    // ----------------------------------------------------------------------------- text
    Tool { name: "get_text", desc: "The text of the PDF's pages (the text already in the file, in reading order, with line breaks). Scanned pages without a text layer return nothing.", args: &[("page", T::Int, "One page; omit for a range or all pages."), ("first_page", T::Int, "First page of a range."), ("last_page", T::Int, "Last page of a range.")], required: &[] },
    Tool { name: "get_text_lines", desc: "The lines of text on a page with their boxes [x0, y0, x1, y1] in points, to find where things are before placing or marking up.", args: &[PAGE], required: &["page"] },
    Tool { name: "find_text", desc: "Search the PDF's text. Returns each match with its page and boxes. Spaces in the query match line breaks too.", args: &[("query", T::Str, "Text to find."), ("page", T::Int, "Search one page only."), ("case_sensitive", T::Bool, "Match case (default false).")], required: &["query"] },
    Tool { name: "select_text", desc: "Select text in the window (as the Select Text tool does) by search, by area or the whole page, and return it.", args: &[PAGE, ("query", T::Str, "Select this text."), ("occurrence", T::Int, "Which match on the page, from 1 (default 1)."), ("rect", T::Rect, "Select the text inside this area instead."), ("copy", T::Bool, "Also copy it to the clipboard.")], required: &["page"] },
    Tool { name: "copy_text", desc: "Copy text to the system clipboard: the given text, or else the text selected in the window.", args: &[("text", T::Str, "Text to copy.")], required: &[] },
    Tool { name: "markup_text", desc: "Highlight, underline or strike through text of the PDF. Targets: every match of a query (or one occurrence), the text in an area, or else the current selection. Each line becomes one object.", args: &[("style", T::Enum(&["highlight", "underline", "strikeout"]), "Markup style."), ("query", T::Str, "Text to mark."), ("page", T::Int, "Limit to this page (needed with rect)."), ("occurrence", T::Int, "Only this match (from 1, counted over the searched pages); default all matches."), ("rect", T::Rect, "Mark the text inside this area."), ("color", T::Str, COLORS)], required: &["style"] },
    // ------------------------------------------------------------------ snapshot, clipboard
    Tool { name: "snapshot", desc: "Capture an area of a page (or the whole page), edits included, as a picture: it goes to the system clipboard and PDFflow's clipboard (paste places it), can be saved to a file, and is returned.", args: &[PAGE, ("rect", T::Rect, "Area [x0, y0, x1, y1] in points; default the whole page."), ("dpi", T::Num, "Resolution (default 150)."), ("save_path", T::Str, "Save the picture here (.png or .jpg)."), ("return_image", T::Bool, "Return the picture (default true).")], required: &["page"] },
    Tool { name: "paste", desc: "Paste PDFflow's clipboard (the last snapshot or copied object) onto a page.", args: &[("page", T::Int, "Page (default the current page)."), ("x", T::Num, "Centre x in points."), ("y", T::Num, "Centre y in points.")], required: &[] },
    Tool { name: "copy_object", desc: "Copy an object to PDFflow's clipboard (for paste).", args: &[PAGE, INDEX], required: &["page", "index"] },
    // ---------------------------------------------------------------------------- pages
    Tool { name: "insert_blank_page", desc: "Insert a blank page.", args: &[("after", T::Int, "Insert after this page (0 = at the start; default the current page)."), ("width", T::Num, "Width in points (default the current page's)."), ("height", T::Num, "Height in points.")], required: &[] },
    Tool { name: "duplicate_page", desc: "Duplicate a page with its edits; the copy follows it.", args: &[PAGE], required: &["page"] },
    Tool { name: "rotate_page", desc: "Rotate a page clockwise.", args: &[PAGE, ("degrees", T::Int, "90, 180, 270 (or -90).")], required: &["page", "degrees"] },
    Tool { name: "move_page", desc: "Move a page to a new position.", args: &[PAGE, ("to", T::Int, "Its new page number.")], required: &["page", "to"] },
    Tool { name: "delete_page", desc: "Delete a page.", args: &[PAGE], required: &["page"] },
    // -------------------------------------------------------------------------- objects
    Tool { name: "list_objects", desc: "The objects added on a page (text, shapes, ink, highlights, paths, pictures, comments) with their index, box and settings.", args: &[PAGE], required: &["page"] },
    Tool { name: "add_text", desc: "Type text on a page (real, selectable text in the Sarabun font: Thai and Latin). Use \\n for new lines; there is no automatic wrapping.", args: &[PAGE, ("x", T::Num, "Left edge in points."), ("y", T::Num, "Top edge in points."), ("text", T::Str, "The text."), ("size", T::Num, "Font size in points (default 14)."), ("color", T::Str, COLORS), ("bold", T::Bool, "Bold.")], required: &["page", "x", "y", "text"] },
    Tool { name: "add_shape", desc: "Draw a rectangle or ellipse (give rect) or a line or arrow (give from and to).", args: &[PAGE, ("shape", T::Enum(&["rectangle", "ellipse", "line", "arrow"]), "Shape."), ("rect", T::Rect, "Box for a rectangle or ellipse."), ("from", T::Point, "Start of a line or arrow."), ("to", T::Point, "End (the arrow head)."), ("stroke_color", T::Str, COLORS), ("fill_color", T::Str, "Fill colour (rectangle, ellipse); omit for none."), ("width", T::Num, "Stroke width in points (default 2; 0 for none).")], required: &["page", "shape"] },
    Tool { name: "add_whiteout", desc: "Cover an area with white (to hide content; type over it with add_text).", args: &[PAGE, ("rect", T::Rect, "Area to cover.")], required: &["page", "rect"] },
    Tool { name: "add_ink", desc: "Draw freehand ink strokes (or highlighter strokes, which multiply like a marker).", args: &[PAGE, ("strokes", T::Strokes, "Strokes, each a list of [x, y] points."), ("color", T::Str, COLORS), ("width", T::Num, "Width in points (default 2)."), ("highlighter", T::Bool, "Highlighter ink.")], required: &["page", "strokes"] },
    Tool { name: "add_path", desc: "Draw a vector path through points (as the Pen tools make), straight or smoothed, open or closed.", args: &[PAGE, ("points", T::Points, "Anchor points [[x, y], ...]."), ("closed", T::Bool, "Close the path."), ("smooth", T::Bool, "Smooth curve through the points."), ("stroke_color", T::Str, COLORS), ("fill_color", T::Str, "Fill colour for a closed path."), ("width", T::Num, "Stroke width (default 2).")], required: &["page", "points"] },
    Tool { name: "add_image", desc: "Place a picture file (PNG, JPEG, GIF, BMP, WebP) on a page.", args: &[PAGE, ("path", T::Str, "Absolute path of the picture."), ("rect", T::Rect, "Box to fill; or give x, y and width."), ("x", T::Num, "Left edge."), ("y", T::Num, "Top edge."), ("width", T::Num, "Width in points (height keeps the proportions).")], required: &["page", "path"] },
    Tool { name: "add_comment", desc: "Add a comment (sticky note); it is saved as a PDF comment that Acrobat and Preview show.", args: &[PAGE, ("x", T::Num, "Left of the note icon."), ("y", T::Num, "Top of the note icon."), ("text", T::Str, "Comment text."), ("author", T::Str, "Author (default the user's name)."), ("color", T::Str, COLORS)], required: &["page", "x", "y", "text"] },
    Tool { name: "place_signature", desc: "Place one of the user's saved drawn signatures on a page.", args: &[PAGE, ("x", T::Num, "Left edge."), ("y", T::Num, "Top edge."), ("width", T::Num, "Width in points (default 150)."), ("signature", T::Int, "Which saved signature, from 0 (default the selected one)."), ("color", T::Str, COLORS)], required: &["page", "x", "y"] },
    Tool { name: "update_object", desc: "Change an object: move or resize it, or change its text, size, colours or width.", args: &[PAGE, INDEX, ("rect", T::Rect, "New box (not for text, which sizes itself)."), ("x", T::Num, "Move the left edge here."), ("y", T::Num, "Move the top edge here."), ("text", T::Str, "New text (text objects and comments)."), ("size", T::Num, "Font size (text)."), ("bold", T::Bool, "Bold (text)."), ("color", T::Str, "Text, ink or comment colour."), ("stroke_color", T::Str, "Stroke colour (shapes, paths)."), ("fill_color", T::Str, "Fill colour, or 'none'."), ("width", T::Num, "Stroke width.")], required: &["page", "index"] },
    Tool { name: "delete_object", desc: "Delete an object.", args: &[PAGE, INDEX], required: &["page", "index"] },
    Tool { name: "arrange_object", desc: "Change an object's stacking order.", args: &[PAGE, INDEX, ("to", T::Enum(&["front", "back", "forward", "backward"]), "Where to.")], required: &["page", "index", "to"] },
    Tool { name: "duplicate_object", desc: "Duplicate an object, offset by dx, dy.", args: &[PAGE, INDEX, ("dx", T::Num, "Offset x (default 12)."), ("dy", T::Num, "Offset y (default 12).")], required: &["page", "index"] },
    Tool { name: "select_object", desc: "Select an object in the window (as clicking it with the Move tool does).", args: &[PAGE, INDEX], required: &["page", "index"] },
    // ------------------------------------------------------------------------- pictures
    Tool { name: "adjust_picture", desc: "Apply an Image › Adjustments command to a placed picture (one undo step). Parameters by adjustment: brightness_contrast {brightness -150..150, contrast -50..100}; levels {black 0-255, white 0-255, gamma 0.1-9.99}; exposure {exposure -20..20, offset, gamma}; vibrance {vibrance, saturation -100..100}; hue_saturation {hue -180..180, saturation, lightness -100..100, colorize}; posterize {levels 2-255}; threshold {level 1-255}; photo_filter {color, density 0-100}; gradient_map {from, to}; shadows_highlights {shadows 0-100, highlights 0-100}; black_white, invert, desaturate, equalize, hdr_toning, auto_tone, auto_contrast, auto_color take none.", args: &[PAGE, INDEX, ("adjustment", T::Enum(&["brightness_contrast", "levels", "exposure", "vibrance", "hue_saturation", "black_white", "photo_filter", "invert", "posterize", "threshold", "gradient_map", "shadows_highlights", "hdr_toning", "desaturate", "equalize", "auto_tone", "auto_contrast", "auto_color"]), "Adjustment."), ("brightness", T::Num, ""), ("contrast", T::Num, ""), ("black", T::Num, ""), ("white", T::Num, ""), ("gamma", T::Num, ""), ("exposure", T::Num, ""), ("offset", T::Num, ""), ("vibrance", T::Num, ""), ("saturation", T::Num, ""), ("hue", T::Num, ""), ("lightness", T::Num, ""), ("colorize", T::Bool, ""), ("levels", T::Int, ""), ("level", T::Num, ""), ("color", T::Str, ""), ("density", T::Num, ""), ("from", T::Str, "Gradient start colour."), ("to", T::Str, "Gradient end colour."), ("shadows", T::Num, ""), ("highlights", T::Num, "")], required: &["page", "index", "adjustment"] },
    Tool { name: "crop_picture", desc: "Crop a placed picture to an area of the page; the kept part stays where it was.", args: &[PAGE, INDEX, ("rect", T::Rect, "Area to keep, in page points.")], required: &["page", "index", "rect"] },
    Tool { name: "export_picture", desc: "Save a placed picture's pixels (with its edits) to a PNG or JPEG file.", args: &[PAGE, INDEX, ("path", T::Str, "Absolute path (.png or .jpg).")], required: &["page", "index", "path"] },
    // ------------------------------------------------------------------- comments, signing
    Tool { name: "list_comments", desc: "Every comment in the document: page, index, author, date, colour and text.", args: &[], required: &[] },
    Tool { name: "list_signatures", desc: "Digital signatures in the opened file and whether each is valid (content unchanged, signature matches, certificate trusted and current).", args: &[], required: &[] },
    Tool { name: "list_digital_ids", desc: "The user's Digital IDs (certificates) that can sign, with their expiry dates.", args: &[], required: &[] },
    Tool { name: "sign_pdf", desc: "Digitally sign the document with one of the user's Digital IDs and save the signed copy, which is then opened. Only do this when the user asked to sign and gave the password.", args: &[("digital_id", T::Int, "Which Digital ID, from 0 (list_digital_ids)."), ("password", T::Str, "The Digital ID's password."), ("output_path", T::Str, "Absolute path of the signed PDF."), ("reason", T::Str, "Reason for signing."), ("location", T::Str, "Location."), ("page", T::Int, "Page of a visible signature box."), ("rect", T::Rect, "Visible signature box; omit for an invisible signature.")], required: &["digital_id", "password", "output_path"] },
];

fn schema(t: &T, desc: &str) -> Value {
    let mut s = match t {
        T::Str => json!({"type": "string"}),
        T::Num => json!({"type": "number"}),
        T::Int => json!({"type": "integer"}),
        T::Bool => json!({"type": "boolean"}),
        T::Rect => json!({"type": "array", "items": {"type": "number"}, "minItems": 4, "maxItems": 4}),
        T::Point => json!({"type": "array", "items": {"type": "number"}, "minItems": 2, "maxItems": 2}),
        T::Pages => json!({"type": "array", "items": {"type": "integer"}}),
        T::Points => json!({"type": "array", "items": {"type": "array", "items": {"type": "number"}, "minItems": 2, "maxItems": 2}}),
        T::Strokes => json!({"type": "array", "items": {"type": "array", "items": {"type": "array", "items": {"type": "number"}, "minItems": 2, "maxItems": 2}}}),
        T::Enum(v) => json!({"type": "string", "enum": v}),
    };
    if !desc.is_empty() {
        s["description"] = json!(desc);
    }
    s
}

pub fn list() -> Vec<Value> {
    TOOLS
        .iter()
        .map(|t| {
            let props: Map<String, Value> = t.args.iter().map(|(n, ty, d)| ((*n).to_owned(), schema(ty, d))).collect();
            let read_only = matches!(t.name, "document_info" | "render_page" | "get_text" | "get_text_lines" | "find_text" | "list_objects" | "list_comments" | "list_signatures" | "list_digital_ids");
            json!({
                "name": t.name,
                "description": t.desc,
                "inputSchema": {"type": "object", "properties": props, "required": t.required},
                "annotations": {"readOnlyHint": read_only, "openWorldHint": false},
            })
        })
        .collect()
}

pub fn exists(name: &str) -> bool {
    TOOLS.iter().any(|t| t.name == name)
}
