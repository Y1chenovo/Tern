//! Toolbar, tab bar, centre view (welcome / terminal / files), monitor,
//! dialogs and toasts.

use super::widgets::{self as w, Icon, Kind};
use super::*;
use egui::{
    Align, Align2, Color32, CornerRadius, Layout, Pos2, Rect, Sense, Stroke, StrokeKind, UiBuilder,
    Vec2,
};

/// Frame shared by every modal sheet.
pub(super) fn sheet_frame(t: &theme::Tokens) -> egui::Frame {
    egui::Frame::NONE
        .fill(t.card)
        .corner_radius(CornerRadius::same(16))
        .inner_margin(egui::Margin::same(22))
        .shadow(egui::Shadow {
            offset: [0, 16],
            blur: 48,
            spread: 0,
            color: Color32::from_black_alpha(if t.dark { 150 } else { 50 }),
        })
        .stroke(if t.dark {
            Stroke::new(1.0_f32, t.separator)
        } else {
            Stroke::NONE
        })
}

/// Rounded square tile with a centred icon (used at the top of sheets / welcome).
pub(super) fn icon_tile(ui: &mut egui::Ui, icon: Icon, fill: Color32, fg: Color32, size: f32) {
    let (r, _) = ui.allocate_exact_size(Vec2::splat(size), Sense::hover());
    ui.painter()
        .rect_filled(r, CornerRadius::same((size * 0.26) as u8), fill);
    w::paint_icon(
        ui.painter(),
        Rect::from_center_size(r.center(), Vec2::splat(size * 0.5)),
        icon,
        fg,
    );
}

impl GuiApp {
    // ------------------------------------------------------------------
    // Toolbar
    // ------------------------------------------------------------------
    pub(super) fn ui_toolbar(&mut self, ui: &mut egui::Ui) {
        // The native title bar above shares the toolbar colour, so the two
        // read as one band: sit the controls a touch above the bar's middle.
        let full = ui.max_rect();
        let custom = self.custom_chrome();
        // Our own caption sits above; empty toolbar space drags the window too
        // (decided at the end, once we know where the controls are).
        let toolbar_bg = custom
            .then(|| ui.interact(full, ui.id().with("toolbar_drag"), Sense::click_and_drag()));
        let rect = if custom {
            full
        } else {
            Rect::from_min_max(full.min, Pos2::new(full.max.x, full.max.y - 4.0))
        };

        // Left: sidebar toggle + app name
        let mut left = ui.new_child(
            UiBuilder::new()
                .max_rect(rect)
                .layout(Layout::left_to_right(Align::Center)),
        );
        if w::icon_toggle(
            &mut left,
            Icon::Sidebar,
            false,
            self.t("主机列表", "Host list"),
        )
        .clicked()
        {
            self.config.show_sidebar = !self.config.show_sidebar;
            self.persist();
        }
        left.add_space(6.0);
        let mut mode = if self.view_mode == ViewMode::Single {
            0
        } else {
            1
        };
        if w::segmented(
            &mut left,
            &[
                (Some(Icon::Terminal), self.t("会话", "Sessions")),
                (Some(Icon::Grid), self.t("群控", "Group")),
            ],
            &mut mode,
            0.0,
        ) {
            self.set_view_mode(if mode == 0 {
                ViewMode::Single
            } else {
                ViewMode::Group
            });
        }

        // Centre: quick connect
        let fw = 380.0f32.min(rect.width() * 0.42);
        let center = Rect::from_center_size(rect.center(), Vec2::new(fw, w::CTRL_H));
        let mut mid = ui.new_child(
            UiBuilder::new()
                .max_rect(center)
                .layout(Layout::left_to_right(Align::Center)),
        );
        let hint = self.t("快速连接　user@host:port", "Quick connect   user@host:port");
        let resp = w::Field::new(&mut self.quick_connect)
            .icon(Icon::Bolt)
            .hint(hint)
            .clearable()
            .centered()
            .id(egui::Id::new("quick_connect"))
            .show(&mut mid);
        if resp.lost_focus()
            && ui.input(|i| i.key_pressed(egui::Key::Enter))
            && !self.quick_connect.trim().is_empty()
        {
            self.quick_connect_submit();
        }

        // Right: monitor toggle + settings
        let mut right = ui.new_child(
            UiBuilder::new()
                .max_rect(rect)
                .layout(Layout::right_to_left(Align::Center)),
        );
        let gear = w::icon_button(&mut right, Icon::Gear, self.t("设置", "Settings"), true);
        let pop_id = egui::Id::new("settings_popover");
        w::popover(&right, &gear, pop_id, 296.0, |ui| self.ui_settings(ui));
        if w::icon_toggle(
            &mut right,
            Icon::Chart,
            self.config.show_monitor,
            self.t("资源监控", "Monitor"),
        )
        .clicked()
        {
            self.config.show_monitor = !self.config.show_monitor;
            self.persist();
        }
        if let Some(bg) = toolbar_bg {
            // Only a press on empty space moves the window, never one that
            // started on a control (even if the pointer wobbles while pressed).
            let controls = [
                left.min_rect().expand(4.0),
                center.expand(4.0),
                right.min_rect().expand(4.0),
            ];
            let on_empty = ui
                .ctx()
                .input(|i| i.pointer.press_origin())
                .is_some_and(|p| !controls.iter().any(|r| r.contains(p)));
            if bg.drag_started_by(egui::PointerButton::Primary) && on_empty {
                ui.ctx().send_viewport_cmd(egui::ViewportCommand::StartDrag);
            }
            if bg.double_clicked() && on_empty {
                let max = ui.ctx().input(|i| i.viewport().maximized.unwrap_or(false));
                ui.ctx()
                    .send_viewport_cmd(egui::ViewportCommand::Maximized(!max));
            }
        }
    }

    pub(super) fn set_lang(&mut self, lang: LangPref) {
        if self.lang == lang {
            return;
        }
        self.lang = lang;
        let zh = self.zh();
        for s in &mut self.sessions {
            if s.params.host.is_empty() {
                s.title = if zh {
                    "新会话".into()
                } else {
                    "New session".into()
                };
            }
        }
        self.persist();
    }

    fn ui_settings(&mut self, ui: &mut egui::Ui) {
        let t = self.tok();
        let avail = ui.available_width();
        ui.spacing_mut().item_spacing.y = 4.0;
        w::caption(ui, self.t("外观", "Appearance"));
        let mut th = if self.theme == ThemePref::Light { 0 } else { 1 };
        let seg2 = (avail - 4.0) / 2.0;
        if w::segmented(
            ui,
            &[
                (Some(Icon::Sun), self.t("浅色", "Light")),
                (Some(Icon::Moon), self.t("深色", "Dark")),
            ],
            &mut th,
            seg2,
        ) {
            self.theme = if th == 0 {
                ThemePref::Light
            } else {
                ThemePref::Dark
            };
            self.persist();
        }
        w::caption(ui, self.t("语言", "Language"));
        let mut lg = if self.zh() { 0 } else { 1 };
        if w::segmented(ui, &[(None, "中文"), (None, "English")], &mut lg, seg2) {
            self.set_lang(if lg == 0 { LangPref::Zh } else { LangPref::En });
        }
        w::caption(ui, self.t("终端配色", "Terminal colors"));
        let mut tt = match self.config.term_theme {
            profiles::TermThemePref::Dark => 0,
            profiles::TermThemePref::Light => 1,
            profiles::TermThemePref::System => 2,
        };
        let seg3 = (avail - 4.0) / 3.0;
        if w::segmented(
            ui,
            &[
                (None, self.t("深色", "Dark")),
                (None, self.t("浅色", "Light")),
                (None, self.t("跟随", "Auto")),
            ],
            &mut tt,
            seg3,
        ) {
            self.config.term_theme = [
                profiles::TermThemePref::Dark,
                profiles::TermThemePref::Light,
                profiles::TermThemePref::System,
            ][tt];
            self.persist();
        }
        ui.add_space(4.0);
        let mut bar = self.config.show_cmdbar;
        if w::switch(
            ui,
            &mut bar,
            self.t("终端下方显示命令输入栏", "Command bar under terminal"),
        )
        .changed()
        {
            self.config.show_cmdbar = bar;
            self.persist();
        }
        if super::chrome::custom_chrome_available() {
            let mut sys = self.config.system_titlebar;
            if w::switch(
                ui,
                &mut sys,
                self.t("使用系统标题栏", "Use system title bar"),
            )
            .changed()
            {
                self.config.system_titlebar = sys;
                self.persist();
            }
        }
        let mut auto_rc = self.config.auto_reconnect;
        if w::switch(
            ui,
            &mut auto_rc,
            self.t("断线自动重连", "Auto-reconnect on disconnect"),
        )
        .changed()
        {
            self.config.auto_reconnect = auto_rc;
            if !auto_rc {
                for slot in &mut self.sessions {
                    slot.auto_reconnect_at = None;
                    slot.auto_reconnect_attempts = 0;
                }
            }
            self.persist();
        }
        w::caption(ui, self.t("终端字号", "Terminal font size"));
        let reset_label = self.t("默认", "Default");
        let gap = 6.0;
        let reset_w = ((avail - gap) * 0.39)
            .round()
            .max(w::button_width(ui, true, reset_label));
        let step_w = (avail - reset_w - gap).max(120.0);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = gap;
            let val = format!("{:.0} pt", self.term_font_size);
            let (dec, inc) = w::font_stepper(
                ui,
                &val,
                self.term_font_size > 9.0,
                self.term_font_size < 32.0,
                step_w,
            );
            if dec {
                self.set_font_size(self.term_font_size - 1.0);
            }
            if inc {
                self.set_font_size(self.term_font_size + 1.0);
            }
            if w::button_sized(
                ui,
                Kind::Secondary,
                Some(Icon::Refresh),
                reset_label,
                (self.term_font_size - 14.0).abs() > 0.01,
                reset_w,
            )
            .clicked()
            {
                self.set_font_size(14.0);
            }
        });
        let mut cv = self.config.ctrl_v_to_apps;
        if w::switch(
            ui,
            &mut cv,
            self.t(
                "全屏程序接管 Ctrl+V（Vim 列选择）",
                "Pass Ctrl+V to full-screen apps (Vim)",
            ),
        )
        .changed()
        {
            self.config.ctrl_v_to_apps = cv;
            term::CTRL_V_TO_APPS.store(cv, std::sync::atomic::Ordering::Relaxed);
            self.persist();
        }
        w::caption(
            ui,
            self.t("回滚行数（新会话生效）", "Scrollback lines (new sessions)"),
        );
        let opts = [1_000usize, 5_000, 10_000, 50_000];
        let mut sb = opts
            .iter()
            .position(|n| *n >= self.config.scrollback_lines)
            .unwrap_or(opts.len() - 1);
        let seg4 = (avail - 4.0) / 4.0;
        if w::segmented(
            ui,
            &[(None, "1k"), (None, "5k"), (None, "10k"), (None, "50k")],
            &mut sb,
            seg4,
        ) {
            self.config.scrollback_lines = opts[sb];
            self.persist();
        }
        w::caption(ui, self.t("下载位置", "Downloads"));
        let dir = self.download_dir().display().to_string();
        let (r, _) = ui.allocate_exact_size(Vec2::new(avail, 20.0), Sense::hover());
        w::paint_text_trunc(
            ui,
            &ui.painter_at(r),
            &dir,
            egui::FontId::monospace(11.5),
            t.text2,
            r.left() + 6.0,
            r.center().y,
            r.width() - 6.0,
        );
        let gap = 6.0;
        let half_w = (avail - gap) / 2.0;
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = gap;
            if w::button_sized(
                ui,
                Kind::Secondary,
                None,
                self.t("更改…", "Change…"),
                true,
                half_w,
            )
            .clicked()
            {
                self.open_picker(super::picker::PickFor::DownloadDir);
            }
            if w::button_sized(
                ui,
                Kind::Secondary,
                None,
                self.t("在文件夹中显示", "Show in folder"),
                true,
                half_w,
            )
            .clicked()
            {
                let d = self.download_dir();
                let _ = std::fs::create_dir_all(&d);
                native::reveal(&d);
            }
        });
        ui.add_space(4.0);
        w::menu_separator(ui);
        ui.add_space(2.0);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = gap;
            if w::button_sized(
                ui,
                Kind::Secondary,
                Some(Icon::Keyboard),
                self.t("键盘快捷键", "Shortcuts"),
                true,
                half_w,
            )
            .clicked()
            {
                self.show_shortcuts = true;
                ui.memory_mut(|m| m.close_popup());
            }
            if w::button_sized(
                ui,
                Kind::Secondary,
                Some(Icon::Folder),
                self.t("配置文件夹", "Config folder"),
                true,
                half_w,
            )
            .on_hover_text(self.t("打开配置文件夹", "Open config folder"))
            .clicked()
            {
                native::reveal(&profiles::config_dir());
                ui.memory_mut(|m| m.close_popup());
            }
        });
        ui.add_space(2.0);
        w::menu_separator(ui);
        let font = self
            .cjk_font
            .as_ref()
            .and_then(|f| {
                std::path::Path::new(f)
                    .file_name()
                    .and_then(|s| s.to_str())
                    .map(|s| s.to_string())
            })
            .unwrap_or_else(|| self.t("默认字体", "default font").to_string());
        let g = w::galley(
            ui,
            &format!("Tern {}  ·  {font}", env!("CARGO_PKG_VERSION")),
            w::regular(11.0),
            t.text3,
        );
        let (r, _) = ui.allocate_exact_size(Vec2::new(avail, 18.0), Sense::hover());
        let mid = w::optical_mid(ui, &g);
        ui.painter().galley(
            Pos2::new(r.center().x - g.size().x / 2.0, r.center().y - mid),
            g,
            t.text3,
        );
    }

    fn quick_connect_submit(&mut self) {
        let Some((user, host, port)) = parse_quick(&self.quick_connect) else {
            self.toast_msg(
                self.t(
                    "格式：user@host 或 user@host:port",
                    "Format: user@host or user@host:port",
                ),
                StatusKind::Error,
            );
            return;
        };
        let port_num = port.unwrap_or(22);
        let found = self
            .config
            .profiles
            .iter()
            .find(|p| {
                p.host.eq_ignore_ascii_case(&host)
                    && p.port == port_num
                    && user.as_ref().is_none_or(|u| u == &p.user)
            })
            .or_else(|| {
                (user.is_none() && port.is_none())
                    .then(|| {
                        self.config
                            .profiles
                            .iter()
                            .find(|p| p.name.eq_ignore_ascii_case(&host))
                    })
                    .flatten()
            })
            .map(|p| p.id.clone());
        self.quick_connect.clear();
        if let Some(id) = found {
            self.connect_profile(&id, false);
            return;
        }
        let params = ConnParams {
            host,
            port: port_num,
            user: user.unwrap_or_else(|| "root".into()),
            ..Default::default()
        };
        self.connect_params(params, false, None);
    }

    // ------------------------------------------------------------------
    // Tab bar
    // ------------------------------------------------------------------
    pub(super) fn ui_tab_bar(&mut self, ui: &mut egui::Ui) {
        let t = self.tok();
        let mut switch_to = None;
        let mut close = None;
        let mut new_tab = false;
        let mut dup: Option<usize> = None;
        let mut scrolled_to = false;

        let (row, _) =
            ui.allocate_exact_size(Vec2::new(ui.available_width(), w::CTRL_H), Sense::hover());

        // Right: view switch + connect action (laid out first so tabs get the rest).
        let mut right = ui.new_child(
            UiBuilder::new()
                .max_rect(row)
                .layout(Layout::right_to_left(Align::Center)),
        );
        {
            let ui = &mut right;
            let idx = self.active;
            let slot = &self.sessions[idx];
            let (connecting, connected) = slot.state();
            let ended = slot.shared.lock().unwrap().session_ended;
            let can_reconnect = !connecting
                && !connected
                && !slot.params.host.is_empty()
                && (slot.view == CenterView::Files || !ended);
            if connecting || connected {
                if w::button(
                    ui,
                    Kind::Destructive,
                    Some(Icon::Power),
                    self.t("断开", "Disconnect"),
                    true,
                )
                .clicked()
                {
                    self.disconnect_at(idx);
                }
            } else if can_reconnect
                && w::button(
                    ui,
                    Kind::Primary,
                    Some(Icon::Refresh),
                    self.t("重新连接", "Reconnect"),
                    true,
                )
                .clicked()
            {
                self.reconnect(idx);
            }
            ui.add_space(6.0);
            let mut sel = if self.sessions[idx].view == CenterView::Terminal {
                0
            } else {
                1
            };
            if w::segmented(
                ui,
                &[
                    (Some(Icon::Terminal), self.t("终端", "Terminal")),
                    (Some(Icon::Folder), self.t("文件", "Files")),
                ],
                &mut sel,
                86.0,
            ) {
                self.sessions[idx].view = if sel == 0 {
                    CenterView::Terminal
                } else {
                    CenterView::Files
                };
                if sel == 0 {
                    self.focus_term = true;
                } else if let Some(osc_cwd) = self.sessions[idx].term.take_osc7_cwd() {
                    let (connected, cur_cwd) = {
                        let s = self.sessions[idx].shared.lock().unwrap();
                        (s.connected, s.files.cwd.clone())
                    };
                    if connected && osc_cwd != cur_cwd {
                        self.refresh_files(idx, Some(osc_cwd));
                    }
                }
            }
        }
        let used = right.min_rect().width();

        // Left: recessed tab strip whose tabs shrink to fit, then scroll.
        let tabs_rect =
            Rect::from_min_max(row.min, Pos2::new(row.right() - used - 14.0, row.bottom()));
        let inset = 2.0;
        let tab_h = w::CTRL_H - inset * 2.0;
        let gap = 2.0;
        let plus_gap = 6.0;
        let max_track_w = (tabs_rect.width() - w::CTRL_H - plus_gap).max(72.0);
        let avail = (max_track_w - inset * 2.0).max(64.0);
        let n = self.sessions.len();
        let natural: Vec<f32> = self
            .sessions
            .iter()
            .map(|s| {
                let (connecting, connected) = s.state();
                let kind = s.shared.lock().unwrap().status_kind;
                let has_dot = connected || connecting || kind == StatusKind::Error;
                let dot_w = if has_dot { 13.0 } else { 0.0 };
                (dot_w + w::galley(ui, &s.title, w::semibold(12.5), t.text).size().x + 50.0)
                    .clamp(104.0, 220.0)
            })
            .collect();
        let total: f32 = natural.iter().sum::<f32>() + gap * (n.saturating_sub(1)) as f32;
        let widths: Vec<f32> = if total <= avail {
            natural
        } else {
            // Water-fill: short titles keep their natural width, long ones
            // share what is left, never below a legible minimum.
            let budget = avail - gap * (n.saturating_sub(1)) as f32;
            let mut sorted = natural.clone();
            sorted.sort_by(|a, b| a.total_cmp(b));
            let mut cap = budget / n as f32;
            let mut rest = budget;
            for (k, w) in sorted.iter().enumerate() {
                let share = rest / (n - k) as f32;
                if *w <= share {
                    rest -= *w;
                } else {
                    cap = share;
                    break;
                }
            }
            // Below this a name stops being readable — scroll instead.
            natural.iter().map(|w| w.min(cap).max(116.0)).collect()
        };
        let strip_w: f32 = widths.iter().sum::<f32>() + gap * (n.saturating_sub(1)) as f32;
        let inner_w = avail.min(strip_w);
        let track_rect =
            Rect::from_min_size(tabs_rect.min, Vec2::new(inner_w + inset * 2.0, w::CTRL_H));
        let track_fill = if t.dark {
            w::lerp(t.bg, t.control, 0.58)
        } else {
            t.control
        };
        ui.painter()
            .rect_filled(track_rect, CornerRadius::same(8), track_fill);

        let track_inner = track_rect.shrink(inset);
        let mut tu = ui.new_child(
            UiBuilder::new()
                .max_rect(track_inner)
                .layout(Layout::left_to_right(Align::Center)),
        );
        tu.set_clip_rect(
            track_inner
                .expand2(Vec2::new(0.0, 1.0))
                .intersect(ui.clip_rect()),
        );
        tu.spacing_mut().item_spacing = Vec2::ZERO;
        tu.spacing_mut().interact_size.y = tab_h;
        // A plain mouse wheel scrolls the strip sideways (no Shift needed).
        tu.style_mut().always_scroll_the_only_direction = true;
        let out = egui::ScrollArea::horizontal()
            .id_salt("tabs_scroll")
            .max_width(inner_w)
            .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysHidden)
            .show(&mut tu, |ui| {
                ui.spacing_mut().interact_size.y = tab_h;
                ui.spacing_mut().item_spacing = Vec2::new(gap, 0.0);
                ui.horizontal(|ui| {
                    ui.spacing_mut().interact_size.y = tab_h;
                    ui.spacing_mut().item_spacing = Vec2::new(gap, 0.0);
                    let mut prev_hovered = false;
                    #[allow(clippy::needless_range_loop)]
                    for i in 0..n {
                        let slot = &self.sessions[i];
                        let (connecting, connected) = slot.state();
                        let kind = slot.shared.lock().unwrap().status_kind;
                        let active = i == self.active;
                        let title = slot.title.clone();
                        let width = widths[i];
                        let (rect, resp) =
                            ui.allocate_exact_size(Vec2::new(width, tab_h), Sense::click());
                        // `hovered()` is false while the pointer is on the × (it is a
                        // separate widget on top), so test containment instead.
                        let pointer_in = resp.contains_pointer();
                        let a = ui
                            .ctx()
                            .animate_bool_with_time(resp.id.with("act"), active, 0.15);
                        let h =
                            ui.ctx()
                                .animate_bool_with_time(resp.id.with("hov"), pointer_in, 0.12);
                        let p = ui.painter();
                        if i > 0 && !active && self.active != i - 1 && !pointer_in && !prev_hovered
                        {
                            p.line_segment(
                                [
                                    Pos2::new(rect.left() - gap * 0.5, rect.top() + 5.0),
                                    Pos2::new(rect.left() - gap * 0.5, rect.bottom() - 5.0),
                                ],
                                Stroke::new(1.0_f32, t.separator),
                            );
                        }
                        prev_hovered = pointer_in;
                        if a > 0.01 {
                            let thumb_fill = if t.dark { t.control_hover } else { t.card };
                            p.rect_filled(
                                rect.translate(Vec2::new(0.0, 0.6)),
                                CornerRadius::same(6),
                                t.shadow.gamma_multiply(a),
                            );
                            p.rect_filled(
                                rect,
                                CornerRadius::same(6),
                                w::lerp(track_fill, thumb_fill, a),
                            );
                        } else if h > 0.01 {
                            let hov_fill = if t.dark {
                                w::lerp(track_fill, t.control, 0.65 * h)
                            } else {
                                w::lerp(track_fill, t.card, 0.55 * h)
                            };
                            p.rect_filled(rect, CornerRadius::same(6), hov_fill);
                        }
                        let dc = if connected {
                            t.ok
                        } else if connecting {
                            t.warn
                        } else if kind == StatusKind::Error {
                            t.err
                        } else {
                            t.text3
                        };
                        let has_dot = connected || connecting || kind == StatusKind::Error;
                        let show_close = active || pointer_in;
                        // The unread dot gives way to the × while hovered.
                        let show_dot = slot.unread && !active && !pointer_in;
                        if active && self.tab_scroll_pending {
                            resp.scroll_to_me(Some(Align::Center));
                            scrolled_to = true;
                        }
                        let dot_slot = if has_dot { 13.0 } else { 0.0 };
                        let pad_l = 12.0;
                        let pad_r = 24.0;
                        let max_content_w = (rect.width() - pad_l - pad_r).max(20.0);
                        let font = if active {
                            w::semibold(12.5)
                        } else {
                            w::regular(12.5)
                        };
                        let color = if active {
                            t.text
                        } else if pointer_in {
                            w::lerp(t.text2, t.text, 0.55)
                        } else {
                            t.text2
                        };
                        // Keep a " (n)" suffix visible when the name is truncated.
                        let (base, suffix) = match title.rfind(" (") {
                            Some(k) if title.ends_with(')') => (&title[..k], &title[k..]),
                            _ => (title.as_str(), ""),
                        };
                        let full_title_w = w::galley(ui, &title, font.clone(), color).size().x;
                        let actual_content_w = (dot_slot + full_title_w).min(max_content_w);
                        let start_x = (rect.center().x - actual_content_w / 2.0)
                            .clamp(rect.left() + pad_l, rect.right() - pad_r - actual_content_w);
                        if has_dot {
                            w::status_dot(
                                ui.ctx(),
                                p,
                                Pos2::new(start_x + 3.5, rect.center().y),
                                dc,
                                if connected {
                                    w::DotState::Live
                                } else if connecting {
                                    w::DotState::Busy
                                } else {
                                    w::DotState::Steady
                                },
                            );
                        }
                        let text_x = start_x + dot_slot;
                        let text_max_w = (max_content_w - dot_slot).max(14.0);
                        let pr = ui.painter_at(rect);
                        if full_title_w <= text_max_w || suffix.is_empty() {
                            w::paint_text_trunc(
                                ui,
                                &pr,
                                &title,
                                font,
                                color,
                                text_x,
                                rect.center().y,
                                text_max_w,
                            );
                        } else {
                            let sg = w::galley(ui, suffix, font.clone(), color);
                            let bw = w::paint_text_trunc(
                                ui,
                                &pr,
                                base,
                                font,
                                color,
                                text_x,
                                rect.center().y,
                                (text_max_w - sg.size().x).max(14.0),
                            );
                            let smid = w::optical_mid(ui, &sg);
                            pr.galley(Pos2::new(text_x + bw, rect.center().y - smid), sg, color);
                        }
                        if show_dot {
                            p.circle_filled(
                                Pos2::new(rect.right() - 12.0, rect.center().y),
                                3.0,
                                t.accent,
                            );
                        }
                        let cr = Rect::from_center_size(
                            Pos2::new(rect.right() - 12.0, rect.center().y),
                            Vec2::splat(16.0),
                        );
                        let cresp =
                            ui.interact(cr, ui.id().with(("tab_close", slot.uid)), Sense::click());
                        if show_close {
                            if cresp.hovered() {
                                p.rect_filled(
                                    cr,
                                    CornerRadius::same(5),
                                    if t.dark {
                                        t.control_press
                                    } else {
                                        t.control_hover
                                    },
                                );
                            }
                            w::paint_icon(
                                p,
                                Rect::from_center_size(cr.center(), Vec2::splat(9.5)),
                                Icon::Close,
                                if cresp.hovered() {
                                    t.text
                                } else if active {
                                    t.text2
                                } else {
                                    t.text3
                                },
                            );
                        }
                        if cresp.clicked() && show_close {
                            close = Some(i);
                        } else if resp.clicked() || cresp.clicked() {
                            switch_to = Some(i);
                        }
                        if resp.middle_clicked() {
                            close = Some(i);
                        }
                        let mut tip = if slot.params.host.is_empty() {
                            title.clone()
                        } else {
                            format!(
                                "{}@{}:{}",
                                slot.params.user, slot.params.host, slot.params.port
                            )
                        };
                        if let Some(rt) = slot.term.title() {
                            tip.push('\n');
                            tip.push_str(rt);
                        }
                        let resp = resp.on_hover_text(tip);
                        resp.context_menu(|ui| {
                            ui.set_width(210.0);
                            if w::menu_item(
                                ui,
                                self.t("复制此会话", "Duplicate session"),
                                "",
                                false,
                            ) {
                                dup = Some(i);
                                ui.close_menu();
                            }
                            if w::menu_item(
                                ui,
                                self.t("关闭标签", "Close tab"),
                                "Ctrl+Shift+W",
                                false,
                            ) {
                                close = Some(i);
                                ui.close_menu();
                            }
                        });
                    }
                });
            });
        // Soft fades at the edges when there are more tabs out of view.
        let max_off = (out.content_size.x - out.inner_rect.width()).max(0.0);
        if max_off > 0.5 {
            let fw = 28.0;
            let ir = out.inner_rect;
            let lk = (out.state.offset.x / fw).clamp(0.0, 1.0);
            let rk = ((max_off - out.state.offset.x) / fw).clamp(0.0, 1.0);
            let p = tu.painter();
            if lk > 0.0 {
                w::fade_edge(
                    p,
                    Rect::from_min_size(ir.left_top(), Vec2::new(fw, ir.height())),
                    track_fill.gamma_multiply(lk),
                    true,
                );
            }
            if rk > 0.0 {
                w::fade_edge(
                    p,
                    Rect::from_min_size(
                        Pos2::new(ir.right() - fw, ir.top()),
                        Vec2::new(fw, ir.height()),
                    ),
                    track_fill.gamma_multiply(rk),
                    false,
                );
            }
        }
        let plus_rect = Rect::from_min_size(
            Pos2::new(track_rect.right() + plus_gap, tabs_rect.top()),
            Vec2::splat(w::CTRL_H),
        );
        let plus_resp = ui.interact(plus_rect, ui.id().with("tab_new_btn"), Sense::click());
        let plus_h =
            ui.ctx()
                .animate_bool_with_time(plus_resp.id.with("h"), plus_resp.hovered(), 0.12);
        let plus_fill = if plus_resp.is_pointer_button_down_on() {
            t.control_press
        } else {
            w::lerp(track_fill, t.control_hover, plus_h)
        };
        ui.painter()
            .rect_filled(plus_rect, CornerRadius::same(8), plus_fill);
        w::paint_icon(
            ui.painter(),
            Rect::from_center_size(plus_rect.center(), Vec2::splat(14.0)),
            Icon::Plus,
            if plus_resp.hovered() { t.text } else { t.text2 },
        );
        if plus_resp
            .on_hover_text(self.t("新建标签  Ctrl+Shift+T", "New tab  Ctrl+Shift+T"))
            .clicked()
        {
            new_tab = true;
        }
        if scrolled_to {
            self.tab_scroll_pending = false;
        }

        if new_tab {
            self.new_tab();
        }
        if let Some(i) = dup {
            let params = self.sessions[i].params.clone();
            if !params.host.is_empty() {
                self.connect_params(params, true, None);
            } else {
                self.new_tab();
            }
        }
        if let Some(i) = switch_to {
            self.switch_tab(i);
        }
        if let Some(i) = close {
            self.request_close_tab(i);
        }
    }

    // ------------------------------------------------------------------
    // Centre
    // ------------------------------------------------------------------
    pub(super) fn ui_center(&mut self, ui: &mut egui::Ui) {
        self.handle_dropped_files(ui.ctx());
        let idx = self.active.min(self.sessions.len() - 1);
        let slot = &self.sessions[idx];
        if slot.view == CenterView::Files {
            self.ui_files(ui);
        } else if slot.is_fresh() {
            self.ui_welcome(ui);
        } else {
            self.ui_terminal(ui, idx);
        }
    }

    fn handle_dropped_files(&mut self, ctx: &egui::Context) {
        if self.modal_open() {
            return;
        }
        let (hovering, dropped): (bool, Vec<PathBuf>) = ctx.input(|i| {
            (
                !i.raw.hovered_files.is_empty(),
                i.raw
                    .dropped_files
                    .iter()
                    .filter_map(|f| f.path.clone())
                    .collect(),
            )
        });
        let idx = self.active;
        let connected = self.sessions[idx].state().1;
        if hovering {
            let t = self.tok();
            let screen = ctx.screen_rect();
            let painter = ctx.layer_painter(egui::LayerId::new(
                egui::Order::Foreground,
                egui::Id::new("drop_overlay"),
            ));
            painter.rect_filled(screen, 0.0, Color32::from_black_alpha(90));
            let card = Rect::from_center_size(screen.center(), Vec2::new(380.0, 150.0));
            painter.rect_filled(card, CornerRadius::same(18), t.card);
            painter.rect_stroke(
                card.shrink(10.0),
                CornerRadius::same(12),
                Stroke::new(1.5_f32, if connected { t.accent } else { t.warn }),
                StrokeKind::Inside,
            );
            w::paint_icon(
                &painter,
                Rect::from_center_size(card.center() - Vec2::new(0.0, 22.0), Vec2::splat(30.0)),
                Icon::Upload,
                if connected { t.accent } else { t.warn },
            );
            let msg = if connected {
                self.t(
                    "松开即可上传到当前远端目录（支持文件夹）",
                    "Drop to upload here — folders too",
                )
            } else {
                self.t("请先连接会话", "Connect a session first")
            };
            painter.text(
                card.center() + Vec2::new(0.0, 24.0),
                Align2::CENTER_CENTER,
                msg,
                w::regular(14.0),
                t.text,
            );
        }
        if !dropped.is_empty() && connected {
            self.sessions[idx].view = CenterView::Files;
            self.upload_paths(idx, dropped);
        }
    }

    fn ui_welcome(&mut self, ui: &mut egui::Ui) {
        let t = self.tok();
        let recent = self.recent_ids(5);
        let mut connect: Option<String> = None;
        let mut new_host = false;
        let mut quick = false;
        egui::ScrollArea::vertical()
            .id_salt("welcome")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                let col_w = 440.0f32.min(ui.available_width() - 32.0);
                let top = ((ui.available_height() - if recent.is_empty() { 260.0 } else { 520.0 })
                    * 0.42)
                    .max(24.0);
                ui.add_space(top);
                ui.vertical_centered(|ui| {
                    ui.set_max_width(col_w);
                    let (r, _) = ui.allocate_exact_size(Vec2::splat(72.0), Sense::hover());
                    w::paint_logo(ui.painter(), r);
                    ui.add_space(18.0);
                    let g = w::galley(
                        ui,
                        self.t("开始新的会话", "Start a session"),
                        w::semibold(24.0),
                        t.text,
                    );
                    let (r, _) = ui.allocate_exact_size(g.size(), Sense::hover());
                    ui.painter().galley(r.min, g, t.text);
                    ui.add_space(6.0);
                    let g = w::galley(
                        ui,
                        self.t(
                            "通过物理网卡直连服务器，不受本机 VPN 影响。",
                            "Reach your servers over the physical network, around the local VPN.",
                        ),
                        w::regular(13.0),
                        t.text2,
                    );
                    let (r, _) = ui.allocate_exact_size(g.size(), Sense::hover());
                    ui.painter().galley(r.min, g, t.text2);
                    ui.add_space(22.0);
                    // buttons, centred as a group
                    let b1 = self.t("新建主机", "New host");
                    let b2 = self.t("快速连接", "Quick connect");
                    let gw = 150.0 * 2.0 + 10.0;
                    let (row, _) =
                        ui.allocate_exact_size(Vec2::new(gw, w::CTRL_H + 4.0), Sense::hover());
                    let mut child = ui.new_child(
                        UiBuilder::new()
                            .max_rect(row)
                            .layout(Layout::left_to_right(Align::Center)),
                    );
                    child.spacing_mut().item_spacing.x = 10.0;
                    if w::button_sized(&mut child, Kind::Primary, Some(Icon::Plus), b1, true, 150.0)
                        .clicked()
                    {
                        new_host = true;
                    }
                    if w::button_sized(
                        &mut child,
                        Kind::Secondary,
                        Some(Icon::Bolt),
                        b2,
                        true,
                        150.0,
                    )
                    .clicked()
                    {
                        quick = true;
                    }
                });
                if !recent.is_empty() {
                    ui.add_space(40.0);
                    ui.vertical_centered(|ui| {
                        ui.set_max_width(col_w);
                        let (cr, _) =
                            ui.allocate_exact_size(Vec2::new(col_w, 22.0), Sense::hover());
                        let g = w::galley(ui, self.t("最近", "Recent"), w::semibold(12.0), t.text3);
                        ui.painter().galley(
                            Pos2::new(cr.left() + 4.0, cr.center().y - g.size().y / 2.0),
                            g,
                            t.text3,
                        );
                        ui.add_space(2.0);
                        let row_h = 52.0;
                        let n = recent.len();
                        let (list, _) = ui.allocate_exact_size(
                            Vec2::new(col_w, row_h * n as f32),
                            Sense::hover(),
                        );
                        ui.painter().rect_filled(
                            list,
                            CornerRadius::same(12),
                            if t.dark { t.card } else { t.chrome },
                        );
                        for (k, id) in recent.iter().enumerate() {
                            let Some(p) = self.config.profiles.iter().find(|p| &p.id == id) else {
                                continue;
                            };
                            let r = Rect::from_min_size(
                                list.min + Vec2::new(0.0, row_h * k as f32),
                                Vec2::new(col_w, row_h),
                            );
                            let resp = ui.interact(r, ui.id().with(("recent", k)), Sense::click());
                            let h = ui.ctx().animate_bool_with_time(
                                resp.id.with("h"),
                                resp.hovered(),
                                0.12,
                            );
                            let cr = CornerRadius {
                                nw: if k == 0 { 12 } else { 0 },
                                ne: if k == 0 { 12 } else { 0 },
                                sw: if k + 1 == n { 12 } else { 0 },
                                se: if k + 1 == n { 12 } else { 0 },
                            };
                            ui.painter()
                                .rect_filled(r, cr, t.row_hover.gamma_multiply(h));
                            if k > 0 {
                                ui.painter().line_segment(
                                    [
                                        Pos2::new(r.left() + 58.0, r.top()),
                                        Pos2::new(r.right() - 14.0, r.top()),
                                    ],
                                    Stroke::new(1.0_f32, t.separator),
                                );
                            }
                            let tile = Rect::from_center_size(
                                Pos2::new(r.left() + 32.0, r.center().y),
                                Vec2::splat(32.0),
                            );
                            ui.painter()
                                .rect_filled(tile, CornerRadius::same(8), t.accent_soft);
                            w::paint_icon(
                                ui.painter(),
                                Rect::from_center_size(tile.center(), Vec2::splat(16.0)),
                                Icon::Server,
                                t.accent,
                            );
                            let pr = ui.painter_at(r);
                            w::paint_text_trunc(
                                ui,
                                &pr,
                                &p.display_label(),
                                w::semibold(13.0),
                                t.text,
                                r.left() + 58.0,
                                r.center().y - 9.0,
                                col_w - 200.0,
                            );
                            w::paint_text_trunc(
                                ui,
                                &pr,
                                &format!("{}@{}:{}", p.user, p.host, p.port),
                                w::regular(12.0),
                                t.text2,
                                r.left() + 58.0,
                                r.center().y + 9.0,
                                col_w - 200.0,
                            );
                            let when = p.last_connected.map(format_mtime).unwrap_or_default();
                            let g = w::galley(ui, &when, w::regular(11.5), t.text3);
                            ui.painter().galley(
                                Pos2::new(
                                    r.right() - 34.0 - g.size().x,
                                    r.center().y - g.size().y / 2.0,
                                ),
                                g,
                                t.text3,
                            );
                            w::paint_icon(
                                ui.painter(),
                                Rect::from_center_size(
                                    Pos2::new(r.right() - 18.0, r.center().y),
                                    Vec2::splat(11.0),
                                ),
                                Icon::ChevronRight,
                                if resp.hovered() { t.accent } else { t.text3 },
                            );
                            if resp.clicked() {
                                connect = Some(id.clone());
                            }
                        }
                    });
                }
                ui.add_space(28.0);
                ui.vertical_centered(|ui| {
                    let g = w::galley(
                        ui,
                        self.t(
                            "把文件拖进窗口即可上传  ·  Ctrl+Shift+T 新标签",
                            "Drop files to upload  ·  Ctrl+Shift+T new tab",
                        ),
                        w::regular(11.5),
                        t.text3,
                    );
                    let (r, _) = ui.allocate_exact_size(g.size(), Sense::hover());
                    ui.painter().galley(r.min, g, t.text3);
                });
            });
        if new_host {
            self.editor = Some(self.blank_editor());
        }
        if quick {
            ui.memory_mut(|m| m.request_focus(egui::Id::new("quick_connect")));
        }
        if let Some(id) = connect {
            self.connect_profile(&id, false);
        }
    }

    fn ui_terminal(&mut self, ui: &mut egui::Ui, idx: usize) {
        let t = self.tok();
        let (connected, ended, status, kind) = {
            let s = self.sessions[idx].shared.lock().unwrap();
            (
                s.connected,
                s.session_ended,
                s.status.clone(),
                s.status_kind,
            )
        };

        if ended && !self.sessions[idx].is_busy() {
            let color = if kind == StatusKind::Error {
                t.err
            } else {
                t.warn
            };
            let mut reconnect = false;
            let mut cancel_retry = false;
            let mut close = false;
            let mut edit = false;
            let retry_info = self.sessions[idx].auto_reconnect_at.map(|at| {
                let secs = at
                    .saturating_duration_since(Instant::now())
                    .as_secs_f32()
                    .ceil()
                    .max(1.0) as u64;
                let attempt = self.sessions[idx]
                    .auto_reconnect_attempts
                    .clamp(1, conn::MAX_AUTO_RECONNECT);
                (secs, attempt)
            });
            let profile_id = self.sessions[idx].params.profile_id.clone();
            let lower = status.to_lowercase();
            let friendly = if let Some((secs, attempt)) = retry_info {
                if self.zh() {
                    format!(
                        "连接已断开，{secs} 秒后自动重连（第 {attempt}/{} 次）",
                        conn::MAX_AUTO_RECONNECT
                    )
                } else {
                    format!(
                        "Disconnected — reconnecting in {secs}s (attempt {attempt}/{})",
                        conn::MAX_AUTO_RECONNECT
                    )
                }
            } else if lower.contains("authentication failed") || status.contains("认证失败") {
                self.t(
                    "认证失败：请检查用户名、密码或私钥",
                    "Sign-in failed: check the user name, password or key",
                )
                .to_string()
            } else {
                status.clone()
            };
            egui::Frame::NONE
                .fill(color.gamma_multiply(if t.dark { 0.16 } else { 0.10 }))
                .corner_radius(CornerRadius::same(10))
                .inner_margin(egui::Margin {
                    left: 12,
                    right: 6,
                    top: 6,
                    bottom: 6,
                })
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.set_height(w::CTRL_H);
                        let (ir, _) = ui.allocate_exact_size(Vec2::splat(16.0), Sense::hover());
                        w::paint_icon(
                            ui.painter(),
                            ir,
                            if kind == StatusKind::Error {
                                Icon::Warning
                            } else {
                                Icon::Power
                            },
                            color,
                        );
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if w::button(
                                ui,
                                Kind::Primary,
                                Some(Icon::Refresh),
                                if retry_info.is_some() {
                                    self.t("立即重连", "Reconnect now")
                                } else {
                                    self.t("重新连接", "Reconnect")
                                },
                                true,
                            )
                            .clicked()
                            {
                                reconnect = true;
                            }
                            if retry_info.is_some() {
                                if w::button(
                                    ui,
                                    Kind::Secondary,
                                    None,
                                    self.t("取消自动重连", "Cancel retry"),
                                    true,
                                )
                                .clicked()
                                {
                                    cancel_retry = true;
                                }
                            } else if kind == StatusKind::Error
                                && profile_id.is_some()
                                && w::button(
                                    ui,
                                    Kind::Secondary,
                                    Some(Icon::Pencil),
                                    self.t("编辑主机", "Edit host"),
                                    true,
                                )
                                .clicked()
                            {
                                edit = true;
                            }
                            if w::button(ui, Kind::Plain, None, self.t("关闭", "Close"), true)
                                .clicked()
                            {
                                close = true;
                            }
                            let avail = ui.available_width();
                            let (r, _) =
                                ui.allocate_exact_size(Vec2::new(avail, w::CTRL_H), Sense::hover());
                            w::paint_text_trunc(
                                ui,
                                &ui.painter_at(r),
                                &friendly,
                                w::regular(13.0),
                                t.text,
                                r.left() + 4.0,
                                r.center().y,
                                r.width() - 8.0,
                            );
                        })
                        .response
                        .on_hover_text(&status);
                    });
                });
            ui.add_space(8.0);
            if cancel_retry {
                self.sessions[idx].auto_reconnect_at = None;
                self.sessions[idx].auto_reconnect_attempts = 0;
            }
            if reconnect {
                self.reconnect(idx);
            }
            if edit {
                if let Some(id) = &profile_id {
                    self.open_editor(id);
                }
            }
            if close {
                self.request_close_tab(idx);
                return;
            }
        }

        if self.find.is_some() {
            self.ui_find(ui, idx);
        }

        let font = egui::FontId::monospace(self.term_font_size);
        let pal = theme::term_palette(self.term_theme_pref());
        let modal = self.modal_open();
        let zh = self.zh();
        let id = self.sessions[idx].term_id();
        let scrolled = self.sessions[idx].term.scrollback_offset();

        // Split the area: terminal on top, command bar below.
        let show_bar = self.config.show_cmdbar && connected;
        let avail = ui.available_rect_before_wrap();
        let lines = (self.cmd_input.split('\n').count()).clamp(1, 6) as f32;
        let bar_h = if show_bar {
            (lines * 18.0 + 22.0).max(42.0)
        } else {
            0.0
        };
        let gap = if show_bar { 10.0 } else { 0.0 };
        let term_rect =
            Rect::from_min_max(avail.min, Pos2::new(avail.max.x, avail.max.y - bar_h - gap));
        ui.allocate_rect(avail, Sense::hover());

        let (resp, out) = {
            let slot = &mut self.sessions[idx];
            let mut tu = ui.new_child(
                UiBuilder::new()
                    .max_rect(term_rect)
                    .layout(Layout::top_down(Align::Min)),
            );
            slot.term
                .show(&mut tu, id, &font, &pal, connected && !modal, zh)
        };
        let term_focused = ui.ctx().memory(|m| m.has_focus(id));
        if term_focused || resp.hovered() {
            self.sessions[idx].unread = false;
        }
        if connected && !modal && (self.focus_term || ui.ctx().memory(|m| m.focused().is_none())) {
            resp.request_focus();
            self.focus_term = false;
        }

        if show_bar {
            let bar = Rect::from_min_max(Pos2::new(avail.min.x, avail.max.y - bar_h), avail.max);
            let uid = self.sessions[idx].uid;
            self.ui_cmdbar(ui, &[idx], uid, bar, None);
        }

        // Back to bottom capsule
        if scrolled > 0 {
            let label = format!("{}  {scrolled}", self.t("回到底部", "Jump to bottom"));
            let g = w::galley(ui, &label, w::regular(12.0), Color32::WHITE);
            let size = Vec2::new(g.size().x + 40.0, 30.0);
            let r = Rect::from_center_size(
                Pos2::new(resp.rect.center().x, resp.rect.bottom() - 30.0),
                size,
            );
            let pill = ui.interact(r, ui.id().with(("to_bottom", idx)), Sense::click());
            let bg = if pill.hovered() {
                t.accent
            } else {
                Color32::from_rgba_unmultiplied(28, 28, 30, 225)
            };
            ui.painter().rect_filled(
                r.translate(Vec2::new(0.0, 2.0)),
                CornerRadius::same(15),
                Color32::from_black_alpha(40),
            );
            ui.painter().rect_filled(r, CornerRadius::same(15), bg);
            w::paint_icon(
                ui.painter(),
                Rect::from_center_size(Pos2::new(r.left() + 18.0, r.center().y), Vec2::splat(12.0)),
                Icon::ArrowDown,
                Color32::WHITE,
            );
            ui.painter().galley(
                Pos2::new(r.left() + 30.0, r.center().y - g.size().y / 2.0),
                g,
                Color32::WHITE,
            );
            if pill.clicked() {
                self.sessions[idx].term.scroll_to_bottom();
            }
        }

        // Size HUD after a resize (like Terminal.app)
        let now = ui.input(|i| i.time);
        if let Some((c, r, at)) = self.size_hud {
            let age = now - at;
            if age < 1.2 {
                let alpha = if age < 0.9 {
                    1.0
                } else {
                    ((1.2 - age) / 0.3) as f32
                };
                let txt = format!("{c} × {r}");
                let g = w::galley(ui, &txt, egui::FontId::monospace(15.0), Color32::WHITE);
                let rr =
                    Rect::from_center_size(resp.rect.center(), g.size() + Vec2::new(36.0, 20.0));
                ui.painter().rect_filled(
                    rr,
                    CornerRadius::same(12),
                    Color32::from_rgba_unmultiplied(20, 20, 22, (210.0 * alpha) as u8),
                );
                ui.painter().galley_with_override_text_color(
                    rr.center() - g.size() / 2.0,
                    g,
                    Color32::WHITE.gamma_multiply(alpha),
                );
                ui.ctx().request_repaint();
            } else {
                self.size_hud = None;
            }
        }

        if let Some((cols, rows)) = out.resized {
            self.send_resize(idx, cols, rows);
            if connected {
                self.size_hud = Some((cols, rows, now));
            }
        }
        let mut bytes = out.input_bytes(|t| self.sessions[idx].term.encode_paste(t));
        bytes.extend(out.local_bytes);
        if !bytes.is_empty() {
            self.send_bytes(idx, bytes);
        }
        if out.font_delta != 0.0 {
            self.set_font_size(self.term_font_size + out.font_delta);
        }
        if out.copied {
            self.toast_msg(self.t("已拷贝", "Copied"), StatusKind::Ok);
        }
        if out.open_find {
            self.find = Some(FindBar {
                query: String::new(),
                focus: true,
                cache: None,
                searched_at: 0.0,
            });
        }
        if let Some(url) = out.open_url {
            native::open_url(&url);
        }
    }

    /// Command input bar: compose (incl. Chinese IME), Enter to send, history.
    /// `targets`: session indices that receive the command. `badge`: optional chip
    /// shown at the left (群控 target count).
    pub(super) fn ui_cmdbar(
        &mut self,
        ui: &mut egui::Ui,
        targets: &[usize],
        uid: u64,
        bar: Rect,
        badge: Option<String>,
    ) {
        let t = self.tok();
        let zh = self.zh();
        let fid = egui::Id::new(("cmdbar", uid));
        let focused = ui.ctx().memory(|m| m.has_focus(fid));

        // Track IME composition so Enter / arrows confirm the IME, not the command.
        // On Windows the Enter that confirms an IME candidate arrives in the same
        // frame as the IME commit (after it), so any IME activity this frame
        // means Enter belongs to the IME.
        let mut ime_this_frame = false;
        ui.input(|i| {
            for e in &i.events {
                if matches!(
                    e,
                    egui::Event::Ime(egui::ImeEvent::Commit(_) | egui::ImeEvent::Preedit(_))
                ) {
                    ime_this_frame = true;
                }
                if let egui::Event::Ime(ime) = e {
                    match ime {
                        egui::ImeEvent::Preedit(s) => self.cmd_composing = !s.is_empty(),
                        egui::ImeEvent::Commit(_) | egui::ImeEvent::Disabled => {
                            self.cmd_composing = false
                        }
                        egui::ImeEvent::Enabled => {}
                    }
                }
            }
        });

        let mut send = false;
        let mut hist: i32 = 0;
        let mut esc = false;
        let single_line = !self.cmd_input.contains('\n');
        if focused && !self.cmd_composing {
            ui.input_mut(|i| {
                i.events.retain(|e| match e {
                    egui::Event::Key {
                        key: egui::Key::Enter,
                        pressed,
                        modifiers,
                        ..
                    } if !modifiers.shift => {
                        if *pressed && !ime_this_frame {
                            send = true;
                        }
                        false
                    }
                    egui::Event::Key {
                        key: egui::Key::ArrowUp,
                        pressed: true,
                        modifiers,
                        ..
                    } if single_line && modifiers.is_none() => {
                        hist = -1;
                        false
                    }
                    egui::Event::Key {
                        key: egui::Key::ArrowDown,
                        pressed: true,
                        modifiers,
                        ..
                    } if single_line && modifiers.is_none() => {
                        hist = 1;
                        false
                    }
                    egui::Event::Key {
                        key: egui::Key::Escape,
                        pressed: true,
                        ..
                    } => {
                        esc = true;
                        false
                    }
                    _ => true,
                })
            });
        }

        // Background capsule
        let fill = if t.dark { t.card } else { t.control };
        let f = ui
            .ctx()
            .animate_bool_with_time(fid.with("f"), focused, 0.15);
        ui.painter().rect_filled(
            bar,
            CornerRadius::same(12),
            w::lerp(fill, if t.dark { t.control } else { t.bg }, f),
        );
        if f > 0.0 {
            ui.painter().rect_stroke(
                bar,
                CornerRadius::same(12),
                Stroke::new(1.0_f32, t.accent.gamma_multiply(0.6 * f)),
                StrokeKind::Inside,
            );
        }
        // Leading: prompt chevron, or a target-count chip in group mode.
        let lead = match &badge {
            Some(text) => {
                let g = w::galley(ui, text, w::semibold(12.0), Color32::WHITE);
                let chip = Rect::from_min_size(
                    Pos2::new(bar.left() + 8.0, bar.top() + 9.0),
                    Vec2::new(g.size().x + 20.0, 24.0),
                );
                // Grey chip when nothing would receive the command.
                let k = ui.ctx().animate_bool_with_time(
                    egui::Id::new("bc_chip"),
                    !targets.is_empty(),
                    0.18,
                );
                ui.painter().rect_filled(
                    chip,
                    CornerRadius::same(12),
                    w::lerp(t.control_press, t.accent, k),
                );
                ui.painter().galley(
                    Pos2::new(chip.left() + 10.0, chip.center().y - g.size().y / 2.0),
                    g,
                    Color32::WHITE,
                );
                chip.width() + 14.0
            }
            None => {
                w::paint_icon(
                    ui.painter(),
                    Rect::from_center_size(
                        Pos2::new(bar.left() + 18.0, bar.top() + 21.0),
                        Vec2::splat(13.0),
                    ),
                    Icon::ChevronRight,
                    if focused { t.accent } else { t.text3 },
                );
                32.0
            }
        };
        // Buttons (right)
        let btn_y = bar.top() + 21.0;
        let send_r =
            Rect::from_center_size(Pos2::new(bar.right() - 21.0, btn_y), Vec2::splat(28.0));
        let stop_r =
            Rect::from_center_size(Pos2::new(bar.right() - 55.0, btn_y), Vec2::splat(28.0));
        let snip_r =
            Rect::from_center_size(Pos2::new(bar.right() - 89.0, btn_y), Vec2::splat(28.0));
        let has_text = !self.cmd_input.is_empty();
        let sresp = ui
            .interact(send_r, fid.with("send"), Sense::click())
            .on_hover_text(if zh { "发送  Enter" } else { "Send  Enter" });
        let sh = ui
            .ctx()
            .animate_bool_with_time(fid.with("sh"), sresp.hovered(), 0.12);
        ui.painter().circle_filled(
            send_r.center(),
            13.0,
            if has_text {
                w::lerp(t.accent, Color32::from_rgb(64, 156, 255), sh)
            } else {
                t.control_hover
            },
        );
        w::paint_icon(
            ui.painter(),
            Rect::from_center_size(send_r.center(), Vec2::splat(13.0)),
            Icon::ArrowUp,
            if has_text { Color32::WHITE } else { t.text3 },
        );
        if sresp.clicked() && has_text {
            send = true;
        }
        let mut stop_ui = ui.new_child(
            UiBuilder::new()
                .max_rect(stop_r)
                .layout(Layout::left_to_right(Align::Center)),
        );
        let ctrl_c = w::icon_button_sized(
            &mut stop_ui,
            Icon::Stop,
            if zh {
                "发送 Ctrl+C（中断）"
            } else {
                "Send Ctrl+C (interrupt)"
            },
            true,
            28.0,
            false,
        )
        .clicked();
        let mut snip_ui = ui.new_child(
            UiBuilder::new()
                .max_rect(snip_r)
                .layout(Layout::left_to_right(Align::Center)),
        );
        let snip_resp = w::icon_button_sized(
            &mut snip_ui,
            Icon::Bolt,
            if zh { "命令片段" } else { "Snippets" },
            true,
            28.0,
            false,
        );
        let is_group = badge.is_some();
        w::popover(ui, &snip_resp, fid.with("snippets"), 330.0, |ui| {
            self.ui_snippets_popover(ui, targets, fid, is_group);
        });

        // Text field
        let inner = Rect::from_min_max(
            Pos2::new(bar.left() + lead, bar.top() + 6.0),
            Pos2::new(snip_r.left() - 6.0, bar.bottom() - 6.0),
        );
        let mut child = ui.new_child(
            UiBuilder::new()
                .max_rect(inner)
                .layout(Layout::top_down(Align::Min)),
        );
        let hint = match (zh, badge.is_some()) {
            (true, false) => "输入命令，Enter 发送 · Shift+Enter 换行 · ↑↓ 历史",
            (false, false) => "Type a command · Enter to send · Shift+Enter newline · ↑↓ history",
            (true, true) => "输入命令，Enter 同时发送到所有选中的主机",
            (false, true) => "Type a command · Enter sends it to every selected host",
        };
        let te = egui::TextEdit::multiline(&mut self.cmd_input)
            .id(fid)
            .frame(false)
            .margin(egui::Margin::symmetric(4, 6))
            .desired_width(inner.width())
            .desired_rows(1)
            .font(egui::FontId::monospace(13.0))
            .hint_text(
                egui::RichText::new(hint)
                    .color(t.text3)
                    .font(w::regular(12.5)),
            );
        let r = child.add(te);
        if r.changed() {
            self.cmd_hist_pos = None;
        }

        // History
        if hist != 0 && !self.config.cmd_history.is_empty() {
            let n = self.config.cmd_history.len();
            let pos = match (self.cmd_hist_pos, hist) {
                (None, -1) => {
                    // Keep what was being typed; ↓ past the newest entry brings it back.
                    self.cmd_draft = self.cmd_input.clone();
                    Some(n - 1)
                }
                (Some(p), -1) => Some(p.saturating_sub(1)),
                (Some(p), 1) if p + 1 < n => Some(p + 1),
                (None, 1) => None,
                _ => None,
            };
            let leaving = self.cmd_hist_pos.is_some() && pos.is_none();
            let unchanged = self.cmd_hist_pos.is_none() && pos.is_none();
            self.cmd_hist_pos = pos;
            if !unchanged {
                self.cmd_input = match pos {
                    Some(p) => self.config.cmd_history[p].clone(),
                    None if leaving => std::mem::take(&mut self.cmd_draft),
                    None => String::new(),
                };
            }
            // caret to end
            let mut st = egui::text_edit::TextEditState::load(ui.ctx(), fid).unwrap_or_default();
            let end = egui::text::CCursor::new(self.cmd_input.chars().count());
            st.cursor
                .set_char_range(Some(egui::text::CCursorRange::one(end)));
            st.store(ui.ctx(), fid);
        }
        if esc {
            self.focus_term = true;
            ui.ctx().memory_mut(|m| m.surrender_focus(fid));
        }
        if ctrl_c {
            for &i in targets {
                self.send_bytes(i, vec![0x03]);
            }
        }
        if send {
            if targets.is_empty() {
                self.toast_msg(
                    self.t(
                        "请先选中至少一台在线主机",
                        "Select at least one online host",
                    ),
                    StatusKind::Info,
                );
                r.request_focus();
                return;
            }
            let text = std::mem::take(&mut self.cmd_input);
            for &i in targets {
                if let Some(bytes) = self
                    .sessions
                    .get(i)
                    .map(|slot| slot.term.encode_command(&text))
                {
                    self.send_bytes(i, bytes);
                }
                if let Some(slot) = self.sessions.get_mut(i) {
                    slot.term.scroll_to_bottom();
                }
            }
            let trimmed = text.trim();
            if !trimmed.is_empty()
                && self.config.cmd_history.last().map(|s| s.as_str()) != Some(trimmed)
            {
                self.config.cmd_history.push(trimmed.to_string());
                let len = self.config.cmd_history.len();
                if len > 300 {
                    self.config.cmd_history.drain(..len - 300);
                }
                self.persist();
            }
            self.cmd_hist_pos = None;
            r.request_focus();
        }
    }

    fn ui_find(&mut self, ui: &mut egui::Ui, idx: usize) {
        /// A long line cut down to the part around the match (char `at`).
        fn excerpt(line: &str, at: usize) -> String {
            const BEFORE: usize = 80;
            const MAX: usize = 300;
            let n = line.chars().count();
            if n <= MAX {
                return line.to_string();
            }
            let start = at.saturating_sub(BEFORE).min(n - MAX);
            let mut out = String::new();
            if start > 0 {
                out.push('…');
            }
            out.extend(line.chars().skip(start).take(MAX));
            if start + MAX < n {
                out.push('…');
            }
            out
        }
        let t = self.tok();
        let mut close = false;
        // Some(None): the whole log; Some(Some(hit)): one matching line.
        let mut copy_line: Option<Option<(u64, usize, String)>> = None;
        let mut save_log = false;
        let zh = self.zh();
        let Some(find) = self.find.as_mut() else {
            return;
        };
        ui.horizontal(|ui| {
            ui.set_height(w::CTRL_H);
            let id = egui::Id::new(("find_field", idx));
            let r = w::Field::new(&mut find.query)
                .icon(Icon::Search)
                .hint(if zh {
                    "在输出中查找"
                } else {
                    "Find in output"
                })
                .width(300.0)
                .clearable()
                .id(id)
                .show(ui);
            if find.focus {
                r.request_focus();
                find.focus = false;
            }
            if (r.has_focus() || r.lost_focus()) && ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                close = true;
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if w::icon_button(
                    ui,
                    Icon::Close,
                    if zh { "关闭  Esc" } else { "Close  Esc" },
                    true,
                )
                .clicked()
                {
                    close = true;
                }
                if w::button(
                    ui,
                    Kind::Plain,
                    None,
                    if zh { "保存日志" } else { "Save log" },
                    true,
                )
                .clicked()
                {
                    save_log = true;
                }
                if w::button(
                    ui,
                    Kind::Plain,
                    None,
                    if zh { "拷贝全部" } else { "Copy all" },
                    true,
                )
                .clicked()
                {
                    copy_line = Some(None);
                }
            });
        });
        let q = find.query.trim().to_lowercase();
        if !q.is_empty() {
            ui.add_space(6.0);
            let key = (
                q.clone(),
                self.sessions[idx].uid,
                self.sessions[idx].term.log_gen(),
            );
            // New output alone (same query and tab) re-runs the search at most
            // 4× a second, so a stream of output doesn't rescan the 2 MB log
            // every frame. A new query or tab is searched at once.
            let now = ui.input(|i| i.time);
            let stale = match find.cache.as_ref() {
                None => true,
                Some((k, _)) if k == &key => false,
                Some((k, _)) if k.0 == key.0 && k.1 == key.1 => {
                    if now - find.searched_at >= 0.25 {
                        true
                    } else {
                        ui.ctx()
                            .request_repaint_after(std::time::Duration::from_millis(260));
                        false
                    }
                }
                Some(_) => true,
            };
            if stale {
                find.searched_at = now;
                let log = self.sessions[idx].term.log();
                let base = self.sessions[idx].term.log_trimmed();
                let mut offset = 0usize;
                let all: Vec<(usize, &str, usize, usize)> = log
                    .split('\n')
                    .enumerate()
                    .filter_map(|(i, l)| {
                        let start = offset;
                        offset += l.len() + 1;
                        let lower = l.to_lowercase();
                        // Match position in characters (for the excerpt).
                        let at = lower.find(&q)?;
                        Some((i, l, lower[..at].chars().count(), start))
                    })
                    .collect();
                let n = all.len();
                let tail = all
                    .iter()
                    .skip(n.saturating_sub(500))
                    .map(|(i, l, at, start)| (*i, excerpt(l, *at), base + *start as u64, l.len()))
                    .collect();
                find.cache = Some((key, (n, tail)));
            }
            let (n, hits) = find.cache.as_ref().map(|c| &c.1).unwrap();
            let n = *n;
            egui::Frame::NONE
                .fill(if t.dark { t.card } else { t.chrome })
                .corner_radius(CornerRadius::same(10))
                .inner_margin(egui::Margin::symmetric(10, 8))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    let g = w::galley(
                        ui,
                        &if zh {
                            format!("{n} 处匹配 · 点按一行即可拷贝")
                        } else {
                            format!("{n} matches · click a line to copy")
                        },
                        w::regular(11.5),
                        t.text3,
                    );
                    let (r, _) = ui
                        .allocate_exact_size(Vec2::new(ui.available_width(), 18.0), Sense::hover());
                    ui.painter().galley(
                        Pos2::new(r.left(), r.center().y - g.size().y / 2.0),
                        g,
                        t.text3,
                    );
                    ui.spacing_mut().item_spacing.y = 0.0;
                    egui::ScrollArea::vertical()
                        .id_salt("find_hits")
                        .max_height((ui.ctx().screen_rect().height() * 0.26).max(110.0))
                        .stick_to_bottom(true)
                        .show_rows(ui, 20.0, hits.len(), |ui, range| {
                            for (ln, line, at, len) in &hits[range] {
                                let (r, resp) = ui.allocate_exact_size(
                                    Vec2::new(ui.available_width(), 20.0),
                                    Sense::click(),
                                );
                                if resp.hovered() {
                                    ui.painter()
                                        .rect_filled(r, CornerRadius::same(5), t.row_hover);
                                }
                                let p = ui.painter_at(r);
                                let g = w::galley(
                                    ui,
                                    &format!("{:>5}", ln + 1),
                                    egui::FontId::monospace(11.5),
                                    t.text3,
                                );
                                p.galley(
                                    Pos2::new(r.left() + 4.0, r.center().y - g.size().y / 2.0),
                                    g,
                                    t.text3,
                                );
                                w::paint_text_trunc(
                                    ui,
                                    &p,
                                    line,
                                    egui::FontId::monospace(12.0),
                                    t.text,
                                    r.left() + 52.0,
                                    r.center().y,
                                    r.width() - 56.0,
                                );
                                if resp.clicked() {
                                    copy_line = Some(Some((*at, *len, line.clone())));
                                }
                            }
                        });
                });
        }
        ui.add_space(8.0);
        if let Some(which) = copy_line {
            let log = self.sessions[idx].term.log();
            let text = match which {
                None => log.to_string(),
                // The whole line, not the excerpt shown in the list (unless
                // it has been trimmed from the log since).
                Some((at, len, shown)) => {
                    let trimmed = self.sessions[idx].term.log_trimmed();
                    at.checked_sub(trimmed)
                        .and_then(|s| log.get(s as usize..s as usize + len))
                        .map(str::to_string)
                        .unwrap_or(shown)
                }
            };
            ui.ctx().copy_text(text);
            self.toast_msg(self.t("已拷贝", "Copied"), StatusKind::Ok);
        }
        if save_log {
            self.save_log(idx);
        }
        if close {
            self.find = None;
            self.focus_term = true;
        }
    }

    fn save_log(&mut self, idx: usize) {
        let slot = &self.sessions[idx];
        let dir = self.download_dir();
        let _ = std::fs::create_dir_all(&dir);
        let host: String = slot
            .params
            .host
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '.' || c == '-' {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S");
        let path = dir.join(format!(
            "ssh-{}-{stamp}.log",
            if host.is_empty() {
                "session".into()
            } else {
                host
            }
        ));
        match std::fs::write(&path, slot.term.log()) {
            Ok(()) => {
                let msg = format!(
                    "{} {}",
                    self.t("日志已保存到", "Log saved to"),
                    path.display()
                );
                self.toast_msg(msg, StatusKind::Ok);
                native::reveal(&path);
            }
            Err(e) => self.toast_msg(format!("{e}"), StatusKind::Error),
        }
    }

    // ------------------------------------------------------------------
    // Monitor
    // ------------------------------------------------------------------
    pub(super) fn ui_monitor(&mut self, ui: &mut egui::Ui) {
        let t = self.tok();
        let idx = self.active.min(self.sessions.len() - 1);
        let (connected, mon, bound, remote, forward, connecting) = {
            let s = self.sessions[idx].shared.lock().unwrap();
            (
                s.connected,
                s.monitor.clone(),
                s.bound_local.clone(),
                s.remote_addr.clone(),
                s.forward.clone(),
                s.connecting,
            )
        };
        let params = self.sessions[idx].params.clone();
        let zh = self.zh();

        egui::ScrollArea::vertical()
            .id_salt("monitor")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                if !connected {
                    ui.add_space(ui.available_height() * 0.3);
                    ui.vertical_centered(|ui| {
                        let (r, _) = ui.allocate_exact_size(Vec2::splat(30.0), Sense::hover());
                        w::paint_icon(ui.painter(), r, Icon::Chart, t.text3);
                        ui.add_space(8.0);
                        let msg = if connecting {
                            self.t("正在连接…", "Connecting…")
                        } else {
                            self.t(
                                "连接后显示服务器状态",
                                "Server status appears\nonce connected",
                            )
                        };
                        let g = w::galley(ui, msg, w::regular(12.5), t.text3);
                        let (r, _) = ui.allocate_exact_size(g.size(), Sense::hover());
                        ui.painter().galley(r.min, g, t.text3);
                    });
                    return;
                }

                // Host header
                let host = mon.hostname.clone().unwrap_or_else(|| params.host.clone());
                let (hr, _) =
                    ui.allocate_exact_size(Vec2::new(ui.available_width(), 44.0), Sense::hover());
                let p = ui.painter_at(hr.expand(6.0));
                let name_w = w::paint_text_trunc(
                    ui,
                    &p,
                    &host,
                    w::semibold(16.0),
                    t.text,
                    hr.left() + 2.0,
                    hr.top() + 12.0,
                    hr.width() - 24.0,
                );
                w::status_dot(
                    ui.ctx(),
                    &p,
                    Pos2::new(hr.left() + 2.0 + name_w + 12.0, hr.top() + 12.0),
                    t.ok,
                    w::DotState::Live,
                );
                let sub = match (&mon.kernel, mon.uptime_secs) {
                    (Some(k), Some(u)) => format!(
                        "{k} · {} {}",
                        self.t("已运行", "up"),
                        format_duration(u, zh)
                    ),
                    (Some(k), None) => k.clone(),
                    _ => String::new(),
                };
                w::paint_text_trunc(
                    ui,
                    &p,
                    &sub,
                    w::regular(11.5),
                    t.text2,
                    hr.left() + 2.0,
                    hr.top() + 33.0,
                    hr.width() - 4.0,
                );
                ui.add_space(8.0);

                // Rings
                let width = ui.available_width();
                let ring = ((width - 48.0) / 3.0).min(60.0);
                let mut cpu = mon.cpu_percent;
                let mut mem = mon.mem_percent;
                let mut disk = mon.disk_percent;
                // Smoothly animate gauge values.
                for (v, key) in [(&mut cpu, "cpu"), (&mut mem, "mem"), (&mut disk, "disk")] {
                    if let Some(x) = v {
                        *x = w::ease_value(ui, egui::Id::new(("ring", key, idx)), *x, 0.6);
                    }
                }
                w::card(ui, |ui| {
                    ui.horizontal(|ui| {
                        let gap = ((ui.available_width() - ring * 3.0) / 2.0).max(0.0);
                        ui.spacing_mut().item_spacing.x = gap;
                        w::ring(ui, cpu, "CPU", pct_color(&t, mon.cpu_percent), ring);
                        w::ring(
                            ui,
                            mem,
                            self.t("内存", "Memory"),
                            pct_color(&t, mon.mem_percent),
                            ring,
                        );
                        w::ring(
                            ui,
                            disk,
                            self.t("磁盘", "Disk"),
                            pct_color(&t, mon.disk_percent),
                            ring,
                        );
                    });
                });

                // Trends
                w::card(ui, |ui| {
                    trend(
                        ui,
                        &t,
                        "CPU",
                        &mon.cpu_hist,
                        mon.cpu_percent.map(|c| format!("{c:.0}%")),
                        t.accent,
                    );
                    ui.add_space(6.0);
                    let mem_txt = match (mon.mem_used, mon.mem_total) {
                        (Some(u), Some(tt)) => {
                            Some(format!("{} / {}", format_bytes(u), format_bytes(tt)))
                        }
                        _ => None,
                    };
                    trend(
                        ui,
                        &t,
                        self.t("内存", "Memory"),
                        &mon.mem_hist,
                        mem_txt,
                        Color32::from_rgb(175, 82, 222),
                    );
                });

                // Details
                w::card(ui, |ui| {
                    ui.spacing_mut().item_spacing.y = 2.0;
                    net_row(ui, &t, mon.net_rx_rate, mon.net_tx_rate, zh);
                    w::kv_row(
                        ui,
                        self.t("负载", "Load"),
                        &mon.load_avg
                            .map(|l| format!("{:.2}   {:.2}   {:.2}", l[0], l[1], l[2]))
                            .unwrap_or_else(|| "–".into()),
                        false,
                    );
                    w::kv_row(
                        ui,
                        self.t("CPU 核数", "Cores"),
                        &mon.cpu_cores
                            .map(|c| c.to_string())
                            .unwrap_or_else(|| "–".into()),
                        false,
                    );
                    let disk_txt = match (mon.disk_used, mon.disk_total) {
                        (Some(u), Some(tt)) => {
                            format!("{} / {}", format_bytes(u), format_bytes(tt))
                        }
                        _ => "–".into(),
                    };
                    w::kv_row(ui, self.t("磁盘", "Disk"), &disk_txt, false);
                });
                if let Some(err) = &mon.error {
                    if mon.cpu_percent.is_none() && mon.mem_total.is_none() {
                        let g = w::galley(ui, err, w::regular(11.5), t.warn);
                        let (r, _) = ui.allocate_exact_size(g.size(), Sense::hover());
                        ui.painter().galley(r.min, g, t.warn);
                    }
                }

                // Connection path
                ui.add_space(6.0);
                w::caption(ui, self.t("连接路径", "Connection path"));
                w::card(ui, |ui| {
                    ui.spacing_mut().item_spacing.y = 2.0;
                    let mode = match params.bind_mode {
                        BindMode::Auto => self.t("自动", "Auto").to_string(),
                        BindMode::Addr => params.bind_addr.clone(),
                        BindMode::Iface => params.bind_iface.clone(),
                    };
                    w::kv_row(ui, self.t("出站", "Bind"), &mode, false);
                    w::kv_row(
                        ui,
                        self.t("本地", "Local"),
                        bound.as_deref().unwrap_or("–"),
                        true,
                    );
                    w::kv_row(
                        ui,
                        self.t("远端", "Remote"),
                        remote.as_deref().unwrap_or("–"),
                        true,
                    );
                    if !forward.is_empty() {
                        w::kv_row(ui, self.t("转发", "Forward"), &forward, true);
                    }
                });
            });
    }

    // ------------------------------------------------------------------
    // Dialogs & toast
    // ------------------------------------------------------------------
    pub(super) fn ui_dialogs(&mut self, ctx: &egui::Context) {
        self.ui_editor(ctx);
        self.ui_password_prompt(ctx);
        self.ui_confirm(ctx);
        self.ui_text_prompt(ctx);
        self.ui_hostkey_dialog(ctx);
        self.ui_shortcuts(ctx);
        self.ui_file_editor(ctx);
        self.ui_chmod_prompt(ctx);
        self.ui_snippet_editor(ctx);
        self.ui_batch(ctx);
        // Last, so it stacks above whichever sheet opened it.
        self.ui_picker(ctx);
    }

    /// Title + message block used by alert-style sheets.
    fn sheet_header(&self, ui: &mut egui::Ui, icon: Icon, tint: Color32, title: &str, body: &str) {
        let t = self.tok();
        ui.vertical_centered(|ui| {
            icon_tile(
                ui,
                icon,
                tint.gamma_multiply(if t.dark { 0.25 } else { 0.14 }),
                tint,
                52.0,
            );
            ui.add_space(14.0);
            let g = w::galley(ui, title, w::semibold(16.0), t.text);
            let (r, _) = ui.allocate_exact_size(g.size(), Sense::hover());
            ui.painter().galley(r.min, g, t.text);
            if !body.is_empty() {
                ui.add_space(6.0);
                ui.add(
                    egui::Label::new(egui::RichText::new(body).size(12.5).color(t.text2)).wrap(),
                );
            }
        });
        ui.add_space(18.0);
    }

    /// Two equal-width buttons (secondary, primary). Returns (left, right) clicked.
    fn sheet_buttons(
        &self,
        ui: &mut egui::Ui,
        left: &str,
        right: &str,
        right_kind: Kind,
    ) -> (bool, bool) {
        let bw = (ui.available_width() - 10.0) / 2.0;
        let mut out = (false, false);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 10.0;
            out.0 = w::button_sized(ui, Kind::Secondary, None, left, true, bw).clicked();
            out.1 = w::button_sized(ui, right_kind, None, right, true, bw).clicked();
        });
        out
    }

    fn ui_password_prompt(&mut self, ctx: &egui::Context) {
        if self.pw_prompt.is_none() {
            if let Some((params, tab_uid)) = self.pw_queue.pop_front() {
                self.pw_prompt = Some(PasswordPrompt {
                    params,
                    password: String::new(),
                    remember: false,
                    focus: true,
                    tab_uid,
                });
            }
        }
        let Some(mut pp) = self.pw_prompt.take() else {
            return;
        };
        let t = self.tok();
        let mut submit = false;
        let mut cancel = false;
        let resp = egui::Modal::new(egui::Id::new("pw_prompt"))
            .frame(sheet_frame(&t))
            .show(ctx, |ui| {
                ui.set_width(320.0);
                let who = format!("{}@{}:{}", pp.params.user, pp.params.host, pp.params.port);
                self.sheet_header(
                    ui,
                    Icon::Key,
                    t.accent,
                    self.t("输入密码", "Enter password"),
                    &who,
                );
                let r = w::Field::new(&mut pp.password)
                    .password(true)
                    .hint(self.t("密码", "Password"))
                    .id(egui::Id::new("pw_field"))
                    .show(ui);
                if pp.focus {
                    r.request_focus();
                    pp.focus = false;
                }
                if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    submit = true;
                }
                if pp.params.profile_id.is_some() {
                    ui.add_space(8.0);
                    w::switch(
                        ui,
                        &mut pp.remember,
                        self.t("记住密码", "Remember password"),
                    );
                }
                ui.add_space(18.0);
                let (c, s) = self.sheet_buttons(
                    ui,
                    self.t("取消", "Cancel"),
                    self.t("连接", "Connect"),
                    Kind::Primary,
                );
                cancel |= c;
                submit |= s;
            });
        if resp.should_close() {
            cancel = true;
        }
        if submit && !pp.password.is_empty() {
            let mut params = pp.params;
            params.password = pp.password;
            // Saved only once the server accepts it (see `poll_sessions`).
            params.remember_password = pp.remember && params.profile_id.is_some();
            let idx = match pp.tab_uid.and_then(|u| self.slot_index(u)) {
                Some(i) => i,
                None => {
                    if self.active_slot().is_fresh() {
                        self.active
                    } else if let Some(i) = self.sessions.iter().position(|s| s.is_fresh()) {
                        self.switch_tab(i);
                        i
                    } else {
                        self.new_tab()
                    }
                }
            };
            let uid = self.sessions[idx].uid;
            self.connect_params(params, false, Some(uid));
        } else if !cancel {
            self.pw_prompt = Some(pp);
        }
    }

    fn ui_confirm(&mut self, ctx: &egui::Context) {
        let Some(c) = self.confirm.take() else {
            return;
        };
        let t = self.tok();
        let zh = self.zh();
        let (title, body, action) = match &c {
            Confirm::DeleteHost(id) => {
                let name = self
                    .config
                    .profiles
                    .iter()
                    .find(|p| &p.id == id)
                    .map(|p| p.display_label())
                    .unwrap_or_default();
                (
                    if zh {
                        format!("删除「{name}」？")
                    } else {
                        format!("Delete \"{name}\"?")
                    },
                    self.t("此操作无法撤销。", "This can't be undone.")
                        .to_string(),
                    self.t("删除", "Delete"),
                )
            }
            Confirm::DeleteFolder(name) => (
                if zh {
                    format!("删除分组「{name}」？")
                } else {
                    format!("Delete folder \"{name}\"?")
                },
                self.t("分组内的主机不会被删除。", "Hosts inside are kept.")
                    .to_string(),
                self.t("删除", "Delete"),
            ),
            Confirm::DeleteFiles { items, .. } => (
                match (zh, items.len()) {
                    (true, 1) => format!("删除「{}」？", items[0].0),
                    (true, n) => format!("删除这 {n} 项？"),
                    (false, 1) => format!("Delete \"{}\"?", items[0].0),
                    (false, n) => format!("Delete {n} items?"),
                },
                if items.iter().any(|(_, d)| *d) {
                    self.t(
                        "将从服务器永久删除，文件夹会连同其中的所有内容一起删除。",
                        "They'll be permanently removed from the server, folders with everything inside.",
                    )
                } else {
                    self.t(
                        "将从服务器永久删除。",
                        "They'll be permanently removed from the server.",
                    )
                }
                .to_string(),
                self.t("删除", "Delete"),
            ),
            Confirm::Quit(what) => (
                self.t("退出 Tern？", "Quit Tern?").to_string(),
                if zh {
                    format!("{what}。退出后连接会断开，未保存的修改会丢失。")
                } else {
                    let mut s = what.clone();
                    if let Some(c) = s.get_mut(..1) {
                        c.make_ascii_uppercase();
                    }
                    format!("{s}. Quitting closes the connections and drops unsaved changes.")
                },
                self.t("退出", "Quit"),
            ),
            Confirm::CloseTab(uid) => (
                match self.slot_index(*uid).map(|i| self.sessions[i].title.clone()) {
                    Some(name) if self.zh() => format!("关闭会话“{name}”？"),
                    Some(name) => format!("Close “{name}”?"),
                    None => self.t("关闭这个会话？", "Close this session?").to_string(),
                },
                self.t(
                    "连接仍在进行中，关闭后会断开。",
                    "The connection is still active and will be closed.",
                )
                .to_string(),
                self.t("关闭", "Close"),
            ),
        };
        let mut yes = false;
        let mut no = false;
        let resp = egui::Modal::new(egui::Id::new("confirm"))
            .frame(sheet_frame(&t))
            .show(ctx, |ui| {
                ui.set_width(300.0);
                self.sheet_header(ui, Icon::Warning, t.err, &title, &body);
                let (a, b) = self.sheet_buttons(
                    ui,
                    self.t("取消", "Cancel"),
                    action,
                    Kind::DestructiveFilled,
                );
                no |= a;
                yes |= b;
            });
        if resp.should_close() {
            no = true;
        }
        if yes {
            match c {
                Confirm::DeleteHost(id) => self.delete_profile(&id),
                Confirm::DeleteFolder(name) => self.delete_folder(&name),
                Confirm::DeleteFiles {
                    tab_uid,
                    dir,
                    items,
                } => {
                    if let Some(i) = self.slot_index(tab_uid) {
                        self.delete_items(i, dir, items);
                    }
                }
                Confirm::CloseTab(uid) => {
                    if let Some(i) = self.slot_index(uid) {
                        self.close_tab(i);
                    }
                }
                Confirm::Quit(_) => {
                    self.quit_confirmed = true;
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
            }
        } else if !no {
            self.confirm = Some(c);
        }
    }

    fn ui_text_prompt(&mut self, ctx: &egui::Context) {
        let Some(mut tp) = self.text_prompt.take() else {
            return;
        };
        let t = self.tok();
        let (title, hint, icon) = match &tp {
            TextPrompt::NewFolder(_) => (
                self.t("新建分组", "New folder"),
                self.t("分组名称", "Folder name"),
                Icon::Folder,
            ),
            TextPrompt::RenameFolder { .. } => (
                self.t("重命名分组", "Rename folder"),
                self.t("名称", "Name"),
                Icon::Pencil,
            ),
            TextPrompt::NewRemoteFile { .. } => (
                self.t("新建文件", "New file"),
                self.t("文件名，例如 main.py", "File name, e.g. main.py"),
                Icon::File,
            ),
            TextPrompt::NewRemoteDir { .. } => (
                self.t("新建文件夹", "New folder"),
                self.t("文件夹名称", "Folder name"),
                Icon::Folder,
            ),
            TextPrompt::RenameRemote { .. } => (
                self.t("重命名", "Rename"),
                self.t("名称", "Name"),
                Icon::Pencil,
            ),
        };
        let mut ok = false;
        let mut cancel = false;
        let resp = egui::Modal::new(egui::Id::new("text_prompt"))
            .frame(sheet_frame(&t))
            .show(ctx, |ui| {
                ui.set_width(300.0);
                self.sheet_header(ui, icon, t.accent, title, "");
                let buf = match &mut tp {
                    TextPrompt::NewFolder(s) => s,
                    TextPrompt::RenameFolder { to, .. } => to,
                    TextPrompt::NewRemoteDir { name, .. } => name,
                    TextPrompt::NewRemoteFile { name, .. } => name,
                    TextPrompt::RenameRemote { to, .. } => to,
                };
                let fid = egui::Id::new("text_prompt_field");
                let r = w::Field::new(buf).hint(hint).id(fid).show(ui);
                if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    ok = true;
                } else if !r.lost_focus() && !ui.ctx().memory(|m| m.focused().is_some()) {
                    r.request_focus();
                }
                ui.add_space(18.0);
                let (a, b) = self.sheet_buttons(
                    ui,
                    self.t("取消", "Cancel"),
                    self.t("好", "OK"),
                    Kind::Primary,
                );
                cancel |= a;
                ok |= b;
            });
        if resp.should_close() {
            cancel = true;
        }
        if ok {
            match tp {
                TextPrompt::NewFolder(name) => self.add_folder(&name),
                TextPrompt::RenameFolder { from, to } => self.rename_folder(&from, &to),
                TextPrompt::NewRemoteDir { tab_uid, dir, name } => {
                    if let Some(i) = self.slot_index(tab_uid) {
                        self.mkdir_remote(i, &dir, &name);
                    }
                }
                TextPrompt::NewRemoteFile { tab_uid, dir, name } => {
                    if let Some(i) = self.slot_index(tab_uid) {
                        self.create_remote_file(i, &dir, &name);
                    }
                }
                TextPrompt::RenameRemote {
                    tab_uid,
                    dir,
                    from,
                    to,
                } => {
                    if let Some(i) = self.slot_index(tab_uid) {
                        if from != to.trim() {
                            self.rename_remote(i, &dir, &from, &to);
                        }
                    }
                }
            }
        } else if !cancel {
            self.text_prompt = Some(tp);
        }
    }

    fn ui_hostkey_dialog(&mut self, ctx: &egui::Context) {
        let Some(prompt) = self.hostkey_prompt.clone() else {
            return;
        };
        let t = self.tok();
        let mut decided: Option<bool> = None;
        // New host, or a new key type for a known one: ask, no alarm.
        let first = prompt.previous.is_none() || prompt.other_type;
        egui::Modal::new(egui::Id::new("hostkey")).frame(sheet_frame(&t)).show(ctx, |ui| {
            ui.set_width(400.0);
            let title = format!(
                "{}  {}:{}",
                if prompt.other_type {
                    self.t("新的密钥类型", "New key type")
                } else if first {
                    self.t("首次连接", "New host")
                } else {
                    self.t("主机密钥已变化", "Host key changed")
                },
                prompt.host,
                prompt.port
            );
            let (icon, tint, body) = if prompt.other_type {
                (
                    Icon::Key,
                    t.accent,
                    self.t(
                        "这台服务器出示了一种之前没保存过的主机密钥类型，通常是服务器新增了密钥或升级了 SSH。如果能拿到服务器的指纹，请核对后再信任；信任后两种密钥都会记住。",
                        "This server shows a type of host key that wasn't saved for it before — usually it added a key or upgraded SSH. If you can, compare the fingerprint with the server's own before trusting it. Both keys are then remembered.",
                    ),
                )
            } else if first {
                (
                    Icon::Key,
                    t.accent,
                    self.t(
                        "第一次连接这台服务器。如果能拿到服务器的指纹（如 ssh-keygen -lf /etc/ssh/ssh_host_*_key.pub），请先核对一致再信任；信任后会记住它，以后密钥变化时会提醒你。",
                        "This is the first connection to this server. If you can, compare the fingerprint with the server's own (ssh-keygen -lf /etc/ssh/ssh_host_*_key.pub) before trusting it. Tern remembers it and warns you if it ever changes.",
                    ),
                )
            } else {
                (
                    Icon::Warning,
                    t.warn,
                    self.t(
                        "服务器的身份与上次不同。可能是重装了系统，也可能有人在拦截连接。只有确认是预期变化时才信任。",
                        "The server's identity differs from last time. It may have been reinstalled — or the connection may be intercepted. Trust it only if you expected this.",
                    ),
                )
            };
            self.sheet_header(ui, icon, tint, &title, body);
            egui::Frame::NONE
                .fill(t.control)
                .corner_radius(CornerRadius::same(10))
                .inner_margin(egui::Margin::symmetric(12, 10))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    if let Some(old) = &prompt.previous {
                        let before = if prompt.other_type {
                            self.t("已保存的密钥", "Saved key")
                        } else {
                            self.t("之前", "Before")
                        };
                        ui.label(egui::RichText::new(before).size(11.0).color(t.text3));
                        ui.add(egui::Label::new(egui::RichText::new(old).monospace().size(11.5).color(t.text2)).wrap());
                        ui.add_space(6.0);
                    }
                    let now_label = if first {
                        format!("{} ({})", self.t("指纹", "Fingerprint"), prompt.algorithm)
                    } else {
                        format!("{} ({})", self.t("现在", "Now"), prompt.algorithm)
                    };
                    ui.label(egui::RichText::new(now_label).size(11.0).color(t.text3));
                    ui.add(egui::Label::new(egui::RichText::new(&prompt.fingerprint).monospace().size(11.5).color(t.text)).wrap());
                });
            ui.add_space(18.0);
            let bw = (ui.available_width() - 10.0) / 2.0;
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 10.0;
                if first {
                    if w::button_sized(ui, Kind::Secondary, None, self.t("取消", "Cancel"), true, bw).clicked() {
                        decided = Some(false);
                    }
                    if w::button_sized(ui, Kind::Primary, None, self.t("信任并连接", "Trust and connect"), true, bw).clicked() {
                        decided = Some(true);
                    }
                } else {
                    if w::button_sized(ui, Kind::Destructive, None, self.t("信任新密钥", "Trust new key"), true, bw).clicked() {
                        decided = Some(true);
                    }
                    if w::button_sized(ui, Kind::Primary, None, self.t("拒绝连接", "Don't connect"), true, bw).clicked() {
                        decided = Some(false);
                    }
                }
            });
        });
        if let Some(d) = decided {
            *prompt.decision.lock().unwrap() = Some(d);
            self.hostkey_prompt = None;
        }
    }

    fn ui_shortcuts(&mut self, ctx: &egui::Context) {
        if !self.show_shortcuts {
            return;
        }
        let t = self.tok();
        let mut close = false;
        let resp = egui::Modal::new(egui::Id::new("shortcuts"))
            .frame(sheet_frame(&t))
            .show(ctx, |ui| {
                ui.set_width(380.0);
                let g = w::galley(
                    ui,
                    self.t("键盘快捷键", "Keyboard shortcuts"),
                    w::semibold(16.0),
                    t.text,
                );
                let (r, _) =
                    ui.allocate_exact_size(Vec2::new(ui.available_width(), 26.0), Sense::hover());
                ui.painter().galley(
                    Pos2::new(r.left(), r.center().y - g.size().y / 2.0),
                    g,
                    t.text,
                );
                ui.add_space(10.0);
                for (k, zh, en) in [
                    ("Ctrl+Shift+T", "新建标签", "New tab"),
                    ("Ctrl+Shift+W", "关闭标签", "Close tab"),
                    ("Ctrl+Tab", "下一个标签", "Next tab"),
                    ("Ctrl+Shift+N", "新建主机", "New host"),
                    (
                        "Ctrl+Shift+G",
                        "切换会话 / 群控",
                        "Sessions / group control",
                    ),
                    ("Ctrl+Shift+F", "在输出中查找", "Find in output"),
                    ("Ctrl+Shift+C / V", "拷贝 / 粘贴", "Copy / paste"),
                    (
                        "Ctrl+Shift+= / −",
                        "放大 / 缩小字号",
                        "Bigger / smaller text",
                    ),
                    ("Ctrl + 滚轮", "调整字号", "Adjust text size"),
                    (
                        "Ctrl + 点击链接",
                        "打开终端中的链接",
                        "Open link in terminal",
                    ),
                    (
                        "Shift + 拖动",
                        "在 vim / htop 中选择文字",
                        "Select text in vim / htop",
                    ),
                ] {
                    let (r, _) = ui
                        .allocate_exact_size(Vec2::new(ui.available_width(), 28.0), Sense::hover());
                    let g = w::galley(ui, self.t(zh, en), w::regular(13.0), t.text);
                    ui.painter().galley(
                        Pos2::new(r.left(), r.center().y - g.size().y / 2.0),
                        g,
                        t.text,
                    );
                    let kg = w::galley(
                        ui,
                        if k.contains("滚") && !self.zh() {
                            "Ctrl + wheel"
                        } else if k.contains("链接") && !self.zh() {
                            "Ctrl + click"
                        } else if k.contains("拖") && !self.zh() {
                            "Shift + drag"
                        } else {
                            k
                        },
                        egui::FontId::monospace(12.0),
                        t.text2,
                    );
                    let kr = Rect::from_min_size(
                        Pos2::new(r.right() - kg.size().x - 16.0, r.center().y - 11.0),
                        Vec2::new(kg.size().x + 16.0, 22.0),
                    );
                    ui.painter()
                        .rect_filled(kr, CornerRadius::same(6), t.control);
                    ui.painter()
                        .galley(kr.center() - kg.size() / 2.0, kg, t.text2);
                }
                ui.add_space(14.0);
                if w::button_sized(
                    ui,
                    Kind::Secondary,
                    None,
                    self.t("完成", "Done"),
                    true,
                    ui.available_width(),
                )
                .clicked()
                {
                    close = true;
                }
            });
        if resp.should_close() || close {
            self.show_shortcuts = false;
        }
    }

    pub(super) fn ui_toast(&mut self, ctx: &egui::Context) {
        let Some((msg, kind, start, until)) = self.toast.clone() else {
            return;
        };
        let now = ctx.input(|i| i.time);
        if now > until {
            self.toast = None;
            return;
        }
        let appear = ((now - start) / 0.2).clamp(0.0, 1.0) as f32;
        let vanish = ((until - now) / 0.3).clamp(0.0, 1.0) as f32;
        let a = appear.min(vanish);
        let ease = 1.0 - (1.0 - a).powi(3);
        ctx.request_repaint();
        let t = self.tok();
        let (icon, ic) = match kind {
            StatusKind::Ok => (Icon::Check, t.ok),
            StatusKind::Error => (Icon::Warning, t.err),
            StatusKind::Busy => (Icon::Warning, t.warn),
            StatusKind::Info => (Icon::Check, t.accent),
        };
        let screen = ctx.screen_rect();
        let painter = ctx.layer_painter(egui::LayerId::new(
            egui::Order::Tooltip,
            egui::Id::new("toast"),
        ));
        let font = w::regular(13.0);
        let mut job = egui::text::LayoutJob::single_section(
            msg,
            egui::TextFormat::simple(font, Color32::WHITE),
        );
        job.wrap = egui::text::TextWrapping::truncate_at_width(520.0);
        let g = ctx.fonts(|f| f.layout_job(job));
        let size = Vec2::new(g.size().x + 54.0, 40.0);
        let center = Pos2::new(
            screen.center().x,
            screen.bottom() - 54.0 + (1.0 - ease) * 14.0,
        );
        let r = Rect::from_center_size(center, size);
        painter.rect_filled(
            r.translate(Vec2::new(0.0, 4.0)),
            CornerRadius::same(20),
            Color32::from_black_alpha((50.0 * ease) as u8),
        );
        painter.rect_filled(
            r,
            CornerRadius::same(20),
            Color32::from_rgba_unmultiplied(30, 30, 32, (235.0 * ease) as u8),
        );
        w::paint_icon(
            &painter,
            Rect::from_center_size(Pos2::new(r.left() + 22.0, r.center().y), Vec2::splat(15.0)),
            icon,
            ic.gamma_multiply(ease),
        );
        painter.galley_with_override_text_color(
            Pos2::new(r.left() + 38.0, r.center().y - g.size().y / 2.0),
            g,
            Color32::WHITE.gamma_multiply(ease),
        );
    }
}

// ----------------------------------------------------------------------
// Drawing helpers
// ----------------------------------------------------------------------

fn pct_color(t: &theme::Tokens, pct: Option<f32>) -> Color32 {
    match pct {
        Some(p) if p >= 90.0 => t.err,
        Some(p) if p >= 75.0 => t.warn,
        Some(_) => t.accent,
        None => t.text3,
    }
}

fn net_row(ui: &mut egui::Ui, t: &theme::Tokens, rx: Option<f64>, tx: Option<f64>, zh: bool) {
    let (r, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 22.0), Sense::hover());
    let p = ui.painter_at(r);
    let kg = w::galley(
        ui,
        if zh { "网络" } else { "Network" },
        w::regular(12.0),
        t.text2,
    );
    p.galley(
        Pos2::new(r.left(), r.center().y - kg.size().y / 2.0),
        kg,
        t.text2,
    );
    let fmt = |v: Option<f64>| v.map(format_rate).unwrap_or_else(|| "–".into());
    let txg = w::galley(ui, &fmt(tx), w::regular(12.0), t.text);
    let rxg = w::galley(ui, &fmt(rx), w::regular(12.0), t.text);
    let mut x = r.right() - txg.size().x;
    p.galley(Pos2::new(x, r.center().y - txg.size().y / 2.0), txg, t.text);
    x -= 14.0;
    w::paint_icon(
        &p,
        Rect::from_center_size(Pos2::new(x + 5.0, r.center().y), Vec2::splat(10.0)),
        Icon::ArrowUp,
        t.accent,
    );
    x -= 12.0 + rxg.size().x;
    p.galley(Pos2::new(x, r.center().y - rxg.size().y / 2.0), rxg, t.text);
    x -= 14.0;
    w::paint_icon(
        &p,
        Rect::from_center_size(Pos2::new(x + 5.0, r.center().y), Vec2::splat(10.0)),
        Icon::ArrowDown,
        t.ok,
    );
}

/// Label + value on one line, smooth area chart below.
fn trend(
    ui: &mut egui::Ui,
    t: &theme::Tokens,
    label: &str,
    hist: &VecDeque<f32>,
    value: Option<String>,
    color: Color32,
) {
    let (hr, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 20.0), Sense::hover());
    let p = ui.painter_at(hr);
    let lg = w::galley(ui, label, w::regular(12.0), t.text2);
    p.galley(
        Pos2::new(hr.left(), hr.center().y - lg.size().y / 2.0),
        lg,
        t.text2,
    );
    let vg = w::galley(
        ui,
        value.as_deref().unwrap_or("–"),
        w::regular(12.0),
        t.text,
    );
    p.galley(
        Pos2::new(hr.right() - vg.size().x, hr.center().y - vg.size().y / 2.0),
        vg,
        t.text,
    );

    let (r, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 38.0), Sense::hover());
    let p = ui.painter_at(r);
    if hist.len() < 2 {
        p.line_segment(
            [r.left_bottom(), r.right_bottom()],
            Stroke::new(1.0_f32, t.separator),
        );
        return;
    }
    // Stretch the available history across the full width.
    let n = (hist.len() - 1).max(1) as f32;
    let pts: Vec<Pos2> = hist
        .iter()
        .enumerate()
        .map(|(i, v)| {
            Pos2::new(
                r.left() + r.width() * (i as f32 / n),
                r.bottom() - 1.0 - (r.height() - 3.0) * (v / 100.0).clamp(0.0, 1.0),
            )
        })
        .collect();
    for wnd in pts.windows(2) {
        let quad = vec![
            wnd[0],
            wnd[1],
            Pos2::new(wnd[1].x, r.bottom()),
            Pos2::new(wnd[0].x, r.bottom()),
        ];
        p.add(egui::Shape::convex_polygon(
            quad,
            color.gamma_multiply(0.14),
            Stroke::NONE,
        ));
    }
    p.add(egui::Shape::line(pts.clone(), Stroke::new(1.6_f32, color)));
    if let Some(last) = pts.last() {
        p.circle_filled(*last, 2.6, color);
    }
}
