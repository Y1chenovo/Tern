//! Left sidebar (host library) and the host editor modal.

use super::panels::sheet_frame;
use super::widgets::{self as w, Icon, Kind};
use super::*;

use egui::{Align, Color32, CornerRadius, Layout, Pos2, Rect, Sense, Stroke, UiBuilder, Vec2};
use std::collections::BTreeSet;

const ROW_H: f32 = 46.0;

/// Sidebar data derived from the host list (see `with_host_cache`).
pub(super) struct HostCache {
    key: u64,
    /// Frame the key was last checked in (checked once per frame).
    pass: u64,
    /// Indices into `config.profiles`, most recently used first.
    order: Vec<usize>,
    /// Per profile (same index): lower-case text the search box matches.
    haystack: Vec<String>,
    folders: Arc<Vec<String>>,
}

impl GuiApp {
    pub(super) fn folder_names(&self) -> Arc<Vec<String>> {
        self.with_host_cache(|c| Arc::clone(&c.folders))
    }

    /// Run `f` on the sidebar cache, rebuilding it first if the hosts or
    /// folders changed since it was built (a cheap hash, no sorting or
    /// string building on ordinary frames).
    fn with_host_cache<T>(&self, f: impl FnOnce(&HostCache) -> T) -> T {
        use std::hash::{Hash, Hasher};
        let pass = self.ctx.cumulative_pass_nr();
        {
            let mut cache = self.host_cache.borrow_mut();
            if let Some(c) = cache.as_mut().filter(|c| c.pass == pass) {
                return f(c);
            }
        }
        let mut h = std::collections::hash_map::DefaultHasher::new();
        for p in &self.config.profiles {
            (
                &p.id,
                p.last_connected,
                &p.name,
                &p.user,
                &p.host,
                &p.folder,
            )
                .hash(&mut h);
        }
        self.config.folders.hash(&mut h);
        let key = h.finish();
        let mut cache = self.host_cache.borrow_mut();
        if cache.as_ref().map(|c| c.key) != Some(key) {
            // Sort keys are built once per profile, not once per comparison.
            let mut rows: Vec<(u64, String, usize)> = self
                .config
                .profiles
                .iter()
                .enumerate()
                .map(|(i, p)| {
                    (
                        p.last_connected.unwrap_or(0),
                        p.display_label().to_lowercase(),
                        i,
                    )
                })
                .collect();
            rows.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
            let mut set: BTreeSet<String> = BTreeSet::new();
            for f in &self.config.folders {
                if !f.trim().is_empty() {
                    set.insert(f.clone());
                }
            }
            for p in &self.config.profiles {
                if !p.folder.trim().is_empty() {
                    set.insert(p.folder.clone());
                }
            }
            let haystack = self
                .config
                .profiles
                .iter()
                .map(|p| {
                    format!(
                        "{}\n{}\n{}\n{}",
                        p.display_label(),
                        p.host,
                        p.user,
                        p.folder
                    )
                    .to_lowercase()
                })
                .collect();
            *cache = Some(HostCache {
                key,
                pass,
                order: rows.into_iter().map(|(_, _, i)| i).collect(),
                haystack,
                folders: Arc::new(set.into_iter().collect()),
            });
        }
        let c = cache.as_mut().expect("built above");
        c.pass = pass;
        f(c)
    }

    /// (id) sorted: most recently connected first, then by name; only hosts
    /// matching `pred` and the (lower-case) search `filter`.
    fn sorted_ids(&self, filter: &str, pred: impl Fn(&profiles::Profile) -> bool) -> Vec<String> {
        self.sorted_rows(filter, pred)
            .into_iter()
            .map(|(_, id)| id)
            .collect()
    }

    /// Like [`Self::sorted_ids`], with each host's index in `config.profiles`.
    fn sorted_rows(
        &self,
        filter: &str,
        pred: impl Fn(&profiles::Profile) -> bool,
    ) -> Vec<(usize, String)> {
        self.with_host_cache(|c| {
            c.order
                .iter()
                .filter_map(|&i| {
                    let p = self.config.profiles.get(i)?;
                    let hit =
                        filter.is_empty() || c.haystack.get(i).is_some_and(|h| h.contains(filter));
                    (hit && pred(p)).then(|| (i, p.id.clone()))
                })
                .collect()
        })
    }

    /// Live state of every saved host with a tab, worked out once per frame
    /// (instead of scanning all tabs for each sidebar row).
    fn live_states(&self) -> std::collections::HashMap<String, StatusKind> {
        let mut out = std::collections::HashMap::new();
        for slot in &self.sessions {
            let Some(id) = slot.params.profile_id.as_ref() else {
                continue;
            };
            let (connecting, connected) = slot.state();
            let st = if connected {
                StatusKind::Ok
            } else if connecting {
                StatusKind::Busy
            } else {
                continue;
            };
            let e = out.entry(id.clone()).or_insert(st);
            if st == StatusKind::Ok {
                *e = StatusKind::Ok;
            }
        }
        out
    }

    pub(super) fn recent_ids(&self, n: usize) -> Vec<String> {
        let mut ids = self.sorted_ids("", |_| true);
        ids.truncate(n);
        ids
    }

    fn ui_sidebar_header(&mut self, ui: &mut egui::Ui) {
        let t = self.tok();
        ui.horizontal(|ui| {
            ui.set_height(w::CTRL_H);
            let g = w::galley(ui, self.t("主机", "Hosts"), w::semibold(17.0), t.text);
            let (r, _) = ui.allocate_exact_size(g.size(), Sense::hover());
            ui.painter().galley(r.min, g, t.text);
            let n = self.config.profiles.len();
            if n > 0 {
                let g = w::galley(ui, &n.to_string(), w::regular(12.0), t.text3);
                let (r, _) = ui.allocate_exact_size(g.size(), Sense::hover());
                ui.painter().galley(r.min + Vec2::new(0.0, 2.0), g, t.text3);
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let add = w::icon_button(ui, Icon::Plus, self.t("新建", "New"), true);
                w::popover(ui, &add, egui::Id::new("add_popover"), 215.0, |ui| {
                    if w::menu_item(ui, self.t("新建主机", "New host"), "Ctrl+Shift+N", false) {
                        self.editor = Some(self.blank_editor());
                        ui.memory_mut(|m| m.close_popup());
                    }
                    if w::menu_item(ui, self.t("新建分组", "New folder"), "", false) {
                        self.text_prompt = Some(TextPrompt::NewFolder(String::new()));
                        ui.memory_mut(|m| m.close_popup());
                    }
                    w::menu_separator(ui);
                    if w::menu_item(
                        ui,
                        self.t("从 ~/.ssh/config 导入", "Import ~/.ssh/config"),
                        "",
                        false,
                    ) {
                        self.import_ssh_config();
                        ui.memory_mut(|m| m.close_popup());
                    }
                });
            });
        });
    }

    pub(super) fn ui_sidebar(&mut self, ui: &mut egui::Ui) {
        let t = self.tok();
        self.ui_sidebar_header(ui);
        ui.add_space(10.0);
        let hint = self.t("搜索", "Search");
        w::Field::new(&mut self.host_filter)
            .icon(Icon::Search)
            .hint(hint)
            .clearable()
            .show(ui);
        ui.add_space(8.0);

        let filter = self.host_filter.trim().to_lowercase();
        let folders = self.folder_names();
        let live = self.live_states();
        let favorites = self.sorted_rows(&filter, |p| p.favorite);
        // One pass for "no folder" and every folder's hosts.
        let mut root: Vec<(usize, String)> = Vec::new();
        let mut by_folder: std::collections::HashMap<String, Vec<(usize, String)>> =
            std::collections::HashMap::new();
        for (i, id) in self.sorted_rows(&filter, |_| true) {
            let folder = &self.config.profiles[i].folder;
            if folder.trim().is_empty() {
                root.push((i, id));
            } else {
                by_folder.entry(folder.clone()).or_default().push((i, id));
            }
        }

        egui::ScrollArea::vertical()
            .id_salt("host_tree")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 2.0;
                if self.config.profiles.is_empty() && folders.is_empty() {
                    ui.add_space(48.0);
                    ui.vertical_centered(|ui| {
                        let (r, _) = ui.allocate_exact_size(Vec2::splat(28.0), Sense::hover());
                        w::paint_icon(ui.painter(), r, Icon::Server, t.text3);
                        ui.add_space(10.0);
                        let g = w::galley(
                            ui,
                            self.t("还没有主机", "No hosts yet"),
                            w::regular(13.0),
                            t.text2,
                        );
                        let (r, _) = ui.allocate_exact_size(g.size(), Sense::hover());
                        ui.painter().galley(r.min, g, t.text2);
                        ui.add_space(12.0);
                        if w::button(
                            ui,
                            Kind::Secondary,
                            Some(Icon::Plus),
                            self.t("新建主机", "New host"),
                            true,
                        )
                        .clicked()
                        {
                            self.editor = Some(self.blank_editor());
                        }
                    });
                    return;
                }
                let any = !favorites.is_empty() || !root.is_empty() || !folders.is_empty();
                if !any
                    || (!filter.is_empty()
                        && favorites.is_empty()
                        && root.is_empty()
                        && by_folder.is_empty())
                {
                    ui.add_space(24.0);
                    ui.vertical_centered(|ui| {
                        let g = w::galley(
                            ui,
                            self.t("没有匹配的主机", "No matching hosts"),
                            w::regular(12.5),
                            t.text3,
                        );
                        let (r, _) = ui.allocate_exact_size(g.size(), Sense::hover());
                        ui.painter().galley(r.min, g, t.text3);
                    });
                    return;
                }

                if !favorites.is_empty() {
                    w::caption(ui, self.t("收藏", "Favorites"));
                    for (i, id) in &favorites {
                        self.host_row(ui, *i, id, live.get(id).copied(), "fav", 0.0);
                    }
                    ui.add_space(6.0);
                }
                if !root.is_empty() {
                    if !favorites.is_empty() || !folders.is_empty() {
                        w::caption(ui, self.t("全部", "All hosts"));
                    }
                    for (i, id) in &root {
                        self.host_row(ui, *i, id, live.get(id).copied(), "root", 0.0);
                    }
                    ui.add_space(6.0);
                }
                for folder in folders.iter() {
                    let kids = by_folder.remove(folder).unwrap_or_default();
                    if !filter.is_empty()
                        && kids.is_empty()
                        && !folder.to_lowercase().contains(&filter)
                    {
                        continue;
                    }
                    let open = self.folder_header(ui, folder, kids.len());
                    if open {
                        for (i, id) in &kids {
                            self.host_row(ui, *i, id, live.get(id).copied(), "folder", 14.0);
                        }
                    }
                }
            });
    }

    /// Collapsible folder header. Returns whether it is open.
    fn folder_header(&mut self, ui: &mut egui::Ui, folder: &str, count: usize) -> bool {
        let t = self.tok();
        let state_id = egui::Id::new(("folder_open", folder));
        let mut open = ui.data_mut(|d| *d.get_persisted_mut_or(state_id, true));
        let (rect, resp) =
            ui.allocate_exact_size(Vec2::new(ui.available_width(), 30.0), Sense::click());
        let h = ui
            .ctx()
            .animate_bool_with_time(resp.id.with("h"), resp.hovered(), 0.12);
        ui.painter()
            .rect_filled(rect, CornerRadius::same(8), t.row_hover.gamma_multiply(h));
        let rot = ui
            .ctx()
            .animate_bool_with_time(state_id.with("rot"), open, 0.15);
        // chevron: interpolate between right (closed) and down (open)
        let c = Pos2::new(rect.left() + 13.0, rect.center().y);
        let ang = rot * std::f32::consts::FRAC_PI_2;
        let rotp = |x: f32, y: f32| {
            let (s, co) = ang.sin_cos();
            Pos2::new(c.x + x * co - y * s, c.y + x * s + y * co)
        };
        ui.painter().add(egui::Shape::line(
            vec![rotp(-1.8, -3.6), rotp(1.8, 0.0), rotp(-1.8, 3.6)],
            Stroke::new(1.4_f32, t.text3),
        ));
        w::paint_icon(
            ui.painter(),
            Rect::from_center_size(
                Pos2::new(rect.left() + 32.0, rect.center().y),
                Vec2::splat(15.0),
            ),
            Icon::Folder,
            t.text2,
        );
        w::paint_text_trunc(
            ui,
            &ui.painter_at(rect),
            folder,
            w::semibold(12.5),
            t.text,
            rect.left() + 46.0,
            rect.center().y,
            rect.width() - 80.0,
        );
        let g = w::galley(ui, &count.to_string(), w::regular(11.5), t.text3);
        ui.painter().galley(
            Pos2::new(
                rect.right() - 10.0 - g.size().x,
                rect.center().y - g.size().y / 2.0,
            ),
            g,
            t.text3,
        );
        if resp.clicked() {
            open = !open;
            ui.data_mut(|d| d.insert_persisted(state_id, open));
        }
        let folder_c = folder.to_string();
        resp.context_menu(|ui| {
            ui.set_width(210.0);
            if w::menu_item(ui, self.t("重命名…", "Rename…"), "", false) {
                self.text_prompt = Some(TextPrompt::RenameFolder {
                    from: folder_c.clone(),
                    to: folder_c.clone(),
                });
                ui.close_menu();
            }
            if w::menu_item(ui, self.t("删除分组", "Delete folder"), "", true) {
                self.confirm = Some(Confirm::DeleteFolder(folder_c.clone()));
                ui.close_menu();
            }
        });
        open
    }

    fn host_row(
        &mut self,
        ui: &mut egui::Ui,
        idx: usize,
        id: &str,
        live: Option<StatusKind>,
        section: &str,
        indent: f32,
    ) {
        let t = self.tok();
        // `idx` is where the host was when the list was built; an earlier
        // row's action this frame may have moved it.
        let p = match self.config.profiles.get(idx).filter(|p| p.id == id) {
            Some(p) => p,
            None => match self.config.profiles.iter().find(|p| p.id == id) {
                Some(p) => p,
                None => return,
            },
        };
        let label = p.display_label();
        let sub = if p.port == 22 {
            format!("{}@{}", p.user, p.host)
        } else {
            format!("{}@{}:{}", p.user, p.host, p.port)
        };
        let favorite = p.favorite;
        let selected = self.selected_profile_id.as_deref() == Some(id);

        let (full, _) =
            ui.allocate_exact_size(Vec2::new(ui.available_width(), ROW_H), Sense::hover());
        let rect = Rect::from_min_max(full.min + Vec2::new(indent, 0.0), full.max);
        let resp = ui.interact(
            rect,
            ui.id().with(("host_row", id, section)),
            Sense::click(),
        );
        let hovered = resp.hovered() || ui.rect_contains_pointer(rect);
        let h = ui
            .ctx()
            .animate_bool_with_time(resp.id.with("h"), hovered, 0.12);
        let s = ui
            .ctx()
            .animate_bool_with_time(resp.id.with("s"), selected, 0.15);
        let painter = ui.painter();
        painter.rect_filled(
            rect,
            CornerRadius::same(9),
            t.row_hover.gamma_multiply(h * (1.0 - s)),
        );
        painter.rect_filled(rect, CornerRadius::same(9), t.selected.gamma_multiply(s));

        let dot_c = match live {
            Some(StatusKind::Ok) => t.ok,
            Some(_) => t.warn,
            None => t.text3,
        };
        if live.is_some() {
            w::status_dot(
                ui.ctx(),
                painter,
                Pos2::new(rect.left() + 14.0, rect.center().y),
                dot_c,
                if live == Some(StatusKind::Ok) {
                    w::DotState::Live
                } else {
                    w::DotState::Busy
                },
            );
        }

        let right_reserved = if hovered {
            60.0
        } else if favorite {
            26.0
        } else {
            10.0
        };
        let text_left = rect.left() + 28.0;
        let max_w = rect.right() - right_reserved - text_left;
        let pr = ui.painter_at(rect);
        w::paint_text_trunc(
            ui,
            &pr,
            &label,
            w::regular(13.0),
            t.text,
            text_left,
            rect.center().y - 8.0,
            max_w,
        );
        w::paint_text_trunc(
            ui,
            &pr,
            &sub,
            w::regular(11.5),
            t.text2,
            text_left,
            rect.center().y + 9.0,
            max_w,
        );

        let mut clicked_action = false;
        if hovered {
            let mut bar = ui.new_child(
                UiBuilder::new()
                    .max_rect(Rect::from_min_max(
                        Pos2::new(rect.right() - 58.0, rect.top()),
                        Pos2::new(rect.right() - 4.0, rect.bottom()),
                    ))
                    .layout(Layout::right_to_left(Align::Center)),
            );
            bar.spacing_mut().item_spacing.x = 2.0;
            if w::icon_button_sized(
                &mut bar,
                Icon::Play,
                self.t("连接", "Connect"),
                true,
                26.0,
                false,
            )
            .clicked()
            {
                self.connect_profile(id, false);
                clicked_action = true;
            }
            if w::icon_button_sized(
                &mut bar,
                Icon::Pencil,
                self.t("编辑", "Edit"),
                true,
                26.0,
                false,
            )
            .clicked()
            {
                self.open_editor(id);
                clicked_action = true;
            }
        } else if favorite {
            w::paint_icon(
                ui.painter(),
                Rect::from_center_size(
                    Pos2::new(rect.right() - 14.0, rect.center().y),
                    Vec2::splat(10.0),
                ),
                Icon::StarFill,
                Color32::from_rgb(255, 189, 46).gamma_multiply(0.9),
            );
        }

        if !clicked_action {
            if resp.double_clicked() {
                self.connect_profile(id, false);
            } else if resp.clicked() {
                self.selected_profile_id = Some(id.to_string());
            }
        }

        resp.context_menu(|ui| {
            // Only built while the menu is open (not for every row, every frame).
            let folders = self.folder_names();
            ui.set_width(210.0);
            if w::menu_item(ui, self.t("连接", "Connect"), "", false) {
                self.connect_profile(id, false);
                ui.close_menu();
            }
            if w::menu_item(
                ui,
                self.t("在新标签中连接", "Connect in new tab"),
                "",
                false,
            ) {
                self.connect_profile(id, true);
                ui.close_menu();
            }
            w::menu_separator(ui);
            if w::menu_item(ui, self.t("编辑…", "Edit…"), "", false) {
                self.open_editor(id);
                ui.close_menu();
            }
            if w::menu_item(ui, self.t("复制主机", "Duplicate"), "", false) {
                self.duplicate_profile(id);
                ui.close_menu();
            }
            if w::menu_item(
                ui,
                if favorite {
                    self.t("取消收藏", "Remove from favorites")
                } else {
                    self.t("加入收藏", "Add to favorites")
                },
                "",
                false,
            ) {
                self.toggle_favorite(id);
                ui.close_menu();
            }
            if w::menu_item(ui, self.t("拷贝 ssh 命令", "Copy ssh command"), "", false) {
                if let Some(p) = self.config.profiles.iter().find(|p| p.id == id).cloned() {
                    let mut parts = vec!["ssh".to_string()];
                    if p.port != 22 {
                        parts.push("-p".into());
                        parts.push(p.port.to_string());
                    }
                    let id_path = p.identity.trim();
                    if !id_path.is_empty() {
                        parts.push("-i".into());
                        if id_path.contains(char::is_whitespace) {
                            parts.push(format!("\"{}\"", id_path.replace('"', "\\\"")));
                        } else {
                            parts.push(id_path.to_string());
                        }
                    }
                    if p.bind_mode == profiles::BindModePref::Addr && !p.bind_addr.trim().is_empty()
                    {
                        parts.push("-b".into());
                        parts.push(p.bind_addr.trim().to_string());
                    }
                    if let Some((listen, rh, rp)) =
                        parse_forward(&p.forward_local, &p.forward_remote)
                    {
                        let rhost = if rh.contains(':') {
                            format!("[{rh}]")
                        } else {
                            rh
                        };
                        parts.push("-L".into());
                        parts.push(format!("{listen}:{rhost}:{rp}"));
                    }
                    let host = if p.host.contains(':') && !p.host.starts_with('[') {
                        format!("[{}]", p.host)
                    } else {
                        p.host.clone()
                    };
                    parts.push(format!("{}@{host}", p.user));
                    ui.ctx().copy_text(parts.join(" "));
                    self.toast_msg(
                        self.t("已拷贝 ssh 命令", "Copied ssh command"),
                        StatusKind::Ok,
                    );
                }
                ui.close_menu();
            }
            if !folders.is_empty() {
                let current = self
                    .config
                    .profiles
                    .iter()
                    .find(|p| p.id == id)
                    .map(|p| p.folder.clone())
                    .unwrap_or_default();
                w::menu_separator(ui);
                w::menu_caption(ui, self.t("移到分组", "Move to"));
                if w::menu_item(
                    ui,
                    self.t("无分组", "No folder"),
                    if current.is_empty() { "✓" } else { "" },
                    false,
                ) {
                    self.move_profile(id, "");
                    ui.close_menu();
                }
                for f in folders.iter() {
                    if w::menu_item(ui, f, if &current == f { "✓" } else { "" }, false) {
                        self.move_profile(id, f);
                        ui.close_menu();
                    }
                }
            }
            w::menu_separator(ui);
            if w::menu_item(ui, self.t("删除…", "Delete…"), "", true) {
                self.confirm = Some(Confirm::DeleteHost(id.to_string()));
                ui.close_menu();
            }
        });
    }

    fn move_profile(&mut self, id: &str, folder: &str) {
        if let Some(p) = self.config.profiles.iter_mut().find(|p| p.id == id) {
            p.folder = folder.to_string();
        }
        self.persist();
    }

    fn toggle_favorite(&mut self, id: &str) {
        if let Some(p) = self.config.profiles.iter_mut().find(|p| p.id == id) {
            p.favorite = !p.favorite;
        }
        self.persist();
    }

    fn duplicate_profile(&mut self, id: &str) {
        let Some(mut copy) = self.config.profiles.iter().find(|p| p.id == id).cloned() else {
            return;
        };
        let fresh = profiles::Profile::new_blank("copy");
        copy.id = fresh.id;
        copy.name = format!("{} {}", copy.display_label(), self.t("副本", "copy"));
        copy.favorite = false;
        copy.last_connected = None;
        // A copy must have its credentials entered or saved again explicitly.
        copy.password_obfuscated = None;
        copy.key_passphrase_obfuscated = None;
        copy.remember_password = false;
        copy.remember_key_passphrase = false;
        let new_id = copy.id.clone();
        self.config.profiles.push(copy);
        self.persist();
        self.open_editor(&new_id);
    }

    pub(super) fn delete_profile(&mut self, id: &str) {
        if let Some(mut p) = self.config.profiles.iter().find(|p| p.id == id).cloned() {
            self.credentials
                .submit(id.to_string(), move || p.forget_secrets(), |_, _| {});
        }
        self.config.profiles.retain(|p| p.id != id);
        if self.selected_profile_id.as_deref() == Some(id) {
            self.selected_profile_id = None;
        }
        self.persist();
        self.toast_msg(self.t("已删除主机", "Host deleted"), StatusKind::Info);
    }

    pub(super) fn add_folder(&mut self, name: &str) {
        let name = name.trim().to_string();
        if name.is_empty() {
            return;
        }
        if !self.config.folders.contains(&name) {
            self.config.folders.push(name);
            self.config.folders.sort();
            self.persist();
        }
    }

    pub(super) fn import_ssh_config(&mut self) {
        let zh = self.zh();
        let Some(path) = profiles::default_ssh_config_path() else {
            self.toast_msg(
                self.t("无法定位 ~/.ssh/config", "Could not locate ~/.ssh/config"),
                StatusKind::Error,
            );
            return;
        };
        if !path.exists() {
            self.toast_msg(
                self.t("未找到 ~/.ssh/config", "~/.ssh/config not found"),
                StatusKind::Info,
            );
            return;
        }
        let parsed = match profiles::load_ssh_config(&path) {
            Ok(p) => p,
            Err(e) => {
                self.toast_msg(
                    format!(
                        "{}: {e:#}",
                        self.t("读取 ~/.ssh/config 失败", "Failed to read ~/.ssh/config")
                    ),
                    StatusKind::Error,
                );
                return;
            }
        };
        if parsed.is_empty() {
            self.toast_msg(
                self.t(
                    "~/.ssh/config 中没有可导入的主机",
                    "No importable hosts found in ~/.ssh/config",
                ),
                StatusKind::Info,
            );
            return;
        }
        let (added, skipped) = profiles::merge_imported_profiles(&mut self.config.profiles, parsed);
        if added > 0 {
            self.add_folder("SSH Config");
            self.persist();
            let msg = match (zh, skipped > 0) {
                (true, true) => format!("已导入 {added} 台主机（跳过 {skipped} 台重复）"),
                (true, false) => format!("已导入 {added} 台主机"),
                (false, true) => format!("Imported {added} hosts ({skipped} duplicates skipped)"),
                (false, false) => format!("Imported {added} hosts"),
            };
            self.toast_msg(msg, StatusKind::Ok);
        } else {
            let msg = if zh {
                format!("没有新主机可导入（已跳过 {skipped} 台重复）")
            } else {
                format!("No new hosts to import ({skipped} duplicates skipped)")
            };
            self.toast_msg(msg, StatusKind::Info);
        }
    }

    pub(super) fn rename_folder(&mut self, from: &str, to: &str) {
        let to = to.trim();
        if to.is_empty() || from == to {
            return;
        }
        for f in &mut self.config.folders {
            if f == from {
                *f = to.to_string();
            }
        }
        if !self.config.folders.iter().any(|f| f == to) {
            self.config.folders.push(to.to_string());
        }
        self.config.folders.sort();
        self.config.folders.dedup();
        for p in &mut self.config.profiles {
            if p.folder == from {
                p.folder = to.to_string();
            }
        }
        self.persist();
    }

    pub(super) fn delete_folder(&mut self, name: &str) {
        self.config.folders.retain(|f| f != name);
        for p in &mut self.config.profiles {
            if p.folder == name {
                p.folder.clear();
            }
        }
        self.persist();
    }

    // ------------------------------------------------------------------
    // Host editor
    // ------------------------------------------------------------------

    pub(super) fn blank_editor(&self) -> HostEditor {
        HostEditor {
            id: None,
            tab: EditorTab::General,
            name: String::new(),
            folder: String::new(),
            host: String::new(),
            port: "22".into(),
            user: "root".into(),
            identity: String::new(),
            password: String::new(),
            remember_password: false,
            key_passphrase: String::new(),
            remember_key_passphrase: false,
            show_password: false,
            dns_server: String::new(),
            forward_local: String::new(),
            forward_remote: String::new(),
            bind_mode: BindMode::Auto,
            bind_addr: String::new(),
            selected_candidate: self
                .candidates
                .iter()
                .position(|c| !c.is_tunnel)
                .unwrap_or(0),
            error: String::new(),
            focus_first: true,
            found_keys: Arc::new(default_keys()),
            password_unreadable: false,
            passphrase_unreadable: false,
            credentials_loading: false,
        }
    }

    pub(super) fn open_editor(&mut self, id: &str) {
        if self.credentials_busy(id) {
            return;
        }
        let Some(p) = self.config.profiles.iter().find(|p| p.id == id).cloned() else {
            return;
        };
        self.selected_profile_id = Some(id.to_string());
        let selected_candidate = self
            .candidates
            .iter()
            .position(|c| c.name == p.bind_iface && c.ip.to_string() == p.bind_addr)
            .or_else(|| {
                self.candidates
                    .iter()
                    .position(|c| !p.bind_iface.is_empty() && c.name == p.bind_iface)
            })
            .or_else(|| self.candidates.iter().position(|c| !c.is_tunnel))
            .unwrap_or(0);
        self.editor = Some(HostEditor {
            id: Some(p.id.clone()),
            tab: EditorTab::General,
            name: p.name.clone(),
            folder: p.folder.clone(),
            host: p.host.clone(),
            port: p.port.to_string(),
            user: p.user.clone(),
            identity: p.identity.clone(),
            password: String::new(),
            remember_password: p.remember_password,
            key_passphrase: String::new(),
            remember_key_passphrase: p.remember_key_passphrase,
            show_password: false,
            dns_server: p.dns_server.clone(),
            forward_local: p.forward_local.clone(),
            forward_remote: p.forward_remote.clone(),
            bind_mode: p.bind_mode.into(),
            bind_addr: p.bind_addr.clone(),
            selected_candidate,
            error: self
                .t("正在读取已保存的凭据…", "Reading saved credentials…")
                .into(),
            focus_first: true,
            found_keys: Arc::new(default_keys()),
            password_unreadable: true,
            passphrase_unreadable: true,
            credentials_loading: true,
        });
        let id = p.id.clone();
        self.credentials.submit(
            id.clone(),
            move || (p.read_password(), p.read_key_passphrase()),
            move |app, (pw, pp)| {
                if let Some(ed) = app
                    .editor
                    .as_mut()
                    .filter(|ed| ed.id.as_ref() == Some(&id) && ed.credentials_loading)
                {
                    ed.credentials_loading = false;
                    ed.password_unreadable = pw.is_err();
                    ed.passphrase_unreadable = pp.is_err();
                    if ed.password.is_empty() {
                        ed.password = pw.ok().flatten().unwrap_or_default();
                    }
                    if ed.key_passphrase.is_empty() {
                        ed.key_passphrase = pp.ok().flatten().unwrap_or_default();
                    }
                    ed.error.clear();
                    if ed.password_unreadable || ed.passphrase_unreadable {
                        app.toast_msg(app.t(
                            "无法读取部分已保存凭据；留空会保留原值，也可输入新密码",
                            "Some saved credentials could not be read; leave blank to keep them, or enter new values",
                        ), StatusKind::Error);
                    }
                }
            },
        );
    }

    /// Build connection params from the editor (without saving).
    fn editor_params(&self, ed: &HostEditor) -> Result<ConnParams, String> {
        let port = ed
            .port
            .trim()
            .parse::<u16>()
            .ok()
            .filter(|p| *p > 0)
            .ok_or_else(|| {
                self.t(
                    "端口必须在 1–65535 之间",
                    "Port must be between 1 and 65535",
                )
                .to_string()
            })?;
        if ed.host.trim().is_empty() {
            return Err(self.t("请填写主机地址", "Enter a host address").into());
        }
        if ed.user.trim().is_empty() {
            return Err(self.t("请填写用户名", "Enter an SSH username").into());
        }
        let (bind_addr, bind_iface) = match ed.bind_mode {
            BindMode::Iface => match self.candidates.get(ed.selected_candidate) {
                Some(c) => (c.ip.to_string(), c.name.clone()),
                None => return Err(self.t("请选择网卡", "Select a network interface").into()),
            },
            BindMode::Addr => (ed.bind_addr.trim().to_string(), String::new()),
            BindMode::Auto => (String::new(), String::new()),
        };
        let p = ConnParams {
            label: if ed.name.trim().is_empty() {
                format!("{}@{}", ed.user.trim(), ed.host.trim())
            } else {
                ed.name.trim().to_string()
            },
            host: ed.host.trim().to_string(),
            port,
            user: ed.user.trim().to_string(),
            identity: ed.identity.trim().to_string(),
            password: ed.password.clone(),
            key_passphrase: ed.key_passphrase.clone(),
            dns_server: ed.dns_server.trim().to_string(),
            forward_local: ed.forward_local.trim().to_string(),
            forward_remote: ed.forward_remote.trim().to_string(),
            bind_mode: ed.bind_mode,
            bind_addr,
            bind_iface,
            profile_id: ed.id.clone(),
            remember_password: false,
        };
        if (!p.forward_local.is_empty() || !p.forward_remote.is_empty())
            && parse_forward(&p.forward_local, &p.forward_remote).is_none()
        {
            return Err(self
                .t(
                    "端口转发格式：本地 127.0.0.1:8080（或 8080）→ 远端 host:port",
                    "Forward: local 127.0.0.1:8080 (or 8080) → remote host:port",
                )
                .into());
        }
        if p.bind_mode == BindMode::Addr && p.bind_addr.parse::<std::net::IpAddr>().is_err() {
            return Err(self
                .t("请填写有效的本地绑定 IP", "Enter a valid local bind IP")
                .into());
        }
        Ok(p)
    }

    /// Save the editor into a profile; returns its id.
    fn save_editor(&mut self, ed: &HostEditor) -> Result<String, String> {
        if ed.credentials_loading || ed.id.as_ref().is_some_and(|id| self.credentials.busy(id)) {
            return Err(self
                .t(
                    "凭据操作尚未完成，请稍候",
                    "Please wait for the credential operation",
                )
                .into());
        }
        let params = self.editor_params(ed)?;
        let mut prof = ed
            .id
            .as_ref()
            .and_then(|id| self.config.profiles.iter().find(|p| &p.id == id).cloned())
            .unwrap_or_else(|| profiles::Profile::new_blank(&params.label));
        prof.name = ed.name.trim().to_string();
        prof.host = params.host.clone();
        prof.port = params.port;
        prof.user = params.user.clone();
        prof.identity = params.identity.clone();
        // A remembered secret we couldn't read is kept unless the user typed
        // a new one or turned "remember" off.
        let keep_pw = ed.password_unreadable && ed.remember_password && ed.password.is_empty();
        let keep_pp =
            ed.passphrase_unreadable && ed.remember_key_passphrase && ed.key_passphrase.is_empty();
        let mut credential_profile = prof.clone();
        let secrets = ed.clone();
        self.credentials.submit(
            prof.id.clone(),
            move || {
                let mut failed = false;
                let mut password_saved = false;
                let mut passphrase_saved = false;
                if !keep_pw {
                    let previous = credential_profile.remember_password;
                    credential_profile.remember_password = secrets.remember_password;
                    if credential_profile.set_password(&secrets.password).is_err() {
                        credential_profile.remember_password = previous;
                        failed = true;
                    } else {
                        password_saved = true;
                    }
                }
                if !keep_pp {
                    let previous = credential_profile.remember_key_passphrase;
                    credential_profile.remember_key_passphrase = secrets.remember_key_passphrase;
                    if credential_profile
                        .set_key_passphrase(&secrets.key_passphrase)
                        .is_err()
                    {
                        credential_profile.remember_key_passphrase = previous;
                        failed = true;
                    } else {
                        passphrase_saved = true;
                    }
                }
                (credential_profile, password_saved, passphrase_saved, failed)
            },
            |app, (saved, password_saved, passphrase_saved, failed)| {
                if let Some(p) = app.config.profiles.iter_mut().find(|p| p.id == saved.id) {
                    // Failed / untouched fields may have been migrated or
                    // updated by an earlier queued operation. Keep that result.
                    if password_saved {
                        p.password_obfuscated = saved.password_obfuscated;
                        p.remember_password = saved.remember_password;
                    }
                    if passphrase_saved {
                        p.key_passphrase_obfuscated = saved.key_passphrase_obfuscated;
                        p.remember_key_passphrase = saved.remember_key_passphrase;
                    }
                    app.persist();
                    if failed {
                        app.credential_warning();
                    }
                }
            },
        );
        prof.dns_server = params.dns_server.clone();
        prof.forward_local = params.forward_local.clone();
        prof.forward_remote = params.forward_remote.clone();
        prof.bind_mode = params.bind_mode.into();
        prof.bind_addr = params.bind_addr.clone();
        prof.bind_iface = params.bind_iface.clone();
        prof.folder = ed.folder.trim().to_string();
        if !prof.folder.is_empty() && !self.config.folders.contains(&prof.folder) {
            self.config.folders.push(prof.folder.clone());
            self.config.folders.sort();
        }
        let id = prof.id.clone();
        match self.config.profiles.iter_mut().find(|p| p.id == id) {
            Some(slot) => *slot = prof,
            None => self.config.profiles.push(prof),
        }
        self.selected_profile_id = Some(id.clone());
        self.persist();
        Ok(id)
    }

    pub(super) fn ui_editor(&mut self, ctx: &egui::Context) {
        let Some(mut ed) = self.editor.take() else {
            return;
        };
        let t = self.tok();
        let is_new = ed.id.is_none();
        let mut close = false;
        let mut action: Option<u8> = None; // 1 save, 2 save+connect, 3 connect only
        let zh = self.zh();
        let tr = |a: &'static str, b: &'static str| if zh { a } else { b };
        let folders = self.folder_names();
        let cands = self.candidates.clone();
        let found_keys = ed.found_keys.clone();
        let mut refresh = false;
        let mut browse = false;

        let modal = egui::Modal::new(egui::Id::new("host_editor"))
            .frame(sheet_frame(&t))
            .show(ctx, |ui| {
                ui.set_width(470.0);
                // Header
                ui.horizontal(|ui| {
                    ui.set_height(w::CTRL_H);
                    let title = if is_new {
                        tr("新建主机", "New host")
                    } else {
                        tr("编辑主机", "Edit host")
                    };
                    let g = w::galley(ui, title, w::semibold(18.0), t.text);
                    let (r, _) = ui.allocate_exact_size(g.size(), Sense::hover());
                    ui.painter().galley(r.min, g, t.text);
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if w::icon_button(ui, Icon::Close, tr("关闭", "Close"), true).clicked() {
                            close = true;
                        }
                    });
                });
                ui.add_space(14.0);
                let mut tab = match ed.tab {
                    EditorTab::General => 0,
                    EditorTab::Auth => 1,
                    EditorTab::Network => 2,
                };
                let seg_w = (ui.available_width() - 4.0) / 3.0;
                if w::segmented(
                    ui,
                    &[
                        (None, tr("常规", "General")),
                        (None, tr("认证", "Sign-in")),
                        (None, tr("网络", "Network")),
                    ],
                    &mut tab,
                    seg_w,
                ) {
                    ed.tab = [EditorTab::General, EditorTab::Auth, EditorTab::Network][tab];
                }
                ui.add_space(18.0);

                let body_h = 268.0;
                let (body, _) =
                    ui.allocate_exact_size(Vec2::new(ui.available_width(), body_h), Sense::hover());
                {
                    let mut child = ui.new_child(
                        UiBuilder::new()
                            .max_rect(body)
                            .layout(Layout::top_down(Align::Min)),
                    );
                    let ui = &mut child;
                    ui.spacing_mut().item_spacing.y = 10.0;
                    match ed.tab {
                        EditorTab::General => {
                            form_row(ui, tr("名称", "Name"), |ui| {
                                w::Field::new(&mut ed.name)
                                    .hint(tr("可选，例如 东京 VPS", "Optional, e.g. Tokyo VPS"))
                                    .show(ui);
                            });
                            form_row(ui, tr("主机", "Host"), |ui| {
                                let r = w::Field::new(&mut ed.host)
                                    .hint("example.com  /  203.0.113.10")
                                    .id(egui::Id::new("ed_host"))
                                    .show(ui);
                                if ed.focus_first {
                                    r.request_focus();
                                    ed.focus_first = false;
                                }
                            });
                            form_row(ui, tr("端口", "Port"), |ui| {
                                w::Field::new(&mut ed.port).hint("22").width(90.0).show(ui);
                                ui.add_space(14.0);
                                let g =
                                    w::galley(ui, tr("用户", "User"), w::regular(13.0), t.text2);
                                let (r, _) = ui.allocate_exact_size(g.size(), Sense::hover());
                                ui.painter().galley(r.min, g, t.text2);
                                ui.add_space(4.0);
                                w::Field::new(&mut ed.user).hint("root").show(ui);
                            });
                            form_row(ui, tr("分组", "Folder"), |ui| {
                                let fw = ui.available_width() - 36.0;
                                w::Field::new(&mut ed.folder)
                                    .hint(tr("无", "None"))
                                    .width(fw)
                                    .show(ui);
                                let b = w::icon_button(
                                    ui,
                                    Icon::ChevronDown,
                                    tr("选择分组", "Choose folder"),
                                    !folders.is_empty(),
                                );
                                w::popover(ui, &b, egui::Id::new("ed_folder_pop"), 180.0, |ui| {
                                    if w::menu_item(ui, tr("无分组", "No folder"), "", false) {
                                        ed.folder.clear();
                                        ui.memory_mut(|m| m.close_popup());
                                    }
                                    for f in folders.iter() {
                                        if w::menu_item(ui, f, "", false) {
                                            ed.folder = f.clone();
                                            ui.memory_mut(|m| m.close_popup());
                                        }
                                    }
                                });
                            });
                        }
                        EditorTab::Auth => {
                            let show = ed.show_password;
                            form_row(ui, tr("密码", "Password"), |ui| {
                                let fw = ui.available_width() - 36.0;
                                w::Field::new(&mut ed.password)
                                    .password(!show)
                                    .hint(if ed.password_unreadable {
                                        tr(
                                            "已保存（系统钥匙串暂不可用），留空保持不变",
                                            "Saved (keychain unavailable) — leave empty to keep",
                                        )
                                    } else {
                                        tr("留空则在连接时询问", "Leave empty to be asked")
                                    })
                                    .width(fw)
                                    .show(ui);
                                if w::icon_button(
                                    ui,
                                    if show { Icon::EyeOff } else { Icon::Eye },
                                    tr("显示 / 隐藏", "Show / hide"),
                                    true,
                                )
                                .clicked()
                                {
                                    ed.show_password = !show;
                                }
                            });
                            form_row(ui, "", |ui| {
                                w::switch(
                                    ui,
                                    &mut ed.remember_password,
                                    tr("记住密码", "Remember password"),
                                );
                            });
                            form_row(ui, tr("私钥", "Private key"), |ui| {
                                // Reserve the button's real width so the row
                                // never overflows (English label is longer).
                                let bw =
                                    w::button_width(ui, false, tr("选择…", "Choose…")).max(68.0);
                                let fw = ui.available_width() - bw - ui.spacing().item_spacing.x;
                                w::Field::new(&mut ed.identity)
                                    .hint("~/.ssh/id_ed25519")
                                    .width(fw)
                                    .mono()
                                    .show(ui);
                                if w::button_sized(
                                    ui,
                                    Kind::Secondary,
                                    None,
                                    tr("选择…", "Choose…"),
                                    true,
                                    bw,
                                )
                                .clicked()
                                {
                                    browse = true;
                                }
                            });
                            if !found_keys.is_empty() && ed.identity.trim().is_empty() {
                                form_row(ui, "", |ui| {
                                    ui.spacing_mut().item_spacing.x = 4.0;
                                    for k in found_keys.iter() {
                                        let name =
                                            k.file_name().and_then(|s| s.to_str()).unwrap_or("key");
                                        if w::button(ui, Kind::Plain, Some(Icon::Key), name, true)
                                            .clicked()
                                        {
                                            ed.identity = k.display().to_string();
                                        }
                                    }
                                });
                            }
                            form_row(ui, tr("私钥口令", "Passphrase"), |ui| {
                                w::Field::new(&mut ed.key_passphrase)
                                    .password(!show)
                                    .hint(tr("私钥加密时才需要", "Only for encrypted keys"))
                                    .show(ui);
                            });
                            form_row(ui, "", |ui| {
                                w::switch(
                                    ui,
                                    &mut ed.remember_key_passphrase,
                                    tr("记住私钥口令", "Remember passphrase"),
                                );
                            });
                            if ed.remember_password || ed.remember_key_passphrase {
                                form_row(ui, "", |ui| {
                                    note(
                                        ui,
                                        &t,
                                        tr(
                                            "已保存的密码存储在系统凭据库中。",
                                            "Saved in the system credential store.",
                                        ),
                                        t.text3,
                                    );
                                });
                            }
                        }
                        EditorTab::Network => {
                            let mut mode = match ed.bind_mode {
                                BindMode::Auto => 0,
                                BindMode::Iface => 1,
                                BindMode::Addr => 2,
                            };
                            form_row(ui, tr("出站", "Route via"), |ui| {
                                let sw = (ui.available_width() - 4.0) / 3.0;
                                if w::segmented(
                                    ui,
                                    &[
                                        (None, tr("自动", "Auto")),
                                        (None, tr("网卡", "Interface")),
                                        (None, "IP"),
                                    ],
                                    &mut mode,
                                    sw,
                                ) {
                                    ed.bind_mode =
                                        [BindMode::Auto, BindMode::Iface, BindMode::Addr][mode];
                                }
                            });
                            match ed.bind_mode {
                                BindMode::Auto => {
                                    let auto = cands.iter().find(|c| !c.is_tunnel).cloned();
                                    form_row(ui, "", |ui| match &auto {
                                        Some(c) => note(
                                            ui,
                                            &t,
                                            &format!(
                                                "{} {} · {}",
                                                tr("将使用", "Uses"),
                                                c.name,
                                                c.ip
                                            ),
                                            t.text2,
                                        ),
                                        None => note(
                                            ui,
                                            &t,
                                            tr(
                                                "没有找到物理网卡，请手动指定",
                                                "No physical interface found — choose one",
                                            ),
                                            t.warn,
                                        ),
                                    });
                                }
                                BindMode::Iface => {
                                    form_row(ui, tr("网卡", "Interface"), |ui| {
                                        let fw = ui.available_width() - 36.0;
                                        if cands.is_empty() {
                                            note(
                                                ui,
                                                &t,
                                                tr("没有可用网卡", "No interfaces"),
                                                t.warn,
                                            );
                                        } else {
                                            let i = ed.selected_candidate;
                                            let fmt = |c: &bypass::IfaceCandidate| {
                                                if c.is_tunnel {
                                                    format!("{}  {}  · VPN", c.name, c.ip)
                                                } else {
                                                    format!("{}  {}", c.name, c.ip)
                                                }
                                            };
                                            let (r, resp) = ui.allocate_exact_size(
                                                Vec2::new(fw, w::CTRL_H),
                                                Sense::click(),
                                            );
                                            let hh = ui.ctx().animate_bool_with_time(
                                                resp.id.with("h"),
                                                resp.hovered(),
                                                0.12,
                                            );
                                            ui.painter().rect_filled(
                                                r,
                                                CornerRadius::same(7),
                                                w::lerp(t.control, t.control_hover, hh),
                                            );
                                            w::paint_text_trunc(
                                                ui,
                                                &ui.painter_at(r),
                                                &cands.get(i).map(fmt).unwrap_or_else(|| {
                                                    tr("选择网卡…", "Choose an interface…")
                                                        .to_string()
                                                }),
                                                w::regular(13.0),
                                                t.text,
                                                r.left() + 10.0,
                                                r.center().y,
                                                r.width() - 34.0,
                                            );
                                            w::paint_icon(
                                                ui.painter(),
                                                Rect::from_center_size(
                                                    Pos2::new(r.right() - 14.0, r.center().y),
                                                    Vec2::splat(11.0),
                                                ),
                                                Icon::ChevronDown,
                                                t.text2,
                                            );
                                            w::popover(
                                                ui,
                                                &resp,
                                                egui::Id::new("iface_pop"),
                                                fw,
                                                |ui| {
                                                    for (k, c) in cands.iter().enumerate() {
                                                        if w::menu_item(
                                                            ui,
                                                            &fmt(c),
                                                            if k == i { "✓" } else { "" },
                                                            false,
                                                        ) {
                                                            ed.selected_candidate = k;
                                                            ui.memory_mut(|m| m.close_popup());
                                                        }
                                                    }
                                                },
                                            );
                                        }
                                        if w::icon_button(
                                            ui,
                                            Icon::Refresh,
                                            tr("刷新", "Refresh"),
                                            true,
                                        )
                                        .clicked()
                                        {
                                            refresh = true;
                                        }
                                    });
                                    if cands
                                        .get(ed.selected_candidate)
                                        .is_some_and(|c| c.is_tunnel)
                                    {
                                        form_row(ui, "", |ui| {
                                            note(
                                                ui,
                                                &t,
                                                tr(
                                                    "这是 VPN / 虚拟网卡，流量可能仍走隧道",
                                                    "This is a VPN / virtual adapter",
                                                ),
                                                t.warn,
                                            )
                                        });
                                    }
                                }
                                BindMode::Addr => {
                                    form_row(ui, tr("本地 IP", "Local IP"), |ui| {
                                        w::Field::new(&mut ed.bind_addr)
                                            .hint("192.168.1.10")
                                            .mono()
                                            .show(ui);
                                    });
                                }
                            }
                            form_row(ui, "DNS", |ui| {
                                w::Field::new(&mut ed.dns_server)
                                    .hint(tr("可选，例如 1.1.1.1", "Optional, e.g. 1.1.1.1"))
                                    .mono()
                                    .show(ui);
                            });
                            form_row(ui, tr("端口转发", "Forward"), |ui| {
                                let fw = (ui.available_width() - 30.0) / 2.0;
                                w::Field::new(&mut ed.forward_local)
                                    .hint("127.0.0.1:8080")
                                    .width(fw)
                                    .mono()
                                    .show(ui);
                                let (r, _) = ui.allocate_exact_size(
                                    Vec2::new(14.0, w::CTRL_H),
                                    Sense::hover(),
                                );
                                w::paint_icon(
                                    ui.painter(),
                                    Rect::from_center_size(r.center(), Vec2::splat(12.0)),
                                    Icon::ChevronRight,
                                    t.text3,
                                );
                                w::Field::new(&mut ed.forward_remote)
                                    .hint("127.0.0.1:80")
                                    .width(fw)
                                    .mono()
                                    .show(ui);
                            });
                            form_row(ui, "", |ui| {
                                note(
                                    ui,
                                    &t,
                                    tr(
                                        "相当于 ssh -L，会话断开时自动停止",
                                        "Like ssh -L; stops with the session",
                                    ),
                                    t.text3,
                                )
                            });
                        }
                    }
                }

                // Error + footer
                let (er, _) =
                    ui.allocate_exact_size(Vec2::new(ui.available_width(), 22.0), Sense::hover());
                if !ed.error.is_empty() {
                    w::paint_icon(
                        ui.painter(),
                        Rect::from_center_size(
                            Pos2::new(er.left() + 8.0, er.center().y),
                            Vec2::splat(13.0),
                        ),
                        Icon::Warning,
                        t.err,
                    );
                    w::paint_text_trunc(
                        ui,
                        &ui.painter_at(er),
                        &ed.error,
                        w::regular(12.5),
                        t.err,
                        er.left() + 22.0,
                        er.center().y,
                        er.width() - 24.0,
                    );
                }
                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    ui.set_height(w::CTRL_H);
                    if is_new
                        && w::button(ui, Kind::Plain, None, tr("仅连接", "Connect only"), true)
                            .on_hover_text(tr("不保存，直接连接", "Connect without saving"))
                            .clicked()
                    {
                        action = Some(3);
                    }
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if w::button(
                            ui,
                            Kind::Primary,
                            None,
                            tr("保存并连接", "Save & Connect"),
                            true,
                        )
                        .on_hover_text("Ctrl+Enter")
                        .clicked()
                        {
                            action = Some(2);
                        }
                        if w::button(ui, Kind::Secondary, None, tr("保存", "Save"), true).clicked()
                        {
                            action = Some(1);
                        }
                        if w::button(ui, Kind::Plain, None, tr("取消", "Cancel"), true).clicked()
                        {
                            close = true;
                        }
                    });
                });
                if ui.input(|i| i.key_pressed(egui::Key::Enter) && i.modifiers.command) {
                    action = Some(2);
                }
            });
        if modal.should_close() {
            close = true;
        }
        if refresh {
            // Keep the same adapter selected (by name + IP), not the same list slot.
            let prev = self
                .candidates
                .get(ed.selected_candidate)
                .map(|c| (c.name.clone(), c.ip));
            self.refresh_ifaces();
            ed.selected_candidate = prev
                .and_then(|(name, ip)| {
                    self.candidates
                        .iter()
                        .position(|c| c.name == name && c.ip == ip)
                        .or_else(|| self.candidates.iter().position(|c| c.name == name))
                })
                .or_else(|| self.candidates.iter().position(|c| !c.is_tunnel))
                .unwrap_or(0);
        }
        if browse {
            self.open_picker(super::picker::PickFor::Identity);
        }
        self.finish_editor(ed, close, action);
    }

    fn finish_editor(&mut self, mut ed: HostEditor, mut close: bool, action: Option<u8>) {
        if action.is_some() && ed.credentials_loading {
            ed.error = self
                .t("正在读取凭据，请稍候", "Please wait for saved credentials")
                .into();
            self.editor = Some(ed);
            return;
        }
        if let Some(a) = action {
            match a {
                3 => match self.editor_params(&ed) {
                    Ok(p) => {
                        self.connect_params(p, false, None);
                        close = true;
                    }
                    Err(e) => ed.error = e,
                },
                _ => match self.save_editor(&ed) {
                    Ok(id) => {
                        close = true;
                        if a == 2 {
                            // Use the editor's password even when it is not remembered.
                            if let Ok(mut params) = self.editor_params(&ed) {
                                params.profile_id = Some(id);
                                self.connect_params(params, false, None);
                            }
                        } else {
                            self.toast_msg(self.t("已保存", "Saved"), StatusKind::Ok);
                        }
                    }
                    Err(e) => ed.error = e,
                },
            }
        }
        if !close {
            self.editor = Some(ed);
        }
    }
}

const LABEL_W: f32 = 84.0;

/// Form row: right-aligned label column + content, fixed 28 px height.
fn form_row(ui: &mut egui::Ui, label: &str, add: impl FnOnce(&mut egui::Ui)) {
    let t = theme::tok(ui.ctx());
    ui.horizontal(|ui| {
        ui.set_height(w::CTRL_H);
        let (r, _) = ui.allocate_exact_size(Vec2::new(LABEL_W, w::CTRL_H), Sense::hover());
        if !label.is_empty() {
            let g = w::galley(ui, label, w::regular(13.0), t.text2);
            ui.painter().galley(
                Pos2::new(r.right() - g.size().x, r.center().y - g.size().y / 2.0),
                g,
                t.text2,
            );
        }
        ui.add_space(4.0);
        add(ui);
    });
}

/// Single-line secondary note.
fn note(ui: &mut egui::Ui, _t: &theme::Tokens, text: &str, color: Color32) {
    let (r, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), w::CTRL_H), Sense::hover());
    w::paint_text_trunc(
        ui,
        &ui.painter_at(r),
        text,
        w::regular(12.0),
        color,
        r.left() + 2.0,
        r.center().y,
        r.width() - 4.0,
    );
}

/// Common private keys in ~/.ssh that exist on this machine.
fn default_keys() -> Vec<PathBuf> {
    let home = std::env::var("USERPROFILE")
        .or_else(|_| std::env::var("HOME"))
        .map(PathBuf::from);
    let Ok(home) = home else {
        return Vec::new();
    };
    ["id_ed25519", "id_ecdsa", "id_rsa"]
        .iter()
        .map(|n| home.join(".ssh").join(n))
        .filter(|p| p.is_file())
        .collect()
}
