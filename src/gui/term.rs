//! xterm-compatible terminal view: `vt100` parser state + egui painter rendering.
//!
//! Handles colours (16 / 256 / truecolor), cursor, alternate screen (vim, htop,
//! less …), scrollback, mouse selection + copy, bracketed paste, application
//! cursor keys and basic mouse reporting.

use super::theme::TERM_BOLD;
use eframe::egui::{
    self, Align2, Color32, CornerRadius, FontFamily, FontId, Pos2, Rect, Sense, Stroke, Vec2,
};

/// Plain-text log cap (used for search / copy-all / save).
const LOG_MAX: usize = 2 * 1024 * 1024;
/// Inner padding between the frame and the character grid.
pub const TERM_PAD: f32 = 12.0;

#[derive(Default)]
pub struct TermCallbacks {
    pub title: Option<String>,
    pub bell: bool,
}

impl vt100::Callbacks for TermCallbacks {
    fn audible_bell(&mut self, _: &mut vt100::Screen) {
        self.bell = true;
    }
    fn set_window_title(&mut self, _: &mut vt100::Screen, title: &[u8]) {
        let t = String::from_utf8_lossy(title).trim().to_string();
        self.title = if t.is_empty() { None } else { Some(t) };
    }
}

/// Grid position. Field order matters: ordering is row-major.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
struct CellPos {
    row: u16,
    col: u16,
}

/// A selection end in *content* coordinates: `row` counts lines relative to
/// the top of the live screen (negative = scrollback), so the selection stays
/// on the same text while the view scrolls.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
struct SelPos {
    row: i64,
    col: u16,
}

#[derive(Clone)]
pub struct TermPalette {
    pub fg: Color32,
    pub bg: Color32,
    pub cursor: Color32,
    pub selection: Color32,
    pub ansi: [Color32; 16],
}

#[derive(Default)]
enum EscState {
    #[default]
    Normal,
    Esc,
    Csi,
    Osc,
    OscEsc,
    /// `ESC (`, `ESC )` … : one more character (the charset) follows.
    EscArg,
}

/// One piece of user input, in the order it was typed.
pub enum TermInput {
    /// Keystrokes, already encoded.
    Keys(Vec<u8>),
    /// Pasted text, not yet encoded: each receiving host encodes it for its
    /// own mode (bracketed paste or not), see [`TermView::encode_paste`].
    Paste(String),
}

/// Settings: plain Ctrl+V goes to full-screen apps (vim block selection)
/// instead of pasting there. Off by default (Ctrl+V always pastes, like most
/// terminals); Ctrl+Shift+V / Shift+Insert always paste.
pub static CTRL_V_TO_APPS: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// How long to wait for the release that tells Ctrl+C from Ctrl+Insert.
const PENDING_COPY_WAIT: f64 = 0.35;

/// A key released this frame while Command/Ctrl was still held.
fn released_with_command(ui: &egui::Ui, key: egui::Key) -> bool {
    ui.input(|i| {
        i.events.iter().any(|e| {
            matches!(e, egui::Event::Key {
                key: k,
                pressed: false,
                modifiers,
                ..
            } if *k == key && modifiers.command && !modifiers.shift)
        })
    })
}

/// What the terminal widget wants the app to do after this frame.
#[derive(Default)]
pub struct TermOutput {
    /// Keystrokes and pastes for the remote PTY, in order (safe to mirror to
    /// other hosts).
    pub input: Vec<TermInput>,
    /// Bytes that only make sense for this terminal (mouse reports, wheel ->
    /// arrows in full-screen apps, redraw after Clear). Never mirrored.
    pub local_bytes: Vec<u8>,
    /// New PTY size (cols, rows) when the widget was resized.
    pub resized: Option<(u16, u16)>,
    /// Ctrl+wheel font size change.
    pub font_delta: f32,
    /// Text placed on the clipboard (for a toast).
    pub copied: bool,
    /// User asked to open the search bar (context menu).
    pub open_find: bool,
    /// User asked to open a detected URL (`Ctrl+Click` or context menu).
    pub open_url: Option<String>,
}

impl TermOutput {
    fn keys(&mut self, b: &[u8]) {
        if let Some(TermInput::Keys(last)) = self.input.last_mut() {
            last.extend_from_slice(b);
        } else if !b.is_empty() {
            self.input.push(TermInput::Keys(b.to_vec()));
        }
    }

    /// The input as bytes for one receiving terminal (pastes encoded with
    /// `encode`, i.e. that terminal's [`TermView::encode_paste`]).
    pub fn input_bytes(&self, encode: impl Fn(&str) -> Vec<u8>) -> Vec<u8> {
        let mut out = Vec::new();
        for part in &self.input {
            match part {
                TermInput::Keys(b) => out.extend_from_slice(b),
                TermInput::Paste(t) => out.extend(encode(t)),
            }
        }
        out
    }
}

pub struct TermView {
    parser: vt100::Parser<TermCallbacks>,
    log: String,
    /// Bytes dropped from the front of `log` so far (keeps offsets into
    /// it valid across trims).
    log_trimmed: u64,
    log_gen: u64,
    log_esc: EscState,
    log_utf8: Vec<u8>,
    log_cr: bool,
    sel_anchor: Option<SelPos>,
    sel_head: Option<SelPos>,
    /// PTY size last reported to the server, and a size waiting to settle.
    sent_size: (u16, u16),
    pending_size: Option<(u16, u16, f64)>,
    sel_text: String,
    dragging: bool,
    wheel_accum: f32,
    /// Ctrl+wheel steps not yet turned into a font size change (touchpads
    /// send many small deltas).
    zoom_accum: f32,
    /// Mouse buttons (bit 0 left, 1 middle, 2 right) pressed in the terminal
    /// and reported to the application, and the cell last reported.
    mouse_buttons: u8,
    mouse_last_cell: Option<CellPos>,
    /// The log just dropped a wide character for one backspace: the next
    /// backspace belongs to the same character.
    log_bs_wide: bool,
    /// The context menu is open (it then stays usable in mouse-reporting apps).
    menu_open: bool,
    /// IME composition text (shown inline at the cursor).
    preedit: String,
    /// Scrollbar visibility 0..1 (fades in on hover / while scrolled back).
    sb_alpha: f32,
    /// Windows: Ctrl+C and Ctrl+Insert both arrive as "copy" with no key
    /// event; which one it was is known only when the key is released.
    /// (count, time of the first) of such copies waiting for that.
    pending_copy: (u32, f64),
    /// Scrollback capacity (lines) this terminal was created with.
    scrollback_cap: usize,
    /// While dragging a selection at the scrollback limit: the anchor's
    /// surrounding text and an estimate of how far output moved it.
    anchor_resync: Option<(Vec<String>, usize)>,
    /// Per visible row: (fingerprint, shapes at the row origin).
    row_cache: Vec<Option<RowPaint>>,
    /// Geometry / palette the row cache was built for.
    row_cache_geo: u64,
    /// Currently hovered URL under Ctrl/Cmd `(row, start_col, end_col_excl, url)`.
    hover_url: Option<(u16, u16, u16, String)>,
    /// URL captured when opening the context menu.
    ctx_url: Option<String>,
    /// Captures OSC payloads (`ESC ] ... BEL/ST`) for OSC 7 directory tracking.
    osc_is_osc: bool,
    osc_buf: String,
    osc7_cwd: Option<String>,
}

impl TermView {
    pub fn new(rows: u16, cols: u16, scrollback: usize) -> Self {
        Self {
            parser: vt100::Parser::new_with_callbacks(
                rows.max(2),
                cols.max(10),
                scrollback,
                TermCallbacks::default(),
            ),
            log: String::new(),
            log_trimmed: 0,
            log_gen: 0,
            log_esc: EscState::Normal,
            log_utf8: Vec::new(),
            log_cr: false,
            sel_anchor: None,
            sel_head: None,
            sent_size: (0, 0),
            pending_size: None,
            sel_text: String::new(),
            dragging: false,
            wheel_accum: 0.0,
            zoom_accum: 0.0,
            mouse_buttons: 0,
            mouse_last_cell: None,
            log_bs_wide: false,
            menu_open: false,
            preedit: String::new(),
            sb_alpha: 0.0,
            pending_copy: (0, 0.0),
            scrollback_cap: scrollback,
            anchor_resync: None,
            row_cache: Vec::new(),
            row_cache_geo: 0,
            hover_url: None,
            ctx_url: None,
            osc_is_osc: false,
            osc_buf: String::new(),
            osc7_cwd: None,
        }
    }

    /// Latest remote working directory reported via OSC 7, if any.
    pub fn take_osc7_cwd(&mut self) -> Option<String> {
        self.osc7_cwd.take()
    }

    fn finish_osc(&mut self) {
        self.osc_is_osc = false;
        if let Some(cwd) = parse_osc7_cwd(&self.osc_buf) {
            self.osc7_cwd = Some(cwd);
        }
        self.osc_buf.clear();
    }

    fn url_at_cell(&self, row: u16, col: u16) -> Option<(u16, u16, String)> {
        let screen = self.parser.screen();
        let (rows, cols) = screen.size();
        if row >= rows || col >= cols {
            return None;
        }
        let mut cells: Vec<(u16, &str)> = Vec::with_capacity(cols as usize);
        for c in 0..cols {
            if let Some(cell) = screen.cell(row, c) {
                if !cell.is_wide_continuation() {
                    let s = cell.contents();
                    cells.push((c, if s.is_empty() { " " } else { s }));
                }
            } else {
                cells.push((c, " "));
            }
        }
        find_url_in_line(&cells, col)
    }

    /// Reset emulator state and log (new connection in a reused tab), with
    /// the current scrollback setting.
    pub fn reset(&mut self, scrollback: usize) {
        let (rows, cols) = self.parser.screen().size();
        *self = Self::new(rows, cols, scrollback);
    }

    /// Lines currently held in the scrollback.
    fn scrollback_len(&mut self) -> usize {
        let screen = self.parser.screen_mut();
        let offset = screen.scrollback();
        screen.set_scrollback(usize::MAX); // clamps to the length
        let len = screen.scrollback();
        screen.set_scrollback(offset);
        len
    }

    pub fn size(&self) -> (u16, u16) {
        let (rows, cols) = self.parser.screen().size();
        (cols, rows)
    }

    pub fn has_sent_size(&self) -> bool {
        self.sent_size != (0, 0)
    }

    pub fn title(&self) -> Option<&str> {
        self.parser.callbacks().title.as_deref()
    }

    pub fn take_bell(&mut self) -> bool {
        std::mem::take(&mut self.parser.callbacks_mut().bell)
    }

    pub fn is_empty(&self) -> bool {
        self.log.is_empty()
    }

    pub fn log(&self) -> &str {
        &self.log
    }

    /// See `log_trimmed` (field).
    pub fn log_trimmed(&self) -> u64 {
        self.log_trimmed
    }

    /// Changes whenever the log text changes (for caching searches).
    pub fn log_gen(&self) -> u64 {
        self.log_gen
    }

    /// Clear the visible screen and scrollback (local only).
    pub fn clear_screen(&mut self) {
        self.parser.process(b"\x1b[H\x1b[2J\x1b[3J");
        let (rows, cols) = self.parser.screen().size();
        // vt100 keeps scrollback on ESC[3J; rebuild a fresh grid but keep modes simple.
        let title = self.parser.callbacks().title.clone();
        // Keep the program's input modes (bracketed paste, app cursor, mouse...).
        let modes = self.parser.screen().input_mode_formatted();
        // input_mode_formatted() leaves out the alternate screen: a full-screen
        // app (htop, vim) must stay on it, or its next redraw lands on the
        // main screen.
        let alternate = self.parser.screen().alternate_screen();
        self.parser = vt100::Parser::new_with_callbacks(
            rows,
            cols,
            self.scrollback_cap,
            TermCallbacks::default(),
        );
        if alternate {
            self.parser.process(b"\x1b[?1049h");
        }
        self.parser.process(&modes);
        self.parser.callbacks_mut().title = title;
        self.clear_selection();
    }

    /// Feed remote PTY output.
    pub fn feed(&mut self, bytes: &[u8]) {
        // Keep a selection anchored on the same text as new lines push the
        // content up (both while dragging and once finished, so copy / Ctrl+C
        // still work during ongoing output).
        let track = self.sel_anchor.is_some();
        let before = if track { self.scrollback_len() } else { 0 };
        if track && self.anchor_resync.is_none() {
            // Near or at the scrollback limit its length stops telling how
            // far content moved: note the text around the anchor (rows at
            // and above it, which new output can't overwrite) to find it
            // again once this frame's output is in.
            let lf = bytes.iter().filter(|b| **b == b'\n').count();
            let rows = self.parser.screen().size().0 as usize;
            if before + lf + rows >= self.scrollback_cap {
                if let Some(a) = self.sel_anchor {
                    let sig = (0..3)
                        .rev()
                        .filter_map(|k| self.abs_row_text(a.row - k))
                        .collect::<Vec<_>>();
                    self.anchor_resync = Some((sig, 0));
                }
            }
        }
        self.parser.process(bytes);
        if track {
            let after = self.scrollback_len();
            match self.anchor_resync.as_mut() {
                // Resolved once per frame in `show` (see `resync_anchor`).
                Some((_, estimate)) => {
                    *estimate += (after.saturating_sub(before))
                        .max(bytes.iter().filter(|b| **b == b'\n').count());
                }
                // Below the limit the scrollback grows by exactly the number
                // of lines that scrolled off (0 in full-screen apps).
                None => {
                    let delta = after.saturating_sub(before) as i64;
                    if delta != 0 {
                        if let Some(a) = self.sel_anchor.as_mut() {
                            a.row -= delta;
                        }
                        if !self.dragging {
                            if let Some(h) = self.sel_head.as_mut() {
                                h.row -= delta;
                            }
                        }
                    }
                }
            }
        }
        self.append_log_bytes(bytes);
        if !self.dragging
            && self.sel_anchor.is_some()
            && self.anchor_resync.is_none()
            && (self.sel_text.is_empty() || self.current_sel_text() != self.sel_text)
        {
            // Keep the copied snapshot, but drop the now-stale highlight.
            self.sel_anchor = None;
            self.sel_head = None;
        }
    }

    /// Print a local status line (dimmed) into the terminal.
    pub fn feed_notice(&mut self, text: &str, color_code: u8) {
        // CAN aborts any escape sequence the output left unfinished (e.g.
        // cut off by a gap), so the notice itself isn't swallowed.
        let mut s = String::from("\x18");
        let (_, col) = self.parser.screen().cursor_position();
        if col > 0 {
            s.push_str("\r\n");
        }
        s.push_str(&format!("\x1b[{color_code}m{text}\x1b[0m\r\n"));
        self.parser.process(s.as_bytes());
        // Same for the log: an unfinished sequence or UTF-8 sequence there
        // must not swallow the notice; it starts on a line of its own.
        self.log_esc = EscState::Normal;
        self.osc_is_osc = false;
        self.osc_buf.clear();
        self.log_utf8.clear();
        self.log_cr = false;
        let mut line = String::new();
        if !self.log.is_empty() && !self.log.ends_with('\n') {
            line.push('\n');
        }
        line.push_str(text);
        line.push('\n');
        self.append_log_text(&line);
    }

    fn append_log_bytes(&mut self, bytes: &[u8]) {
        self.log_utf8.extend_from_slice(bytes);
        let valid = match std::str::from_utf8(&self.log_utf8) {
            Ok(_) => self.log_utf8.len(),
            Err(e) if e.error_len().is_none() => e.valid_up_to(),
            Err(_) => {
                // Invalid sequence: decode lossily and move on.
                let text = String::from_utf8_lossy(&self.log_utf8).into_owned();
                self.log_utf8.clear();
                self.append_log_text(&text);
                return;
            }
        };
        let rest = self.log_utf8.split_off(valid);
        let text =
            String::from_utf8(std::mem::replace(&mut self.log_utf8, rest)).unwrap_or_default();
        self.append_log_text(&text);
    }

    fn append_log_text(&mut self, text: &str) {
        for ch in text.chars() {
            match self.log_esc {
                EscState::Esc => {
                    self.log_esc = match ch {
                        '[' => EscState::Csi,
                        // OSC, DCS, APC, PM, SOS: strings ended by BEL / ST.
                        ']' => {
                            self.osc_is_osc = true;
                            self.osc_buf.clear();
                            EscState::Osc
                        }
                        'P' | '_' | '^' | 'X' => {
                            self.osc_is_osc = false;
                            self.osc_buf.clear();
                            EscState::Osc
                        }
                        '(' | ')' | '*' | '+' | '#' | '%' => EscState::EscArg,
                        _ => EscState::Normal,
                    };
                    continue;
                }
                EscState::EscArg => {
                    self.log_esc = EscState::Normal;
                    continue;
                }
                EscState::Csi => {
                    if ('@'..='~').contains(&ch) {
                        self.log_esc = EscState::Normal;
                    }
                    continue;
                }
                EscState::Osc => {
                    if ch == '\x07' {
                        if self.osc_is_osc {
                            self.finish_osc();
                        }
                        self.log_esc = EscState::Normal;
                    } else if ch == '\x1b' {
                        self.log_esc = EscState::OscEsc;
                    } else if self.osc_is_osc && self.osc_buf.len() < 2048 {
                        self.osc_buf.push(ch);
                    }
                    continue;
                }
                EscState::OscEsc => {
                    if ch == '\\' {
                        if self.osc_is_osc {
                            self.finish_osc();
                        }
                        self.log_esc = EscState::Normal;
                    } else {
                        self.log_esc = EscState::Osc;
                    }
                    continue;
                }
                EscState::Normal => {}
            }
            if ch != '\x08' {
                self.log_bs_wide = false;
            }
            match ch {
                '\x1b' => self.log_esc = EscState::Esc,
                '\r' => self.log_cr = true,
                '\n' => {
                    self.log_cr = false;
                    self.log.push('\n');
                }
                '\x08' => {
                    // A backspace moves back one column: a wide (CJK)
                    // character takes two of them.
                    if std::mem::take(&mut self.log_bs_wide) {
                    } else if !self.log.ends_with('\n') {
                        if let Some(c) = self.log.pop() {
                            self.log_bs_wide = unicode_width::UnicodeWidthChar::width(c) == Some(2);
                        }
                    }
                    continue;
                }
                '\t' => self.push_log_char('\t'),
                c if (c as u32) < 0x20 || c == '\x7f' => {}
                c => self.push_log_char(c),
            }
        }
        self.log_gen = self.log_gen.wrapping_add(1);
        // Trim in batches (drop 256 KB at a time) instead of shifting the
        // whole 2 MB buffer on every chunk of output.
        if self.log.len() > LOG_MAX + 256 * 1024 {
            let mut cut = self.log.len() - LOG_MAX;
            while !self.log.is_char_boundary(cut) {
                cut += 1;
            }
            let extra = self.log[cut..].find('\n').map(|i| i + 1).unwrap_or(0);
            self.log.drain(..cut + extra);
            self.log_trimmed += (cut + extra) as u64;
        }
    }

    fn push_log_char(&mut self, c: char) {
        if self.log_cr {
            // Bare carriage return: the line is being redrawn.
            let start = self.log.rfind('\n').map(|i| i + 1).unwrap_or(0);
            self.log.truncate(start);
            self.log_cr = false;
        }
        self.log.push(c);
    }

    /// A visible selection exists (so Ctrl+C copies instead of interrupting).
    pub fn has_selection(&self) -> bool {
        self.sel_range().is_some() && !self.sel_text.is_empty()
    }

    fn to_sel(&self, c: CellPos) -> SelPos {
        SelPos {
            row: c.row as i64 - self.parser.screen().scrollback() as i64,
            col: c.col,
        }
    }

    pub fn clear_selection(&mut self) {
        self.sel_anchor = None;
        self.sel_head = None;
        self.sel_text.clear();
    }

    pub fn select_all_visible(&mut self) {
        let (rows, cols) = self.parser.screen().size();
        self.sel_anchor = Some(self.to_sel(CellPos { row: 0, col: 0 }));
        self.sel_head = Some(self.to_sel(CellPos {
            row: rows - 1,
            col: cols,
        }));
        self.update_sel_text();
    }

    fn sel_range(&self) -> Option<(SelPos, SelPos)> {
        let (a, b) = (self.sel_anchor?, self.sel_head?);
        if a == b {
            return None;
        }
        Some(if a < b { (a, b) } else { (b, a) })
    }

    fn current_sel_text(&mut self) -> String {
        let Some((s, e)) = self.sel_range() else {
            return String::new();
        };
        let screen = self.parser.screen_mut();
        let (rows, cols) = screen.size();
        let saved = screen.scrollback();
        let mut out = String::new();
        let mut line = String::new();
        for abs in s.row..=e.row {
            // Bring `abs` on screen: live rows need no scrollback.
            let want = if abs < 0 { (-abs) as usize } else { 0 };
            screen.set_scrollback(want);
            let sb = screen.scrollback() as i64; // clamped to what exists
            let row = abs + sb;
            if row < 0 || row >= rows as i64 {
                continue;
            }
            let row = row as u16;
            let c0 = if abs == s.row { s.col } else { 0 };
            let c1 = if abs == e.row { e.col } else { cols };
            line.push_str(&screen.contents_between(row, c0, row, c1));
            // A soft-wrapped row continues on the next one without a newline.
            if abs != e.row && !screen.row_wrapped(row) {
                out.push_str(line.trim_end());
                out.push('\n');
                line.clear();
            }
        }
        out.push_str(line.trim_end());
        screen.set_scrollback(saved);
        out
    }

    /// Copy the selected text. The selection may reach into scrollback that
    /// is no longer on screen, so each line is read by scrolling the view to
    /// it (the original view is restored afterwards).
    fn update_sel_text(&mut self) {
        self.sel_text = self.current_sel_text();
    }

    /// Text of a row by absolute position (negative = scrollback).
    fn abs_row_text(&mut self, abs: i64) -> Option<String> {
        let screen = self.parser.screen_mut();
        let (rows, cols) = screen.size();
        let saved = screen.scrollback();
        screen.set_scrollback(if abs < 0 { (-abs) as usize } else { 0 });
        let row = abs + screen.scrollback() as i64;
        let text = (0..rows as i64)
            .contains(&row)
            .then(|| screen.contents_between(row as u16, 0, row as u16, cols));
        screen.set_scrollback(saved);
        text
    }

    /// Apply a pending anchor move (see `feed`): once per frame.
    fn resync_anchor(&mut self) {
        let Some((sig, estimate)) = self.anchor_resync.take() else {
            return;
        };
        let shift = self.find_shift(&sig, estimate).unwrap_or(estimate) as i64;
        if let Some(a) = self.sel_anchor.as_mut() {
            a.row -= shift;
        }
        if !self.dragging {
            if let Some(h) = self.sel_head.as_mut() {
                h.row -= shift;
            }
            if self.sel_anchor.is_some()
                && (self.sel_text.is_empty() || self.current_sel_text() != self.sel_text)
            {
                self.sel_anchor = None;
                self.sel_head = None;
            }
        }
    }

    /// How many lines the selection anchor moved, found by locating its
    /// text (`sig`: the two rows above the anchor and the anchor row) near
    /// `estimate`.
    fn find_shift(&mut self, sig: &[String], estimate: usize) -> Option<usize> {
        let anchor = self.sel_anchor?.row;
        if sig.is_empty() || sig.iter().all(|l| l.trim().is_empty()) {
            return None;
        }
        let max = (estimate * 2 + 50).min(400);
        let n = sig.len() as i64;
        let matches = |me: &mut Self, s: usize| {
            sig.iter().enumerate().all(|(k, l)| {
                me.abs_row_text(anchor - s as i64 - (n - 1) + k as i64)
                    .as_deref()
                    == Some(l)
            })
        };
        // Closest to the estimate first.
        for d in 0..=max {
            for s in [estimate.checked_add(d), estimate.checked_sub(d)]
                .into_iter()
                .flatten()
            {
                if s <= max && matches(self, s) {
                    return Some(s);
                }
            }
        }
        None
    }

    fn word_bounds(&self, row: u16, col: u16) -> (u16, u16) {
        let screen = self.parser.screen();
        let (_, cols) = screen.size();
        let word_cell = |c: u16| {
            screen.cell(row, c).is_some_and(|cell| {
                let s = cell.contents();
                !s.is_empty()
                    && !s.chars().all(|ch| {
                        ch.is_whitespace()
                            || "\"'`()[]{}<>|,;，。、；：？！“”‘’（）【】《》「」『』—…"
                                .contains(ch)
                    })
            })
        };
        // The right half of a wide (CJK) character has no contents of its own.
        let is_word = |c: u16| {
            if screen
                .cell(row, c)
                .is_some_and(|cell| cell.is_wide_continuation())
            {
                c > 0 && word_cell(c - 1)
            } else {
                word_cell(c)
            }
        };
        if !is_word(col) {
            return (col, col + 1);
        }
        let mut start = col;
        while start > 0 && is_word(start - 1) {
            start -= 1;
        }
        let mut end = col + 1;
        while end < cols && is_word(end) {
            end += 1;
        }
        (start, end)
    }

    /// Encode text for the PTY (newline → CR, bracketed paste when requested).
    pub fn encode_paste(&self, text: &str) -> Vec<u8> {
        let normalized = text.replace("\r\n", "\r").replace('\n', "\r");
        if self.parser.screen().bracketed_paste() {
            // Pasted text must not be able to end the paste early and run as typed input.
            let normalized = normalized.replace('\x1b', "");
            let mut v = b"\x1b[200~".to_vec();
            v.extend_from_slice(normalized.as_bytes());
            v.extend_from_slice(b"\x1b[201~");
            v
        } else {
            normalized.into_bytes()
        }
    }

    fn scroll_view(&mut self, delta_lines: i32) {
        let screen = self.parser.screen_mut();
        let cur = screen.scrollback() as i64;
        let next = (cur + delta_lines as i64).max(0) as usize;
        screen.set_scrollback(next);
    }

    pub fn scrollback_offset(&self) -> usize {
        self.parser.screen().scrollback()
    }

    pub fn scroll_to_bottom(&mut self) {
        self.parser.screen_mut().set_scrollback(0);
    }

    /// Resize the grid. When it loses rows on the main screen, the content
    /// first scrolls up (into the scrollback) so the cursor line — usually the
    /// prompt — stays on screen instead of being cut off at the bottom.
    fn resize_grid(&mut self, rows: u16, cols: u16) {
        let screen = self.parser.screen();
        let (old_rows, _) = screen.size();
        if rows < old_rows && !screen.alternate_screen() {
            let (cr, _) = screen.cursor_position();
            if cr >= rows {
                let n = cr - rows + 1;
                self.parser
                    .process(format!("\x1b[{n}S\x1b[{n}A").as_bytes());
            }
        }
        self.parser.screen_mut().set_size(rows, cols);
    }

    fn mouse_report(&self, button: u8, pressed: bool, pos: CellPos) -> Vec<u8> {
        let screen = self.parser.screen();
        match screen.mouse_protocol_encoding() {
            vt100::MouseProtocolEncoding::Sgr => format!(
                "\x1b[<{};{};{}{}",
                button,
                pos.col + 1,
                pos.row + 1,
                if pressed { 'M' } else { 'm' }
            )
            .into_bytes(),
            _ => {
                // X10 releases don't say which button, only the modifiers.
                let b = if pressed || button >= 64 {
                    button
                } else {
                    3 | (button & 0b1_1100)
                };
                let enc = |v: u16| (32 + (v + 1).min(222)) as u8;
                vec![0x1b, b'[', b'M', 32 + b, enc(pos.col), enc(pos.row)]
            }
        }
    }

    /// Paint the terminal into the remaining space of `ui` and process input.
    pub fn show(
        &mut self,
        ui: &mut egui::Ui,
        id: egui::Id,
        font: &FontId,
        pal: &TermPalette,
        input_enabled: bool,
        zh: bool,
    ) -> (egui::Response, TermOutput) {
        let mut out = TermOutput::default();
        self.resync_anchor();
        let rect = ui.available_rect_before_wrap();
        ui.allocate_rect(rect, Sense::hover());
        let resp = ui.interact(rect, id, Sense::click_and_drag());

        // Measure the advance the text layout really uses (it snaps glyphs
        // to pixels), so cells drawn by column line up with row galleys.
        let (char_w, char_h) = ui.fonts(|f| {
            let probe = f.layout_no_wrap("M".repeat(64), font.clone(), Color32::WHITE);
            (
                (probe.size().x / 64.0).max(1.0),
                f.row_height(font).max(1.0),
            )
        });
        let inner = rect.shrink(TERM_PAD);
        let cols = ((inner.width() - 8.0) / char_w).floor().clamp(20.0, 1000.0) as u16;
        let rows = (inner.height() / char_h).floor().clamp(4.0, 500.0) as u16;
        if self.parser.screen().size() != (rows, cols) {
            self.resize_grid(rows, cols);
            self.clear_selection();
        }
        // Tell the server only once the size has settled (window drags and
        // view transitions would otherwise send a resize every frame).
        if self.sent_size != (cols, rows) {
            let now = ui.input(|i| i.time);
            match self.pending_size {
                Some((c, r, since)) if (c, r) == (cols, rows) => {
                    if now - since >= 0.12 || self.sent_size == (0, 0) {
                        out.resized = Some((cols, rows));
                        self.sent_size = (cols, rows);
                        self.pending_size = None;
                    } else {
                        ui.ctx()
                            .request_repaint_after(std::time::Duration::from_millis(130));
                    }
                }
                _ => {
                    self.pending_size = Some((cols, rows, now));
                    if self.sent_size == (0, 0) {
                        out.resized = Some((cols, rows));
                        self.sent_size = (cols, rows);
                        self.pending_size = None;
                    } else {
                        ui.ctx()
                            .request_repaint_after(std::time::Duration::from_millis(130));
                    }
                }
            }
        }
        let origin = inner.min;

        let cell_at = |p: Pos2, round: bool| -> CellPos {
            let fx = (p.x - origin.x) / char_w;
            let fy = (p.y - origin.y) / char_h;
            let col = if round { fx.round() } else { fx.floor() };
            CellPos {
                row: fy.floor().clamp(0.0, rows as f32 - 1.0) as u16,
                col: col.clamp(
                    0.0,
                    if round {
                        cols as f32
                    } else {
                        cols as f32 - 1.0
                    },
                ) as u16,
            }
        };

        // ---------- mouse ----------
        let modifiers = ui.input(|i| i.modifiers);
        let mouse_mode = self.parser.screen().mouse_protocol_mode();
        let report_mouse =
            input_enabled && mouse_mode != vt100::MouseProtocolMode::None && !modifiers.shift;

        if resp.clicked() || resp.drag_started() || resp.secondary_clicked() {
            resp.request_focus();
        }
        let hover_pos = ui
            .input(|i| i.pointer.hover_pos())
            .filter(|p| rect.contains(*p));
        self.hover_url = if resp.hovered() && !self.dragging && modifiers.command {
            hover_pos.and_then(|p| {
                let c = cell_at(p, false);
                self.url_at_cell(c.row, c.col)
                    .map(|(s, e, u)| (c.row, s, e, u))
            })
        } else {
            None
        };
        if self.hover_url.is_some() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }
        let ctrl_clicked_url = if modifiers.command && resp.clicked_by(egui::PointerButton::Primary)
        {
            self.hover_url.as_ref().map(|u| u.3.clone()).or_else(|| {
                resp.interact_pointer_pos().and_then(|p| {
                    let c = cell_at(p, false);
                    self.url_at_cell(c.row, c.col).map(|u| u.2)
                })
            })
        } else {
            None
        };
        if let Some(url) = ctrl_clicked_url.as_ref() {
            out.open_url = Some(url.clone());
        }
        if resp.secondary_clicked() {
            self.ctx_url = ui
                .input(|i| i.pointer.interact_pos())
                .filter(|p| rect.contains(*p))
                .and_then(|p| {
                    let c = cell_at(p, false);
                    self.url_at_cell(c.row, c.col).map(|u| u.2)
                });
        }

        // xterm modifier bits in mouse reports (Shift is kept for selecting).
        let mouse_mods = |m: egui::Modifiers| -> u8 {
            (if m.alt { 8 } else { 0 }) | (if m.ctrl || m.command { 16 } else { 0 })
        };
        if report_mouse {
            let events: Vec<(Pos2, egui::PointerButton, bool, egui::Modifiers)> = ui.input(|i| {
                i.events
                    .iter()
                    .filter_map(|e| match e {
                        egui::Event::PointerButton {
                            pos,
                            button,
                            pressed,
                            modifiers,
                        } => Some((*pos, *button, *pressed, *modifiers)),
                        _ => None,
                    })
                    .collect()
            });
            for (pos, button, pressed, m) in events {
                if ctrl_clicked_url.is_some() && button == egui::PointerButton::Primary {
                    continue;
                }
                let code = match button {
                    egui::PointerButton::Primary => 0u8,
                    egui::PointerButton::Middle => 1,
                    egui::PointerButton::Secondary => 2,
                    _ => continue,
                };
                let cell = cell_at(pos, false);
                if pressed {
                    if resp.hovered() && rect.contains(pos) {
                        out.local_bytes
                            .extend(self.mouse_report(code | mouse_mods(m), true, cell));
                        self.mouse_buttons |= 1 << code;
                        self.mouse_last_cell = Some(cell);
                    }
                } else if self.mouse_buttons & (1 << code) != 0 {
                    self.mouse_buttons &= !(1 << code);
                    if mouse_mode != vt100::MouseProtocolMode::Press {
                        out.local_bytes.extend(self.mouse_report(
                            code | mouse_mods(m),
                            false,
                            cell,
                        ));
                    }
                }
            }
            // Motion: while a button is held (button-event tracking, 1002)
            // or always over the terminal (any-event tracking, 1003).
            let motion = matches!(
                mouse_mode,
                vt100::MouseProtocolMode::ButtonMotion | vt100::MouseProtocolMode::AnyMotion
            );
            if motion {
                if let Some(p) = ui.input(|i| i.pointer.latest_pos()) {
                    let cell = cell_at(p, false);
                    if self.mouse_last_cell != Some(cell) {
                        let held = (0..3u8).find(|b| self.mouse_buttons & (1 << b) != 0);
                        let any =
                            mouse_mode == vt100::MouseProtocolMode::AnyMotion && resp.hovered();
                        if held.is_some() || any {
                            let btn = held.unwrap_or(3) + 32 + mouse_mods(modifiers);
                            out.local_bytes.extend(self.mouse_report(btn, true, cell));
                        }
                        self.mouse_last_cell = Some(cell);
                    }
                }
            }
        } else {
            self.mouse_buttons = 0;
            if ctrl_clicked_url.is_some() {
                self.dragging = false;
            } else {
                if resp.drag_started_by(egui::PointerButton::Primary) {
                    // Anchor at the press position, not where the drag threshold was crossed.
                    let origin_pos = ui
                        .input(|i| i.pointer.press_origin())
                        .or(resp.interact_pointer_pos());
                    if let Some(p) = origin_pos {
                        let c = self.to_sel(cell_at(p, true));
                        self.sel_anchor = Some(c);
                        self.sel_head = Some(c);
                        self.dragging = true;
                    }
                }
                if self.dragging && resp.dragged_by(egui::PointerButton::Primary) {
                    if let Some(p) = resp.interact_pointer_pos() {
                        self.sel_head = Some(self.to_sel(cell_at(p, true)));
                        // Auto-scroll into scrollback while dragging above the top edge.
                        if p.y < inner.top() {
                            self.scroll_view(1);
                        } else if p.y > inner.bottom() {
                            self.scroll_view(-1);
                        }
                    }
                }
                if self.dragging && resp.drag_stopped() {
                    self.dragging = false;
                    self.update_sel_text();
                    if self.sel_text.is_empty() {
                        self.sel_anchor = None;
                        self.sel_head = None;
                    }
                }
                if resp.double_clicked() {
                    if let Some(p) = resp.interact_pointer_pos() {
                        let c = cell_at(p, false);
                        let (s, e) = self.word_bounds(c.row, c.col);
                        self.sel_anchor = Some(self.to_sel(CellPos { row: c.row, col: s }));
                        self.sel_head = Some(self.to_sel(CellPos { row: c.row, col: e }));
                        self.update_sel_text();
                    }
                } else if resp.triple_clicked() {
                    if let Some(p) = resp.interact_pointer_pos() {
                        let c = cell_at(p, false);
                        self.sel_anchor = Some(self.to_sel(CellPos { row: c.row, col: 0 }));
                        self.sel_head = Some(self.to_sel(CellPos {
                            row: c.row,
                            col: cols,
                        }));
                        self.update_sel_text();
                    }
                } else if resp.clicked() {
                    self.clear_selection();
                }
            }
        }

        // Wheel: font size (Ctrl), mouse report, alternate-screen arrows, or scrollback.
        if resp.hovered() {
            // The terminal owns the wheel: a surrounding scroll area (the
            // group grid) must not scroll along with it.
            ui.input_mut(|i| {
                i.smooth_scroll_delta = Vec2::ZERO;
                i.raw_scroll_delta = Vec2::ZERO;
            });
            let wheel: Vec<(f32, egui::Modifiers)> = ui.input(|i| {
                i.events
                    .iter()
                    .filter_map(|e| match e {
                        egui::Event::MouseWheel {
                            unit,
                            delta,
                            modifiers,
                        } => {
                            let lines = match unit {
                                egui::MouseWheelUnit::Line => delta.y,
                                egui::MouseWheelUnit::Point => delta.y / char_h,
                                egui::MouseWheelUnit::Page => delta.y * rows as f32,
                            };
                            Some((lines, *modifiers))
                        }
                        _ => None,
                    })
                    .collect()
            });
            for (lines, mods) in wheel {
                if mods.ctrl || mods.command {
                    self.zoom_accum += lines;
                    let steps = self.zoom_accum.trunc();
                    self.zoom_accum -= steps;
                    out.font_delta += steps;
                    continue;
                }
                self.wheel_accum += lines;
                let whole = self.wheel_accum.trunc() as i32;
                if whole == 0 {
                    continue;
                }
                self.wheel_accum -= whole as f32;
                let screen = self.parser.screen();
                if report_mouse {
                    let p = ui.input(|i| i.pointer.hover_pos()).unwrap_or(origin);
                    let btn = if whole > 0 { 64 } else { 65 } | mouse_mods(mods);
                    for _ in 0..whole.unsigned_abs() {
                        out.local_bytes
                            .extend(self.mouse_report(btn, true, cell_at(p, false)));
                    }
                } else if screen.alternate_screen() {
                    if input_enabled {
                        let seq: &[u8] = match (whole > 0, screen.application_cursor()) {
                            (true, true) => b"\x1bOA",
                            (true, false) => b"\x1b[A",
                            (false, true) => b"\x1bOB",
                            (false, false) => b"\x1b[B",
                        };
                        for _ in 0..whole.unsigned_abs() * 3 {
                            out.local_bytes.extend_from_slice(seq);
                        }
                    }
                } else {
                    self.scroll_view(whole * 3);
                }
            }
        }

        // ---------- keyboard ----------
        // Widget focus (independent of OS window focus, which some WMs report late).
        let focused = ui.ctx().memory(|m| m.has_focus(id));
        let window_focused = ui.input(|i| i.focused);
        if focused {
            ui.memory_mut(|m| {
                m.set_focus_lock_filter(
                    id,
                    egui::EventFilter {
                        tab: true,
                        horizontal_arrows: true,
                        vertical_arrows: true,
                        escape: true,
                    },
                )
            });
            if input_enabled {
                self.handle_keys(ui, &mut out);
            }
        }
        // A Ctrl+C held back to tell it from Ctrl+Insert must not fire later
        // in a terminal that lost the keyboard meanwhile.
        if !(focused && input_enabled) {
            self.pending_copy.0 = 0;
        }

        // Scrollbar fades in only while hovering or browsing scrollback.
        let browsing = self.parser.screen().scrollback() > 0;
        self.sb_alpha = ui.ctx().animate_bool_with_time(
            id.with("sb"),
            resp.hovered() || browsing || self.dragging,
            0.25,
        );

        // Let the OS input method (Chinese / Japanese …) compose directly in the terminal.
        if focused && input_enabled {
            let (cr, cc) = self.parser.screen().cursor_position();
            let cc = cc.min(cols.saturating_sub(1));
            let cursor_rect = Rect::from_min_size(
                Pos2::new(origin.x + cc as f32 * char_w, origin.y + cr as f32 * char_h),
                Vec2::new(char_w.max(2.0), char_h),
            );
            // The OS places its candidate window by `rect`: the cursor, not
            // the corner of the whole terminal.
            ui.ctx().output_mut(|o| {
                o.ime = Some(egui::output::IMEOutput {
                    rect: cursor_rect,
                    cursor_rect,
                });
            });
        } else if !self.preedit.is_empty() {
            self.preedit.clear();
        }

        // ---------- paint ----------
        self.paint(
            ui,
            rect,
            origin,
            char_w,
            char_h,
            font,
            pal,
            focused && window_focused,
        );

        // Context menu — not while the application takes mouse clicks
        // (Shift + right click still opens it; once open it stays).
        if report_mouse && !self.menu_open {
            return (resp, out);
        }
        let menu = resp.context_menu(|ui| {
            ui.set_width(210.0);
            use super::widgets::{menu_item, menu_separator};
            let t = |a: &'static str, b: &'static str| if zh { a } else { b };
            if let Some(url) = self.ctx_url.clone() {
                if menu_item(ui, t("打开链接", "Open link"), "Ctrl+Click", false) {
                    out.open_url = Some(url);
                    ui.close_menu();
                } else if menu_item(ui, t("拷贝链接", "Copy link"), "", false) {
                    ui.ctx().copy_text(url);
                    out.copied = true;
                    ui.close_menu();
                }
                menu_separator(ui);
            }
            if self.has_selection() && menu_item(ui, t("拷贝", "Copy"), "Ctrl+Shift+C", false) {
                ui.ctx().copy_text(self.sel_text.clone());
                out.copied = true;
                ui.close_menu();
            }
            if input_enabled && menu_item(ui, t("粘贴", "Paste"), "Ctrl+Shift+V", false) {
                // The clipboard text arrives as an Event::Paste on the next frame.
                ui.ctx()
                    .send_viewport_cmd(egui::ViewportCommand::RequestPaste);
                ui.close_menu();
            }
            if menu_item(ui, t("全选", "Select all"), "", false) {
                self.select_all_visible();
                ui.close_menu();
            }
            menu_separator(ui);
            if menu_item(ui, t("查找…", "Find…"), "Ctrl+Shift+F", false) {
                out.open_find = true;
                ui.close_menu();
            }
            if menu_item(ui, t("清屏", "Clear"), "", false) {
                self.clear_screen();
                if input_enabled {
                    out.local_bytes.push(0x0c); // ask the shell to redraw its prompt
                }
                ui.close_menu();
            }
        });
        self.menu_open = menu.is_some();

        (resp, out)
    }

    fn handle_keys(&mut self, ui: &egui::Ui, out: &mut TermOutput) {
        let (events, mods) = ui.input(|i| (i.events.clone(), i.modifiers));
        let app_cursor = self.parser.screen().application_cursor();
        let mut typed = false;
        // The character of an Alt+key we already sent as ESC+char; its Text
        // event (if the platform emits one) must not be sent again. The
        // frame's modifiers can't tell, since Alt may be released already.
        let mut alt_letter: Option<u8> = None;
        // Enter / Escape / Backspace while a composition was under way this
        // frame (it may have just ended with them) belong to the IME:
        // confirming, cancelling or editing it — not to the shell. Text
        // committed without a composition (Wayland sends every key that way)
        // doesn't count.
        let mut composing_seen = !self.preedit.is_empty();
        for ev in events {
            // A held-back Ctrl+C (see `pending_copy`) goes out before any
            // other input that follows it: typing on right after Ctrl+C must
            // not land before the interrupt.
            if self.pending_copy.0 > 0 {
                let other_input = match &ev {
                    egui::Event::Key {
                        key: egui::Key::C | egui::Key::Insert,
                        pressed: false,
                        ..
                    } => false,
                    egui::Event::Key { pressed: true, .. }
                    | egui::Event::Text(_)
                    | egui::Event::Paste(_)
                    | egui::Event::Ime(egui::ImeEvent::Commit(_)) => true,
                    _ => false,
                };
                if other_input {
                    for _ in 0..self.pending_copy.0 {
                        out.keys(&[0x03]);
                    }
                    self.pending_copy.0 = 0;
                    typed = true;
                }
            }
            // While the IME is composing, keys belong to the IME, not the shell.
            let composing = !self.preedit.is_empty();
            match ev {
                egui::Event::Ime(ime) => match ime {
                    egui::ImeEvent::Preedit(text) => {
                        composing_seen |= !text.is_empty();
                        self.preedit = text;
                    }
                    egui::ImeEvent::Commit(text) => {
                        self.preedit.clear();
                        if !text.is_empty() {
                            out.keys(text.as_bytes());
                            typed = true;
                        }
                    }
                    egui::ImeEvent::Disabled => self.preedit.clear(),
                    egui::ImeEvent::Enabled => {}
                },
                egui::Event::Text(_) | egui::Event::Key { .. } if composing => {}
                egui::Event::Paste(text) => {
                    // egui sends no key event for clipboard shortcuts, only the
                    // modifiers tell them apart: Ctrl+V (command, no shift),
                    // Ctrl+Shift+V, Shift+Insert. Plain Ctrl+V pastes, unless
                    // the user chose to hand it to full-screen apps (vim's
                    // block selection). macOS: Cmd+V always pastes.
                    // (A quick tap can be over before the frame ends: then the
                    // V release in this frame still shows Ctrl held.)
                    let ctrl_v = !cfg!(target_os = "macos")
                        && ((mods.command && !mods.shift)
                            || released_with_command(ui, egui::Key::V));
                    if ctrl_v
                        && CTRL_V_TO_APPS.load(std::sync::atomic::Ordering::Relaxed)
                        && self.parser.screen().alternate_screen()
                    {
                        out.keys(&[0x16]);
                    } else {
                        out.input.push(TermInput::Paste(text));
                    }
                    typed = true;
                }
                egui::Event::Copy => {
                    // Ctrl+Shift+C always copies; Ctrl+C copies only when a
                    // selection exists, otherwise it interrupts. macOS: Cmd+C
                    // only ever copies (Ctrl+C there is a normal key).
                    if mods.shift || self.has_selection() || cfg!(target_os = "macos") {
                        if self.has_selection() {
                            ui.ctx().copy_text(self.sel_text.clone());
                            out.copied = true;
                        }
                        self.sel_anchor = None;
                        self.sel_head = None;
                        if !mods.shift {
                            self.sel_text.clear();
                        }
                    } else if cfg!(windows) {
                        // Ctrl+C or Ctrl+Insert? Decided when the key is
                        // released (Ctrl+Insert with nothing selected does
                        // nothing, like in other terminals).
                        if self.pending_copy.0 == 0 {
                            self.pending_copy.1 = ui.input(|i| i.time);
                        }
                        self.pending_copy.0 += 1;
                        ui.ctx()
                            .request_repaint_after(std::time::Duration::from_millis(
                                (PENDING_COPY_WAIT * 1000.0) as u64 + 20,
                            ));
                    } else {
                        out.keys(&[0x03]);
                        typed = true;
                    }
                }
                egui::Event::Cut => {
                    if cfg!(target_os = "macos") {
                        // Cmd+X: nothing to cut in a terminal.
                    } else if mods.command || mods.ctrl || released_with_command(ui, egui::Key::X) {
                        out.keys(&[0x18]); // Ctrl+X / Ctrl+Shift+X (nano!)
                        typed = true;
                    } else {
                        // Shift+Delete arrives as "cut": send Delete.
                        out.keys(b"\x1b[3~");
                        typed = true;
                    }
                }
                egui::Event::Key {
                    key: k @ (egui::Key::C | egui::Key::Insert),
                    pressed: false,
                    ..
                } if self.pending_copy.0 > 0 => {
                    if k == egui::Key::C {
                        for _ in 0..self.pending_copy.0 {
                            out.keys(&[0x03]);
                        }
                        typed = true;
                    }
                    self.pending_copy.0 = 0;
                }
                egui::Event::Text(text) => {
                    if (mods.ctrl || mods.command) && !mods.alt {
                        continue;
                    }
                    // Alt+key was already sent as ESC+char by the key handler.
                    if let Some(ch) = alt_letter.take() {
                        if text.len() == 1 && text.as_bytes()[0].eq_ignore_ascii_case(&ch) {
                            continue;
                        }
                    }
                    let alt_meta = mods.alt && !mods.ctrl && !cfg!(target_os = "macos");
                    if alt_meta && text.len() == 1 && text.as_bytes()[0].is_ascii_alphanumeric() {
                        continue;
                    }
                    if !text.is_empty() {
                        // Alt as Meta: Alt+Shift+1 → ESC "!", Alt+< → ESC "<"
                        // (characters whose key the handler can't know).
                        // macOS keeps Option for typing special characters.
                        if alt_meta && text.chars().count() == 1 {
                            out.keys(b"\x1b");
                        }
                        out.keys(text.as_bytes());
                        typed = true;
                    }
                }
                egui::Event::Key {
                    key: egui::Key::Enter | egui::Key::Escape | egui::Key::Backspace,
                    pressed: true,
                    ..
                } if composing_seen => {}
                egui::Event::Key {
                    key: egui::Key::Insert,
                    pressed: true,
                    modifiers,
                    ..
                } if modifiers.shift && !modifiers.ctrl && !modifiers.alt && !cfg!(windows) => {
                    // Shift+Insert pastes (egui only maps it on Windows).
                    ui.ctx()
                        .send_viewport_cmd(egui::ViewportCommand::RequestPaste);
                }
                egui::Event::Key {
                    key,
                    physical_key,
                    pressed: true,
                    modifiers,
                    ..
                } => {
                    // Windows reports AltGr as Ctrl+Alt, and egui drops the
                    // character it types: ask the keyboard layout for it, so
                    // AltGr+Q still types "@" on a German keyboard.
                    #[cfg(windows)]
                    if modifiers.ctrl && modifiers.alt {
                        if let Some(text) =
                            physical_key.and_then(|k| win_altgr::altgr_text(k, modifiers.shift))
                        {
                            out.keys(text.as_bytes());
                            typed = true;
                            continue;
                        }
                    }
                    #[cfg(not(windows))]
                    let _ = physical_key;
                    if let Some(bytes) = map_key_to_pty_bytes(key, modifiers, app_cursor) {
                        alt_letter = match bytes.as_slice() {
                            [0x1b, ch] if modifiers.alt && ch.is_ascii_graphic() => Some(*ch),
                            _ => None,
                        };
                        out.keys(&bytes);
                        typed = true;
                    }
                }
                _ => {}
            }
        }
        // A release that never came (focus lost): it was most likely Ctrl+C.
        if self.pending_copy.0 > 0 && ui.input(|i| i.time) - self.pending_copy.1 > PENDING_COPY_WAIT
        {
            for _ in 0..self.pending_copy.0 {
                out.keys(&[0x03]);
            }
            self.pending_copy.0 = 0;
            typed = true;
        }
        if typed {
            self.scroll_to_bottom();
            self.sel_anchor = None;
            self.sel_head = None;
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn paint(
        &mut self,
        ui: &egui::Ui,
        rect: Rect,
        origin: Pos2,
        char_w: f32,
        char_h: f32,
        font: &FontId,
        pal: &TermPalette,
        focused: bool,
    ) {
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, CornerRadius::same(12), pal.bg);

        let sel = self.sel_range();
        let screen = self.parser.screen();
        let (rows, cols) = screen.size();
        let scrolled = screen.scrollback();

        let is_selected = |r: u16, c: u16| -> bool {
            match sel {
                Some((s, e)) => {
                    let p = SelPos {
                        row: r as i64 - scrolled as i64,
                        col: c,
                    };
                    p >= s && p < e
                }
                None => false,
            }
        };

        // A real bold face is used when one is installed with exactly the
        // same advance as the regular face; otherwise bold is drawn twice
        // with a sub-pixel offset.
        let bold_font = ui.fonts(|f| {
            let fam = FontFamily::Name(TERM_BOLD.into());
            if !f.families().contains(&fam) {
                return None;
            }
            let bf = FontId::new(font.size, fam);
            let w = f
                .layout_no_wrap("M".repeat(64), bf.clone(), Color32::WHITE)
                .size()
                .x
                / 64.0;
            ((w - char_w).abs() < 0.01).then_some(bf)
        });
        // Cached rows hold laid-out text, which is only valid for the font
        // atlas it was made with: a new atlas (scale change, or egui
        // rebuilding a full one) or pixel density drops the cache.
        let atlas = ui.fonts(|f| std::sync::Arc::as_ptr(&f.texture_atlas()) as usize);
        let geo = {
            use std::hash::{Hash, Hasher};
            let mut h = std::collections::hash_map::DefaultHasher::new();
            (
                char_w.to_bits(),
                char_h.to_bits(),
                font.size.to_bits(),
                cols,
                atlas,
                ui.ctx().pixels_per_point().to_bits(),
            )
                .hash(&mut h);
            font.family.hash(&mut h);
            bold_font.is_some().hash(&mut h);
            (pal.fg, pal.bg, pal.selection).hash(&mut h);
            h.finish()
        };
        if self.row_cache_geo != geo || self.row_cache.len() != rows as usize {
            self.row_cache = vec![None; rows as usize];
            self.row_cache_geo = geo;
        }

        // One text layout per row: ASCII text (all colours) goes into a single
        // LayoutJob with spaces holding the place of blank and wide cells.
        // Non-ASCII glyphs are drawn centred in their own cells so fallback
        // fonts (CJK, symbols) never shift the grid. What a row draws, laid
        // out, is cached while its cells (and selection) are unchanged.
        for r in 0..rows {
            let y = origin.y + r as f32 * char_h;
            let offset = Vec2::new(origin.x, y);

            // Cheap fingerprint of the row as it will be drawn.
            let hash = {
                use std::hash::{Hash, Hasher};
                let mut h = std::collections::hash_map::DefaultHasher::new();
                for c in 0..cols {
                    match screen.cell(r, c) {
                        Some(cell) => {
                            cell.contents().hash(&mut h);
                            (
                                cell.is_wide(),
                                cell.is_wide_continuation(),
                                cell.bold(),
                                cell.dim(),
                                cell.inverse(),
                                cell.underline(),
                                color_key(cell.fgcolor()),
                                color_key(cell.bgcolor()),
                            )
                                .hash(&mut h);
                        }
                        None => 0u8.hash(&mut h),
                    }
                    is_selected(r, c).hash(&mut h);
                }
                h.finish()
            };
            let cached = self
                .row_cache
                .get(r as usize)
                .and_then(|c| c.as_ref())
                .is_some_and(|c| c.hash == hash);
            if !cached {
                let mut built = build_row(
                    screen,
                    r,
                    cols,
                    char_w,
                    char_h,
                    font,
                    &bold_font,
                    pal,
                    &|c| is_selected(r, c),
                );
                ui.fonts(|f| built.realize(f, font, pal.fg));
                if let Some(slot) = self.row_cache.get_mut(r as usize) {
                    *slot = Some(RowPaint { hash, ..built });
                }
            }
            if let Some(Some(row)) = self.row_cache.get(r as usize) {
                row.paint(&painter, offset);
            }
        }

        if let Some((r, c0, c1, _)) = &self.hover_url {
            if *r < rows && *c1 > *c0 {
                let y = origin.y + (*r as f32 + 1.0) * char_h - 1.5;
                painter.line_segment(
                    [
                        Pos2::new(origin.x + *c0 as f32 * char_w, y),
                        Pos2::new(origin.x + *c1 as f32 * char_w, y),
                    ],
                    Stroke::new(1.2_f32, pal.cursor),
                );
            }
        }

        // Cursor
        if scrolled == 0 && !screen.hide_cursor() {
            let (cr, cc) = screen.cursor_position();
            // Right after the last column is written the cursor waits past
            // the edge (pending wrap); xterm shows it on the last column.
            let cc = cc.min(cols.saturating_sub(1));
            if cr < rows && cc < cols {
                let wide = screen.cell(cr, cc).is_some_and(|c| c.is_wide());
                let w = if wide { 2.0 } else { 1.0 };
                let r = Rect::from_min_size(
                    Pos2::new(origin.x + cc as f32 * char_w, origin.y + cr as f32 * char_h),
                    Vec2::new(char_w * w, char_h),
                );
                if focused {
                    painter.rect_filled(r, CornerRadius::same(1), pal.cursor);
                    if let Some(cell) = screen.cell(cr, cc) {
                        let t = cell.contents();
                        if !t.is_empty() && t != " " {
                            painter.text(
                                r.center(),
                                Align2::CENTER_CENTER,
                                t,
                                font.clone(),
                                pal.bg,
                            );
                        }
                    }
                } else {
                    painter.rect_stroke(
                        r,
                        CornerRadius::same(1),
                        Stroke::new(1.0_f32, pal.cursor),
                        egui::StrokeKind::Inside,
                    );
                }
            }
        }

        // IME composition, drawn inline at the cursor.
        if !self.preedit.is_empty() {
            let (cr, cc) = screen.cursor_position();
            let cc = cc.min(cols.saturating_sub(1));
            let pos = Pos2::new(origin.x + cc as f32 * char_w, origin.y + cr as f32 * char_h);
            let g = painter.layout_no_wrap(self.preedit.clone(), font.clone(), pal.fg);
            let r = Rect::from_min_size(pos, Vec2::new(g.size().x.max(char_w), char_h));
            painter.rect_filled(r.expand(1.0), CornerRadius::same(2), pal.bg);
            painter.galley(pos, g, pal.fg);
            painter.line_segment(
                [
                    Pos2::new(r.left(), r.bottom() - 1.0),
                    Pos2::new(r.right(), r.bottom() - 1.0),
                ],
                Stroke::new(1.0_f32, pal.cursor),
            );
        }

        // Scrollbar (only when there is scrollback on the normal screen)
        if !screen.alternate_screen() {
            let cur = scrolled;
            let total = {
                let s = self.parser.screen_mut();
                s.set_scrollback(usize::MAX);
                let t = s.scrollback();
                s.set_scrollback(cur);
                t
            };
            if total > 0 {
                let track = Rect::from_min_max(
                    Pos2::new(rect.right() - 7.0, rect.top() + 8.0),
                    Pos2::new(rect.right() - 3.0, rect.bottom() - 8.0),
                );
                let visible = rows as f32;
                let all = total as f32 + visible;
                let h = (track.height() * visible / all).max(18.0);
                let frac = 1.0 - (cur as f32 / total as f32);
                let top = track.top() + (track.height() - h) * frac;
                let thumb =
                    Rect::from_min_size(Pos2::new(track.left(), top), Vec2::new(track.width(), h));
                let alpha = (110.0 * self.sb_alpha) as u8;
                if alpha > 0 {
                    painter.rect_filled(
                        thumb,
                        CornerRadius::same(2),
                        Color32::from_rgba_unmultiplied(pal.fg.r(), pal.fg.g(), pal.fg.b(), alpha),
                    );
                }
            }
        }
    }
}

fn is_url_byte(b: u8) -> bool {
    b.is_ascii_graphic()
        && !matches!(
            b,
            b'<' | b'>' | b'"' | b'\'' | b'`' | b'{' | b'}' | b'|' | b'^' | b'\\'
        )
}

fn find_url_in_run(run: &str, run_cols: &[u16], col: u16) -> Option<(u16, u16, String)> {
    let mut pos = 0;
    while pos < run.len() {
        let rest = &run[pos..];
        let off = match (rest.find("https://"), rest.find("http://")) {
            (Some(a), Some(b)) => a.min(b),
            (Some(a), None) | (None, Some(a)) => a,
            (None, None) => break,
        };
        let start = pos + off;
        let mut raw = &run[start..];
        loop {
            if raw.ends_with(['.', ',', ';', ':', '!', '?']) {
                raw = &raw[..raw.len() - 1];
            } else if let Some((open, close)) = [('(', ')'), ('[', ']')]
                .into_iter()
                .find(|(_, c)| raw.ends_with(*c))
            {
                let opens = raw.bytes().filter(|&b| b == open as u8).count();
                let closes = raw.bytes().filter(|&b| b == close as u8).count();
                if closes > opens {
                    raw = &raw[..raw.len() - 1];
                } else {
                    break;
                }
            } else {
                break;
            }
        }
        let after_scheme = raw.split_once("://").map(|(_, r)| r).unwrap_or("");
        let host = after_scheme.split(['/', '?', '#']).next().unwrap_or("");
        if !host.is_empty() && !raw.is_empty() {
            let start_col = run_cols[start];
            let end_col = run_cols[start + raw.len() - 1] + 1;
            if col >= start_col && col < end_col {
                return Some((start_col, end_col, raw.to_string()));
            }
        }
        pos = start + 1;
    }
    None
}

/// Detect an `http://` or `https://` URL at cell column `col` on a single screen row.
fn find_url_in_line(cells: &[(u16, &str)], col: u16) -> Option<(u16, u16, String)> {
    let mut run = String::new();
    let mut run_cols: Vec<u16> = Vec::new();
    for &(c, text) in cells {
        let bytes = text.as_bytes();
        let contiguous = run_cols.last().is_none_or(|&prev| c == prev + 1);
        if contiguous && bytes.len() == 1 && is_url_byte(bytes[0]) {
            run.push(bytes[0] as char);
            run_cols.push(c);
        } else {
            if !run.is_empty() {
                if let Some(hit) = find_url_in_run(&run, &run_cols, col) {
                    return Some(hit);
                }
                run.clear();
                run_cols.clear();
            }
            if bytes.len() == 1 && is_url_byte(bytes[0]) {
                run.push(bytes[0] as char);
                run_cols.push(c);
            }
        }
    }
    if !run.is_empty() {
        return find_url_in_run(&run, &run_cols, col);
    }
    None
}

fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// Parse an OSC 7 payload (`7;file://<host>/<path>`) into a clean Unix path.
fn parse_osc7_cwd(osc: &str) -> Option<String> {
    let uri = osc.strip_prefix("7;")?;
    let rest = uri.strip_prefix("file://")?;
    let slash = rest.find('/')?;
    let raw_path = &rest[slash..];
    let src = raw_path.as_bytes();
    let mut bytes = Vec::with_capacity(src.len());
    let mut i = 0;
    while i < src.len() {
        if src[i] == b'%' && i + 2 < src.len() {
            if let (Some(hi), Some(lo)) = (hex_val(src[i + 1]), hex_val(src[i + 2])) {
                bytes.push((hi << 4) | lo);
                i += 3;
                continue;
            }
        }
        bytes.push(src[i]);
        i += 1;
    }
    let decoded = String::from_utf8(bytes).ok()?;
    if !decoded.starts_with('/') || decoded.chars().any(|c| c == '\0' || c.is_control()) {
        return None;
    }
    if decoded.len() > 1 {
        let t = decoded.trim_end_matches('/');
        Some(if t.is_empty() { "/".into() } else { t.into() })
    } else {
        Some(decoded)
    }
}

fn lerp_color(a: Color32, b: Color32, t: f32) -> Color32 {
    let l = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    Color32::from_rgb(l(a.r(), b.r()), l(a.g(), b.g()), l(a.b(), b.b()))
}

fn resolve_color(c: vt100::Color, is_fg: bool, bold: bool, pal: &TermPalette) -> Color32 {
    match c {
        vt100::Color::Default => {
            if is_fg {
                pal.fg
            } else {
                pal.bg
            }
        }
        vt100::Color::Idx(i) => {
            let i = if is_fg && bold && i < 8 { i + 8 } else { i };
            idx_color(i, pal)
        }
        vt100::Color::Rgb(r, g, b) => Color32::from_rgb(r, g, b),
    }
}

fn idx_color(i: u8, pal: &TermPalette) -> Color32 {
    match i {
        0..=15 => pal.ansi[i as usize],
        16..=231 => {
            let i = i - 16;
            let lv = [0u8, 95, 135, 175, 215, 255];
            Color32::from_rgb(
                lv[(i / 36) as usize],
                lv[((i / 6) % 6) as usize],
                lv[(i % 6) as usize],
            )
        }
        _ => {
            let v = 8 + (i - 232) * 10;
            Color32::from_rgb(v, v, v)
        }
    }
}

/// Map egui key + modifiers to bytes a remote PTY expects (xterm).
pub fn map_key_to_pty_bytes(
    key: egui::Key,
    modifiers: egui::Modifiers,
    app_cursor: bool,
) -> Option<Vec<u8>> {
    use egui::Key;
    let ctrl = modifiers.ctrl || modifiers.command;
    let alt = modifiers.alt;
    let shift = modifiers.shift;

    if ctrl && !shift {
        let letter = match key {
            Key::A => Some(0x01),
            Key::B => Some(0x02),
            Key::C => Some(0x03),
            Key::D => Some(0x04),
            Key::E => Some(0x05),
            Key::F => Some(0x06),
            Key::G => Some(0x07),
            Key::H => Some(0x08),
            Key::I => Some(0x09),
            Key::J => Some(0x0a),
            Key::K => Some(0x0b),
            Key::L => Some(0x0c),
            Key::M => Some(0x0d),
            Key::N => Some(0x0e),
            Key::O => Some(0x0f),
            Key::P => Some(0x10),
            Key::Q => Some(0x11),
            Key::R => Some(0x12),
            Key::S => Some(0x13),
            Key::T => Some(0x14),
            Key::U => Some(0x15),
            Key::V => Some(0x16),
            Key::W => Some(0x17),
            Key::X => Some(0x18),
            Key::Y => Some(0x19),
            Key::Z => Some(0x1a),
            Key::Space | Key::Num2 => Some(0x00),
            Key::OpenBracket | Key::Num3 => Some(0x1b),
            Key::Backslash | Key::Num4 => Some(0x1c),
            Key::CloseBracket | Key::Num5 => Some(0x1d),
            Key::Num6 => Some(0x1e),
            Key::Minus | Key::Slash | Key::Num7 => Some(0x1f),
            Key::Num8 => Some(0x7f),
            _ => None,
        };
        if let Some(b) = letter {
            // Ctrl+Alt+key: ESC + the control character.
            return Some(if alt { vec![0x1b, b] } else { vec![b] });
        }
    }

    // xterm modifier parameter: 1 + shift(1) + alt(2) + ctrl(4)
    let m = 1 + shift as u8 + 2 * alt as u8 + 4 * ctrl as u8;
    let csi_letter = |c: u8| -> Vec<u8> {
        if m > 1 {
            format!("\x1b[1;{m}{}", c as char).into_bytes()
        } else if app_cursor {
            vec![0x1b, b'O', c]
        } else {
            vec![0x1b, b'[', c]
        }
    };
    let csi_tilde = |n: u8| -> Vec<u8> {
        if m > 1 {
            format!("\x1b[{n};{m}~").into_bytes()
        } else {
            format!("\x1b[{n}~").into_bytes()
        }
    };
    let ss3_f = |c: u8| -> Vec<u8> {
        if m > 1 {
            format!("\x1b[1;{m}{}", c as char).into_bytes()
        } else {
            vec![0x1b, b'O', c]
        }
    };

    let seq: Option<Vec<u8>> = match key {
        Key::Enter => Some(b"\r".to_vec()),
        Key::Tab => Some(if shift {
            b"\x1b[Z".to_vec()
        } else {
            b"\t".to_vec()
        }),
        Key::Backspace => Some(if ctrl { vec![0x08] } else { vec![0x7f] }),
        Key::Escape => Some(b"\x1b".to_vec()),
        Key::ArrowUp => Some(csi_letter(b'A')),
        Key::ArrowDown => Some(csi_letter(b'B')),
        Key::ArrowRight => Some(csi_letter(b'C')),
        Key::ArrowLeft => Some(csi_letter(b'D')),
        Key::Home => Some(csi_letter(b'H')),
        Key::End => Some(csi_letter(b'F')),
        Key::PageUp => Some(csi_tilde(5)),
        Key::PageDown => Some(csi_tilde(6)),
        Key::Insert => Some(csi_tilde(2)),
        Key::Delete => Some(csi_tilde(3)),
        Key::F1 => Some(ss3_f(b'P')),
        Key::F2 => Some(ss3_f(b'Q')),
        Key::F3 => Some(ss3_f(b'R')),
        Key::F4 => Some(ss3_f(b'S')),
        Key::F5 => Some(csi_tilde(15)),
        Key::F6 => Some(csi_tilde(17)),
        Key::F7 => Some(csi_tilde(18)),
        Key::F8 => Some(csi_tilde(19)),
        Key::F9 => Some(csi_tilde(20)),
        Key::F10 => Some(csi_tilde(21)),
        Key::F11 => Some(csi_tilde(23)),
        Key::F12 => Some(csi_tilde(24)),
        _ => None,
    };

    if let Some(s) = seq {
        // Alt prefixes simple keys with ESC (modified CSI keys already encode Alt).
        if alt && matches!(key, Key::Enter | Key::Backspace | Key::Escape | Key::Tab) {
            let mut v = vec![0x1b];
            v.extend(s);
            return Some(v);
        }
        return Some(s);
    }

    if alt && !ctrl {
        let name = key.name();
        let ch = if name.len() == 1 {
            Some(name.as_bytes()[0])
        } else {
            // Unshifted punctuation (US positions); shifted characters
            // depend on the layout and come from the Text event instead.
            // (macOS: Option+punctuation types a character, see Text.)
            match key {
                _ if cfg!(target_os = "macos") => None,
                Key::Comma => Some(b','),
                Key::Period => Some(b'.'),
                Key::Minus => Some(b'-'),
                Key::Equals => Some(b'='),
                Key::Semicolon => Some(b';'),
                Key::Quote => Some(b'\''),
                Key::Slash => Some(b'/'),
                Key::Backslash => Some(b'\\'),
                Key::OpenBracket => Some(b'['),
                Key::CloseBracket => Some(b']'),
                Key::Backtick => Some(b'`'),
                _ => None,
            }
        };
        match ch {
            Some(c) if c.is_ascii_alphabetic() => {
                let c = if shift {
                    c.to_ascii_uppercase()
                } else {
                    c.to_ascii_lowercase()
                };
                return Some(vec![0x1b, c]);
            }
            Some(c) if !shift && c.is_ascii_graphic() => return Some(vec![0x1b, c]),
            _ => {}
        }
    }
    None
}

/// Windows: the character AltGr (+Shift) gives on a physical key in the
/// current keyboard layout, if any.
#[cfg(windows)]
mod win_altgr {
    use eframe::egui::Key;
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
        GetKeyboardLayout, MapVirtualKeyExW, ToUnicodeEx, MAPVK_VSC_TO_VK_EX, VK_CONTROL,
        VK_LCONTROL, VK_LSHIFT, VK_MENU, VK_RMENU, VK_SHIFT,
    };

    /// Set-1 scan code of a key at its (US layout) physical position.
    fn scan_code(k: Key) -> Option<u32> {
        use Key::*;
        Some(match k {
            Num1 => 0x02,
            Num2 => 0x03,
            Num3 => 0x04,
            Num4 => 0x05,
            Num5 => 0x06,
            Num6 => 0x07,
            Num7 => 0x08,
            Num8 => 0x09,
            Num9 => 0x0A,
            Num0 => 0x0B,
            Minus => 0x0C,
            Equals => 0x0D,
            Q => 0x10,
            W => 0x11,
            E => 0x12,
            R => 0x13,
            T => 0x14,
            Y => 0x15,
            U => 0x16,
            I => 0x17,
            O => 0x18,
            P => 0x19,
            OpenBracket => 0x1A,
            CloseBracket => 0x1B,
            A => 0x1E,
            S => 0x1F,
            D => 0x20,
            F => 0x21,
            G => 0x22,
            H => 0x23,
            J => 0x24,
            K => 0x25,
            L => 0x26,
            Semicolon => 0x27,
            Quote => 0x28,
            Backtick => 0x29,
            Backslash => 0x2B,
            Z => 0x2C,
            X => 0x2D,
            C => 0x2E,
            V => 0x2F,
            B => 0x30,
            N => 0x31,
            M => 0x32,
            Comma => 0x33,
            Period => 0x34,
            Slash => 0x35,
            _ => return None,
        })
    }

    pub fn altgr_text(physical: Key, shift: bool) -> Option<String> {
        let scan = scan_code(physical)?;
        // SAFETY: plain Win32 calls; the key-state and output buffers are
        // local arrays of the sizes passed.
        unsafe {
            let hkl = GetKeyboardLayout(0);
            let vk = MapVirtualKeyExW(scan, MAPVK_VSC_TO_VK_EX, hkl);
            if vk == 0 {
                return None;
            }
            let mut state = [0u8; 256];
            for k in [VK_CONTROL, VK_LCONTROL, VK_MENU, VK_RMENU] {
                state[k as usize] = 0x80;
            }
            if shift {
                state[VK_SHIFT as usize] = 0x80;
                state[VK_LSHIFT as usize] = 0x80;
            }
            let mut buf = [0u16; 8];
            // Flag 4: don't touch the keyboard's dead-key state.
            let n = ToUnicodeEx(
                vk,
                scan,
                state.as_ptr(),
                buf.as_mut_ptr(),
                buf.len() as i32,
                4,
                hkl,
            );
            if n <= 0 {
                return None;
            }
            let text = String::from_utf16_lossy(&buf[..n as usize]);
            // Ctrl+Alt+letter in layouts without AltGr gives a control
            // character (or nothing): that is a shortcut, not text.
            (!text.chars().any(char::is_control)).then_some(text)
        }
    }
}

/// What one terminal row draws, relative to the row's top-left corner.
#[derive(Clone, Default)]
struct RowPaint {
    hash: u64,
    /// Cell backgrounds.
    bgs: Vec<(Rect, Color32)>,
    /// ASCII text of the row (all colours / styles).
    job: Option<egui::text::LayoutJob>,
    /// Wide / non-ASCII glyphs, each centred in its cell(s).
    glyphs: Vec<(Pos2, String, FontId, Color32)>,
    /// Underlines under those glyphs.
    lines: Vec<([Pos2; 2], Color32)>,
    /// Faux-bold second pass (no bold face installed).
    faux_bold: Vec<(Pos2, String, Color32)>,
    /// The above laid out once (see [`RowPaint::realize`]): (top-left,
    /// galley, colour). Valid for the font atlas they were made with.
    laid: Vec<(Pos2, std::sync::Arc<egui::Galley>, Color32)>,
}

impl RowPaint {
    /// Lay out the row's text once; painting then reuses the galleys every
    /// frame instead of laying out (or hashing) each row and glyph again.
    fn realize(&mut self, fonts: &egui::text::Fonts, font: &FontId, fallback: Color32) {
        if let Some(job) = self.job.take() {
            self.laid
                .push((Pos2::ZERO, fonts.layout_job(job), fallback));
        }
        for (pos, text, f, c) in self.glyphs.drain(..) {
            let g = fonts.layout_no_wrap(text, f, c);
            let at = Align2::CENTER_CENTER.anchor_size(pos, g.size()).min;
            self.laid.push((at, g, c));
        }
        for (pos, text, c) in self.faux_bold.drain(..) {
            let g = fonts.layout_no_wrap(text, font.clone(), c);
            self.laid.push((pos, g, c));
        }
    }

    fn paint(&self, p: &egui::Painter, at: Vec2) {
        for (r, c) in &self.bgs {
            p.rect_filled(r.translate(at), CornerRadius::ZERO, *c);
        }
        for (pos, g, c) in &self.laid {
            p.galley(*pos + at, std::sync::Arc::clone(g), *c);
        }
        for (pts, c) in &self.lines {
            p.line_segment([pts[0] + at, pts[1] + at], Stroke::new(1.0_f32, *c));
        }
    }
}

/// A hashable stand-in for a vt100 colour.
fn color_key(c: vt100::Color) -> u32 {
    match c {
        vt100::Color::Default => 0,
        vt100::Color::Idx(i) => 0x100 | i as u32,
        vt100::Color::Rgb(r, g, b) => 0x0100_0000 | (r as u32) << 16 | (g as u32) << 8 | b as u32,
    }
}

#[allow(clippy::too_many_arguments)]
fn build_row(
    screen: &vt100::Screen,
    r: u16,
    cols: u16,
    char_w: f32,
    char_h: f32,
    font: &FontId,
    bold_font: &Option<FontId>,
    pal: &TermPalette,
    is_selected: &dyn Fn(u16) -> bool,
) -> RowPaint {
    let mut out = RowPaint::default();
    let mut job = egui::text::LayoutJob::default();
    let mut seg = String::new();
    // (colour, underline, bold) of the open text segment.
    type Fmt = (Color32, bool, bool);
    let mut seg_fmt: Option<Fmt> = None;
    let fmt = |(fg, ul, bold): Fmt| egui::TextFormat {
        font_id: match (bold_font, bold) {
            (Some(bf), true) => bf.clone(),
            _ => font.clone(),
        },
        color: fg,
        underline: if ul {
            Stroke::new(1.0_f32, fg)
        } else {
            Stroke::NONE
        },
        ..Default::default()
    };
    let push = |job: &mut egui::text::LayoutJob,
                seg: &mut String,
                seg_fmt: &mut Option<Fmt>,
                text: &str,
                f: Option<Fmt>| {
        // Blanks (f = None) join whatever segment is open, except an
        // underlined one: a plain blank must not carry the underline.
        let f2 = match f {
            Some(x) => Some(x),
            None => match *seg_fmt {
                Some((fg, true, b)) => Some((fg, false, b)),
                other => other,
            },
        };
        if f2 != *seg_fmt && !seg.is_empty() {
            job.append(
                &std::mem::take(seg),
                0.0,
                fmt(seg_fmt.unwrap_or((pal.fg, false, false))),
            );
        }
        *seg_fmt = f2;
        seg.push_str(text);
    };
    let mut c = 0u16;
    while c < cols {
        let Some(cell) = screen.cell(r, c) else {
            push(&mut job, &mut seg, &mut seg_fmt, " ", None);
            c += 1;
            continue;
        };
        if cell.is_wide_continuation() {
            c += 1;
            continue;
        }
        let width: u16 = if cell.is_wide() { 2 } else { 1 };
        let (mut fg, mut bg) = (
            resolve_color(cell.fgcolor(), true, cell.bold(), pal),
            resolve_color(cell.bgcolor(), false, false, pal),
        );
        if cell.inverse() {
            std::mem::swap(&mut fg, &mut bg);
        }
        if cell.dim() {
            fg = lerp_color(fg, bg, 0.45);
        }
        if is_selected(c) {
            bg = pal.selection;
        }
        let x = c as f32 * char_w;
        if bg != pal.bg {
            out.bgs.push((
                Rect::from_min_size(
                    Pos2::new(x, 0.0),
                    Vec2::new(char_w * width as f32 + 0.5, char_h + 0.5),
                ),
                bg,
            ));
        }
        let text = cell.contents();
        let ul = cell.underline();
        let bold = cell.bold();
        if text.is_empty() || text == " " {
            let f = ul.then_some((fg, true, false));
            push(&mut job, &mut seg, &mut seg_fmt, " ", f);
            c += width;
            continue;
        }
        if width == 1 && text.is_ascii() {
            push(
                &mut job,
                &mut seg,
                &mut seg_fmt,
                text,
                Some((fg, ul, bold && bold_font.is_some())),
            );
            if bold && bold_font.is_none() {
                match out.faux_bold.last_mut() {
                    Some((p0, s, bfg))
                        if *bfg == fg
                            && ((p0.x - 0.6) / char_w).round() as usize + s.len() == c as usize =>
                    {
                        s.push_str(text)
                    }
                    _ => out
                        .faux_bold
                        .push((Pos2::new(x + 0.6, 0.0), text.to_string(), fg)),
                }
            }
        } else {
            push(
                &mut job,
                &mut seg,
                &mut seg_fmt,
                if width == 2 { "  " } else { " " },
                None,
            );
            let cell_rect =
                Rect::from_min_size(Pos2::new(x, 0.0), Vec2::new(char_w * width as f32, char_h));
            let glyph_font = match (bold_font, bold) {
                (Some(bf), true) => bf.clone(),
                _ => font.clone(),
            };
            if bold && bold_font.is_none() {
                // faux bold, like the ASCII text
                out.glyphs.push((
                    cell_rect.center() + Vec2::new(0.6, 0.0),
                    text.to_string(),
                    glyph_font.clone(),
                    fg,
                ));
            }
            out.glyphs
                .push((cell_rect.center(), text.to_string(), glyph_font, fg));
            if ul {
                out.lines.push((
                    [
                        Pos2::new(cell_rect.left(), char_h - 1.5),
                        Pos2::new(cell_rect.right(), char_h - 1.5),
                    ],
                    fg,
                ));
            }
        }
        c += width;
    }
    // Trailing blanks draw nothing; skip them (underlined ones are visible).
    if !seg_fmt.is_some_and(|(_, ul, _)| ul) {
        let keep = seg.trim_end().len();
        seg.truncate(keep);
    }
    if !seg.is_empty() {
        job.append(&seg, 0.0, fmt(seg_fmt.unwrap_or((pal.fg, false, false))));
    }
    if !job.sections.is_empty() {
        out.job = Some(job);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arrows_respect_application_cursor_mode() {
        let none = egui::Modifiers::NONE;
        assert_eq!(
            map_key_to_pty_bytes(egui::Key::ArrowUp, none, false).unwrap(),
            b"\x1b[A"
        );
        assert_eq!(
            map_key_to_pty_bytes(egui::Key::ArrowUp, none, true).unwrap(),
            b"\x1bOA"
        );
        assert_eq!(
            map_key_to_pty_bytes(egui::Key::ArrowLeft, egui::Modifiers::CTRL, true).unwrap(),
            b"\x1b[1;5D"
        );
    }

    #[test]
    fn alt_keys_send_escape_prefix() {
        let alt = egui::Modifiers::ALT;
        let alt_shift = egui::Modifiers::ALT | egui::Modifiers::SHIFT;
        let ctrl_alt = egui::Modifiers::ALT | egui::Modifiers::CTRL;
        assert_eq!(
            map_key_to_pty_bytes(egui::Key::B, alt, false).unwrap(),
            b"\x1bb"
        );
        assert_eq!(
            map_key_to_pty_bytes(egui::Key::B, alt_shift, false).unwrap(),
            b"\x1bB"
        );
        // Shifted digits / punctuation depend on the layout: left to Text.
        assert!(map_key_to_pty_bytes(egui::Key::Num1, alt_shift, false).is_none());
        assert_eq!(
            map_key_to_pty_bytes(egui::Key::Num1, alt, false).unwrap(),
            b"\x1b1"
        );
        if !cfg!(target_os = "macos") {
            assert_eq!(
                map_key_to_pty_bytes(egui::Key::Period, alt, false).unwrap(),
                b"\x1b."
            );
        }
        assert_eq!(
            map_key_to_pty_bytes(egui::Key::A, ctrl_alt, false).unwrap(),
            vec![0x1b, 0x01]
        );
    }

    #[test]
    fn ctrl_letters_map_to_control_bytes() {
        assert_eq!(
            map_key_to_pty_bytes(egui::Key::C, egui::Modifiers::CTRL, false).unwrap(),
            [3]
        );
        assert_eq!(
            map_key_to_pty_bytes(egui::Key::X, egui::Modifiers::CTRL, false).unwrap(),
            [0x18]
        );
    }

    #[test]
    fn log_handles_cr_backspace_and_escapes() {
        let mut t = TermView::new(24, 80, 1000);
        t.feed(b"\x1b[31mhello\x1b[0m\r\nabc\x08\x08XY\r\nprogress 10%\rprogress 99%\r\n");
        assert_eq!(t.log(), "hello\naXY\nprogress 99%\n");
    }

    #[test]
    fn log_keeps_split_utf8() {
        let mut t = TermView::new(24, 80, 1000);
        let s = "中文".as_bytes();
        t.feed(&s[..2]);
        t.feed(&s[2..]);
        assert_eq!(t.log(), "中文");
    }

    #[test]
    fn paste_uses_cr_and_bracketed_mode() {
        let mut t = TermView::new(24, 80, 1000);
        assert_eq!(t.encode_paste("a\nb"), b"a\rb");
        t.feed(b"\x1b[?2004h");
        assert_eq!(t.encode_paste("x"), b"\x1b[200~x\x1b[201~");
    }
}

#[cfg(test)]
mod notice_tests {
    #[test]
    fn notice_survives_unfinished_escape_sequence() {
        let mut t = super::TermView::new(5, 40, 100);
        t.feed(b"\x1b]0;half a title"); // OSC left open (output cut off)
        t.feed_notice("NOTICE", 33);
        assert!(t.parser.screen().contents().contains("NOTICE"));
        // …and in the log too, on a line of its own.
        assert_eq!(t.log(), "NOTICE\n");
        t.feed(b"$ \x1b[1");
        t.feed_notice("AGAIN", 33);
        assert_eq!(t.log(), "NOTICE\n$ \nAGAIN\n");
    }

    #[test]
    fn shrinking_keeps_the_cursor_line() {
        let mut t = super::TermView::new(10, 40, 100);
        for i in 1..10 {
            t.feed(format!("line {i}\r\n").as_bytes());
        }
        t.feed(b"prompt$ ");
        t.resize_grid(4, 40);
        let screen = t.parser.screen();
        let rows: Vec<String> = screen.rows(0, 40).collect();
        assert_eq!(rows[3].trim_end(), "prompt$");
        assert_eq!(rows[2].trim_end(), "line 9");
        assert_eq!(screen.cursor_position(), (3, 8));
        // What scrolled off is in the scrollback, not lost.
        assert_eq!(t.scrollback_len(), 6);
    }

    #[test]
    fn log_backspace_over_wide_characters() {
        let mut t = super::TermView::new(5, 40, 100);
        t.feed("x中".as_bytes());
        // A shell erases a wide character with two backspaces.
        t.feed(b"\x08\x08  \x08\x08");
        t.feed(b"y");
        assert_eq!(t.log(), "xy");
    }
}

#[cfg(test)]
mod anchor_tests {
    use super::*;

    #[test]
    fn anchor_follows_text_with_full_scrollback() {
        let mut t = TermView::new(5, 20, 10);
        for i in 0..40 {
            t.feed(format!("line-{i}\r\n").as_bytes());
        }
        t.dragging = true;
        t.sel_anchor = Some(SelPos { row: 2, col: 0 });
        let before = t.abs_row_text(2).unwrap();
        for i in 40..43 {
            t.feed(format!("line-{i}\r\n").as_bytes());
        }
        t.resync_anchor();
        let a = t.sel_anchor.unwrap();
        assert_eq!(t.abs_row_text(a.row).unwrap(), before);
        assert_eq!(a.row, -1);
    }

    #[test]
    fn anchor_stays_when_output_does_not_scroll() {
        let mut t = TermView::new(10, 20, 10);
        t.feed(b"a\r\nb\r\nc\r\n");
        t.dragging = true;
        t.sel_anchor = Some(SelPos { row: 1, col: 0 });
        t.feed(b"d\r\ne\r\n");
        t.resync_anchor();
        assert_eq!(t.sel_anchor.unwrap().row, 1);
    }

    #[test]
    fn selection_survives_scrolling_output_and_clears_when_overwritten() {
        let mut t = TermView::new(5, 20, 10);
        t.feed(b"first\r\nsecond\r\nthird\r\n");
        t.sel_anchor = Some(SelPos { row: 1, col: 0 });
        t.sel_head = Some(SelPos { row: 1, col: 6 });
        t.update_sel_text();
        assert_eq!(t.sel_text, "second");
        assert!(t.has_selection());

        // New lines push the selected row up into scrollback; selection survives.
        for i in 0..8 {
            t.feed(format!("more-{i}\r\n").as_bytes());
            t.resync_anchor();
            assert!(t.has_selection(), "selection lost at line {i}");
        }
        assert_eq!(t.current_sel_text(), "second");

        // When enough output arrives to evict it from scrollback, selection clears.
        for i in 0..20 {
            t.feed(format!("evict-{i}\r\n").as_bytes());
        }
        t.resync_anchor();
        assert!(!t.has_selection());

        // Overwriting the selected line in place also clears selection.
        t.feed(b"\x1b[2J\x1b[Hhello world\r\n");
        t.sel_anchor = Some(SelPos { row: 0, col: 0 });
        t.sel_head = Some(SelPos { row: 0, col: 5 });
        t.update_sel_text();
        assert!(t.has_selection());
        t.feed(b"\x1b[1;1Hother");
        t.resync_anchor();
        assert!(!t.has_selection());
    }
}

#[cfg(test)]
mod url_and_osc7_tests {
    use super::*;

    #[test]
    fn url_detection_handles_query_strings_punctuation_and_balanced_parens() {
        let mut t = TermView::new(4, 96, 100);
        t.feed(b"Docs: (https://example.com/a_(v1)?q=1&b=2). Bare: http://");
        assert_eq!(t.url_at_cell(0, 0), None);
        assert_eq!(
            t.url_at_cell(0, 10),
            Some((7, 41, "https://example.com/a_(v1)?q=1&b=2".to_string()))
        );
        // Trailing '.' and unmatched ')' are not part of the URL.
        assert_eq!(t.url_at_cell(0, 41), None);
        // Incomplete scheme with no host must not match.
        assert_eq!(t.url_at_cell(0, 52), None);
    }

    #[test]
    fn osc7_parses_file_uri_with_percent_encoding_and_bel_or_st() {
        let mut t = TermView::new(4, 80, 100);
        assert_eq!(t.take_osc7_cwd(), None);

        // BEL-terminated with host & percent-encoded space
        t.feed(b"\x1b]7;file://vps-1/var/log/my%20app/\x07");
        assert_eq!(t.take_osc7_cwd().as_deref(), Some("/var/log/my app"));
        assert_eq!(t.take_osc7_cwd(), None);

        // ST-terminated (`ESC \`) with empty host and UTF-8 percent encoding
        t.feed(b"\x1b]7;file:///srv/%E4%B8%AD%E6%96%87\x1b\\");
        assert_eq!(t.take_osc7_cwd().as_deref(), Some("/srv/中文"));

        // Non-OSC-7 must be ignored, control chars rejected
        t.feed(b"\x1b]0;window title\x07\x1b]7;file://host/bad%00path\x07");
        assert_eq!(t.take_osc7_cwd(), None);
    }
}
