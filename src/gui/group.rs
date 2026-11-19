//! 群控 (group control): see every live session at once and drive them together —
//! broadcast commands from one bar, optionally mirror keystrokes typed in any tile.

use super::panels::icon_tile;
use super::widgets::{self as w, Icon, Kind};
use super::*;
use egui::{
    Align, Color32, CornerRadius, Layout, Pos2, Rect, Sense, Stroke, StrokeKind, UiBuilder, Vec2,
};

const TILE_HEADER: f32 = 34.0;
const GAP: f32 = 10.0;

impl GuiApp {
    /// Sessions shown as tiles: connected or connecting.
    fn group_tiles(&self) -> Vec<usize> {
        (0..self.sessions.len())
            .filter(|&i| {
                let s = &self.sessions[i];
                // Failed / dropped sessions stay as tiles so the error is visible.
                s.is_busy() || (!s.params.host.is_empty() && !s.is_fresh())
            })
            .collect()
    }

    /// Tiles that receive broadcast input (connected and not excluded).
    pub(super) fn group_targets(&self) -> Vec<usize> {
        (0..self.sessions.len())
            .filter(|&i| {
                let s = &self.sessions[i];
                s.state().1 && !self.group_excluded.contains(&s.uid)
            })
            .collect()
    }

    pub(super) fn set_view_mode(&mut self, mode: ViewMode) {
        if self.view_mode != mode {
            self.view_mode = mode;
            self.mode_changed_at = self.ctx.input(|i| i.time);
            self.focus_term = mode == ViewMode::Single;
        }
    }

    /// Picker listing saved hosts; clicking connects (stays open for more picks).
    fn host_picker(&mut self, ui: &mut egui::Ui) {
        let t = self.tok();
        let zh = self.zh();
        let connected: Vec<String> = self
            .sessions
            .iter()
            .filter(|s| s.is_busy())
            .filter_map(|s| s.params.profile_id.clone())
            .collect();
        let pending: Vec<String> = self
            .config
            .profiles
            .iter()
            .filter(|p| !connected.contains(&p.id))
            .map(|p| p.id.clone())
            .collect();
        if self.config.profiles.is_empty() {
            w::menu_caption(
                ui,
                if zh {
                    "还没有保存的主机"
                } else {
                    "No saved hosts"
                },
            );
            return;
        }
        if !pending.is_empty()
            && w::menu_item(
                ui,
                &if zh {
                    format!("连接全部（{} 台）", pending.len())
                } else {
                    format!("Connect all ({})", pending.len())
                },
                "",
                false,
            )
        {
            for id in &pending {
                self.connect_profile(id, false);
            }
            ui.memory_mut(|m| m.close_popup());
        }
        w::menu_separator(ui);
        let ids: Vec<(String, String, String)> = self
            .config
            .profiles
            .iter()
            .map(|p| {
                (
                    p.id.clone(),
                    p.display_label(),
                    format!("{}@{}", p.user, p.host),
                )
            })
            .collect();
        egui::ScrollArea::vertical()
            .max_height(320.0)
            .show(ui, |ui| {
                for (id, label, sub) in ids {
                    let on = connected.contains(&id);
                    let (r, resp) = ui
                        .allocate_exact_size(Vec2::new(ui.available_width(), 40.0), Sense::click());
                    let h = ui
                        .ctx()
                        .animate_bool_with_time(resp.id.with("h"), resp.hovered(), 0.1);
                    ui.painter().rect_filled(
                        r,
                        CornerRadius::same(7),
                        t.row_hover.gamma_multiply(h),
                    );
                    let pr = ui.painter_at(r);
                    w::paint_text_trunc(
                        ui,
                        &pr,
                        &label,
                        w::regular(13.0),
                        t.text,
                        r.left() + 10.0,
                        r.center().y - 8.0,
                        r.width() - 50.0,
                    );
                    w::paint_text_trunc(
                        ui,
                        &pr,
                        &sub,
                        w::regular(11.5),
                        t.text2,
                        r.left() + 10.0,
                        r.center().y + 9.0,
                        r.width() - 50.0,
                    );
                    if on {
                        w::status_dot(
                            ui.ctx(),
                            &pr,
                            Pos2::new(r.right() - 16.0, r.center().y),
                            t.ok,
                            w::DotState::Live,
                        );
                    } else {
                        w::paint_icon(
                            ui.painter(),
                            Rect::from_center_size(
                                Pos2::new(r.right() - 16.0, r.center().y),
                                Vec2::splat(12.0),
                            ),
                            Icon::Plus,
                            if resp.hovered() { t.accent } else { t.text3 },
                        );
                    }
                    if resp.clicked() && !on {
                        self.connect_profile(&id, false);
                    }
                }
            });
    }

    pub(super) fn ui_group(&mut self, ui: &mut egui::Ui) {
        let t = self.tok();
        let zh = self.zh();
        let tiles = self.group_tiles();
        let targets = self.group_targets();
        self.handle_group_drop(ui.ctx());

        // ---------- header ----------
        ui.horizontal(|ui| {
            ui.set_height(w::CTRL_H);
            let g = w::galley(
                ui,
                if zh { "群控" } else { "Group control" },
                w::semibold(17.0),
                t.text,
            );
            let (r, _) = ui.allocate_exact_size(g.size(), Sense::hover());
            ui.painter().galley(r.min, g, t.text);
            ui.add_space(6.0);
            let live = tiles
                .iter()
                .filter(|&&i| self.sessions[i].state().1)
                .count();
            let sub = if tiles.is_empty() {
                String::new()
            } else if zh {
                format!("已选 {} / {} 台", targets.len(), live)
            } else {
                format!("{} of {} selected", targets.len(), live)
            };
            let g = w::galley(ui, &sub, w::regular(12.5), t.text2);
            let (r, _) = ui.allocate_exact_size(g.size(), Sense::hover());
            ui.painter().galley(r.min + Vec2::new(0.0, 2.0), g, t.text2);

            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.spacing_mut().item_spacing.x = 8.0;
                // Paired controls share one width so English labels of
                // different length still line up.
                let (l_add, l_files) = if zh {
                    ("连接主机", "群发文件")
                } else {
                    ("Add hosts", "Files")
                };
                let (l_sync, l_all) = if zh {
                    ("同步键盘输入", "全选")
                } else {
                    ("Mirror typing", "Select all")
                };
                let btn_w =
                    w::button_width(ui, true, l_add).max(w::button_width(ui, true, l_files));
                let chip_w = w::toggle_chip_width(ui, true, l_sync)
                    .max(w::toggle_chip_width(ui, true, l_all));
                let add =
                    w::button_sized(ui, Kind::Secondary, Some(Icon::Plus), l_add, true, btn_w);
                w::popover(ui, &add, egui::Id::new("group_add"), 280.0, |ui| {
                    self.host_picker(ui)
                });
                if !tiles.is_empty() {
                    if w::button_sized(
                        ui,
                        Kind::Secondary,
                        Some(Icon::Upload),
                        l_files,
                        true,
                        btn_w,
                    )
                    .on_hover_text(if zh {
                        "批量上传到选中主机，或从每台主机下载同一个文件"
                    } else {
                        "Upload to every selected host, or fetch one file from each"
                    })
                    .clicked()
                    {
                        self.open_batch(0);
                    }
                    // hairline divider between actions and toggles
                    let (dr, _) = ui.allocate_exact_size(Vec2::new(9.0, 16.0), Sense::hover());
                    ui.painter().vline(
                        dr.center().x.round() + 0.5,
                        dr.y_range(),
                        Stroke::new(1.0_f32, t.separator),
                    );
                    let mut sync = self.group_sync;
                    if w::toggle_chip(ui, &mut sync, Some(Icon::Keyboard), l_sync, false, chip_w)
                        .on_hover_text(if zh {
                            "打开后，在任意一个终端里打字会同时发送到所有选中的主机"
                        } else {
                            "Keys typed in any tile go to every selected host"
                        })
                        .changed()
                    {
                        self.group_sync = sync;
                    }
                    ui.add_space(2.0);
                    let mut all = targets.len() == live && live > 0;
                    if w::toggle_chip(ui, &mut all, Some(Icon::Check), l_all, false, chip_w)
                        .on_hover_text(if zh {
                            "打开：所有主机接收群发；关闭：全部取消"
                        } else {
                            "On: every host receives broadcast input"
                        })
                        .changed()
                    {
                        if all {
                            self.group_excluded.clear();
                        } else {
                            for &i in &tiles {
                                self.group_excluded.insert(self.sessions[i].uid);
                            }
                        }
                    }
                }
            });
        });
        ui.add_space(12.0);

        // ---------- empty state ----------
        if tiles.is_empty() {
            let mut connect_all = false;
            ui.add_space((ui.available_height() * 0.22).max(20.0));
            ui.vertical_centered(|ui| {
                icon_tile(ui, Icon::Grid, t.accent, Color32::WHITE, 64.0);
                ui.add_space(18.0);
                let g = w::galley(
                    ui,
                    if zh {
                        "一次操作多台服务器"
                    } else {
                        "Drive many servers at once"
                    },
                    w::semibold(22.0),
                    t.text,
                );
                let (r, _) = ui.allocate_exact_size(g.size(), Sense::hover());
                ui.painter().galley(r.min, g, t.text);
                ui.add_space(6.0);
                let g = w::galley(
                    ui,
                    if zh {
                        "连接几台主机，命令会同时发送到每一台，输出并排显示。"
                    } else {
                        "Connect a few hosts — every command runs on all of them, side by side."
                    },
                    w::regular(13.0),
                    t.text2,
                );
                let (r, _) = ui.allocate_exact_size(g.size(), Sense::hover());
                ui.painter().galley(r.min, g, t.text2);
                ui.add_space(22.0);
                let (row, _) =
                    ui.allocate_exact_size(Vec2::new(310.0, w::CTRL_H + 4.0), Sense::hover());
                let mut c = ui.new_child(
                    UiBuilder::new()
                        .max_rect(row)
                        .layout(Layout::left_to_right(Align::Center)),
                );
                c.spacing_mut().item_spacing.x = 10.0;
                if w::button_sized(
                    &mut c,
                    Kind::Primary,
                    Some(Icon::Bolt),
                    if zh {
                        "连接全部主机"
                    } else {
                        "Connect all hosts"
                    },
                    !self.config.profiles.is_empty(),
                    150.0,
                )
                .clicked()
                {
                    connect_all = true;
                }
                let pick = w::button_sized(
                    &mut c,
                    Kind::Secondary,
                    Some(Icon::Plus),
                    if zh { "选择主机…" } else { "Choose…" },
                    true,
                    150.0,
                );
                w::popover(&c, &pick, egui::Id::new("group_pick_empty"), 280.0, |ui| {
                    self.host_picker(ui)
                });
            });
            if connect_all {
                let ids: Vec<String> = self.config.profiles.iter().map(|p| p.id.clone()).collect();
                for id in ids {
                    self.connect_profile(&id, false);
                }
            }
            return;
        }

        // ---------- grid ----------
        let avail = ui.available_rect_before_wrap();
        let bar_lines = self.cmd_input.split('\n').count().clamp(1, 6) as f32;
        let bar_h = (bar_lines * 18.0 + 22.0).max(42.0);
        let grid_rect =
            Rect::from_min_max(avail.min, Pos2::new(avail.max.x, avail.max.y - bar_h - GAP));
        ui.allocate_rect(avail, Sense::hover());

        let n = tiles.len();
        let max_cols = ((grid_rect.width() + GAP) / (360.0 + GAP)).floor().max(1.0) as usize;
        let ideal = match n {
            1 => 1,
            2 => 2,
            3 | 4 => 2,
            5 | 6 => 3,
            7..=9 => 3,
            _ => 4,
        };
        let cols = ideal.min(max_cols).max(1);
        let rows = n.div_ceil(cols);
        let tile_w = (grid_rect.width() - GAP * (cols as f32 - 1.0)) / cols as f32;
        let fit_h = (grid_rect.height() - GAP * (rows as f32 - 1.0)) / rows as f32;
        let tile_h = fit_h.max(220.0);
        let content_h = tile_h * rows as f32 + GAP * (rows as f32 - 1.0);

        let font = egui::FontId::monospace((self.term_font_size - 2.0).clamp(10.0, 13.0));
        let pal = theme::term_palette(self.term_theme_pref());
        let modal = self.modal_open();
        let mut outputs: Vec<(usize, term::TermOutput)> = Vec::new();
        let mut toggle: Option<u64> = None;
        let mut maximize: Option<usize> = None;
        let mut reconnect: Option<usize> = None;
        let mut close_tile: Option<usize> = None;
        // The tile with keyboard focus is the "current" session (shortcuts
        // like Ctrl+Shift+W, the monitor panel, file actions follow it).
        let mut focused_tile: Option<usize> = None;

        let mut gu = ui.new_child(
            UiBuilder::new()
                .max_rect(grid_rect)
                .layout(Layout::top_down(Align::Min)),
        );
        egui::ScrollArea::vertical()
            .id_salt("group_grid")
            .auto_shrink([false, false])
            .show(&mut gu, |ui| {
                let (area, _) =
                    ui.allocate_exact_size(Vec2::new(grid_rect.width(), content_h), Sense::hover());
                for (k, &i) in tiles.iter().enumerate() {
                    let (row, col) = (k / cols, k % cols);
                    let r = Rect::from_min_size(
                        Pos2::new(
                            area.left() + col as f32 * (tile_w + GAP),
                            area.top() + row as f32 * (tile_h + GAP),
                        ),
                        Vec2::new(tile_w, tile_h),
                    );
                    let id = self.sessions[i].term_id();
                    let focused = ui.ctx().memory(|m| m.has_focus(id));
                    if focused {
                        focused_tile = Some(i);
                    }
                    if !ui.is_rect_visible(r) && !focused && self.sessions[i].term.has_sent_size() {
                        continue;
                    }
                    let uid = self.sessions[i].uid;
                    let included = !self.group_excluded.contains(&uid);
                    let (connecting, connected) = self.sessions[i].state();
                    let offline = !connecting && !connected;
                    let failed =
                        self.sessions[i].shared.lock().unwrap().status_kind == StatusKind::Error;

                    // Tile surface
                    let p = ui.painter();
                    p.rect_filled(
                        r.translate(Vec2::new(0.0, 2.0)),
                        CornerRadius::same(13),
                        Color32::from_black_alpha(if t.dark { 40 } else { 18 }),
                    );
                    p.rect_filled(r, CornerRadius::same(12), pal.bg);
                    let head = Rect::from_min_size(r.min, Vec2::new(r.width(), TILE_HEADER));
                    p.rect_filled(
                        head,
                        CornerRadius {
                            nw: 12,
                            ne: 12,
                            sw: 0,
                            se: 0,
                        },
                        w::lerp(pal.bg, pal.fg, 0.05),
                    );

                    // Header content
                    let hresp = ui.interact(head, ui.id().with(("tile_head", uid)), Sense::click());
                    w::status_dot(
                        ui.ctx(),
                        p,
                        Pos2::new(head.left() + 16.0, head.center().y),
                        if connected {
                            t.ok
                        } else if connecting {
                            t.warn
                        } else if failed {
                            t.err
                        } else {
                            t.text3
                        },
                        if connected {
                            w::DotState::Live
                        } else if connecting {
                            w::DotState::Busy
                        } else {
                            w::DotState::Steady
                        },
                    );
                    let slot = &self.sessions[i];
                    let name = slot.title.clone();
                    let sub = format!("{}@{}", slot.params.user, slot.params.host);
                    let hp = ui.painter_at(head);
                    let nw = w::paint_text_trunc(
                        ui,
                        &hp,
                        &name,
                        w::semibold(12.5),
                        pal.fg,
                        head.left() + 30.0,
                        head.center().y,
                        (head.width() - 120.0) * 0.55,
                    );
                    w::paint_text_trunc(
                        ui,
                        &hp,
                        &sub,
                        w::regular(11.5),
                        w::lerp(pal.fg, pal.bg, 0.45),
                        head.left() + 30.0 + nw + 8.0,
                        head.center().y,
                        head.width() - 120.0 - nw,
                    );
                    // include toggle (check circle) — or reconnect when offline
                    let cr = Rect::from_center_size(
                        Pos2::new(head.right() - 44.0, head.center().y),
                        Vec2::splat(22.0),
                    );
                    if offline {
                        let rresp = ui
                            .interact(cr, ui.id().with(("tile_re", uid)), Sense::click())
                            .on_hover_text(if zh { "重新连接" } else { "Reconnect" });
                        if rresp.hovered() {
                            p.rect_filled(cr, CornerRadius::same(6), w::lerp(pal.bg, pal.fg, 0.12));
                        }
                        w::paint_icon(
                            p,
                            Rect::from_center_size(cr.center(), Vec2::splat(12.0)),
                            Icon::Refresh,
                            if rresp.hovered() {
                                t.accent
                            } else {
                                w::lerp(pal.fg, pal.bg, 0.3)
                            },
                        );
                        if rresp.clicked() {
                            reconnect = Some(i);
                        }
                    } else {
                        let cresp = ui
                            .interact(cr, ui.id().with(("tile_inc", uid)), Sense::click())
                            .on_hover_text(if zh {
                                "是否接收群发命令"
                            } else {
                                "Receive broadcast input"
                            });
                        let a = ui
                            .ctx()
                            .animate_bool_with_time(cresp.id.with("a"), included, 0.15);
                        let cc = cr.center();
                        if a > 0.0 {
                            p.circle_filled(cc, 8.5, t.accent.gamma_multiply(a));
                        }
                        if a < 0.99 {
                            p.circle_stroke(
                                cc,
                                8.0,
                                Stroke::new(
                                    1.4_f32,
                                    w::lerp(pal.fg, pal.bg, 0.5).gamma_multiply(1.0 - a),
                                ),
                            );
                        }
                        if a > 0.01 {
                            w::paint_icon(
                                p,
                                Rect::from_center_size(cc, Vec2::splat(10.0)),
                                Icon::Check,
                                Color32::WHITE.gamma_multiply(a),
                            );
                        }
                        if cresp.clicked() {
                            toggle = Some(uid);
                        }
                    }
                    let mr = Rect::from_center_size(
                        Pos2::new(head.right() - 16.0, head.center().y),
                        Vec2::splat(24.0),
                    );
                    let mresp = ui
                        .interact(mr, ui.id().with(("tile_max", uid)), Sense::click())
                        .on_hover_text(match (offline, zh) {
                            (true, true) => "关闭",
                            (true, false) => "Close",
                            (false, true) => "单独打开",
                            (false, false) => "Open on its own",
                        });
                    if mresp.hovered() {
                        p.rect_filled(mr, CornerRadius::same(6), w::lerp(pal.bg, pal.fg, 0.12));
                    }
                    w::paint_icon(
                        p,
                        Rect::from_center_size(
                            mr.center(),
                            Vec2::splat(if offline { 10.0 } else { 12.0 }),
                        ),
                        if offline { Icon::Close } else { Icon::Expand },
                        w::lerp(pal.fg, pal.bg, 0.3),
                    );
                    if mresp.clicked() && offline {
                        close_tile = Some(i);
                    } else if mresp.clicked() || hresp.double_clicked() {
                        maximize = Some(i);
                    }
                    if hresp.clicked() {
                        ui.memory_mut(|m| m.request_focus(id));
                    }

                    // Terminal
                    let body = Rect::from_min_max(Pos2::new(r.left(), head.bottom()), r.max);
                    let mut tu = ui.new_child(
                        UiBuilder::new()
                            .max_rect(body)
                            .layout(Layout::top_down(Align::Min)),
                    );
                    tu.set_clip_rect(body.intersect(ui.clip_rect()));
                    let (_resp, out) = self.sessions[i].term.show(
                        &mut tu,
                        id,
                        &font,
                        &pal,
                        connected && !modal,
                        zh,
                    );
                    outputs.push((i, out));

                    // Dim excluded tiles, ring the focused one.
                    let p = ui.painter();
                    if !included || offline {
                        p.rect_filled(
                            body,
                            CornerRadius {
                                nw: 0,
                                ne: 0,
                                sw: 12,
                                se: 12,
                            },
                            pal.bg.gamma_multiply(0.55),
                        );
                    }
                    let fr = ui
                        .ctx()
                        .animate_bool_with_time(id.with("tile_focus"), focused, 0.18);
                    let ring = if self.group_sync && included && fr > 0.0 {
                        t.accent
                    } else {
                        w::lerp(t.separator, t.accent, fr * 0.8)
                    };
                    p.rect_stroke(
                        r,
                        CornerRadius::same(12),
                        Stroke::new(
                            1.0 + fr * 0.6,
                            ring.gamma_multiply(0.35 + 0.65 * fr.max(0.4)),
                        ),
                        StrokeKind::Inside,
                    );
                    if connecting {
                        p.text(
                            body.center(),
                            egui::Align2::CENTER_CENTER,
                            if zh {
                                "正在连接…"
                            } else {
                                "Connecting…"
                            },
                            w::regular(12.0),
                            w::lerp(pal.fg, pal.bg, 0.4),
                        );
                    }
                }
            });

        // Route input from tiles.
        let targets_now = self.group_targets();
        for (i, out) in outputs {
            if let Some((c, r)) = out.resized {
                self.send_resize(i, c, r);
            }
            let uid = self.sessions[i].uid;
            let mirror = self.group_sync && !self.group_excluded.contains(&uid);
            let receivers: Vec<usize> = if mirror { targets_now.clone() } else { vec![i] };
            // Keystrokes and pastes are mirrored; each host encodes the paste for
            // its own terminal mode. Mouse / wheel / redraw bytes stay local.
            for &j in &receivers {
                let mut bytes = out.input_bytes(|t| self.sessions[j].term.encode_paste(t));
                if j == i {
                    bytes.extend_from_slice(&out.local_bytes);
                }
                if !bytes.is_empty() {
                    self.send_bytes(j, bytes);
                }
            }
            if !receivers.contains(&i) && !out.local_bytes.is_empty() {
                self.send_bytes(i, out.local_bytes.clone());
            }
            if out.copied {
                self.toast_msg(self.t("已拷贝", "Copied"), StatusKind::Ok);
            }
            if let Some(url) = out.open_url {
                native::open_url(&url);
            }
        }
        if let Some(uid) = toggle {
            if !self.group_excluded.remove(&uid) {
                self.group_excluded.insert(uid);
            }
        }
        if let Some(i) = focused_tile {
            self.active = i;
        }
        if let Some(i) = reconnect {
            self.reconnect(i);
        }
        if let Some(i) = close_tile {
            self.request_close_tab(i);
        }
        if let Some(i) = maximize {
            self.switch_tab(i);
            self.sessions[i].view = CenterView::Terminal;
            self.set_view_mode(ViewMode::Single);
        }

        // ---------- broadcast bar ----------
        let bar = Rect::from_min_max(Pos2::new(avail.min.x, avail.max.y - bar_h), avail.max);
        let badge = if zh {
            format!("{} 台", targets_now.len())
        } else {
            format!("{} hosts", targets_now.len())
        };
        self.ui_cmdbar(ui, &targets_now, 0, bar, Some(badge));
    }

    /// Right panel in group mode: one compact row per live host.
    pub(super) fn ui_group_monitor(&mut self, ui: &mut egui::Ui) {
        let t = self.tok();
        let zh = self.zh();
        let tiles = self.group_tiles();
        let g = w::galley(
            ui,
            if zh { "集群概览" } else { "Fleet" },
            w::semibold(16.0),
            t.text,
        );
        let (r, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 30.0), Sense::hover());
        ui.painter().galley(
            Pos2::new(r.left() + 2.0, r.center().y - g.size().y / 2.0),
            g,
            t.text,
        );
        ui.add_space(6.0);
        if tiles.is_empty() {
            ui.add_space(30.0);
            ui.vertical_centered(|ui| {
                let g = w::galley(
                    ui,
                    if zh {
                        "暂无在线主机"
                    } else {
                        "No hosts online"
                    },
                    w::regular(12.5),
                    t.text3,
                );
                let (r, _) = ui.allocate_exact_size(g.size(), Sense::hover());
                ui.painter().galley(r.min, g, t.text3);
            });
            return;
        }
        let mut focus: Option<egui::Id> = None;
        egui::ScrollArea::vertical()
            .id_salt("fleet")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                for &i in &tiles {
                    let slot = &self.sessions[i];
                    let (connecting, connected) = slot.state();
                    let failed = slot.shared.lock().unwrap().status_kind == StatusKind::Error;
                    let mon = slot.shared.lock().unwrap().monitor.clone();
                    let included = !self.group_excluded.contains(&slot.uid);
                    let name = slot.title.clone();
                    let host_name = mon.hostname.clone().unwrap_or_default();
                    let (r, resp) = ui
                        .allocate_exact_size(Vec2::new(ui.available_width(), 66.0), Sense::click());
                    let h =
                        ui.ctx()
                            .animate_bool_with_time(resp.id.with("h"), resp.hovered(), 0.12);
                    ui.painter().rect_filled(
                        r,
                        CornerRadius::same(12),
                        w::lerp(t.card, t.row_hover, h),
                    );
                    let pr = ui.painter_at(r.expand(6.0));
                    w::status_dot(
                        ui.ctx(),
                        &pr,
                        Pos2::new(r.left() + 16.0, r.top() + 18.0),
                        if connected {
                            t.ok
                        } else if connecting {
                            t.warn
                        } else if failed {
                            t.err
                        } else {
                            t.text3
                        },
                        if connected {
                            w::DotState::Live
                        } else if connecting {
                            w::DotState::Busy
                        } else {
                            w::DotState::Steady
                        },
                    );
                    let nw = w::paint_text_trunc(
                        ui,
                        &pr,
                        &name,
                        w::semibold(12.5),
                        if included { t.text } else { t.text2 },
                        r.left() + 28.0,
                        r.top() + 18.0,
                        r.width() - 110.0,
                    );
                    if !host_name.is_empty() {
                        w::paint_text_trunc(
                            ui,
                            &pr,
                            &host_name,
                            w::regular(11.0),
                            t.text3,
                            r.left() + 34.0 + nw,
                            r.top() + 18.0,
                            r.width() - 120.0 - nw,
                        );
                    }
                    let load = mon
                        .load_avg
                        .map(|l| format!("{:.2}", l[0]))
                        .unwrap_or_else(|| "–".into());
                    let lg = w::galley(ui, &load, w::regular(11.5), t.text2);
                    pr.galley(
                        Pos2::new(
                            r.right() - 12.0 - lg.size().x,
                            r.top() + 18.0 - lg.size().y / 2.0,
                        ),
                        lg,
                        t.text2,
                    );
                    // CPU / MEM bars
                    let bar = |y: f32, label: &str, v: Option<f32>, color: Color32| {
                        let lg = w::galley(ui, label, w::regular(10.5), t.text3);
                        pr.galley(
                            Pos2::new(r.left() + 14.0, y - lg.size().y / 2.0),
                            lg,
                            t.text3,
                        );
                        let track = Rect::from_min_max(
                            Pos2::new(r.left() + 52.0, y - 2.0),
                            Pos2::new(r.right() - 48.0, y + 2.0),
                        );
                        pr.rect_filled(track, CornerRadius::same(2), t.control);
                        if let Some(v) = v {
                            let mut f = track;
                            f.set_width(track.width() * (v / 100.0).clamp(0.0, 1.0));
                            pr.rect_filled(f, CornerRadius::same(2), color);
                        }
                        let vg = w::galley(
                            ui,
                            &v.map(|v| format!("{v:.0}%")).unwrap_or_else(|| "–".into()),
                            w::regular(10.5),
                            t.text2,
                        );
                        pr.galley(
                            Pos2::new(r.right() - 12.0 - vg.size().x, y - vg.size().y / 2.0),
                            vg,
                            t.text2,
                        );
                    };
                    let cpu_c = match mon.cpu_percent {
                        Some(v) if v >= 90.0 => t.err,
                        Some(v) if v >= 75.0 => t.warn,
                        _ => t.accent,
                    };
                    bar(r.top() + 38.0, "CPU", mon.cpu_percent, cpu_c);
                    bar(
                        r.top() + 52.0,
                        if zh { "内存" } else { "MEM" },
                        mon.mem_percent,
                        Color32::from_rgb(175, 82, 222),
                    );
                    if resp.clicked() {
                        focus = Some(slot.term_id());
                    }
                    ui.add_space(8.0);
                }
            });
        if let Some(id) = focus {
            ui.memory_mut(|m| m.request_focus(id));
        }
    }
}
