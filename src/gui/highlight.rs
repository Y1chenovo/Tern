//! Lightweight syntax highlighting and UTF-8-safe find/replace for the built-in editor.

use super::term::TermPalette;
use super::theme::Tokens;
use eframe::egui::{self, Color32, FontId, TextFormat};

const MAX_HIGHLIGHT_BYTES: usize = 256 * 1024;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SyntaxLang {
    Plain,
    Json,
    TomlIni,
    Yaml,
    Shell,
    Nginx,
}

impl SyntaxLang {
    pub fn detect(name: &str, path: &str) -> Self {
        let lower_name = name.to_ascii_lowercase();
        let lower_path = path.to_ascii_lowercase();
        if lower_name == "nginx.conf"
            || (lower_name.ends_with(".conf") && lower_path.contains("nginx"))
        {
            return Self::Nginx;
        }
        if matches!(
            lower_name.as_str(),
            ".bashrc" | ".bash_profile" | ".zshrc" | ".profile" | ".env"
        ) || lower_name.starts_with(".env.")
        {
            return Self::Shell;
        }
        let ext = lower_name.rsplit('.').next().unwrap_or("");
        if lower_name.contains('.') {
            match ext {
                "json" | "jsonc" => Self::Json,
                "toml" | "ini" | "cfg" | "conf" | "service" | "socket" | "timer" | "unit" => {
                    Self::TomlIni
                }
                "yml" | "yaml" => Self::Yaml,
                "sh" | "bash" | "zsh" | "fish" | "env" => Self::Shell,
                _ => Self::Plain,
            }
        } else {
            Self::Plain
        }
    }

    pub fn label(self) -> Option<&'static str> {
        match self {
            Self::Plain => None,
            Self::Json => Some("JSON"),
            Self::TomlIni => Some("TOML / INI"),
            Self::Yaml => Some("YAML"),
            Self::Shell => Some("Shell"),
            Self::Nginx => Some("Nginx"),
        }
    }
}

/// Find non-overlapping `[start, end)` byte ranges matching `query`. Guaranteed
/// to stay on valid UTF-8 character boundaries for both ASCII and Unicode text.
pub fn find_matches(text: &str, query: &str, case_sensitive: bool) -> Vec<(usize, usize)> {
    if query.is_empty() || text.is_empty() {
        return Vec::new();
    }
    if case_sensitive {
        let mut out = Vec::new();
        let mut i = 0;
        while let Some(off) = text[i..].find(query) {
            let start = i + off;
            let end = start + query.len();
            out.push((start, end));
            i = end.max(start + 1);
        }
        return out;
    }
    if text.is_ascii() && query.is_ascii() {
        let tb = text.as_bytes();
        let qb = query.as_bytes();
        if qb.len() > tb.len() {
            return Vec::new();
        }
        let mut out = Vec::new();
        let mut i = 0;
        while i + qb.len() <= tb.len() {
            if tb[i..i + qb.len()].eq_ignore_ascii_case(qb) {
                out.push((i, i + qb.len()));
                i += qb.len();
            } else {
                i += 1;
            }
        }
        return out;
    }

    let q_lower: Vec<char> = query.chars().flat_map(|c| c.to_lowercase()).collect();
    if q_lower.is_empty() {
        return Vec::new();
    }
    let mut lower_chars = Vec::with_capacity(text.len());
    let mut orig_start = Vec::with_capacity(text.len());
    let mut orig_end = Vec::with_capacity(text.len());
    for (start, ch) in text.char_indices() {
        let end = start + ch.len_utf8();
        for lc in ch.to_lowercase() {
            lower_chars.push(lc);
            orig_start.push(start);
            orig_end.push(end);
        }
    }
    let m = q_lower.len();
    if m > lower_chars.len() {
        return Vec::new();
    }
    let mut out = Vec::new();
    let mut k = 0;
    while k + m <= lower_chars.len() {
        let starts_char = k == 0 || orig_start[k] != orig_start[k - 1];
        let ends_char = k + m == lower_chars.len() || orig_end[k + m - 1] != orig_end[k + m];
        if starts_char && ends_char && lower_chars[k..k + m] == q_lower[..] {
            out.push((orig_start[k], orig_end[k + m - 1]));
            k += m;
        } else {
            k += 1;
        }
    }
    out
}

/// Replace every match of `query` in `text` with `replacement`, returning `(new_text, count)`.
pub fn replace_all_matches(
    text: &str,
    query: &str,
    replacement: &str,
    case_sensitive: bool,
) -> (String, usize) {
    let matches = find_matches(text, query, case_sensitive);
    if matches.is_empty() {
        return (text.to_string(), 0);
    }
    let mut out = String::with_capacity(text.len());
    let mut last = 0;
    let count = matches.len();
    for (s, e) in matches {
        out.push_str(&text[last..s]);
        out.push_str(replacement);
        last = e;
    }
    out.push_str(&text[last..]);
    (out, count)
}

/// Convert UTF-8 byte offsets `[start, end)` to character indices `(char_start, char_end)`.
pub fn byte_range_to_char_range(text: &str, start: usize, end: usize) -> (usize, usize) {
    let start = start.min(text.len());
    let end = end.min(text.len()).max(start);
    let c0 = text[..start].chars().count();
    let c1 = c0 + text[start..end].chars().count();
    (c0, c1)
}

#[derive(Clone, Copy)]
struct Span {
    start: usize,
    end: usize,
    color: Color32,
}

fn push_span(out: &mut Vec<Span>, start: usize, end: usize, color: Color32) {
    if end > start {
        out.push(Span { start, end, color });
    }
}

fn scan_quoted(line: &str, base: usize, start: usize, quote: char) -> usize {
    let mut esc = false;
    for (off, ch) in line[start + quote.len_utf8()..].char_indices() {
        if esc {
            esc = false;
            continue;
        }
        if ch == '\\' && quote != '\'' {
            esc = true;
            continue;
        }
        if ch == quote {
            return base + start + quote.len_utf8() + off + ch.len_utf8();
        }
    }
    base + line.len()
}

fn is_ident_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.')
}

fn tokenize_line(
    lang: SyntaxLang,
    line: &str,
    base: usize,
    pal: &TermPalette,
    out: &mut Vec<Span>,
) {
    let comment_col = pal.ansi[8];
    let str_col = pal.ansi[2];
    let key_col = pal.ansi[4];
    let kw_col = pal.ansi[5];
    let num_col = pal.ansi[3];

    let trimmed = line.trim_start();
    let lead = line.len() - trimmed.len();
    if trimmed.is_empty() {
        return;
    }

    // Full-line comments
    if match lang {
        SyntaxLang::Json => trimmed.starts_with("//"),
        SyntaxLang::TomlIni => trimmed.starts_with('#') || trimmed.starts_with(';'),
        SyntaxLang::Yaml | SyntaxLang::Shell | SyntaxLang::Nginx => trimmed.starts_with('#'),
        SyntaxLang::Plain => false,
    } {
        push_span(out, base + lead, base + line.len(), comment_col);
        return;
    }

    // `[section]` headers in TOML/INI
    if lang == SyntaxLang::TomlIni && trimmed.starts_with('[') {
        if let Some(end_b) = trimmed.find(']') {
            push_span(out, base + lead, base + lead + end_b + 1, kw_col);
            if let Some(c) = trimmed[end_b + 1..].find('#') {
                push_span(
                    out,
                    base + lead + end_b + 1 + c,
                    base + line.len(),
                    comment_col,
                );
            }
            return;
        }
    }

    let mut pos = lead;
    let mut expect_nginx_directive = lang == SyntaxLang::Nginx;
    let bytes = line.as_bytes();
    while pos < line.len() {
        let rest = &line[pos..];
        let b = bytes[pos];
        if b.is_ascii_whitespace() {
            pos += 1;
            continue;
        }
        // Comments
        if (b == b'#'
            && matches!(
                lang,
                SyntaxLang::TomlIni | SyntaxLang::Yaml | SyntaxLang::Shell | SyntaxLang::Nginx
            ))
            || (b == b';' && lang == SyntaxLang::TomlIni && pos == lead)
            || (lang == SyntaxLang::Json && rest.starts_with("//"))
        {
            push_span(out, base + pos, base + line.len(), comment_col);
            break;
        }
        // Strings
        if b == b'"' || (b == b'\'' && lang != SyntaxLang::Json) {
            let abs_end = scan_quoted(line, base, pos, b as char);
            let after = line[(abs_end - base).min(line.len())..].trim_start();
            let col =
                if matches!(lang, SyntaxLang::Json | SyntaxLang::Yaml) && after.starts_with(':') {
                    key_col
                } else {
                    str_col
                };
            push_span(out, base + pos, abs_end, col);
            pos = abs_end - base;
            expect_nginx_directive = false;
            continue;
        }
        // YAML / TOML / INI unquoted key (`key:` or `key =`)
        let is_key_pos = pos == lead
            || (lang == SyntaxLang::Yaml && bytes[..pos].iter().all(|&c| c == b' ' || c == b'-'));
        if is_key_pos && matches!(lang, SyntaxLang::TomlIni | SyntaxLang::Yaml) {
            let sep =
                rest.find(|c: char| !(c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.')));
            if let Some(klen) = sep.filter(|&n| n > 0) {
                let after = rest[klen..].trim_start();
                if (lang == SyntaxLang::TomlIni && after.starts_with('='))
                    || (lang == SyntaxLang::Yaml && after.starts_with(':'))
                {
                    push_span(out, base + pos, base + pos + klen, key_col);
                    pos += klen;
                    continue;
                }
            }
        }
        // Shell `$VAR` / `${VAR}`
        if lang == SyntaxLang::Shell && b == b'$' {
            let mut end = pos + 1;
            if end < bytes.len() && bytes[end] == b'{' {
                if let Some(close) = line[end..].find('}') {
                    end += close + 1;
                }
            } else {
                while end < bytes.len()
                    && (bytes[end].is_ascii_alphanumeric() || bytes[end] == b'_')
                {
                    end += 1;
                }
            }
            if end > pos + 1 {
                push_span(out, base + pos, base + end, key_col);
                pos = end;
                continue;
            }
        }
        // Identifiers / keywords / numbers
        if is_ident_byte(b) {
            let mut end = pos + 1;
            while end < bytes.len() && is_ident_byte(bytes[end]) {
                end += 1;
            }
            let tok = &line[pos..end];
            if expect_nginx_directive {
                let col = if matches!(
                    tok,
                    "server" | "location" | "http" | "events" | "upstream" | "stream" | "if"
                ) {
                    kw_col
                } else {
                    key_col
                };
                push_span(out, base + pos, base + end, col);
                expect_nginx_directive = false;
            } else if matches!(tok, "true" | "false" | "null" | "yes" | "no" | "on" | "off")
                || (lang == SyntaxLang::Shell
                    && matches!(
                        tok,
                        "if" | "then"
                            | "else"
                            | "elif"
                            | "fi"
                            | "for"
                            | "in"
                            | "do"
                            | "done"
                            | "while"
                            | "case"
                            | "esac"
                            | "function"
                            | "export"
                            | "local"
                            | "return"
                            | "exit"
                    ))
            {
                push_span(out, base + pos, base + end, kw_col);
            } else if tok.bytes().all(|c| c.is_ascii_digit() || c == b'.')
                && tok.bytes().any(|c| c.is_ascii_digit())
            {
                push_span(out, base + pos, base + end, num_col);
            }
            pos = end;
            continue;
        }
        if !b.is_ascii() {
            let ch_len = rest.chars().next().map(char::len_utf8).unwrap_or(1);
            pos += ch_len;
        } else {
            pos += 1;
        }
        expect_nginx_directive = false;
    }
}

fn tokenize(lang: SyntaxLang, text: &str, pal: &TermPalette) -> Vec<Span> {
    if lang == SyntaxLang::Plain || text.len() > MAX_HIGHLIGHT_BYTES {
        return Vec::new();
    }
    let mut out = Vec::new();
    let mut base = 0;
    for line in text.split_inclusive('\n') {
        let stripped = line.strip_suffix('\n').unwrap_or(line);
        let stripped = stripped.strip_suffix('\r').unwrap_or(stripped);
        tokenize_line(lang, stripped, base, pal, &mut out);
        base += line.len();
    }
    out
}

pub struct EditorHighlight<'a> {
    pub lang: SyntaxLang,
    pub pal: &'a TermPalette,
    pub tok: &'a Tokens,
    pub matches: &'a [(usize, usize)],
    pub active_match: Option<usize>,
}

/// Build an `egui::text::LayoutJob` combining syntax foreground colors and search highlights.
pub fn build_editor_job(
    text: &str,
    font: &FontId,
    wrap_width: f32,
    hl: &EditorHighlight<'_>,
) -> egui::text::LayoutJob {
    let mut job = egui::text::LayoutJob {
        text: text.to_string(),
        wrap: egui::text::TextWrapping {
            max_width: wrap_width,
            ..Default::default()
        },
        ..Default::default()
    };
    if text.is_empty() {
        return job;
    }

    let fg_spans = tokenize(hl.lang, text, hl.pal);
    if fg_spans.is_empty() && hl.matches.is_empty() {
        job.sections.push(egui::text::LayoutSection {
            leading_space: 0.0,
            byte_range: 0..text.len(),
            format: TextFormat {
                font_id: font.clone(),
                color: hl.pal.fg,
                ..Default::default()
            },
        });
        return job;
    }

    let mut bounds = Vec::with_capacity(2 + (fg_spans.len() + hl.matches.len()) * 2);
    bounds.push(0usize);
    bounds.push(text.len());
    for s in &fg_spans {
        bounds.push(s.start.min(text.len()));
        bounds.push(s.end.min(text.len()));
    }
    for &(s, e) in hl.matches {
        bounds.push(s.min(text.len()));
        bounds.push(e.min(text.len()));
    }
    bounds.sort_unstable();
    bounds.dedup();

    let match_bg = hl
        .tok
        .warn
        .gamma_multiply(if hl.tok.dark { 0.30 } else { 0.28 });
    let active_bg = hl
        .tok
        .accent
        .gamma_multiply(if hl.tok.dark { 0.50 } else { 0.38 });

    let mut fi = 0usize;
    let mut mi = 0usize;
    for w in bounds.windows(2) {
        let (a, b) = (w[0], w[1]);
        if a >= b {
            continue;
        }
        while fi < fg_spans.len() && fg_spans[fi].end <= a {
            fi += 1;
        }
        while mi < hl.matches.len() && hl.matches[mi].1 <= a {
            mi += 1;
        }
        let fg = if fi < fg_spans.len() && fg_spans[fi].start <= a && a < fg_spans[fi].end {
            fg_spans[fi].color
        } else {
            hl.pal.fg
        };
        let bg = if mi < hl.matches.len() && hl.matches[mi].0 <= a && a < hl.matches[mi].1 {
            if hl.active_match == Some(mi) {
                active_bg
            } else {
                match_bg
            }
        } else {
            Color32::TRANSPARENT
        };

        if let Some(prev) = job.sections.last_mut() {
            if prev.byte_range.end == a && prev.format.color == fg && prev.format.background == bg {
                prev.byte_range.end = b;
                continue;
            }
        }
        job.sections.push(egui::text::LayoutSection {
            leading_space: 0.0,
            byte_range: a..b,
            format: TextFormat {
                font_id: font.clone(),
                color: fg,
                background: bg,
                ..Default::default()
            },
        });
    }
    job
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gui::theme::{term_palette, tokens};
    use crate::profiles::ThemePref;

    #[test]
    fn syntax_lang_detects_common_config_formats() {
        assert_eq!(
            SyntaxLang::detect("nginx.conf", "/etc/nginx/nginx.conf"),
            SyntaxLang::Nginx
        );
        assert_eq!(
            SyntaxLang::detect("site.conf", "/etc/nginx/conf.d/site.conf"),
            SyntaxLang::Nginx
        );
        assert_eq!(
            SyntaxLang::detect("config.json", "/app/config.json"),
            SyntaxLang::Json
        );
        assert_eq!(
            SyntaxLang::detect("app.toml", "/app/app.toml"),
            SyntaxLang::TomlIni
        );
        assert_eq!(
            SyntaxLang::detect("compose.yaml", "/app/compose.yaml"),
            SyntaxLang::Yaml
        );
        assert_eq!(
            SyntaxLang::detect(".env.production", "/app/.env.production"),
            SyntaxLang::Shell
        );
        assert_eq!(
            SyntaxLang::detect("README.txt", "/app/README.txt"),
            SyntaxLang::Plain
        );
    }

    #[test]
    fn find_and_replace_handle_unicode_and_case_options() {
        let src = "Hello 世界, hello HELLO!";
        let ci = find_matches(src, "hello", false);
        assert_eq!(ci, vec![(0, 5), (14, 19), (20, 25)]);
        assert_eq!(find_matches(src, "hello", true), vec![(14, 19)]);

        let (replaced, n) = replace_all_matches(src, "hello", "Hi", false);
        assert_eq!(n, 3);
        assert_eq!(replaced, "Hi 世界, Hi Hi!");

        let cjk = "配置 Server：SERVER";
        let m = find_matches(cjk, "server", false);
        assert_eq!(m.len(), 2);
        assert_eq!(&cjk[m[0].0..m[0].1], "Server");
        assert_eq!(&cjk[m[1].0..m[1].1], "SERVER");
        assert_eq!(byte_range_to_char_range(cjk, m[1].0, m[1].1), (10, 16));
    }

    #[test]
    fn layout_job_covers_entire_text_without_gaps() {
        let text = "# comment\nserver {\n    listen 80;\n}\n";
        let font = FontId::monospace(13.0);
        let pal = term_palette(ThemePref::Dark);
        let tok = tokens(ThemePref::Dark);
        let matches = find_matches(text, "server", false);
        let hl = EditorHighlight {
            lang: SyntaxLang::Nginx,
            pal: &pal,
            tok: &tok,
            matches: &matches,
            active_match: Some(0),
        };
        let job = build_editor_job(text, &font, f32::INFINITY, &hl);
        let mut cur = 0usize;
        for sec in &job.sections {
            assert_eq!(sec.byte_range.start, cur);
            assert!(sec.byte_range.end > sec.byte_range.start);
            assert!(text.is_char_boundary(sec.byte_range.start));
            assert!(text.is_char_boundary(sec.byte_range.end));
            cur = sec.byte_range.end;
        }
        assert_eq!(cur, text.len());
    }
}
