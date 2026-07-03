//! Port of the data model inside `HierarchicalTableGenerator` (C2):
//! `Piece`, `Cell`, `Title`, `Row`, `TableModel`, `Counter`, and the icon /
//! indent constants. Source: HierarchicalTableGenerator.java (6.9.10-SNAPSHOT).
//!
//! Only the fields/methods reachable from the fragment render path are ported.
//! Markdown pieces (`addMarkdown`) delegate to a hook the caller supplies with
//! commonmark-parity strings (F3 stubs the exact golden strings; see render_sd).

use render_xhtml::XhtmlNode;

// Indent-level codes (HTG:120-125).
pub const NEW_REGULAR: i32 = 0;
pub const CONTINUE_REGULAR: i32 = 1;
pub const NEW_SLICER: i32 = 2;
pub const CONTINUE_SLICER: i32 = 3;
pub const NEW_SLICE: i32 = 4;
pub const CONTINUE_SLICE: i32 = 5;

pub const BACKGROUND_ALT_COLOR: &str = "#F7F7F7";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TableGenerationMode {
    Xml,
    Xhtml,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextAlignment {
    Left,
    Center,
    Right,
}

/// HTG.Counter (HTG:137).
pub struct Counter {
    count: i32,
}
impl Counter {
    pub fn new() -> Counter {
        Counter { count: -1 }
    }
    pub fn row(&mut self) {
        self.count += 1;
    }
    pub fn is_odd(&self) -> bool {
        self.count % 2 == 1
    }
}
impl Default for Counter {
    fn default() -> Self {
        Counter::new()
    }
}

/// HTG.Piece (HTG:146). A run of content inside a Cell.
#[derive(Debug, Clone, Default)]
pub struct Piece {
    pub tag: Option<String>,
    pub reference: Option<String>,
    pub text: Option<String>,
    pub hint: Option<String>,
    pub style: Option<String>,
    pub tag_img: Option<String>,
    /// insertion-ordered (key -> value) attribute list; `class` handled here.
    pub attributes: Option<Vec<(String, String)>>,
    pub children: Vec<XhtmlNode>,
    pub underived: bool,
}

impl Piece {
    /// `new Piece(String tag)` (HTG:157).
    pub fn tag(tag: impl Into<String>) -> Piece {
        Piece {
            tag: Some(tag.into()),
            ..Default::default()
        }
    }
    /// `new Piece(reference, text, hint)` (HTG:162). null-safe.
    pub fn ref_text(
        reference: Option<String>,
        text: Option<String>,
        hint: Option<String>,
    ) -> Piece {
        Piece {
            reference,
            text,
            hint,
            ..Default::default()
        }
    }
    /// null tag piece (a bare-text piece is `new Piece(null, text, null)`).
    pub fn plain_text(text: impl Into<String>) -> Piece {
        Piece {
            text: Some(text.into()),
            ..Default::default()
        }
    }

    pub fn get_tag(&self) -> Option<&str> {
        self.tag.as_deref()
    }
    pub fn get_reference(&self) -> Option<&str> {
        self.reference.as_deref()
    }
    pub fn get_text(&self) -> Option<&str> {
        self.text.as_deref()
    }
    pub fn get_hint(&self) -> Option<&str> {
        self.hint.as_deref()
    }
    pub fn get_style(&self) -> Option<&str> {
        self.style.as_deref()
    }
    pub fn get_tag_img(&self) -> Option<&str> {
        self.tag_img.as_deref()
    }

    pub fn set_hint(&mut self, hint: impl Into<String>) -> &mut Piece {
        self.hint = Some(hint.into());
        self
    }
    pub fn set_reference(&mut self, r: impl Into<String>) -> &mut Piece {
        self.reference = Some(r.into());
        self
    }
    pub fn set_text(&mut self, t: impl Into<String>) -> &mut Piece {
        self.text = Some(t.into());
        self
    }
    pub fn set_tag_img(&mut self, t: impl Into<String>) -> &mut Piece {
        self.tag_img = Some(t.into());
        self
    }

    /// `setStyle` (HTG:248).
    pub fn set_style(&mut self, style: impl Into<String>) -> &mut Piece {
        self.style = Some(style.into());
        self
    }
    /// `addStyle` (HTG:253): append with "; " separator.
    pub fn add_style(&mut self, style: &str) -> &mut Piece {
        self.style = Some(match self.style.take() {
            Some(s) => format!("{}; {}", s, style),
            None => style.to_string(),
        });
        self
    }

    /// `attr(name,value)` (HTG:283) — insertion-ordered put.
    pub fn attr(&mut self, name: &str, value: impl Into<String>) -> &mut Piece {
        let v = value.into();
        let list = self.attributes.get_or_insert_with(Vec::new);
        if let Some(slot) = list.iter_mut().find(|(k, _)| k == name) {
            slot.1 = v;
        } else {
            list.push((name.to_string(), v));
        }
        self
    }

    /// `setClass(role)` (HTG:236): append to `class` with a space.
    pub fn set_class(&mut self, role: &str) -> &mut Piece {
        let list = self.attributes.get_or_insert_with(Vec::new);
        if let Some(slot) = list.iter_mut().find(|(k, _)| k == "class") {
            slot.1 = format!("{} {}", slot.1, role);
        } else {
            list.push(("class".to_string(), role.to_string()));
        }
        self
    }

    /// `addToHint` (HTG:261).
    pub fn add_to_hint(&mut self, text: &str) {
        self.hint = Some(match self.hint.take() {
            None => text.to_string(),
            Some(h) => {
                let sep = if h.ends_with('.') || h.ends_with('?') {
                    " "
                } else {
                    ". "
                };
                format!("{}{}{}", h, sep, text)
            }
        });
    }

    pub fn has_children(&self) -> bool {
        !self.children.is_empty()
    }
    pub fn has_attributes(&self) -> bool {
        self.attributes.as_ref().map(|a| !a.is_empty()).unwrap_or(false)
    }
    pub fn add_html(&mut self, x: XhtmlNode) -> &mut Piece {
        self.children.push(x);
        self
    }
}

/// HTG.Cell (HTG:306).
#[derive(Debug, Clone)]
pub struct Cell {
    pub pieces: Vec<Piece>,
    pub cell_style: Option<String>,
    pub span: i32,
    pub inner_table: bool,
    pub alignment: TextAlignment,
    pub id: Option<String>,
}

impl Default for Cell {
    fn default() -> Self {
        Cell {
            pieces: Vec::new(),
            cell_style: None,
            span: 1,
            inner_table: false,
            alignment: TextAlignment::Left,
            id: None,
        }
    }
}

impl Cell {
    pub fn new() -> Cell {
        Cell::default()
    }
    /// `new Cell(prefix, reference, text, hint, suffix)` (HTG:317).
    pub fn with(
        prefix: Option<&str>,
        reference: Option<String>,
        text: Option<String>,
        hint: Option<String>,
        suffix: Option<&str>,
    ) -> Cell {
        let mut c = Cell::new();
        if let Some(p) = prefix {
            if !p.is_empty() {
                c.pieces.push(Piece::ref_text(None, Some(p.to_string()), None));
            }
        }
        c.pieces.push(Piece::ref_text(reference, text, hint));
        if let Some(s) = suffix {
            if !s.is_empty() {
                c.pieces.push(Piece::ref_text(None, Some(s.to_string()), None));
            }
        }
        c
    }

    pub fn add_piece(&mut self, p: Piece) -> &mut Cell {
        self.pieces.push(p);
        self
    }
    /// `addText(text)` (HTG:526) — returns index of the added piece.
    pub fn add_text(&mut self, text: impl Into<String>) -> usize {
        self.pieces.push(Piece::ref_text(None, Some(text.into()), None));
        self.pieces.len() - 1
    }
    /// `addStyle(style)` (HTG:489): apply to every piece.
    pub fn add_style(&mut self, style: &str) -> &mut Cell {
        for p in &mut self.pieces {
            p.add_style(style);
        }
        self
    }
    /// `addCellStyle` (HTG:495).
    pub fn add_cell_style(&mut self, style: &str) -> &mut Cell {
        self.cell_style = Some(match self.cell_style.take() {
            None => style.to_string(),
            Some(s) => format!("{}; {}", s, style),
        });
        self
    }
    /// `setStyle` (HTG:545) sets cellStyle directly.
    pub fn set_style(&mut self, style: impl Into<String>) -> &mut Cell {
        self.cell_style = Some(style.into());
        self
    }
    pub fn span(&mut self, v: i32) -> &mut Cell {
        self.span = v;
        self
    }
    pub fn center(&mut self) -> &mut Cell {
        self.alignment = TextAlignment::Center;
        self
    }
    pub fn set_id(&mut self, id: impl Into<String>) {
        self.id = Some(id.into());
    }
    pub fn add_to_hint(&mut self, text: &str) {
        for p in &mut self.pieces {
            p.add_to_hint(text);
        }
    }

    /// `addStyledText` (HTG:508).
    pub fn add_styled_text(
        &mut self,
        hint: Option<String>,
        alt: Option<String>,
        fg_color: Option<&str>,
        bg_color: Option<&str>,
        link: Option<String>,
        border: bool,
    ) -> usize {
        let mut p = Piece::ref_text(link, alt, hint);
        p.add_style("padding-left: 3px");
        p.add_style("padding-right: 3px");
        if border {
            p.add_style("border: 1px grey solid");
            p.add_style("font-weight: bold");
        }
        if let Some(fg) = fg_color {
            p.add_style(&format!("color: {}", fg));
            p.add_style(&format!("background-color: {}", bg_color.unwrap_or("")));
        } else {
            p.add_style("color: black");
            // Faithful port of the Java precedence bug (HTG:521):
            // `"background-color: "+bgColor != null ? bgColor : "white"` parses
            // as `("background-color: "+bgColor) != null ? bgColor : "white"`;
            // the left side is never null, so addStyle receives BARE bgColor —
            // which is Java null here, and addStyle string-appends it as the
            // literal "null" (golden: `color: black; null`).
            p.add_style(bg_color.unwrap_or("null"));
        }
        self.pieces.push(p);
        self.pieces.len() - 1
    }

    /// `addImg(icon,hint,link)` (HTG:565).
    pub fn add_img(&mut self, icon: &str, hint: Option<String>, link: Option<String>) -> usize {
        let mut p = Piece::tag("img");
        p.attr("src", icon);
        p.hint = hint;
        p.reference = link;
        self.pieces.push(p);
        self.pieces.len() - 1
    }
}

/// HTG.Title (HTG:594) extends Cell with width/filter/checkboxes.
#[derive(Debug, Clone)]
pub struct Title {
    pub cell: Cell,
    pub width: i32,
    pub filter: bool,
    /// insertion-ordered checkbox map (label -> role).
    pub checkboxes: Vec<(String, String)>,
}

impl Title {
    pub fn new(
        prefix: Option<&str>,
        reference: Option<String>,
        text: Option<String>,
        hint: Option<String>,
        suffix: Option<&str>,
        width: i32,
    ) -> Title {
        Title {
            cell: Cell::with(prefix, reference, text, hint, suffix),
            width,
            filter: false,
            checkboxes: Vec::new(),
        }
    }
    pub fn set_style(mut self, style: &str) -> Title {
        self.cell.set_style(style.to_string());
        self
    }
    pub fn put_checkbox(&mut self, label: &str, role: &str) {
        if let Some(slot) = self.checkboxes.iter_mut().find(|(k, _)| k == label) {
            slot.1 = role.to_string();
        } else {
            self.checkboxes.push((label.to_string(), role.to_string()));
        }
    }
}

/// HTG.Row (HTG:634).
#[derive(Debug, Clone, Default)]
pub struct Row {
    pub sub_rows: Vec<Row>,
    pub cells: Vec<Cell>,
    pub icon: Option<String>,
    pub anchor: Option<String>,
    pub hint: Option<String>,
    pub color: Option<String>,
    pub line_color: i32,
    pub id: Option<String>,
    pub opacity: Option<String>,
    pub top_line: Option<String>,
    pub partner_row: bool,
}

impl Row {
    pub fn new() -> Row {
        Row::default()
    }
    pub fn set_icon(&mut self, icon: impl Into<String>, hint: Option<String>) {
        self.icon = Some(icon.into());
        self.hint = hint;
    }
    pub fn set_anchor(&mut self, anchor: impl Into<String>) {
        self.anchor = Some(anchor.into());
    }
    pub fn set_color(&mut self, color: impl Into<String>) {
        self.color = Some(color.into());
    }
    pub fn set_line_color(&mut self, lc: i32) {
        assert!((0..=2).contains(&lc));
        self.line_color = lc;
    }
    pub fn set_id(&mut self, id: impl Into<String>) {
        self.id = Some(id.into());
    }
}

/// HTG.TableModel (HTG:711).
#[derive(Debug, Clone)]
pub struct TableModel {
    pub id: Option<String>,
    pub active: bool,
    pub titles: Vec<Title>,
    pub rows: Vec<Row>,
    pub doco_ref: Option<String>,
    pub doco_img: Option<String>,
    pub alternating: bool,
    pub show_headings: bool,
    pub border: bool,
    /// `HierarchicalTableGenerator.ACTIVE_TABLES` (HTG:127) — a static the
    /// publisher sets from the IG's `active-tables` parameter
    /// (PublisherIGLoader.java:443). Per-run config, so an instance field here.
    pub active_tables: bool,
}

impl TableModel {
    pub fn new(id: Option<String>, active: bool) -> TableModel {
        TableModel {
            id,
            active,
            titles: Vec::new(),
            rows: Vec::new(),
            doco_ref: None,
            doco_img: None,
            alternating: false,
            show_headings: true,
            border: false,
            active_tables: false,
        }
    }
    /// `isActive()` (HTG:752): `active && ACTIVE_TABLES`.
    pub fn is_active(&self) -> bool {
        self.active && self.active_tables
    }
}


