//! Small design system: vector icons (SF Symbols–like line icons) and controls
//! with fixed metrics, so every row lines up regardless of font or DPI.

use super::theme::{tok, SEMIBOLD};
use eframe::egui::{
    self, Align, Color32, CornerRadius, FontFamily, FontId, Layout, Pos2, Rect, Response, Sense,
    Shape, Stroke, StrokeKind, Ui, UiBuilder, Vec2,
};
use std::f32::consts::PI;

/// Standard control height.
pub const CTRL_H: f32 = 28.0;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[allow(dead_code)]
pub enum Icon {
    Grid,
    Expand,
    Plus,
    Minus,
    Close,
    Sidebar,
    Chart,
    Sun,
    Moon,
    Globe,
    Gear,
    Terminal,
    Folder,
    FolderFill,
    File,
    FileLink,
    Upload,
    Download,
    Refresh,
    Home,
    ArrowUp,
    ArrowDown,
    Pencil,
    Play,
    Stop,
    Search,
    Star,
    StarFill,
    Trash,
    Copy,
    ChevronDown,
    ChevronRight,
    ChevronLeft,
    Desktop,
    Drive,
    Server,
    Bolt,
    Key,
    Eye,
    EyeOff,
    Ellipsis,
    Check,
    Power,
    Warning,
    Keyboard,
}

fn arc(c: Pos2, r: f32, a0: f32, a1: f32, n: usize) -> Vec<Pos2> {
    (0..=n)
        .map(|i| {
            let a = a0 + (a1 - a0) * i as f32 / n as f32;
            Pos2::new(c.x + r * a.cos(), c.y + r * a.sin())
        })
        .collect()
}

/// Paint `icon` centred in `rect` (designed on a 16×16 grid).
pub fn paint_icon(p: &egui::Painter, rect: Rect, icon: Icon, color: Color32) {
    let s = rect.width().min(rect.height()) / 16.0;
    let o = rect.center() - Vec2::splat(8.0 * s);
    let pt = |x: f32, y: f32| Pos2::new(o.x + x * s, o.y + y * s);
    let w = (1.45 * s).max(1.1);
    let st = Stroke::new(w, color);
    let line = |pts: Vec<Pos2>| p.add(Shape::line(pts, st));
    let closed = |pts: Vec<Pos2>| p.add(Shape::closed_line(pts, st));
    let rrect = |x0: f32, y0: f32, x1: f32, y1: f32, r: f32| {
        p.rect_stroke(
            Rect::from_min_max(pt(x0, y0), pt(x1, y1)),
            CornerRadius::same((r * s).round() as u8),
            st,
            StrokeKind::Middle,
        );
    };
    match icon {
        Icon::Grid => {
            for (x, y) in [(2.2, 2.2), (8.8, 2.2), (2.2, 8.8), (8.8, 8.8)] {
                rrect(x, y, x + 5.0, y + 5.0, 1.4);
            }
        }
        Icon::Expand => {
            line(vec![pt(9.5, 2.5), pt(13.5, 2.5), pt(13.5, 6.5)]);
            line(vec![pt(13.5, 2.5), pt(9.0, 7.0)]);
            line(vec![pt(6.5, 13.5), pt(2.5, 13.5), pt(2.5, 9.5)]);
            line(vec![pt(2.5, 13.5), pt(7.0, 9.0)]);
        }
        Icon::Plus => {
            line(vec![pt(8.0, 3.0), pt(8.0, 13.0)]);
            line(vec![pt(3.0, 8.0), pt(13.0, 8.0)]);
        }
        Icon::Minus => {
            line(vec![pt(3.0, 8.0), pt(13.0, 8.0)]);
        }
        Icon::Close => {
            line(vec![pt(4.0, 4.0), pt(12.0, 12.0)]);
            line(vec![pt(12.0, 4.0), pt(4.0, 12.0)]);
        }
        Icon::Sidebar => {
            rrect(2.0, 3.0, 14.0, 13.0, 2.5);
            line(vec![pt(6.5, 3.0), pt(6.5, 13.0)]);
        }
        Icon::Chart => {
            for (x, top) in [(4.0, 9.0), (8.0, 4.0), (12.0, 7.0)] {
                p.rect_filled(
                    Rect::from_min_max(pt(x - 1.1, top), pt(x + 1.1, 13.0)),
                    CornerRadius::same((s).round() as u8),
                    color,
                );
            }
        }
        Icon::Sun => {
            p.circle_stroke(pt(8.0, 8.0), 2.8 * s, st);
            for i in 0..8 {
                let a = i as f32 * PI / 4.0;
                let (c, sn) = (a.cos(), a.sin());
                line(vec![
                    pt(8.0 + 4.8 * c, 8.0 + 4.8 * sn),
                    pt(8.0 + 6.4 * c, 8.0 + 6.4 * sn),
                ]);
            }
        }
        Icon::Moon => {
            // Crescent = outer circle minus an offset circle.
            let c1 = pt(8.0, 8.0);
            let r1 = 5.6 * s;
            let c2 = pt(10.8, 5.6);
            let r2 = 4.6 * s;
            let outer: Vec<Pos2> = arc(c1, r1, 0.0, 2.0 * PI, 96).into_iter().collect();
            // rotate so we start just outside c2
            let start = (0..outer.len())
                .find(|&i| {
                    let prev = outer[(i + outer.len() - 1) % outer.len()];
                    prev.distance(c2) < r2 && outer[i].distance(c2) >= r2
                })
                .unwrap_or(0);
            let mut pts: Vec<Pos2> = Vec::new();
            for k in 0..outer.len() {
                let q = outer[(start + k) % outer.len()];
                if q.distance(c2) >= r2 {
                    pts.push(q);
                } else {
                    break;
                }
            }
            let inner: Vec<Pos2> = arc(c2, r2, 2.0 * PI, 0.0, 96)
                .into_iter()
                .filter(|q| q.distance(c1) <= r1)
                .collect();
            // walk inner arc from the end of the outer arc back to its start
            if let (Some(&end), Some(&begin)) = (pts.last(), pts.first()) {
                let mut inner = inner;
                if !inner.is_empty() {
                    let d_first = inner[0].distance(end);
                    let d_last = inner[inner.len() - 1].distance(end);
                    if d_last < d_first {
                        inner.reverse();
                    }
                }
                pts.extend(inner);
                let _ = begin;
            }
            if pts.len() > 2 {
                closed(pts);
            }
        }
        Icon::Globe => {
            p.circle_stroke(pt(8.0, 8.0), 6.0 * s, st);
            let ell: Vec<Pos2> = (0..=48)
                .map(|i| {
                    let a = i as f32 / 48.0 * 2.0 * PI;
                    pt(8.0 + 2.6 * a.cos(), 8.0 + 6.0 * a.sin())
                })
                .collect();
            line(ell);
            line(vec![pt(2.0, 8.0), pt(14.0, 8.0)]);
            line(vec![pt(3.2, 5.0), pt(12.8, 5.0)]);
            line(vec![pt(3.2, 11.0), pt(12.8, 11.0)]);
        }
        Icon::Gear => {
            let c = pt(8.0, 8.0);
            let mut pts = Vec::new();
            for i in 0..8 {
                let a = i as f32 * PI / 4.0;
                let d = PI / 180.0;
                for (r, da) in [(4.6, -14.0), (6.4, -8.0), (6.4, 8.0), (4.6, 14.0)] {
                    let aa = a + da * d;
                    pts.push(Pos2::new(c.x + r * s * aa.cos(), c.y + r * s * aa.sin()));
                }
            }
            closed(pts);
            p.circle_stroke(c, 2.0 * s, st);
        }
        Icon::Terminal => {
            rrect(1.8, 2.8, 14.2, 13.2, 2.5);
            line(vec![pt(4.8, 6.0), pt(7.0, 8.0), pt(4.8, 10.0)]);
            line(vec![pt(8.5, 10.2), pt(11.5, 10.2)]);
        }
        Icon::Folder | Icon::FolderFill => {
            let pts = vec![
                pt(1.8, 4.0),
                pt(6.0, 4.0),
                pt(7.6, 5.6),
                pt(14.2, 5.6),
                pt(14.2, 13.0),
                pt(1.8, 13.0),
            ];
            if icon == Icon::FolderFill {
                // body (convex) + tab
                p.add(Shape::convex_polygon(
                    vec![pt(1.8, 5.6), pt(14.2, 5.6), pt(14.2, 13.0), pt(1.8, 13.0)],
                    color.gamma_multiply(0.85),
                    Stroke::NONE,
                ));
                p.add(Shape::convex_polygon(
                    vec![pt(1.8, 4.0), pt(6.0, 4.0), pt(7.6, 5.6), pt(1.8, 5.6)],
                    color,
                    Stroke::NONE,
                ));
                p.line_segment([pt(1.8, 7.0), pt(14.2, 7.0)], Stroke::new(w * 0.8, color));
            } else {
                closed(pts);
                line(vec![pt(1.8, 7.0), pt(14.2, 7.0)]);
            }
        }
        Icon::File | Icon::FileLink => {
            closed(vec![
                pt(3.5, 1.8),
                pt(9.8, 1.8),
                pt(12.8, 4.8),
                pt(12.8, 14.2),
                pt(3.5, 14.2),
            ]);
            line(vec![pt(9.6, 1.8), pt(9.6, 5.0), pt(12.8, 5.0)]);
            if icon == Icon::FileLink {
                line(vec![pt(5.8, 11.8), pt(9.6, 8.0)]);
                line(vec![pt(7.0, 8.0), pt(9.6, 8.0), pt(9.6, 10.6)]);
            }
        }
        Icon::Upload | Icon::Download => {
            line(vec![
                pt(3.0, 10.0),
                pt(3.0, 13.5),
                pt(13.0, 13.5),
                pt(13.0, 10.0),
            ]);
            if icon == Icon::Upload {
                line(vec![pt(8.0, 10.5), pt(8.0, 2.5)]);
                line(vec![pt(5.0, 5.5), pt(8.0, 2.5), pt(11.0, 5.5)]);
            } else {
                line(vec![pt(8.0, 2.5), pt(8.0, 10.5)]);
                line(vec![pt(5.0, 7.5), pt(8.0, 10.5), pt(11.0, 7.5)]);
            }
        }
        Icon::Refresh => {
            let c = pt(8.0, 8.4);
            let r = 5.2 * s;
            let a0 = -1.25_f32;
            let a1 = a0 + 1.72 * PI;
            line(arc(c, r, a0, a1, 40));
            // arrow head at the end, pointing clockwise along the tangent
            let tip = Pos2::new(c.x + r * a1.cos(), c.y + r * a1.sin());
            let tan = Vec2::new(-a1.sin(), a1.cos());
            let nor = Vec2::new(a1.cos(), a1.sin());
            let back = tip - tan * 3.2 * s;
            line(vec![
                back + nor * 2.6 * s,
                tip + tan * 0.4 * s,
                back - nor * 2.6 * s,
            ]);
        }
        Icon::Home => {
            line(vec![pt(2.0, 8.2), pt(8.0, 2.8), pt(14.0, 8.2)]);
            line(vec![
                pt(4.0, 6.6),
                pt(4.0, 13.4),
                pt(12.0, 13.4),
                pt(12.0, 6.6),
            ]);
            line(vec![
                pt(6.8, 13.4),
                pt(6.8, 10.0),
                pt(9.2, 10.0),
                pt(9.2, 13.4),
            ]);
        }
        Icon::ArrowUp => {
            line(vec![pt(8.0, 13.5), pt(8.0, 2.8)]);
            line(vec![pt(3.8, 7.0), pt(8.0, 2.8), pt(12.2, 7.0)]);
        }
        Icon::ArrowDown => {
            line(vec![pt(8.0, 2.5), pt(8.0, 13.2)]);
            line(vec![pt(3.8, 9.0), pt(8.0, 13.2), pt(12.2, 9.0)]);
        }
        Icon::Pencil => {
            closed(vec![
                pt(2.8, 13.2),
                pt(3.4, 10.4),
                pt(10.8, 3.0),
                pt(13.0, 5.2),
                pt(5.6, 12.6),
            ]);
            line(vec![pt(9.4, 4.4), pt(11.6, 6.6)]);
        }
        Icon::Play => {
            p.add(Shape::convex_polygon(
                vec![pt(4.5, 2.8), pt(13.0, 8.0), pt(4.5, 13.2)],
                color,
                Stroke::new(w * 0.6, color),
            ));
        }
        Icon::Stop => {
            p.rect_filled(
                Rect::from_min_max(pt(3.8, 3.8), pt(12.2, 12.2)),
                CornerRadius::same((1.8 * s) as u8),
                color,
            );
        }
        Icon::Search => {
            p.circle_stroke(pt(7.0, 7.0), 4.4 * s, st);
            line(vec![pt(10.3, 10.3), pt(13.8, 13.8)]);
        }
        Icon::Star | Icon::StarFill => {
            let c = pt(8.0, 8.4);
            let pts: Vec<Pos2> = (0..10)
                .map(|i| {
                    let r = if i % 2 == 0 { 6.4 } else { 2.7 } * s;
                    let a = -PI / 2.0 + i as f32 * PI / 5.0;
                    Pos2::new(c.x + r * a.cos(), c.y + r * a.sin())
                })
                .collect();
            if icon == Icon::StarFill {
                for i in 0..10 {
                    p.add(Shape::convex_polygon(
                        vec![c, pts[i], pts[(i + 1) % 10]],
                        color,
                        Stroke::new(0.6_f32, color),
                    ));
                }
            } else {
                closed(pts);
            }
        }
        Icon::Trash => {
            line(vec![pt(2.5, 4.4), pt(13.5, 4.4)]);
            line(vec![pt(6.2, 4.4), pt(6.2, 2.6), pt(9.8, 2.6), pt(9.8, 4.4)]);
            line(vec![
                pt(4.0, 4.4),
                pt(4.8, 13.6),
                pt(11.2, 13.6),
                pt(12.0, 4.4),
            ]);
            line(vec![pt(6.8, 7.0), pt(6.8, 11.2)]);
            line(vec![pt(9.2, 7.0), pt(9.2, 11.2)]);
        }
        Icon::Copy => {
            rrect(5.2, 5.2, 13.6, 13.6, 2.0);
            line(vec![pt(2.6, 10.6), pt(2.6, 2.6), pt(10.6, 2.6)]);
        }
        Icon::ChevronDown => {
            line(vec![pt(4.0, 6.0), pt(8.0, 10.0), pt(12.0, 6.0)]);
        }
        Icon::ChevronRight => {
            line(vec![pt(6.0, 4.0), pt(10.0, 8.0), pt(6.0, 12.0)]);
        }
        Icon::ChevronLeft => {
            line(vec![pt(10.0, 4.0), pt(6.0, 8.0), pt(10.0, 12.0)]);
        }
        Icon::Desktop => {
            rrect(1.6, 2.6, 14.4, 11.0, 1.8);
            line(vec![pt(8.0, 11.0), pt(8.0, 13.6)]);
            line(vec![pt(5.0, 13.6), pt(11.0, 13.6)]);
        }
        Icon::Drive => {
            rrect(1.8, 4.4, 14.2, 11.6, 2.0);
            line(vec![pt(1.8, 8.6), pt(14.2, 8.6)]);
            p.circle_filled(pt(11.8, 10.2), 0.8 * s, color);
        }
        Icon::Server => {
            rrect(2.2, 2.6, 13.8, 7.4, 1.8);
            rrect(2.2, 8.6, 13.8, 13.4, 1.8);
            p.circle_filled(pt(4.8, 5.0), 0.9 * s, color);
            p.circle_filled(pt(4.8, 11.0), 0.9 * s, color);
        }
        Icon::Bolt => {
            closed(vec![
                pt(9.4, 1.8),
                pt(3.6, 9.2),
                pt(7.8, 9.2),
                pt(6.6, 14.2),
                pt(12.4, 6.8),
                pt(8.2, 6.8),
            ]);
        }
        Icon::Key => {
            p.circle_stroke(pt(5.0, 8.0), 2.8 * s, st);
            line(vec![pt(7.8, 8.0), pt(14.0, 8.0)]);
            line(vec![pt(11.2, 8.0), pt(11.2, 10.6)]);
            line(vec![pt(13.4, 8.0), pt(13.4, 10.0)]);
        }
        Icon::Eye | Icon::EyeOff => {
            let top: Vec<Pos2> = (0..=24)
                .map(|i| {
                    let t = i as f32 / 24.0;
                    pt(1.8 + 12.4 * t, 8.0 - 4.6 * (PI * t).sin())
                })
                .collect();
            let bot: Vec<Pos2> = (0..=24)
                .map(|i| {
                    let t = 1.0 - i as f32 / 24.0;
                    pt(1.8 + 12.4 * t, 8.0 + 4.6 * (PI * t).sin())
                })
                .collect();
            let mut all = top;
            all.extend(bot);
            closed(all);
            p.circle_stroke(pt(8.0, 8.0), 2.0 * s, st);
            if icon == Icon::EyeOff {
                line(vec![pt(2.6, 13.4), pt(13.4, 2.6)]);
            }
        }
        Icon::Ellipsis => {
            for x in [3.6, 8.0, 12.4] {
                p.circle_filled(pt(x, 8.0), 1.3 * s, color);
            }
        }
        Icon::Check => {
            line(vec![pt(3.2, 8.6), pt(6.6, 12.0), pt(12.8, 4.4)]);
        }
        Icon::Power => {
            line(arc(pt(8.0, 8.6), 5.4 * s, -PI * 0.28, PI * 1.28, 40));
            line(vec![pt(8.0, 2.0), pt(8.0, 8.0)]);
        }
        Icon::Keyboard => {
            rrect(1.4, 3.6, 14.6, 12.6, 2.2);
            for x in [4.3, 6.8, 9.2, 11.7] {
                p.circle_filled(pt(x, 6.4), 0.75 * s, color);
            }
            for x in [5.5, 8.0, 10.5] {
                p.circle_filled(pt(x, 8.4), 0.75 * s, color);
            }
            line(vec![pt(5.4, 10.5), pt(10.6, 10.5)]);
        }
        Icon::Warning => {
            closed(vec![pt(8.0, 2.2), pt(14.4, 13.6), pt(1.6, 13.6)]);
            line(vec![pt(8.0, 6.4), pt(8.0, 9.8)]);
            p.circle_filled(pt(8.0, 11.7), 0.9 * s, color);
        }
    }
}

/// Lay out text once (for measuring + painting).
pub fn galley(ui: &Ui, text: &str, font: FontId, color: Color32) -> std::sync::Arc<egui::Galley> {
    ui.painter().layout_no_wrap(text.to_string(), font, color)
}

pub fn semibold(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name(SEMIBOLD.into()))
}

pub fn regular(size: f32) -> FontId {
    FontId::proportional(size)
}

// ----------------------------------------------------------------------
// Buttons
// ----------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// Filled accent capsule.
    Primary,
    /// Neutral filled.
    Secondary,
    /// Transparent until hovered.
    Plain,
    /// Neutral fill, red text.
    Destructive,
    /// Red fill, white text.
    DestructiveFilled,
}

/// Square icon-only button (28×28).
pub fn icon_button(ui: &mut Ui, icon: Icon, tip: &str, enabled: bool) -> Response {
    icon_button_sized(ui, icon, tip, enabled, CTRL_H, false)
}

/// Icon button that shows an "on" state (accent tint).
pub fn icon_toggle(ui: &mut Ui, icon: Icon, on: bool, tip: &str) -> Response {
    icon_button_sized(ui, icon, tip, true, CTRL_H, on)
}

pub fn icon_button_sized(
    ui: &mut Ui,
    icon: Icon,
    tip: &str,
    enabled: bool,
    size: f32,
    on: bool,
) -> Response {
    let t = tok(ui.ctx());
    let (rect, resp) = ui.allocate_exact_size(
        Vec2::splat(size),
        if enabled {
            Sense::click()
        } else {
            Sense::hover()
        },
    );
    if ui.is_rect_visible(rect) {
        let p = ui.painter();
        let h = ui
            .ctx()
            .animate_bool_with_time(resp.id.with("h"), enabled && resp.hovered(), 0.12);
        let fill = if on {
            t.accent_soft
        } else if enabled && resp.is_pointer_button_down_on() {
            t.control_press
        } else {
            t.control.gamma_multiply(h)
        };
        p.rect_filled(rect, CornerRadius::same(7), fill);
        let color = if !enabled {
            t.text3
        } else if on {
            t.accent
        } else if resp.hovered() {
            t.text
        } else {
            t.text2
        };
        let icon_s = (size * 0.57).round();
        paint_icon(
            p,
            Rect::from_center_size(rect.center(), Vec2::splat(icon_s)),
            icon,
            color,
        );
    }
    if tip.is_empty() {
        resp
    } else {
        resp.on_hover_text(tip)
    }
}

/// Text (+ optional icon) button with fixed 28 px height.
pub fn button(ui: &mut Ui, kind: Kind, icon: Option<Icon>, text: &str, enabled: bool) -> Response {
    button_sized(ui, kind, icon, text, enabled, 0.0)
}

pub fn button_sized(
    ui: &mut Ui,
    kind: Kind,
    icon: Option<Icon>,
    text: &str,
    enabled: bool,
    min_width: f32,
) -> Response {
    let t = tok(ui.ctx());
    let fg = match kind {
        Kind::Primary | Kind::DestructiveFilled => Color32::WHITE,
        Kind::Destructive => t.err,
        Kind::Secondary | Kind::Plain => t.text,
    };
    let font = if kind == Kind::Primary || kind == Kind::DestructiveFilled {
        semibold(13.0)
    } else {
        regular(13.0)
    };
    let g = galley(ui, text, font, fg);
    let icon_w = if icon.is_some() {
        15.0 + if text.is_empty() { 0.0 } else { 6.0 }
    } else {
        0.0
    };
    let pad = if text.is_empty() { 7.0 } else { 12.0 };
    let content_w = icon_w + g.size().x;
    let w = (content_w + pad * 2.0).max(min_width).max(CTRL_H);
    let (rect, resp) = ui.allocate_exact_size(
        Vec2::new(w, CTRL_H),
        if enabled {
            Sense::click()
        } else {
            Sense::hover()
        },
    );
    if ui.is_rect_visible(rect) {
        let hovered = enabled && resp.hovered();
        let pressed = enabled && resp.is_pointer_button_down_on();
        let h = ui
            .ctx()
            .animate_bool_with_time(resp.id.with("h"), hovered, 0.12);
        let fill = match kind {
            Kind::Primary => {
                if pressed {
                    darken(t.accent, 0.12)
                } else {
                    lerp(t.accent, lighten(t.accent, 0.1), h)
                }
            }
            Kind::DestructiveFilled => {
                if pressed {
                    darken(t.err, 0.12)
                } else {
                    lerp(t.err, lighten(t.err, 0.1), h)
                }
            }
            Kind::Secondary | Kind::Destructive => {
                if pressed {
                    t.control_press
                } else {
                    lerp(t.control, t.control_hover, h)
                }
            }
            Kind::Plain => {
                if pressed {
                    t.control_hover
                } else {
                    t.control.gamma_multiply(h)
                }
            }
        };
        let alpha = if enabled { 1.0 } else { 0.42 };
        let p = ui.painter();
        p.rect_filled(rect, CornerRadius::same(7), fill.gamma_multiply(alpha));
        let mut x = rect.center().x - content_w / 2.0;
        let icol = fg.gamma_multiply(alpha);
        if let Some(ic) = icon {
            paint_icon(
                p,
                Rect::from_center_size(Pos2::new(x + 7.5, rect.center().y), Vec2::splat(15.0)),
                ic,
                icol,
            );
            x += icon_w;
        }
        if !text.is_empty() {
            let pos = Pos2::new(x, rect.center().y - optical_mid(ui, &g));
            p.galley_with_override_text_color(pos, g, icol);
        }
    }
    resp
}

fn darken(c: Color32, f: f32) -> Color32 {
    let l = |v: u8| (v as f32 * (1.0 - f)).round() as u8;
    Color32::from_rgb(l(c.r()), l(c.g()), l(c.b()))
}

fn lighten(c: Color32, f: f32) -> Color32 {
    let l = |v: u8| (v as f32 + (255.0 - v as f32) * f).round() as u8;
    Color32::from_rgb(l(c.r()), l(c.g()), l(c.b()))
}

/// macOS-style segmented control. Returns true when the selection changed.
pub fn segmented(
    ui: &mut Ui,
    items: &[(Option<Icon>, &str)],
    selected: &mut usize,
    item_w: f32,
) -> bool {
    let t = tok(ui.ctx());
    let widest = items
        .iter()
        .map(|(_, s)| galley(ui, s, semibold(12.5), t.text).size().x)
        .fold(0.0, f32::max);
    let has_icon = items.iter().any(|(i, _)| i.is_some());
    let seg_w = item_w.max(widest + 24.0 + if has_icon { 20.0 } else { 0.0 });
    let total = Vec2::new(seg_w * items.len() as f32 + 4.0, CTRL_H);
    let (rect, _) = ui.allocate_exact_size(total, Sense::hover());
    let p = ui.painter();
    p.rect_filled(rect, CornerRadius::same(8), t.control);
    let mut changed = false;
    // Animated thumb slides between segments.
    let anim_id = ui
        .id()
        .with(("seg_thumb", rect.min.x as i32, rect.min.y as i32));
    let thumb_x = ui
        .ctx()
        .animate_value_with_time(anim_id, *selected as f32, 0.16);
    if !items.is_empty() {
        let thumb = Rect::from_min_size(
            Pos2::new(rect.left() + 2.0 + seg_w * thumb_x, rect.top() + 2.0),
            Vec2::new(seg_w, CTRL_H - 4.0),
        );
        p.rect_filled(
            thumb.translate(Vec2::new(0.0, 0.6)),
            CornerRadius::same(6),
            t.shadow,
        );
        p.rect_filled(
            thumb,
            CornerRadius::same(6),
            if t.dark { t.control_press } else { t.card },
        );
    }
    for (i, (icon, label)) in items.iter().enumerate() {
        let r = Rect::from_min_size(
            Pos2::new(rect.left() + 2.0 + seg_w * i as f32, rect.top() + 2.0),
            Vec2::new(seg_w, CTRL_H - 4.0),
        );
        let resp = ui.interact(
            r,
            ui.id()
                .with(("seg", rect.min.x as i32, rect.min.y as i32, i)),
            Sense::click(),
        );
        let sel = *selected == i;
        if !sel && i > 0 && *selected != i - 1 && (thumb_x - thumb_x.round()).abs() < 0.01 {
            // hairline divider between unselected segments
            p.line_segment(
                [
                    Pos2::new(r.left(), r.top() + 6.0),
                    Pos2::new(r.left(), r.bottom() - 6.0),
                ],
                Stroke::new(1.0_f32, t.separator),
            );
        }
        let color = if sel {
            t.text
        } else if resp.hovered() {
            lerp(t.text2, t.text, 0.55)
        } else {
            t.text2
        };
        let font = if sel { semibold(12.5) } else { regular(12.5) };
        let g = galley(ui, label, font, color);
        let content_w = g.size().x + if icon.is_some() { 20.0 } else { 0.0 };
        let mut x = r.center().x - content_w / 2.0;
        if let Some(ic) = icon {
            paint_icon(
                p,
                Rect::from_center_size(Pos2::new(x + 7.0, r.center().y), Vec2::splat(14.0)),
                *ic,
                color,
            );
            x += 20.0;
        }
        let mid = optical_mid(ui, &g);
        p.galley_with_override_text_color(Pos2::new(x, r.center().y - mid), g, color);
        if resp.clicked() && !sel {
            *selected = i;
            changed = true;
        }
    }
    changed
}

/// Rounded text field (fixed height) with optional leading icon and clear button.
pub struct Field<'a> {
    text: &'a mut String,
    hint: String,
    width: f32,
    icon: Option<Icon>,
    password: bool,
    mono: bool,
    clear: bool,
    id: Option<egui::Id>,
    centered: bool,
}

impl<'a> Field<'a> {
    pub fn new(text: &'a mut String) -> Self {
        Self {
            text,
            hint: String::new(),
            width: f32::INFINITY,
            icon: None,
            password: false,
            mono: false,
            clear: false,
            id: None,
            centered: false,
        }
    }
    pub fn hint(mut self, h: impl Into<String>) -> Self {
        self.hint = h.into();
        self
    }
    pub fn width(mut self, w: f32) -> Self {
        self.width = w;
        self
    }
    pub fn icon(mut self, i: Icon) -> Self {
        self.icon = Some(i);
        self
    }
    pub fn password(mut self, p: bool) -> Self {
        self.password = p;
        self
    }
    pub fn mono(mut self) -> Self {
        self.mono = true;
        self
    }
    pub fn clearable(mut self) -> Self {
        self.clear = true;
        self
    }
    pub fn id(mut self, id: egui::Id) -> Self {
        self.id = Some(id);
        self
    }
    /// Safari-style: while empty and idle, icon + placeholder sit in the
    /// middle; they glide to the left when the field is focused or typed in.
    pub fn centered(mut self) -> Self {
        self.centered = true;
        self
    }

    pub fn show(self, ui: &mut Ui) -> Response {
        let t = tok(ui.ctx());
        let w = if self.width.is_finite() {
            self.width
        } else {
            ui.available_width()
        }
        .max(40.0);
        let (rect, _) = ui.allocate_exact_size(Vec2::new(w, CTRL_H), Sense::hover());
        let bg_idx = ui.painter().add(Shape::Noop);
        let left = if self.icon.is_some() { 28.0 } else { 9.0 };
        let show_clear = self.clear && !self.text.is_empty();
        let right = if show_clear { 26.0 } else { 9.0 };
        let icon_w = if self.icon.is_some() { 19.0 } else { 0.0 };
        let inner = if self.centered {
            // Symmetric box, nudged right by half the icon so that
            // icon + text together sit on the field's centre line.
            let pad = if self.clear { 36.0 } else { 28.0 };
            Rect::from_min_max(
                Pos2::new(rect.left() + pad + icon_w / 2.0, rect.top()),
                Pos2::new(rect.right() - pad + icon_w / 2.0, rect.bottom()),
            )
        } else {
            Rect::from_min_max(
                Pos2::new(rect.left() + left, rect.top()),
                Pos2::new(rect.right() - right, rect.bottom()),
            )
        };
        let font = if self.mono {
            FontId::monospace(12.5)
        } else {
            regular(13.0)
        };
        let mut child = ui.new_child(
            UiBuilder::new()
                .max_rect(inner)
                .layout(Layout::left_to_right(Align::Center)),
        );
        let hint = self.hint.clone();
        let mut te = egui::TextEdit::singleline(self.text)
            .frame(false)
            .margin(egui::Margin::ZERO)
            .desired_width(inner.width())
            .font(font.clone())
            .vertical_align(Align::Center)
            .password(self.password);
        if self.centered {
            te = te.horizontal_align(Align::Center);
        } else {
            te = te.hint_text(egui::RichText::new(self.hint).color(t.text3));
        }
        if let Some(id) = self.id {
            te = te.id(id);
        }
        let resp = child.add(te);
        let focused = resp.has_focus();
        let f = ui
            .ctx()
            .animate_bool_with_time(resp.id.with("focus"), focused, 0.15);
        ui.painter().set(
            bg_idx,
            Shape::rect_filled(rect, CornerRadius::same(7), lerp(t.control, t.bg, f)),
        );
        if f > 0.0 {
            ui.painter().rect_stroke(
                rect,
                CornerRadius::same(7),
                Stroke::new(1.0_f32, t.accent.gamma_multiply(f)),
                StrokeKind::Inside,
            );
            ui.painter().rect_stroke(
                rect.expand(1.0),
                CornerRadius::same(8),
                Stroke::new(3.0_f32, t.accent.gamma_multiply(0.25 * f)),
                StrokeKind::Outside,
            );
        }
        if self.centered {
            // Text grows outward from the middle; the icon rides just left
            // of it. The placeholder fades out while the field has focus.
            let shown: String = if self.password {
                "•".repeat(self.text.chars().count())
            } else {
                self.text.clone()
            };
            let content_w = if self.text.is_empty() {
                if focused {
                    0.0
                } else {
                    galley(ui, &hint, font.clone(), t.text3).size().x
                }
            } else {
                galley(ui, &shown, font.clone(), t.text)
                    .size()
                    .x
                    .min(inner.width())
            };
            let target = (inner.center().x - content_w / 2.0 - 13.0).max(rect.left() + 15.0);
            let icon_x = ui
                .ctx()
                .animate_value_with_time(resp.id.with("icon_x"), target, 0.12);
            if let Some(ic) = self.icon {
                paint_icon(
                    ui.painter(),
                    Rect::from_center_size(Pos2::new(icon_x, rect.center().y), Vec2::splat(14.0)),
                    ic,
                    t.text3,
                );
            }
            let hint_a = ui.ctx().animate_bool_with_time(
                resp.id.with("hint"),
                self.text.is_empty() && !focused,
                0.15,
            );
            if hint_a > 0.0 {
                let g = galley(ui, &hint, font, t.text3);
                let mid = optical_mid(ui, &g);
                ui.painter_at(rect.shrink(2.0))
                    .galley_with_override_text_color(
                        Pos2::new(inner.center().x - g.size().x / 2.0, rect.center().y - mid),
                        g,
                        t.text3.gamma_multiply(hint_a),
                    );
            }
        } else if let Some(ic) = self.icon {
            paint_icon(
                ui.painter(),
                Rect::from_center_size(
                    Pos2::new(rect.left() + 15.0, rect.center().y),
                    Vec2::splat(14.0),
                ),
                ic,
                t.text3,
            );
        }
        if show_clear {
            let cr = Rect::from_center_size(
                Pos2::new(rect.right() - 14.0, rect.center().y),
                Vec2::splat(16.0),
            );
            let c = ui.interact(cr, resp.id.with("clear"), Sense::click());
            ui.painter().circle_filled(
                cr.center(),
                7.0,
                if c.hovered() { t.text2 } else { t.text3 },
            );
            paint_icon(
                ui.painter(),
                Rect::from_center_size(cr.center(), Vec2::splat(8.0)),
                Icon::Close,
                if t.dark { t.bg } else { Color32::WHITE },
            );
            if c.clicked() {
                self.text.clear();
            }
        }
        resp
    }
}

/// iOS-style switch with a trailing label.
/// The Tern logo drawn as vectors (crisp at any size / DPI). Mirrors
/// `assets/tern.svg`: five flat facets coloured from one cyan→blue gradient.
pub fn paint_logo(p: &egui::Painter, rect: Rect) {
    let s = rect.width() / 1024.0;
    let bg = Color32::from_rgb(20, 20, 22);
    let cr = CornerRadius::same((228.0 * s).round().min(255.0) as u8);
    p.rect_filled(rect, cr, bg);
    p.rect_stroke(
        rect,
        cr,
        Stroke::new(1.0_f32, Color32::from_white_alpha(16)),
        StrokeKind::Inside,
    );
    let (c0, c1) = (
        Color32::from_rgb(0x64, 0xD2, 0xFF),
        Color32::from_rgb(0x0A, 0x84, 0xFF),
    );
    let (ga, gb) = (Vec2::new(176.0, 276.0), Vec2::new(848.0, 778.0));
    let facets: [(&[(f32, f32)], f32); 5] = [
        (&[(512.0, 372.0), (176.0, 290.0), (476.0, 452.0)], 0.62),
        (&[(512.0, 372.0), (848.0, 290.0), (548.0, 452.0)], 1.0),
        (
            &[
                (512.0, 330.0),
                (548.0, 452.0),
                (512.0, 620.0),
                (476.0, 452.0),
            ],
            0.88,
        ),
        (&[(512.0, 620.0), (436.0, 792.0), (512.0, 680.0)], 0.58),
        (&[(512.0, 620.0), (588.0, 792.0), (512.0, 680.0)], 0.9),
    ];
    for (pts, a) in facets {
        let n = pts.len() as f32;
        let c = pts
            .iter()
            .fold(Vec2::ZERO, |acc, &(x, y)| acc + Vec2::new(x, y))
            / n;
        let d = gb - ga;
        let t = ((c - ga).dot(d) / d.length_sq()).clamp(0.0, 1.0);
        let col = lerp(bg, lerp(c0, c1, t), a);
        let poly: Vec<Pos2> = pts
            .iter()
            .map(|&(x, y)| Pos2::new(rect.left() + x * s, rect.top() + (y - 21.0) * s))
            .collect();
        p.add(egui::Shape::convex_polygon(poly, col, Stroke::NONE));
    }
}

/// Natural width of a [`toggle_chip`]; use the max over a group so
/// neighbouring chips share one width.
pub fn toggle_chip_width(ui: &Ui, icon: bool, label: &str) -> f32 {
    galley(ui, label, regular(12.5), Color32::WHITE).size().x + if icon { 20.0 } else { 0.0 } + 28.0
}

/// Natural width of a [`button`] with regular-weight text.
pub fn button_width(ui: &Ui, icon: bool, label: &str) -> f32 {
    galley(ui, label, regular(13.0), Color32::WHITE).size().x + if icon { 21.0 } else { 0.0 } + 24.0
}

/// A single-box toggle in the segmented-control language: one capsule that
/// lights up when on. `accent` paints the "on" state blue (grey → blue);
/// otherwise it raises a neutral thumb like a selected segment.
pub fn toggle_chip(
    ui: &mut Ui,
    on: &mut bool,
    icon: Option<Icon>,
    label: &str,
    accent: bool,
    min_w: f32,
) -> Response {
    let t = tok(ui.ctx());
    let g = galley(ui, label, regular(12.5), t.text);
    let content_w = g.size().x + if icon.is_some() { 20.0 } else { 0.0 };
    let size = Vec2::new((content_w + 28.0).max(min_w), CTRL_H);
    let (rect, mut resp) = ui.allocate_exact_size(size, Sense::click());
    if resp.clicked() {
        *on = !*on;
        resp.mark_changed();
    }
    let ctx = ui.ctx().clone();
    let k = ctx.animate_bool_with_time(resp.id, *on, 0.18);
    let hov = ctx.animate_bool_with_time(resp.id.with("h"), resp.hovered(), 0.12);
    let p = ui.painter();
    p.rect_filled(
        rect,
        CornerRadius::same(8),
        lerp(t.control, t.control_hover, hov * (1.0 - k)),
    );
    let thumb = rect.shrink(2.0);
    if k > 0.0 {
        let fill = if accent {
            t.accent
        } else if t.dark {
            t.control_press
        } else {
            t.card
        };
        if !accent {
            p.rect_filled(
                thumb.translate(Vec2::new(0.0, 0.6)),
                CornerRadius::same(6),
                t.shadow.gamma_multiply(k),
            );
        }
        p.rect_filled(thumb, CornerRadius::same(6), fill.gamma_multiply(k));
    }
    let on_color = if accent { Color32::WHITE } else { t.text };
    let color = lerp(t.text2, on_color, k);
    let mut x = rect.center().x - content_w / 2.0;
    if let Some(ic) = icon {
        paint_icon(
            p,
            Rect::from_center_size(Pos2::new(x + 7.0, rect.center().y), Vec2::splat(14.0)),
            ic,
            color,
        );
        x += 20.0;
    }
    let mid = optical_mid(ui, &g);
    p.galley_with_override_text_color(Pos2::new(x, rect.center().y - mid), g, color);
    resp
}

/// Y offset (from the galley top) of the text's optical middle: halfway
/// between the top of the ink and the baseline, so CJK and Latin labels both
/// line up with the centre of an icon or control next to them.
pub fn optical_mid(ui: &Ui, g: &egui::Galley) -> f32 {
    let Some(row) = g.rows.first() else {
        return g.size().y / 2.0;
    };
    let ink = row.visuals.mesh_bounds;
    let Some(base) = row.glyphs.first().map(|gl| gl.pos.y) else {
        return g.size().y / 2.0;
    };
    if !ink.is_finite() || ink.height() <= 0.0 {
        return g.size().y / 2.0;
    }
    let size = row
        .glyphs
        .first()
        .map(|gl| gl.font_height)
        .unwrap_or(g.size().y);
    if ink.bottom() < base - size * 0.12 {
        // Floating mid-line symbols (such as "–") sit on their actual ink centre.
        return (ink.top() + ink.bottom()) / 2.0;
    }
    let font_id = g
        .job
        .sections
        .first()
        .map(|s| s.format.font_id.clone())
        .unwrap_or_else(|| regular(size));
    let cap = ui.fonts(|f| f.layout_delayed_color("H".to_string(), font_id, f32::INFINITY));
    if let Some(h_ink) = cap
        .rows
        .first()
        .map(|r| r.visuals.mesh_bounds)
        .filter(|b| b.is_finite() && b.height() > 0.0)
    {
        let top = ink.top().min(h_ink.top());
        let bottom = ink.bottom().min(h_ink.bottom() + size * 0.06);
        return (top + bottom) / 2.0;
    }
    let bottom = ink.bottom().min(base + size * 0.09);
    (ink.top() + bottom) / 2.0
}

/// Segmented-style value stepper: `[ - | value | + ]`.
/// Returns `(dec_clicked, inc_clicked)`.
pub fn font_stepper(
    ui: &mut Ui,
    value_label: &str,
    dec_enabled: bool,
    inc_enabled: bool,
    width: f32,
) -> (bool, bool) {
    let t = tok(ui.ctx());
    let w = width.max(100.0);
    let (rect, _) = ui.allocate_exact_size(Vec2::new(w, CTRL_H), Sense::hover());
    let btn_w = 40.0_f32.min((w - 36.0).max(28.0) / 2.0);
    let dec_hit = Rect::from_min_size(rect.min, Vec2::new(btn_w, CTRL_H));
    let inc_hit = Rect::from_min_size(
        Pos2::new(rect.right() - btn_w, rect.top()),
        Vec2::new(btn_w, CTRL_H),
    );

    let dec_resp = ui.interact(
        dec_hit,
        ui.id()
            .with(("step_dec", rect.min.x as i32, rect.min.y as i32)),
        if dec_enabled {
            Sense::click()
        } else {
            Sense::hover()
        },
    );
    let inc_resp = ui.interact(
        inc_hit,
        ui.id()
            .with(("step_inc", rect.min.x as i32, rect.min.y as i32)),
        if inc_enabled {
            Sense::click()
        } else {
            Sense::hover()
        },
    );

    let dec_hov = dec_enabled && dec_resp.hovered();
    let inc_hov = inc_enabled && inc_resp.hovered();
    let dec_h = ui
        .ctx()
        .animate_bool_with_time(dec_resp.id.with("dh"), dec_hov, 0.12);
    let inc_h = ui
        .ctx()
        .animate_bool_with_time(inc_resp.id.with("ih"), inc_hov, 0.12);
    let g = galley(ui, value_label, semibold(13.0), t.text);

    let p = ui.painter();
    p.rect_filled(rect, CornerRadius::same(8), t.control);

    let paint_btn = |thumb: Rect, resp: &Response, enabled: bool, hov: bool, h: f32, icon: Icon| {
        let fill = if enabled && resp.is_pointer_button_down_on() {
            t.control_press
        } else {
            t.control_hover.gamma_multiply(h)
        };
        if fill.a() > 0 {
            p.rect_filled(thumb, CornerRadius::same(6), fill);
        }
        let col = if !enabled {
            t.text3
        } else if hov {
            t.text
        } else {
            t.text2
        };
        paint_icon(
            p,
            Rect::from_center_size(thumb.center(), Vec2::splat(14.0)),
            icon,
            col,
        );
    };

    let dec_thumb = Rect::from_min_size(
        Pos2::new(rect.left() + 2.0, rect.top() + 2.0),
        Vec2::new(btn_w - 2.0, CTRL_H - 4.0),
    );
    let inc_thumb = Rect::from_min_size(
        Pos2::new(rect.right() - btn_w, rect.top() + 2.0),
        Vec2::new(btn_w - 2.0, CTRL_H - 4.0),
    );
    paint_btn(
        dec_thumb,
        &dec_resp,
        dec_enabled,
        dec_hov,
        dec_h,
        Icon::Minus,
    );
    paint_btn(
        inc_thumb,
        &inc_resp,
        inc_enabled,
        inc_hov,
        inc_h,
        Icon::Plus,
    );

    if !dec_hov {
        p.line_segment(
            [
                Pos2::new(rect.left() + btn_w, rect.top() + 6.0),
                Pos2::new(rect.left() + btn_w, rect.bottom() - 6.0),
            ],
            Stroke::new(1.0_f32, t.separator),
        );
    }
    if !inc_hov {
        p.line_segment(
            [
                Pos2::new(rect.right() - btn_w, rect.top() + 6.0),
                Pos2::new(rect.right() - btn_w, rect.bottom() - 6.0),
            ],
            Stroke::new(1.0_f32, t.separator),
        );
    }

    let well = Rect::from_min_max(
        Pos2::new(rect.left() + btn_w, rect.top()),
        Pos2::new(rect.right() - btn_w, rect.bottom()),
    );
    let gx = well.center().x - g.size().x / 2.0;
    let gy = rect.center().y - optical_mid(ui, &g);
    p.galley_with_override_text_color(Pos2::new(gx, gy), g, t.text);

    (
        dec_enabled && dec_resp.clicked(),
        inc_enabled && inc_resp.clicked(),
    )
}

pub fn switch(ui: &mut Ui, on: &mut bool, label: &str) -> Response {
    let t = tok(ui.ctx());
    let g = galley(ui, label, regular(13.0), t.text);
    let track = Vec2::new(36.0, 20.0);
    let natural_w = track.x
        + if label.is_empty() {
            0.0
        } else {
            8.0 + g.size().x
        };
    let avail = ui.available_width();
    let w = if avail.is_finite() {
        natural_w.min(avail.max(track.x))
    } else {
        natural_w
    };
    let (rect, mut resp) = ui.allocate_exact_size(Vec2::new(w, CTRL_H), Sense::click());
    if resp.clicked() {
        *on = !*on;
        resp.mark_changed();
    }
    let ctx = ui.ctx().clone();
    let k = ctx.animate_bool_with_time(resp.id, *on, 0.22);
    let k = k * k * (3.0 - 2.0 * k); // ease in-out
    let hov = ctx.animate_bool_with_time(resp.id.with("h"), resp.hovered(), 0.12);
    let press =
        ctx.animate_bool_with_time(resp.id.with("p"), resp.is_pointer_button_down_on(), 0.12);
    let p = ui.painter();
    let ppp = p.pixels_per_point();
    let snap = |v: f32| (v * ppp).round() / ppp;
    // Snap the track to whole pixels so the knob sits dead-centre at any DPI.
    let tr = Rect::from_min_size(
        Pos2::new(snap(rect.left()), snap(rect.center().y - track.y / 2.0)),
        track,
    );
    let (off, off_hover) = if t.dark {
        (Color32::from_rgb(58, 58, 62), Color32::from_rgb(68, 68, 73))
    } else {
        (
            Color32::from_rgb(226, 226, 231),
            Color32::from_rgb(214, 214, 220),
        )
    };
    let r = CornerRadius::same((track.y / 2.0) as u8);
    p.rect_filled(tr, r, lerp(lerp(off, off_hover, hov), t.accent, k));
    // Knob: a capsule that stretches a little while pressed, like iOS.
    let inset = 2.0;
    let d = track.y - inset * 2.0;
    let kw = d + 5.0 * press;
    let x0 = tr.left() + inset + (track.x - inset * 2.0 - kw) * k;
    let knob = Rect::from_min_size(Pos2::new(x0, tr.top() + inset), Vec2::new(kw, d));
    let kr = CornerRadius::same((d / 2.0) as u8);
    p.add(
        egui::epaint::Shadow {
            offset: [0, 1],
            blur: 2,
            spread: 0,
            color: Color32::from_black_alpha(if t.dark { 45 } else { 38 }),
        }
        .as_shape(knob, kr),
    );
    p.rect_filled(knob, kr, Color32::WHITE);
    if !t.dark {
        p.rect_stroke(
            knob,
            kr,
            Stroke::new(0.5_f32, Color32::from_black_alpha(18)),
            egui::StrokeKind::Outside,
        );
    }
    if !label.is_empty() {
        let max_label_w = (rect.width() - track.x - 8.0).max(0.0);
        if g.size().x <= max_label_w + 0.5 {
            let y = snap(tr.center().y - optical_mid(ui, &g));
            p.galley_with_override_text_color(Pos2::new(tr.right() + 8.0, y), g, t.text);
        } else if max_label_w > 8.0 {
            paint_text_trunc(
                ui,
                p,
                label,
                regular(13.0),
                t.text,
                tr.right() + 8.0,
                tr.center().y,
                max_label_w,
            );
        }
    }
    resp
}

/// Horizontal fade from `color` to transparent, used to hint at more
/// content under the edge of a scrolled strip.
pub fn fade_edge(p: &egui::Painter, rect: Rect, color: Color32, solid_left: bool) {
    let mut m = egui::Mesh::default();
    let (cl, cr) = if solid_left {
        (color, Color32::TRANSPARENT)
    } else {
        (Color32::TRANSPARENT, color)
    };
    m.colored_vertex(rect.left_top(), cl);
    m.colored_vertex(rect.right_top(), cr);
    m.colored_vertex(rect.right_bottom(), cr);
    m.colored_vertex(rect.left_bottom(), cl);
    m.add_triangle(0, 1, 2);
    m.add_triangle(0, 2, 3);
    p.add(egui::Shape::mesh(m));
}

pub fn lerp(a: Color32, b: Color32, k: f32) -> Color32 {
    let l = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * k).round() as u8;
    Color32::from_rgb(l(a.r(), b.r()), l(a.g(), b.g()), l(a.b(), b.b()))
}

/// Small grey section caption ("FAVORITES", "收藏" …).
pub fn caption(ui: &mut Ui, text: &str) {
    let t = tok(ui.ctx());
    let g = galley(ui, text, semibold(11.0), t.text3);
    let (rect, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 22.0), Sense::hover());
    ui.painter().galley_with_override_text_color(
        Pos2::new(rect.left() + 6.0, rect.bottom() - g.size().y - 3.0),
        g,
        t.text3,
    );
}

/// Grouped surface (iOS "inset grouped" card).
pub fn card<R>(ui: &mut Ui, add: impl FnOnce(&mut Ui) -> R) -> R {
    let t = tok(ui.ctx());
    let r = egui::Frame::NONE
        .fill(t.card)
        .stroke(if t.dark {
            Stroke::NONE
        } else {
            Stroke::new(1.0_f32, t.separator)
        })
        .corner_radius(CornerRadius::same(12))
        .inner_margin(egui::Margin::symmetric(12, 10))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            add(ui)
        })
        .inner;
    ui.add_space(10.0);
    r
}

/// Label / value row with fixed height (for info cards).
pub fn kv_row(ui: &mut Ui, key: &str, value: &str, mono: bool) {
    let t = tok(ui.ctx());
    let (rect, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 22.0), Sense::hover());
    let kg = galley(ui, key, regular(12.0), t.text2);
    let vf = if mono {
        FontId::monospace(12.0)
    } else {
        regular(12.0)
    };
    let vg = galley(ui, value, vf, t.text);
    let p = ui.painter_at(rect);
    let kmid = optical_mid(ui, &kg);
    let vmid = optical_mid(ui, &vg);
    p.galley_with_override_text_color(
        Pos2::new(rect.left(), rect.center().y - kmid),
        kg.clone(),
        t.text2,
    );
    let vx = (rect.right() - vg.size().x).max(rect.left() + kg.size().x + 10.0);
    p.galley_with_override_text_color(Pos2::new(vx, rect.center().y - vmid), vg, t.text);
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum DotState {
    /// Connected: slow, calm breathing with a soft expanding halo.
    Live,
    /// Connecting: quicker pulse.
    Busy,
    /// Error / idle: steady.
    Steady,
}

/// Status dot. Live / busy dots "breathe": the core swells gently and a soft
/// halo expands and fades, like the indicator lights on Apple hardware.
pub fn status_dot(
    ctx: &egui::Context,
    p: &egui::Painter,
    c: Pos2,
    color: Color32,
    state: DotState,
) {
    // A live dot breathes only while someone is using the window (focused,
    // input within the last few seconds). Then it finishes its breath and
    // holds still, so an idle window with open sessions costs no repaints.
    let t = ctx.input(|i| i.time);
    let activity_id = egui::Id::new("status_dot_activity");
    let (focused, input_now) =
        ctx.input(|i| (i.focused, !i.events.is_empty() || i.pointer.is_moving()));
    if input_now {
        ctx.data_mut(|d| d.insert_temp(activity_id, t));
    }
    let last_input = ctx
        .data(|d| d.get_temp::<f64>(activity_id))
        .unwrap_or(f64::NEG_INFINITY);
    let period = match state {
        DotState::Busy => 1.1,
        DotState::Live => 2.8,
        DotState::Steady => {
            p.circle_filled(c, 3.8, color);
            return;
        }
    };
    let phase = ((t as f32) % period) / period; // 0..1
    let lively = focused && t - last_input < 6.0;
    // Going to rest: finish the current breath (phase wraps to 0), then stay
    // still — repaints caused by something else don't restart it.
    let rest_id = egui::Id::new("status_dot_resting");
    if state == DotState::Live {
        let resting = ctx.data(|d| d.get_temp::<bool>(rest_id)).unwrap_or(false);
        if lively {
            if resting {
                ctx.data_mut(|d| d.insert_temp(rest_id, false));
            }
        } else if resting || phase < 0.06 {
            if !resting {
                ctx.data_mut(|d| d.insert_temp(rest_id, true));
            }
            p.circle_filled(c, 4.6, color.gamma_multiply(0.18));
            p.circle_filled(c, 3.5, color);
            return;
        }
    }
    // Halo: ease-out expansion, fading as it grows.
    let e = 1.0 - (1.0 - phase).powi(3);
    let halo_r = 3.8 + 5.2 * e;
    let halo_a = 0.38 * (1.0 - e);
    if halo_a > 0.01 {
        p.circle_filled(c, halo_r, color.gamma_multiply(halo_a));
    }
    // Core: gentle sine "breath" in size and glow.
    let breath = 0.5 - 0.5 * (phase * std::f32::consts::TAU).cos(); // 0..1..0
    p.circle_filled(
        c,
        4.6 + 0.8 * breath,
        color.gamma_multiply(0.18 + 0.12 * breath),
    );
    p.circle_filled(c, 3.5 + 0.4 * breath, color);
    // A slow breath needs few frames: ~15 fps for live dots, ~30 for busy.
    let frame = if state == DotState::Busy { 33 } else { 66 };
    ctx.request_repaint_after(std::time::Duration::from_millis(frame));
}

/// `target`, approached smoothly over about `secs`, repainting at ~30 fps
/// only while it moves (egui's own animation repaints at full frame rate).
pub fn ease_value(ui: &Ui, id: egui::Id, target: f32, secs: f32) -> f32 {
    let now = ui.input(|i| i.time);
    let (shown, at) = ui
        .ctx()
        .data(|d| d.get_temp::<(f32, f64)>(id))
        .unwrap_or((target, now));
    let dt = (now - at).max(0.0) as f32;
    // Exponential ease: ~95 % of the way after `secs`.
    let k = 1.0 - (-3.0 * dt / secs.max(0.01)).exp();
    let mut v = shown + (target - shown) * k;
    if (target - v).abs() < 0.05 {
        v = target;
    } else {
        ui.ctx()
            .request_repaint_after(std::time::Duration::from_millis(33));
    }
    ui.ctx().data_mut(|d| d.insert_temp(id, (v, now)));
    v
}

/// Text truncated with "…" to fit `max_w`, painted left-aligned and v-centred.
#[allow(clippy::too_many_arguments)]
pub fn paint_text_trunc(
    ui: &Ui,
    p: &egui::Painter,
    text: &str,
    font: FontId,
    color: Color32,
    left: f32,
    center_y: f32,
    max_w: f32,
) -> f32 {
    let mut job = egui::text::LayoutJob::single_section(
        text.to_string(),
        egui::TextFormat::simple(font, color),
    );
    job.wrap = egui::text::TextWrapping::truncate_at_width(max_w.max(8.0));
    let g = ui.fonts(|f| f.layout_job(job));
    let w = g.size().x;
    let mid = optical_mid(ui, &g);
    p.galley(Pos2::new(left, center_y - mid), g, color);
    w
}

/// Popover anchored under `resp`, toggled by clicking it. Aligns to the
/// widget's right edge when it sits in the right half of the window.
pub fn popover<R>(
    ui: &Ui,
    resp: &Response,
    id: egui::Id,
    width: f32,
    add: impl FnOnce(&mut Ui) -> R,
) -> Option<R> {
    let ctx = ui.ctx().clone();
    if resp.clicked() {
        ui.memory_mut(|m| m.toggle_popup(id));
    }
    if !ui.memory(|m| m.is_popup_open(id)) {
        return None;
    }
    let t = tok(&ctx);
    let screen = ctx.screen_rect();
    let right = resp.rect.center().x > screen.center().x;
    let (pos, pivot) = if right {
        (
            resp.rect.right_bottom() + Vec2::new(0.0, 6.0),
            egui::Align2::RIGHT_TOP,
        )
    } else {
        (
            resp.rect.left_bottom() + Vec2::new(0.0, 6.0),
            egui::Align2::LEFT_TOP,
        )
    };
    let shadow = ctx.style().visuals.popup_shadow;
    let area = egui::Area::new(id)
        .kind(egui::UiKind::Popup)
        .order(egui::Order::Foreground)
        .fixed_pos(pos)
        .pivot(pivot)
        .constrain(true)
        .show(&ctx, |ui| {
            egui::Frame::NONE
                .fill(t.card)
                .corner_radius(CornerRadius::same(12))
                .shadow(shadow)
                .stroke(if t.dark {
                    Stroke::new(1.0_f32, t.separator)
                } else {
                    Stroke::new(1.0_f32, Color32::from_black_alpha(14))
                })
                .inner_margin(egui::Margin::symmetric(12, 10))
                .show(ui, |ui| {
                    ui.set_min_width(width);
                    ui.set_max_width(width);
                    add(ui)
                })
                .inner
        });
    let clicked_outside = ctx.input(|i| i.pointer.any_pressed())
        && ctx
            .input(|i| i.pointer.interact_pos())
            .is_some_and(|p| !area.response.rect.contains(p) && !resp.rect.contains(p));
    if clicked_outside || ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
        ui.memory_mut(|m| m.close_popup());
    }
    Some(area.inner)
}

/// A menu row with label (and optional right-aligned shortcut). Returns clicked.
pub fn menu_item(ui: &mut Ui, text: &str, shortcut: &str, danger: bool) -> bool {
    let t = tok(ui.ctx());
    let w = ui.available_width().max(160.0);
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(w, 26.0), Sense::click());
    let p = ui.painter();
    if resp.hovered() {
        p.rect_filled(rect, CornerRadius::same(6), t.accent);
    }
    let fg = if resp.hovered() {
        Color32::WHITE
    } else if danger {
        t.err
    } else {
        t.text
    };
    let g = galley(ui, text, regular(13.0), fg);
    let mid = optical_mid(ui, &g);
    p.galley_with_override_text_color(Pos2::new(rect.left() + 10.0, rect.center().y - mid), g, fg);
    if !shortcut.is_empty() {
        let sg = galley(ui, shortcut, regular(12.0), t.text3);
        let smid = optical_mid(ui, &sg);
        let sc = if resp.hovered() {
            Color32::from_white_alpha(200)
        } else {
            t.text3
        };
        p.galley_with_override_text_color(
            Pos2::new(rect.right() - 10.0 - sg.size().x, rect.center().y - smid),
            sg,
            sc,
        );
    }
    resp.clicked()
}

/// Small grey heading inside a menu.
pub fn menu_caption(ui: &mut Ui, text: &str) {
    let t = tok(ui.ctx());
    let w = ui.available_width().max(160.0);
    let (rect, _) = ui.allocate_exact_size(Vec2::new(w, 22.0), Sense::hover());
    let g = galley(ui, text, regular(11.5), t.text3);
    let mid = optical_mid(ui, &g);
    ui.painter().galley(
        Pos2::new(rect.left() + 10.0, rect.center().y - mid),
        g,
        t.text3,
    );
}

pub fn menu_separator(ui: &mut Ui) {
    let t = tok(ui.ctx());
    let (rect, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 9.0), Sense::hover());
    ui.painter().line_segment(
        [
            Pos2::new(rect.left() + 8.0, rect.center().y),
            Pos2::new(rect.right() - 8.0, rect.center().y),
        ],
        Stroke::new(1.0_f32, t.separator),
    );
}

/// Circular progress ring with centred percentage.
pub fn ring(ui: &mut Ui, pct: Option<f32>, label: &str, color: Color32, size: f32) {
    let t = tok(ui.ctx());
    let (rect, _) = ui.allocate_exact_size(Vec2::new(size, size + 18.0), Sense::hover());
    let c = Pos2::new(rect.center().x, rect.top() + size / 2.0);
    let r = size / 2.0 - 4.0;
    let p = ui.painter();
    p.circle_stroke(c, r, Stroke::new(5.0_f32, t.control));
    if let Some(v) = pct {
        let frac = (v / 100.0).clamp(0.0, 1.0);
        if frac > 0.0 {
            let pts = arc(
                c,
                r,
                -PI / 2.0,
                -PI / 2.0 + 2.0 * PI * frac,
                (64.0 * frac).max(2.0) as usize,
            );
            p.add(Shape::line(pts, Stroke::new(5.0_f32, color)));
        }
    }
    let txt = pct
        .map(|v| format!("{v:.0}%"))
        .unwrap_or_else(|| "–".into());
    let g = galley(ui, &txt, semibold(13.0), t.text);
    let mid = optical_mid(ui, &g);
    p.galley_with_override_text_color(Pos2::new(c.x - g.size().x / 2.0, c.y - mid), g, t.text);
    let lg = galley(ui, label, regular(11.0), t.text2);
    p.galley_with_override_text_color(
        Pos2::new(c.x - lg.size().x / 2.0, rect.bottom() - lg.size().y),
        lg,
        t.text2,
    );
}

/// Animated indeterminate segment sweeping smoothly across `track`.
pub fn paint_indeterminate_bar(ui: &Ui, track: Rect, color: Color32) {
    if track.width() <= 0.0 || track.height() <= 0.0 {
        return;
    }
    let phase = (ui.input(|i| i.time) * 0.75).fract() as f32;
    let seg_w = (track.width() * 0.28).clamp(24.0, 96.0).min(track.width());
    let travel = track.width() + seg_w;
    let left = (track.left() - seg_w + travel * phase).max(track.left());
    let right = (track.left() - seg_w + travel * phase + seg_w).min(track.right());
    if right > left {
        let seg = Rect::from_min_max(
            Pos2::new(left, track.top()),
            Pos2::new(right, track.bottom()),
        );
        let r = CornerRadius::same((track.height() / 2.0).round().max(1.0) as u8);
        ui.painter().rect_filled(seg, r, color);
    }
    ui.ctx()
        .request_repaint_after(std::time::Duration::from_millis(33));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn measure_samples(ctx: &egui::Context, samples: &[&str]) {
        let font = regular(13.0);
        let mut results = Vec::new();
        let _ = ctx.run(Default::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                for &sample in samples {
                    let g = galley(ui, sample, font.clone(), Color32::WHITE);
                    let m = optical_mid(ui, &g);
                    results.push((sample, m, g.size().y));
                }
            });
        });
        assert_eq!(results.len(), samples.len());
        for (sample, m, h) in &results {
            assert!(
                m.is_finite() && *m > 0.0 && *m < *h,
                "optical_mid for {sample:?} out of bounds: mid={m}, height={h}"
            );
        }
        let user_lower = results.iter().find(|(s, ..)| *s == "user").unwrap().1;
        let user_cap = results.iter().find(|(s, ..)| *s == "User").unwrap().1;
        assert!(
            (user_lower - user_cap).abs() <= 0.25,
            "baseline mismatch between User ({user_cap}) and user ({user_lower})"
        );
    }

    #[test]
    fn optical_mid_bounds_and_baseline_consistency() {
        let ctx = egui::Context::default();
        measure_samples(&ctx, &["English", "user", "User", "12 pt", "–"]);

        let ctx_cjk = egui::Context::default();
        if super::super::theme::install_fonts(&ctx_cjk).is_some() {
            measure_samples(
                &ctx_cjk,
                &["新会话", "中文", "English", "user", "User", "12 pt", "–"],
            );
        }
    }
}
