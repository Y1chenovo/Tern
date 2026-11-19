//! Custom window chrome: our own title bar (centred "Tern" wordmark, window
//! buttons), drag-to-move and edge resizing. Used on Windows instead of the
//! native caption; can be switched off in Settings ("system title bar").

use super::widgets as w;
use super::*;
use egui::{
    Color32, CornerRadius, CursorIcon, Pos2, Rect, ResizeDirection, Sense, Stroke, Vec2,
    ViewportCommand,
};

pub(super) const CAPTION_H: f32 = 34.0;
const BTN_W: f32 = 46.0;
const EDGE: f32 = 6.0;

impl GuiApp {
    /// Whether we draw our own title bar instead of the OS one.
    pub(super) fn custom_chrome(&self) -> bool {
        !self.config.system_titlebar && custom_chrome_available()
    }

    pub(super) fn ui_caption(&mut self, ui: &mut egui::Ui) {
        let t = self.tok();
        let rect = ui.max_rect();
        let ctx = ui.ctx().clone();
        let maximized = ctx.input(|i| i.viewport().maximized.unwrap_or(false));

        // The bar drags the window; double-click toggles maximise. The window
        // buttons are excluded, so a press that wobbles on one never moves
        // the window instead of clicking.
        let buttons =
            Rect::from_min_max(Pos2::new(rect.right() - BTN_W * 3.0, rect.top()), rect.max);
        let bar = ui.interact(rect, ui.id().with("caption_drag"), Sense::click_and_drag());
        let pressed_on_bar = ctx
            .input(|i| i.pointer.press_origin())
            .is_some_and(|p| !buttons.contains(p));
        if bar.drag_started_by(egui::PointerButton::Primary) && pressed_on_bar {
            ctx.send_viewport_cmd(ViewportCommand::StartDrag);
        }
        if bar.double_clicked() && pressed_on_bar {
            ctx.send_viewport_cmd(ViewportCommand::Maximized(!maximized));
        }

        // Centred wordmark (aligned with the quick-connect field below).
        let (c0, c1) = if t.dark {
            (
                Color32::from_rgb(0x8E, 0xE6, 0xFF),
                Color32::from_rgb(0x3D, 0x7B, 0xFF),
            )
        } else {
            (
                Color32::from_rgb(0x12, 0xB0, 0xF2),
                Color32::from_rgb(0x2B, 0x4F, 0xE0),
            )
        };
        paint_wordmark(
            ui,
            Pos2::new(rect.center().x, rect.center().y + 1.0),
            17.0,
            c0,
            c1,
        );

        // Window buttons, Windows 11 proportions.
        let mut x = rect.right();
        for kind in [Btn::Close, Btn::Max, Btn::Min] {
            let r = Rect::from_min_max(
                Pos2::new(x - BTN_W, rect.top()),
                Pos2::new(x, rect.bottom()),
            );
            x -= BTN_W;
            let resp = ui.interact(r, ui.id().with(("win_btn", kind as u8)), Sense::click());
            let h = ctx.animate_bool_with_time(resp.id.with("h"), resp.hovered(), 0.1);
            let p = ui.painter();
            let (fill, fg) = if kind == Btn::Close {
                (
                    Color32::from_rgb(0xC4, 0x2B, 0x1C).gamma_multiply(h),
                    w::lerp(t.text, Color32::WHITE, h),
                )
            } else {
                (t.row_hover.gamma_multiply(h), t.text)
            };
            p.rect_filled(r, CornerRadius::ZERO, fill);
            let c = r.center();
            let s = Stroke::new(1.0_f32, fg);
            let px = |v: f32| {
                (v * ctx.pixels_per_point()).round() / ctx.pixels_per_point()
                    + 0.5 / ctx.pixels_per_point()
            };
            match kind {
                Btn::Min => {
                    p.hline((c.x - 5.0)..=(c.x + 5.0), px(c.y), s);
                }
                Btn::Max if maximized => {
                    // restore: two offset squares
                    let a = Rect::from_center_size(c + Vec2::new(-1.0, 1.0), Vec2::splat(8.0));
                    p.rect_stroke(a, CornerRadius::same(1), s, egui::StrokeKind::Middle);
                    p.line_segment(
                        [
                            Pos2::new(a.left() + 2.0, a.top() - 2.0),
                            Pos2::new(a.right() + 2.0, a.top() - 2.0),
                        ],
                        s,
                    );
                    p.line_segment(
                        [
                            Pos2::new(a.right() + 2.0, a.top() - 2.0),
                            Pos2::new(a.right() + 2.0, a.bottom() - 2.0),
                        ],
                        s,
                    );
                }
                Btn::Max => {
                    let a = Rect::from_center_size(c, Vec2::splat(10.0));
                    p.rect_stroke(a, CornerRadius::same(1), s, egui::StrokeKind::Middle);
                }
                Btn::Close => {
                    p.line_segment([c + Vec2::new(-5.0, -5.0), c + Vec2::new(5.0, 5.0)], s);
                    p.line_segment([c + Vec2::new(5.0, -5.0), c + Vec2::new(-5.0, 5.0)], s);
                }
            }
            if resp.clicked() {
                match kind {
                    Btn::Min => ctx.send_viewport_cmd(ViewportCommand::Minimized(true)),
                    Btn::Max => ctx.send_viewport_cmd(ViewportCommand::Maximized(!maximized)),
                    Btn::Close => ctx.send_viewport_cmd(ViewportCommand::Close),
                }
            }
        }
    }

    /// Edge resizing + a hairline window border (custom chrome only).
    pub(super) fn chrome_edges(&mut self, ctx: &egui::Context) {
        let maximized = ctx.input(|i| {
            i.viewport().maximized.unwrap_or(false) || i.viewport().fullscreen.unwrap_or(false)
        });
        if maximized {
            return;
        }
        let screen = ctx.screen_rect();
        let t = self.tok();
        ctx.layer_painter(egui::LayerId::new(
            egui::Order::Foreground,
            egui::Id::new("win_border"),
        ))
        .rect_stroke(
            screen,
            CornerRadius::ZERO,
            Stroke::new(
                1.0_f32,
                if t.dark {
                    Color32::from_rgb(58, 58, 62)
                } else {
                    Color32::from_rgb(200, 200, 206)
                },
            ),
            egui::StrokeKind::Inside,
        );
        let Some(pos) = ctx.input(|i| i.pointer.hover_pos()) else {
            return;
        };
        let (l, r) = (pos.x - screen.left() < EDGE, screen.right() - pos.x < EDGE);
        let (mut tp, b) = (pos.y - screen.top() < EDGE, screen.bottom() - pos.y < EDGE);
        // Over the caption buttons the top edge belongs to the buttons (only a
        // thin strip at the very right still resizes, like Windows 11).
        if self.custom_chrome()
            && pos.x > screen.right() - BTN_W * 3.0
            && screen.right() - pos.x >= 3.0
        {
            tp = false;
        }
        let dir = match (l, r, tp, b) {
            (true, _, true, _) => Some((ResizeDirection::NorthWest, CursorIcon::ResizeNorthWest)),
            (_, true, true, _) => Some((ResizeDirection::NorthEast, CursorIcon::ResizeNorthEast)),
            (true, _, _, true) => Some((ResizeDirection::SouthWest, CursorIcon::ResizeSouthWest)),
            (_, true, _, true) => Some((ResizeDirection::SouthEast, CursorIcon::ResizeSouthEast)),
            (true, ..) => Some((ResizeDirection::West, CursorIcon::ResizeWest)),
            (_, true, ..) => Some((ResizeDirection::East, CursorIcon::ResizeEast)),
            (_, _, true, _) => Some((ResizeDirection::North, CursorIcon::ResizeNorth)),
            (_, _, _, true) => Some((ResizeDirection::South, CursorIcon::ResizeSouth)),
            _ => None,
        };
        if let Some((d, icon)) = dir {
            ctx.set_cursor_icon(icon);
            if ctx.input(|i| i.pointer.primary_pressed()) {
                ctx.send_viewport_cmd(ViewportCommand::BeginResize(d));
            }
        }
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Btn {
    Min,
    Max,
    Close,
}

/// Custom chrome is the default on Windows; elsewhere it can be tried with
/// `TERN_CUSTOM_TITLEBAR=1`.
pub(super) fn custom_chrome_available() -> bool {
    cfg!(windows) || std::env::var_os("TERN_CUSTOM_TITLEBAR").is_some()
}

/// "Tern" set in semibold with open tracking and a true left→right gradient
/// (per-vertex colours on the glyph mesh).
pub(super) fn paint_wordmark(ui: &egui::Ui, center: Pos2, size: f32, c0: Color32, c1: Color32) {
    let mut job = egui::text::LayoutJob::default();
    job.append(
        "Tern",
        0.0,
        egui::TextFormat {
            font_id: w::semibold(size),
            color: Color32::WHITE,
            extra_letter_spacing: size * 0.14,
            ..Default::default()
        },
    );
    let galley = ui.fonts(|f| f.layout_job(job));
    let tex = ui.fonts(|f| f.font_image_size());
    let (tw, th) = (tex[0] as f32, tex[1] as f32);
    let ppp = ui.ctx().pixels_per_point();
    let o = center - galley.size() / 2.0;
    let origin = Pos2::new((o.x * ppp).round() / ppp, (o.y * ppp).round() / ppp);
    let x0 = origin.x;
    let width = galley.size().x.max(1.0);
    for row in &galley.rows {
        let mut mesh = row.visuals.mesh.clone();
        for v in &mut mesh.vertices {
            v.pos += origin.to_vec2();
            v.uv = Pos2::new(v.uv.x / tw, v.uv.y / th);
            let k = ((v.pos.x - x0) / width).clamp(0.0, 1.0);
            v.color = w::lerp(c0, c1, k);
        }
        ui.painter().add(egui::Shape::mesh(mesh));
    }
}
