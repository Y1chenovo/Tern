//! Reusable command snippets popover and editor modal.

use super::panels::sheet_frame;
use super::widgets::{self as w, Icon, Kind};
use super::*;
use egui::{Align, CornerRadius, Layout, Pos2, Rect, Sense, Stroke, UiBuilder, Vec2};
use std::collections::BTreeMap;

pub(super) struct SnippetEditor {
    pub id: Option<String>,
    pub name: String,
    pub folder: String,
    pub command: String,
    pub send_enter: bool,
    pub error: String,
    pub focus_first: bool,
}

impl SnippetEditor {
    fn blank(prefill_command: String) -> Self {
        Self {
            id: None,
            name: String::new(),
            folder: String::new(),
            command: prefill_command,
            send_enter: true,
            error: String::new(),
            focus_first: true,
        }
    }

    fn from_snippet(s: &profiles::Snippet) -> Self {
        Self {
            id: Some(s.id.clone()),
            name: s.name.clone(),
            folder: s.folder.clone(),
            command: s.command.clone(),
            send_enter: s.send_enter,
            error: String::new(),
            focus_first: true,
        }
    }
}

impl GuiApp {
    fn add_sample_snippets(&mut self) {
        let zh = self.zh();
        let samples = [
            ("查看磁盘", "Disk usage", "df -h", "系统", "System", true),
            (
                "查看内存",
                "Memory usage",
                "free -h",
                "系统",
                "System",
                true,
            ),
            (
                "监听端口",
                "Listening ports",
                "ss -tulpn",
                "网络",
                "Network",
                true,
            ),
            (
                "容器列表",
                "Docker containers",
                "docker ps",
                "容器",
                "Docker",
                true,
            ),
        ];
        for (n_zh, n_en, cmd, f_zh, f_en, enter) in samples {
            self.config.snippets.push(profiles::Snippet {
                id: profiles::new_id(),
                name: if zh { n_zh } else { n_en }.into(),
                folder: if zh { f_zh } else { f_en }.into(),
                command: cmd.into(),
                send_enter: enter,
            });
        }
        self.persist();
    }

    pub(super) fn ui_snippets_popover(
        &mut self,
        ui: &mut egui::Ui,
        targets: &[usize],
        cmdbar_fid: egui::Id,
        is_group: bool,
    ) {
        let t = self.tok();
        let zh = self.zh();
        ui.spacing_mut().item_spacing.y = 6.0;

        ui.horizontal(|ui| {
            ui.set_height(w::CTRL_H);
            let g = w::galley(
                ui,
                if zh { "命令片段" } else { "Snippets" },
                w::semibold(14.0),
                t.text,
            );
            let (r, _) = ui.allocate_exact_size(g.size(), Sense::hover());
            ui.painter().galley(r.min, g, t.text);
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if w::button(
                    ui,
                    Kind::Secondary,
                    Some(Icon::Plus),
                    if zh { "新建" } else { "New" },
                    true,
                )
                .clicked()
                {
                    let prefill = self.cmd_input.trim().to_string();
                    self.snippet_editor = Some(SnippetEditor::blank(prefill));
                    ui.memory_mut(|m| m.close_popup());
                }
            });
        });

        w::Field::new(&mut self.snippet_filter)
            .icon(Icon::Search)
            .hint(if zh {
                "搜索命令片段"
            } else {
                "Search snippets"
            })
            .clearable()
            .id(cmdbar_fid.with("snip_filter"))
            .show(ui);

        if self.config.snippets.is_empty() {
            ui.add_space(10.0);
            ui.vertical_centered(|ui| {
                let g = w::galley(
                    ui,
                    if zh {
                        "还没有保存常用命令"
                    } else {
                        "No saved snippets yet"
                    },
                    w::regular(12.5),
                    t.text2,
                );
                let (r, _) = ui.allocate_exact_size(g.size(), Sense::hover());
                ui.painter().galley(r.min, g, t.text2);
                ui.add_space(6.0);
                if w::button(
                    ui,
                    Kind::Secondary,
                    Some(Icon::Bolt),
                    if zh {
                        "添加常用示例"
                    } else {
                        "Add samples"
                    },
                    true,
                )
                .clicked()
                {
                    self.add_sample_snippets();
                }
            });
            ui.add_space(8.0);
            return;
        }

        let q = self.snippet_filter.trim().to_lowercase();
        let mut root = Vec::new();
        let mut by_folder: BTreeMap<String, Vec<usize>> = BTreeMap::new();
        for (i, s) in self.config.snippets.iter().enumerate() {
            let hit = q.is_empty()
                || s.name.to_lowercase().contains(&q)
                || s.command.to_lowercase().contains(&q)
                || s.folder.to_lowercase().contains(&q);
            if !hit {
                continue;
            }
            if s.folder.trim().is_empty() {
                root.push(i);
            } else {
                by_folder
                    .entry(s.folder.trim().to_string())
                    .or_default()
                    .push(i);
            }
        }

        let mut run_idx: Option<usize> = None;
        let mut edit_idx: Option<usize> = None;

        if root.is_empty() && by_folder.is_empty() {
            ui.add_space(12.0);
            ui.vertical_centered(|ui| {
                let g = w::galley(
                    ui,
                    if zh {
                        "没有匹配的命令片段"
                    } else {
                        "No matching snippets"
                    },
                    w::regular(12.5),
                    t.text3,
                );
                let (r, _) = ui.allocate_exact_size(g.size(), Sense::hover());
                ui.painter().galley(r.min, g, t.text3);
            });
            ui.add_space(10.0);
        } else {
            let snippets_snap = self.config.snippets.clone();
            egui::ScrollArea::vertical()
                .id_salt(cmdbar_fid.with("snippets_scroll"))
                .max_height(260.0)
                .auto_shrink([false, true])
                .show(ui, |ui| {
                    ui.spacing_mut().item_spacing.y = 2.0;
                    let draw_row = |ui: &mut egui::Ui,
                                    idx: usize,
                                    run: &mut Option<usize>,
                                    edit: &mut Option<usize>| {
                        let s = &snippets_snap[idx];
                        let wdt = ui.available_width();
                        let (rect, resp) =
                            ui.allocate_exact_size(Vec2::new(wdt, 44.0), Sense::click());
                        let edit_r = Rect::from_center_size(
                            Pos2::new(rect.right() - 16.0, rect.center().y),
                            Vec2::splat(24.0),
                        );
                        let hovered_edit = ui.rect_contains_pointer(edit_r) && resp.hovered();
                        let h =
                            ui.ctx()
                                .animate_bool_with_time(resp.id.with("h"), resp.hovered(), 0.1);
                        ui.painter().rect_filled(
                            rect,
                            CornerRadius::same(7),
                            t.row_hover.gamma_multiply(h),
                        );

                        let tag = if !s.send_enter {
                            Some(if zh { "草稿" } else { "Draft" })
                        } else {
                            None
                        };
                        let right_pad = if resp.hovered() {
                            32.0
                        } else if tag.is_some() {
                            42.0
                        } else {
                            10.0
                        };
                        let max_w = (rect.width() - 12.0 - right_pad).max(40.0);
                        let pr = ui.painter_at(rect);
                        w::paint_text_trunc(
                            ui,
                            &pr,
                            &s.name,
                            w::semibold(12.5),
                            t.text,
                            rect.left() + 8.0,
                            rect.center().y - 8.0,
                            max_w,
                        );
                        let single_line_cmd = s.command.replace(['\r', '\n'], " ");
                        w::paint_text_trunc(
                            ui,
                            &pr,
                            &single_line_cmd,
                            egui::FontId::monospace(11.0),
                            t.text2,
                            rect.left() + 8.0,
                            rect.center().y + 9.0,
                            max_w,
                        );
                        if resp.hovered() {
                            let mut eui = ui.new_child(
                                UiBuilder::new()
                                    .max_rect(edit_r)
                                    .layout(Layout::left_to_right(Align::Center)),
                            );
                            if w::icon_button_sized(
                                &mut eui,
                                Icon::Pencil,
                                if zh { "编辑" } else { "Edit" },
                                true,
                                24.0,
                                false,
                            )
                            .clicked()
                            {
                                *edit = Some(idx);
                            } else if resp.clicked() && !hovered_edit {
                                *run = Some(idx);
                            }
                        } else {
                            if let Some(lbl) = tag {
                                let g = w::galley(ui, lbl, w::regular(10.5), t.text3);
                                let mid = w::optical_mid(ui, &g);
                                ui.painter().galley(
                                    Pos2::new(
                                        rect.right() - g.size().x - 8.0,
                                        rect.center().y - mid,
                                    ),
                                    g,
                                    t.text3,
                                );
                            }
                            if resp.clicked() {
                                *run = Some(idx);
                            }
                        }
                    };

                    for i in root {
                        draw_row(ui, i, &mut run_idx, &mut edit_idx);
                    }
                    for (folder, idxs) in by_folder {
                        w::caption(ui, &folder);
                        for i in idxs {
                            draw_row(ui, i, &mut run_idx, &mut edit_idx);
                        }
                    }
                });
        }

        if let Some(i) = edit_idx {
            if let Some(s) = self.config.snippets.get(i) {
                self.snippet_editor = Some(SnippetEditor::from_snippet(s));
            }
            ui.memory_mut(|m| m.close_popup());
            return;
        }
        if let Some(i) = run_idx {
            if let Some(s) = self.config.snippets.get(i).cloned() {
                if s.send_enter {
                    if targets.is_empty() {
                        self.toast_msg(
                            self.t(
                                "请先选中至少一台在线主机",
                                "Select at least one online host",
                            ),
                            StatusKind::Info,
                        );
                    } else {
                        let mut bytes = s
                            .command
                            .replace("\r\n", "\n")
                            .replace('\n', "\r")
                            .into_bytes();
                        if !bytes.ends_with(b"\r") {
                            bytes.push(b'\r');
                        }
                        for &slot_idx in targets {
                            self.send_bytes(slot_idx, bytes.clone());
                            if let Some(slot) = self.sessions.get_mut(slot_idx) {
                                slot.term.scroll_to_bottom();
                            }
                        }
                        if is_group {
                            let msg = if zh {
                                format!("已发送「{}」到 {} 台主机", s.name, targets.len())
                            } else {
                                format!("Sent \"{}\" to {} hosts", s.name, targets.len())
                            };
                            self.toast_msg(msg, StatusKind::Ok);
                        }
                        ui.memory_mut(|m| m.close_popup());
                    }
                } else {
                    self.cmd_input = s.command;
                    self.cmd_hist_pos = None;
                    let mut st = egui::text_edit::TextEditState::load(ui.ctx(), cmdbar_fid)
                        .unwrap_or_default();
                    let end = egui::text::CCursor::new(self.cmd_input.chars().count());
                    st.cursor
                        .set_char_range(Some(egui::text::CCursorRange::one(end)));
                    st.store(ui.ctx(), cmdbar_fid);
                    ui.ctx().memory_mut(|m| m.request_focus(cmdbar_fid));
                    ui.memory_mut(|m| m.close_popup());
                }
            }
        }
    }

    pub(super) fn ui_snippet_editor(&mut self, ctx: &egui::Context) {
        let Some(mut ed) = self.snippet_editor.take() else {
            return;
        };
        let t = self.tok();
        let zh = self.zh();
        let mut save = false;
        let mut delete = false;
        let mut cancel = false;

        let resp = egui::Modal::new(egui::Id::new("snippet_editor"))
            .frame(sheet_frame(&t))
            .show(ctx, |ui| {
                ui.set_width(420.0);
                let title = match (ed.id.is_some(), zh) {
                    (false, true) => "新建命令片段",
                    (false, false) => "New snippet",
                    (true, true) => "编辑命令片段",
                    (true, false) => "Edit snippet",
                };
                let g = w::galley(ui, title, w::semibold(16.0), t.text);
                let (r, _) =
                    ui.allocate_exact_size(Vec2::new(ui.available_width(), 26.0), Sense::hover());
                ui.painter().galley(
                    Pos2::new(r.left(), r.center().y - g.size().y / 2.0),
                    g,
                    t.text,
                );
                ui.add_space(10.0);

                w::caption(ui, if zh { "名称" } else { "NAME" });
                let r_name = w::Field::new(&mut ed.name)
                    .hint(if zh {
                        "例如 查看端口"
                    } else {
                        "e.g. Open ports"
                    })
                    .id(egui::Id::new("snip_name"))
                    .show(ui);
                if ed.focus_first {
                    r_name.request_focus();
                    ed.focus_first = false;
                }
                ui.add_space(8.0);

                w::caption(
                    ui,
                    if zh {
                        "分组（可选）"
                    } else {
                        "FOLDER (OPTIONAL)"
                    },
                );
                w::Field::new(&mut ed.folder)
                    .icon(Icon::Folder)
                    .hint(if zh {
                        "例如 系统 / Docker"
                    } else {
                        "e.g. System / Docker"
                    })
                    .id(egui::Id::new("snip_folder"))
                    .show(ui);
                ui.add_space(8.0);

                w::caption(ui, if zh { "命令内容" } else { "COMMAND" });
                egui::Frame::NONE
                    .fill(t.control)
                    .stroke(Stroke::new(1.0_f32, t.separator))
                    .corner_radius(CornerRadius::same(8))
                    .inner_margin(egui::Margin::symmetric(8, 6))
                    .show(ui, |ui| {
                        ui.add(
                            egui::TextEdit::multiline(&mut ed.command)
                                .id(egui::Id::new("snip_cmd"))
                                .frame(false)
                                .desired_width(ui.available_width())
                                .desired_rows(3)
                                .font(egui::FontId::monospace(12.5)),
                        );
                    });
                ui.add_space(10.0);

                w::switch(
                    ui,
                    &mut ed.send_enter,
                    if zh {
                        "点击后立即执行（自动发送回车）"
                    } else {
                        "Run immediately on click (press Enter)"
                    },
                );

                if !ed.error.is_empty() {
                    ui.add_space(8.0);
                    ui.add(
                        egui::Label::new(egui::RichText::new(&ed.error).size(12.0).color(t.err))
                            .wrap(),
                    );
                }

                ui.add_space(16.0);
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 8.0;
                    if ed.id.is_some()
                        && w::button(
                            ui,
                            Kind::Destructive,
                            Some(Icon::Trash),
                            if zh { "删除" } else { "Delete" },
                            true,
                        )
                        .clicked()
                    {
                        delete = true;
                    }
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        ui.spacing_mut().item_spacing.x = 8.0;
                        if w::button(
                            ui,
                            Kind::Primary,
                            None,
                            if zh { "保存" } else { "Save" },
                            true,
                        )
                        .clicked()
                        {
                            save = true;
                        }
                        if w::button(
                            ui,
                            Kind::Secondary,
                            None,
                            if zh { "取消" } else { "Cancel" },
                            true,
                        )
                        .clicked()
                        {
                            cancel = true;
                        }
                    });
                });
            });

        if resp.should_close() || cancel {
            return;
        }
        if delete {
            if let Some(id) = ed.id {
                self.config.snippets.retain(|s| s.id != id);
                self.persist();
            }
            return;
        }
        if save {
            let name = ed.name.trim().to_string();
            let cmd = ed.command.trim_end_matches(['\r', '\n']).to_string();
            if name.is_empty() {
                ed.error = if zh {
                    "请填写名称".into()
                } else {
                    "Enter a name".into()
                };
                self.snippet_editor = Some(ed);
                return;
            }
            if cmd.trim().is_empty() {
                ed.error = if zh {
                    "请填写命令内容".into()
                } else {
                    "Enter a command".into()
                };
                self.snippet_editor = Some(ed);
                return;
            }
            let snippet = profiles::Snippet {
                id: ed.id.clone().unwrap_or_else(profiles::new_id),
                name,
                folder: ed.folder.trim().to_string(),
                command: cmd,
                send_enter: ed.send_enter,
            };
            if let Some(existing) = ed
                .id
                .as_deref()
                .and_then(|id| self.config.snippets.iter_mut().find(|s| s.id == id))
            {
                *existing = snippet;
            } else {
                self.config.snippets.push(snippet);
            }
            self.persist();
            return;
        }
        self.snippet_editor = Some(ed);
    }
}
