//! In-app file / folder picker that follows the app's own look (instead of
//! the OS dialog): places sidebar, path bar, sortable list, keyboard nav.

use super::panels::sheet_frame;
use super::widgets::{self as w, Icon, Kind};
use super::*;
use egui::{Align, Color32, CornerRadius, Key, Layout, Pos2, Rect, Sense, Stroke, UiBuilder, Vec2};

const ROW: f32 = 28.0;

#[derive(Clone, Copy, PartialEq)]
pub(super) enum PickMode {
    File,
    Files,
    Folder,
}

#[derive(Clone, Copy, PartialEq)]
pub(super) enum PickFor {
    Identity,
    Upload(u64),
    BatchUpload,
    DownloadDir,
}

struct Entry {
    name: String,
    path: PathBuf,
    is_dir: bool,
    size: Option<u64>,
    mtime: Option<u64>,
    hidden: bool,
}

pub(super) struct LocalPicker {
    mode: PickMode,
    purpose: PickFor,
    cwd: PathBuf,
    entries: Vec<Entry>,
    /// Bumped whenever `entries` is replaced (keys `vis_cache`).
    entries_gen: u64,
    /// (filter, show hidden, entries_gen) → visible indices.
    vis_cache: Option<(String, bool, u64, std::rc::Rc<Vec<usize>>)>,
    error: Option<String>,
    sel: Vec<PathBuf>,
    anchor: Option<usize>,
    filter: String,
    show_hidden: bool,
    back: Vec<PathBuf>,
    path_edit: Option<String>,
    scroll_to: Option<usize>,
    /// Sidebar places, worked out once when the picker opens.
    places: Vec<(String, Icon, PathBuf)>,
    /// Folder being read in the background (a slow or offline drive must
    /// not freeze the window): result slot, and where "Back" should return
    /// once it has loaded.
    pending: Option<PendingRead>,
    ctx: egui::Context,
}

/// (folder, its entries) or an error message.
type ReadResult = Result<(PathBuf, Vec<Entry>), String>;
/// Filled by the reading thread.
type ReadSlot = Arc<Mutex<Option<ReadResult>>>;

struct PendingRead {
    slot: ReadSlot,
    /// Push this onto the history once the folder has loaded.
    back_to: Option<PathBuf>,
    /// Came from "Back": drop the history entry once it has loaded.
    pop_back: bool,
}

fn read_folder(dir: PathBuf) -> ReadResult {
    let dir = std::fs::canonicalize(&dir)
        .map(strip_verbatim)
        .unwrap_or(dir);
    let rd = std::fs::read_dir(&dir).map_err(|e| e.to_string())?;
    let mut v: Vec<Entry> = rd
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().to_string();
            let path = e.path();
            // follow symlinks for the dir/file decision
            let md = std::fs::metadata(&path).or_else(|_| e.metadata()).ok()?;
            Some(Entry {
                hidden: is_hidden(&name, &md),
                is_dir: md.is_dir(),
                size: md.is_file().then_some(md.len()),
                mtime: md
                    .modified()
                    .ok()
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| d.as_secs()),
                name,
                path,
            })
        })
        .collect();
    v.sort_by(|a, b| {
        b.is_dir
            .cmp(&a.is_dir)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    Ok((dir, v))
}

fn home_dir() -> PathBuf {
    std::env::var("USERPROFILE")
        .or_else(|_| std::env::var("HOME"))
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("."))
}

#[cfg(windows)]
fn is_hidden(name: &str, md: &std::fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    name.starts_with('.') || md.file_attributes() & 0x2 != 0
}

#[cfg(not(windows))]
fn is_hidden(name: &str, _md: &std::fs::Metadata) -> bool {
    name.starts_with('.')
}

/// Sidebar places: (label, icon, path).
fn places(zh: bool, downloads: PathBuf) -> Vec<(String, Icon, PathBuf)> {
    let home = home_dir();
    let mut v = vec![(
        (if zh { "主目录" } else { "Home" }).to_string(),
        Icon::Home,
        home.clone(),
    )];
    // Windows often redirects Desktop / Documents into OneDrive.
    let first_dir = |cands: &[PathBuf]| {
        cands
            .iter()
            .find(|p| p.is_dir())
            .cloned()
            .unwrap_or_else(|| cands[0].clone())
    };
    let od = home.join("OneDrive");
    let desktop = first_dir(&[home.join("Desktop"), od.join("Desktop"), od.join("桌面")]);
    let documents = first_dir(&[
        home.join("Documents"),
        od.join("Documents"),
        od.join("文档"),
    ]);
    for (zh_l, en_l, icon, p) in [
        ("桌面", "Desktop", Icon::Desktop, desktop),
        ("下载", "Downloads", Icon::Download, downloads),
        ("文档", "Documents", Icon::File, documents),
        ("SSH 密钥", "SSH keys", Icon::Key, home.join(".ssh")),
    ] {
        if p.is_dir() && !v.iter().any(|(_, _, q)| q == &p) {
            v.push(((if zh { zh_l } else { en_l }).to_string(), icon, p));
        }
    }
    #[cfg(windows)]
    {
        // Drive letters from a bitmask: no I/O, so an offline network drive
        // can't stall the picker.
        // SAFETY: plain Win32 call without arguments.
        let mask = unsafe { windows_sys::Win32::Storage::FileSystem::GetLogicalDrives() };
        for c in b'C'..=b'Z' {
            let root = PathBuf::from(format!("{}:\\", c as char));
            if mask & (1 << (c - b'A')) != 0 {
                let label = if zh {
                    format!("本地磁盘 ({}:)", c as char)
                } else {
                    format!("Disk ({}:)", c as char)
                };
                v.push((label, Icon::Drive, root));
            }
        }
    }
    #[cfg(not(windows))]
    {
        v.push((
            (if zh { "根目录" } else { "Computer" }).to_string(),
            Icon::Drive,
            PathBuf::from("/"),
        ));
    }
    v
}

impl LocalPicker {
    fn new(
        mode: PickMode,
        purpose: PickFor,
        starts: Vec<PathBuf>,
        places: Vec<(String, Icon, PathBuf)>,
        ctx: egui::Context,
    ) -> Self {
        let mut p = Self {
            mode,
            purpose,
            cwd: PathBuf::new(),
            entries: Vec::new(),
            entries_gen: 0,
            vis_cache: None,
            error: None,
            sel: Vec::new(),
            anchor: None,
            filter: String::new(),
            show_hidden: false,
            back: Vec::new(),
            path_edit: None,
            scroll_to: None,
            places,
            pending: None,
            ctx,
        };
        p.request(starts, None, false);
        p
    }

    /// Start reading in the background (see [`Self::poll`]): the first of
    /// `dirs` that can be read (fallbacks: a remembered
    /// folder may be gone, or on an offline drive — checked off the UI
    /// thread too).
    fn request(&mut self, dirs: Vec<PathBuf>, back_to: Option<PathBuf>, pop_back: bool) {
        let slot = Arc::new(Mutex::new(None));
        let (s2, ctx) = (Arc::clone(&slot), self.ctx.clone());
        std::thread::spawn(move || {
            let mut r = Err(String::new());
            for d in dirs {
                r = read_folder(d);
                if r.is_ok() {
                    break;
                }
            }
            *s2.lock().unwrap() = Some(r);
            ctx.request_repaint();
        });
        // A newer request replaces an older one still running.
        self.pending = Some(PendingRead {
            slot,
            back_to,
            pop_back,
        });
    }

    fn loading(&self) -> bool {
        self.pending.is_some()
    }

    /// Apply a finished background read.
    fn poll(&mut self) {
        let Some(p) = self.pending.as_ref() else {
            return;
        };
        let Some(result) = p.slot.lock().unwrap().take() else {
            return;
        };
        let (back_to, pop_back) = (p.back_to.clone(), p.pop_back);
        self.pending = None;
        match result {
            Ok((dir, entries)) => {
                if pop_back {
                    self.back.pop();
                }
                if let Some(prev) = back_to.filter(|p| !p.as_os_str().is_empty() && *p != dir) {
                    self.back.push(prev);
                }
                self.entries = entries;
                self.entries_gen += 1;
                self.error = None;
                self.cwd = dir;
                self.sel.clear();
                self.anchor = None;
                self.filter.clear();
                self.path_edit = None;
            }
            Err(e) => {
                self.error = Some(e);
                if self.cwd.as_os_str().is_empty() {
                    self.entries.clear();
                    self.entries_gen += 1;
                }
            }
        }
    }

    fn go(&mut self, dir: PathBuf) {
        if dir == self.cwd {
            return;
        }
        let prev = self.cwd.clone();
        self.request(vec![dir], Some(prev), false);
    }

    /// Back in history; the entry is only dropped once that folder loads.
    fn go_back(&mut self) {
        if let Some(prev) = self.back.last().cloned() {
            self.request(vec![prev], None, true);
        }
    }

    /// Visible entries in order; recomputed only when the listing, filter
    /// or hidden-files switch changes.
    fn visible(&mut self) -> std::rc::Rc<Vec<usize>> {
        let f = self.filter.trim().to_lowercase();
        if let Some((cf, ch, cg, v)) = &self.vis_cache {
            if *cf == f && *ch == self.show_hidden && *cg == self.entries_gen {
                return std::rc::Rc::clone(v);
            }
        }
        let v: std::rc::Rc<Vec<usize>> = std::rc::Rc::new(
            (0..self.entries.len())
                .filter(|&i| {
                    let e = &self.entries[i];
                    (self.show_hidden || !e.hidden)
                        && (f.is_empty() || e.name.to_lowercase().contains(&f))
                })
                .collect(),
        );
        self.vis_cache = Some((
            f,
            self.show_hidden,
            self.entries_gen,
            std::rc::Rc::clone(&v),
        ));
        v
    }

    fn selectable(&self, e: &Entry) -> bool {
        match self.mode {
            PickMode::Folder => e.is_dir,
            _ => true,
        }
    }

    /// What the confirm button would return right now.
    fn result(&self) -> Vec<PathBuf> {
        match self.mode {
            PickMode::Folder => {
                if let Some(p) = self.sel.first() {
                    vec![p.clone()]
                } else if self.cwd.as_os_str().is_empty() || self.error.is_some() {
                    // Nothing could be opened: there is no "this folder".
                    Vec::new()
                } else {
                    vec![self.cwd.clone()]
                }
            }
            PickMode::File => self
                .sel
                .iter()
                .filter(|p| self.entries.iter().any(|e| &e.path == *p && !e.is_dir))
                .cloned()
                .collect(),
            // Uploads take files and whole folders.
            PickMode::Files => self.sel.clone(),
        }
    }
}

/// `\\?\C:\x` → `C:\x` (canonicalize on Windows adds the verbatim prefix).
fn strip_verbatim(p: PathBuf) -> PathBuf {
    let s = p.to_string_lossy();
    if let Some(rest) = s.strip_prefix(r"\\?\") {
        if !rest.starts_with("UNC") {
            return PathBuf::from(rest.to_string());
        }
    }
    p
}

impl GuiApp {
    pub(super) fn open_picker(&mut self, purpose: PickFor) {
        // Where to open, best first (tried off the UI thread).
        let (mode, starts) = match purpose {
            PickFor::Identity => (PickMode::File, vec![home_dir().join(".ssh"), home_dir()]),
            PickFor::DownloadDir => (PickMode::Folder, vec![self.download_dir(), home_dir()]),
            PickFor::Upload(_) | PickFor::BatchUpload => (
                PickMode::Files,
                self.picker_last
                    .clone()
                    .into_iter()
                    .chain([home_dir()])
                    .collect(),
            ),
        };
        // A popover (e.g. Settings) must not float above the sheet.
        self.ctx.memory_mut(|m| m.close_popup());
        let places = places(self.zh(), self.download_dir());
        let mut p = LocalPicker::new(mode, purpose, starts, places, self.ctx.clone());
        // Key files live in a dot-folder; show them there.
        p.show_hidden = purpose == PickFor::Identity;
        self.picker = Some(p);
    }

    fn finish_picker(&mut self, purpose: PickFor, paths: Vec<PathBuf>) {
        if paths.is_empty() {
            return;
        }
        let r = match purpose {
            PickFor::Identity => PickResult::Identity(paths[0].clone()),
            PickFor::DownloadDir => PickResult::DownloadDir(paths[0].clone()),
            PickFor::Upload(tab_uid) => PickResult::Upload { tab_uid, paths },
            PickFor::BatchUpload => PickResult::BatchUpload(paths),
        };
        self.picks.lock().unwrap().push(r);
        self.ctx.request_repaint();
    }

    pub(super) fn ui_picker(&mut self, ctx: &egui::Context) {
        let Some(mut pk) = self.picker.take() else {
            return;
        };
        pk.poll();
        let t = self.tok();
        let zh = self.zh();
        let mut done: Option<Vec<PathBuf>> = None;
        let mut cancel = false;
        let mut nav: Option<PathBuf> = None;
        let field_ids = [egui::Id::new("picker_filter"), egui::Id::new("picker_path")];
        // Captured before the fields run: Enter in a field must not also act on the list.
        let typing_at_start = ctx.memory(|m| m.focused().is_some_and(|f| field_ids.contains(&f)));

        let title = match pk.purpose {
            PickFor::Identity => self.t("选择私钥", "Choose a private key"),
            PickFor::DownloadDir => self.t("选择下载目录", "Choose download folder"),
            PickFor::Upload(_) => self.t("选择要上传的文件", "Choose files to upload"),
            PickFor::BatchUpload => self.t("选择要分发的文件", "Choose files to send"),
        };

        let modal = egui::Modal::new(egui::Id::new("local_picker"))
            .frame(sheet_frame(&t))
            .show(ctx, |ui| {
                let width = (ctx.screen_rect().width() - 80.0).clamp(520.0, 720.0);
                let body_h = (ctx.screen_rect().height() - 260.0).clamp(240.0, 420.0);
                ui.set_width(width);

                // ---- title ----
                let g = w::galley(ui, title, w::semibold(16.0), t.text);
                let (r, _) =
                    ui.allocate_exact_size(Vec2::new(ui.available_width(), 26.0), Sense::hover());
                ui.painter().galley(
                    Pos2::new(r.left(), r.center().y - g.size().y / 2.0),
                    g,
                    t.text,
                );
                ui.add_space(12.0);

                // ---- path bar ----
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 6.0;
                    if w::icon_button(
                        ui,
                        Icon::ChevronLeft,
                        self.t("返回", "Back"),
                        !pk.back.is_empty(),
                    )
                    .clicked()
                    {
                        pk.go_back();
                    }
                    let parent = pk.cwd.parent().map(|p| p.to_path_buf());
                    if w::icon_button(
                        ui,
                        Icon::ArrowUp,
                        self.t("上一级  Backspace", "Enclosing folder  Backspace"),
                        parent.is_some(),
                    )
                    .clicked()
                    {
                        nav = parent.clone();
                    }
                    let search_w = 170.0;
                    let path_w = ui.available_width() - search_w - 6.0;
                    // Path: breadcrumb-like read-only display; click to type.
                    if let Some(edit) = pk.path_edit.as_mut() {
                        let resp = w::Field::new(edit)
                            .icon(Icon::Folder)
                            .mono()
                            .width(path_w)
                            .id(egui::Id::new("picker_path"))
                            .show(ui);
                        if !resp.has_focus() && !resp.lost_focus() {
                            resp.request_focus();
                        }
                        if resp.lost_focus() {
                            if ui.input(|i| i.key_pressed(Key::Enter)) {
                                nav = Some(PathBuf::from(edit.trim()));
                            }
                            pk.path_edit = None;
                        }
                    } else {
                        let (pr, presp) =
                            ui.allocate_exact_size(Vec2::new(path_w, w::CTRL_H), Sense::click());
                        let h = ui.ctx().animate_bool_with_time(
                            presp.id.with("h"),
                            presp.hovered(),
                            0.12,
                        );
                        ui.painter().rect_filled(
                            pr,
                            CornerRadius::same(8),
                            w::lerp(t.control, t.control_hover, h),
                        );
                        w::paint_icon(
                            ui.painter(),
                            Rect::from_center_size(
                                Pos2::new(pr.left() + 15.0, pr.center().y),
                                Vec2::splat(14.0),
                            ),
                            Icon::FolderFill,
                            t.accent,
                        );
                        let shown = pk.cwd.display().to_string();
                        // Keep the tail of a long path visible.
                        let max = path_w - 40.0;
                        let mut text = shown.clone();
                        while w::galley(ui, &text, w::regular(12.5), t.text).size().x > max
                            && text.chars().count() > 4
                        {
                            text = format!("…{}", text.chars().skip(4).collect::<String>());
                        }
                        let g = w::galley(ui, &text, w::regular(12.5), t.text);
                        ui.painter().galley(
                            Pos2::new(pr.left() + 30.0, pr.center().y - g.size().y / 2.0),
                            g,
                            t.text,
                        );
                        let presp = presp.on_hover_text(if zh {
                            "点击输入路径"
                        } else {
                            "Click to type a path"
                        });
                        if presp.clicked() {
                            pk.path_edit = Some(shown);
                            ui.memory_mut(|m| m.request_focus(egui::Id::new("picker_path")));
                        }
                    }
                    w::Field::new(&mut pk.filter)
                        .icon(Icon::Search)
                        .hint(if zh { "筛选" } else { "Filter" })
                        .clearable()
                        .width(search_w)
                        .id(egui::Id::new("picker_filter"))
                        .show(ui);
                });
                ui.add_space(10.0);

                // ---- body: places + list ----
                let (body, _) =
                    ui.allocate_exact_size(Vec2::new(ui.available_width(), body_h), Sense::hover());
                let side_w = 160.0;
                let side = Rect::from_min_size(body.min, Vec2::new(side_w, body.height()));
                let list = Rect::from_min_max(Pos2::new(side.right() + 10.0, body.top()), body.max);
                let p = ui.painter();
                p.rect_filled(list, CornerRadius::same(10), t.bg);
                if !t.dark {
                    p.rect_stroke(
                        list,
                        CornerRadius::same(10),
                        Stroke::new(1.0_f32, t.separator),
                        egui::StrokeKind::Inside,
                    );
                }

                // places
                let mut su = ui.new_child(
                    UiBuilder::new()
                        .max_rect(side)
                        .layout(Layout::top_down(Align::Min)),
                );
                egui::ScrollArea::vertical()
                    .id_salt("picker_places")
                    .auto_shrink([false, false])
                    .show(&mut su, |ui| {
                        ui.spacing_mut().item_spacing.y = 2.0;
                        w::caption(ui, if zh { "位置" } else { "PLACES" });
                        for (label, icon, path) in pk.places.clone() {
                            let active = pk.cwd == path;
                            let (r, resp) = ui.allocate_exact_size(
                                Vec2::new(ui.available_width(), 30.0),
                                Sense::click(),
                            );
                            let h = ui.ctx().animate_bool_with_time(
                                resp.id.with("h"),
                                resp.hovered(),
                                0.1,
                            );
                            let fill = if active {
                                t.selected
                            } else {
                                t.row_hover.gamma_multiply(h)
                            };
                            ui.painter().rect_filled(r, CornerRadius::same(7), fill);
                            w::paint_icon(
                                ui.painter(),
                                Rect::from_center_size(
                                    Pos2::new(r.left() + 16.0, r.center().y),
                                    Vec2::splat(14.0),
                                ),
                                icon,
                                t.accent,
                            );
                            let pr = ui.painter_at(r);
                            w::paint_text_trunc(
                                ui,
                                &pr,
                                &label,
                                w::regular(13.0),
                                t.text,
                                r.left() + 32.0,
                                r.center().y,
                                r.width() - 38.0,
                            );
                            if resp.clicked() {
                                nav = Some(path);
                            }
                        }
                    });

                // list
                let inner = list.shrink2(Vec2::new(6.0, 6.0));
                let head = Rect::from_min_size(inner.min, Vec2::new(inner.width(), 24.0));
                let col_size = 80.0;
                let col_time = if inner.width() > 420.0 { 130.0 } else { 0.0 };
                let name_w = inner.width() - col_size - col_time - 8.0;
                {
                    let p = ui.painter();
                    let hc = t.text3;
                    let f = w::semibold(11.0);
                    p.text(
                        Pos2::new(head.left() + 34.0, head.center().y),
                        egui::Align2::LEFT_CENTER,
                        if zh { "名称" } else { "Name" },
                        f.clone(),
                        hc,
                    );
                    if col_time > 0.0 {
                        p.text(
                            Pos2::new(head.left() + name_w + 16.0, head.center().y),
                            egui::Align2::LEFT_CENTER,
                            if zh { "修改时间" } else { "Modified" },
                            f.clone(),
                            hc,
                        );
                    }
                    p.text(
                        Pos2::new(head.right() - 10.0, head.center().y),
                        egui::Align2::RIGHT_CENTER,
                        if zh { "大小" } else { "Size" },
                        f,
                        hc,
                    );
                    p.hline(
                        head.x_range(),
                        head.bottom() + 0.5,
                        Stroke::new(1.0_f32, t.separator),
                    );
                }
                let rows_rect =
                    Rect::from_min_max(Pos2::new(inner.left(), head.bottom() + 3.0), inner.max);
                let vis = pk.visible();
                let sel_set: std::collections::HashSet<&std::path::Path> =
                    pk.sel.iter().map(PathBuf::as_path).collect();
                let mut lu = ui.new_child(
                    UiBuilder::new()
                        .max_rect(rows_rect)
                        .layout(Layout::top_down(Align::Min)),
                );
                lu.set_clip_rect(rows_rect);
                let mut click: Option<(usize, egui::Modifiers)> = None;
                let mut open: Option<usize> = None;
                if pk.loading() && (pk.entries.is_empty() || pk.error.is_some()) {
                    lu.add_space(40.0);
                    lu.vertical_centered(|ui| {
                        ui.add(egui::Spinner::new().size(16.0).color(t.text3));
                    });
                } else if let Some(err) = &pk.error {
                    lu.add_space(30.0);
                    lu.vertical_centered(|ui| {
                        let g = w::galley(ui, err, w::regular(12.5), t.err);
                        let (r, _) = ui.allocate_exact_size(g.size(), Sense::hover());
                        ui.painter().galley(r.min, g, t.err);
                    });
                } else if vis.is_empty() {
                    lu.add_space(40.0);
                    lu.vertical_centered(|ui| {
                        let msg = if pk.filter.trim().is_empty() {
                            if zh {
                                "空文件夹"
                            } else {
                                "Empty folder"
                            }
                        } else if zh {
                            "没有匹配的项目"
                        } else {
                            "No matches"
                        };
                        let g = w::galley(ui, msg, w::regular(12.5), t.text3);
                        let (r, _) = ui.allocate_exact_size(g.size(), Sense::hover());
                        ui.painter().galley(r.min, g, t.text3);
                    });
                }
                egui::ScrollArea::vertical()
                    .id_salt(("picker_list", pk.cwd.display().to_string()))
                    .auto_shrink([false, false])
                    .show(&mut lu, |ui| {
                        ui.spacing_mut().item_spacing.y = 0.0;
                        let wdt = ui.available_width();
                        for (k, &i) in vis.iter().enumerate() {
                            let e = &pk.entries[i];
                            let (rect, resp) =
                                ui.allocate_exact_size(Vec2::new(wdt, ROW), Sense::click());
                            if pk.scroll_to == Some(i) {
                                resp.scroll_to_me(None);
                            }
                            if !ui.is_rect_visible(rect) {
                                continue;
                            }
                            let ok = pk.selectable(e);
                            let is_sel = sel_set.contains(e.path.as_path());
                            let h = ui.ctx().animate_bool_with_time(
                                resp.id.with("h"),
                                resp.hovered(),
                                0.1,
                            );
                            let p = ui.painter();
                            if is_sel {
                                let prev = k > 0
                                    && sel_set.contains(pk.entries[vis[k - 1]].path.as_path());
                                let next = k + 1 < vis.len()
                                    && sel_set.contains(pk.entries[vis[k + 1]].path.as_path());
                                let cr = CornerRadius {
                                    nw: if prev { 0 } else { 7 },
                                    ne: if prev { 0 } else { 7 },
                                    sw: if next { 0 } else { 7 },
                                    se: if next { 0 } else { 7 },
                                };
                                p.rect_filled(rect, cr, t.accent);
                            } else {
                                if k % 2 == 1 {
                                    p.rect_filled(
                                        rect,
                                        CornerRadius::same(7),
                                        if t.dark {
                                            Color32::from_rgba_premultiplied(5, 5, 5, 5)
                                        } else {
                                            Color32::from_rgba_premultiplied(0, 0, 0, 6)
                                        },
                                    );
                                }
                                p.rect_filled(
                                    rect,
                                    CornerRadius::same(7),
                                    t.row_hover.gamma_multiply(h),
                                );
                            }
                            let dim = !ok;
                            let fg = if is_sel {
                                Color32::WHITE
                            } else if dim {
                                t.text3
                            } else {
                                t.text
                            };
                            let fg2 = if is_sel {
                                Color32::from_white_alpha(210)
                            } else {
                                t.text2
                            };
                            let key_like = !e.is_dir
                                && pk.purpose == PickFor::Identity
                                && (e.name.starts_with("id_") || e.name.ends_with(".pem"))
                                && !e.name.ends_with(".pub");
                            let icon = if e.is_dir {
                                Icon::FolderFill
                            } else if key_like {
                                Icon::Key
                            } else {
                                Icon::File
                            };
                            let icol = if is_sel {
                                Color32::WHITE
                            } else if e.is_dir {
                                t.accent
                            } else if key_like {
                                t.warn
                            } else {
                                t.text3
                            };
                            w::paint_icon(
                                p,
                                Rect::from_center_size(
                                    Pos2::new(rect.left() + 18.0, rect.center().y),
                                    Vec2::splat(15.0),
                                ),
                                icon,
                                if e.hidden && !is_sel {
                                    icol.gamma_multiply(0.6)
                                } else {
                                    icol
                                },
                            );
                            let pr = ui.painter_at(rect);
                            w::paint_text_trunc(
                                ui,
                                &pr,
                                &e.name,
                                w::regular(13.0),
                                if e.hidden && !is_sel {
                                    fg.gamma_multiply(0.7)
                                } else {
                                    fg
                                },
                                rect.left() + 34.0,
                                rect.center().y,
                                name_w - 40.0,
                            );
                            if col_time > 0.0 {
                                let g = w::galley(
                                    ui,
                                    &e.mtime.map(format_mtime).unwrap_or_default(),
                                    w::regular(12.0),
                                    fg2,
                                );
                                p.galley(
                                    Pos2::new(
                                        rect.left() + name_w + 10.0,
                                        rect.center().y - g.size().y / 2.0,
                                    ),
                                    g,
                                    fg2,
                                );
                            }
                            let size = if e.is_dir {
                                "—".to_string()
                            } else {
                                e.size.map(format_bytes).unwrap_or_default()
                            };
                            let g = w::galley(ui, &size, w::regular(12.0), fg2);
                            p.galley(
                                Pos2::new(
                                    rect.right() - 10.0 - g.size().x,
                                    rect.center().y - g.size().y / 2.0,
                                ),
                                g,
                                fg2,
                            );
                            if resp.double_clicked() {
                                open = Some(i);
                            } else if resp.clicked() {
                                click = Some((i, ui.input(|i| i.modifiers)));
                            }
                        }
                    });
                pk.scroll_to = None;

                // click / double-click handling
                if let Some((i, m)) = click {
                    let e = &pk.entries[i];
                    if pk.selectable(e) {
                        let multi = pk.mode == PickMode::Files;
                        let path = e.path.clone();
                        if multi && m.shift {
                            if let Some(a) = pk.anchor {
                                let (ka, kb) = (
                                    vis.iter().position(|&x| x == a).unwrap_or(0),
                                    vis.iter().position(|&x| x == i).unwrap_or(0),
                                );
                                let (lo, hi) = (ka.min(kb), ka.max(kb));
                                pk.sel = vis[lo..=hi]
                                    .iter()
                                    .map(|&x| pk.entries[x].path.clone())
                                    .collect();
                            } else {
                                pk.sel = vec![path];
                                pk.anchor = Some(i);
                            }
                        } else if multi && m.command {
                            if let Some(pos) = pk.sel.iter().position(|p| p == &path) {
                                pk.sel.remove(pos);
                            } else {
                                pk.sel.push(path);
                            }
                            pk.anchor = Some(i);
                        } else {
                            pk.sel = vec![path];
                            pk.anchor = Some(i);
                        }
                    }
                }
                if let Some(i) = open {
                    let e = &pk.entries[i];
                    if e.is_dir {
                        nav = Some(e.path.clone());
                    } else if pk.mode != PickMode::Folder {
                        done = Some(vec![e.path.clone()]);
                    }
                }

                // keyboard: arrows move, Enter opens/confirms, Backspace goes up
                let typing = typing_at_start
                    || ui
                        .ctx()
                        .memory(|m| m.focused().is_some_and(|f| field_ids.contains(&f)));
                if !typing && ui.input(|i| i.key_pressed(Key::Backspace)) {
                    nav = pk.cwd.parent().map(|p| p.to_path_buf());
                }
                if !typing && !vis.is_empty() {
                    let (up, down, enter) = ui.input(|i| {
                        (
                            i.key_pressed(Key::ArrowUp),
                            i.key_pressed(Key::ArrowDown),
                            i.key_pressed(Key::Enter),
                        )
                    });
                    let cur = pk.anchor.and_then(|a| vis.iter().position(|&x| x == a));
                    if up || down {
                        let k = match (cur, down) {
                            (None, _) => 0,
                            (Some(c), true) => (c + 1).min(vis.len() - 1),
                            (Some(c), false) => c.saturating_sub(1),
                        };
                        let i = vis[k];
                        pk.anchor = Some(i);
                        pk.sel = if pk.selectable(&pk.entries[i]) {
                            vec![pk.entries[i].path.clone()]
                        } else {
                            Vec::new()
                        };
                        pk.scroll_to = Some(i);
                    }
                    if enter {
                        if let Some(i) = pk.anchor {
                            if pk.entries[i].is_dir && pk.mode != PickMode::Folder {
                                nav = Some(pk.entries[i].path.clone());
                            } else if !pk.result().is_empty() {
                                done = Some(pk.result());
                            }
                        }
                    }
                }

                // ---- footer ----
                ui.add_space(14.0);
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 8.0;
                    let mut hid = pk.show_hidden;
                    if w::switch(
                        ui,
                        &mut hid,
                        if zh {
                            "显示隐藏文件"
                        } else {
                            "Show hidden"
                        },
                    )
                    .changed()
                    {
                        pk.show_hidden = hid;
                    }
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        let res = pk.result();
                        let label = match pk.mode {
                            PickMode::Folder => {
                                if pk.sel.is_empty() {
                                    self.t("选择当前文件夹", "Use this folder").to_string()
                                } else {
                                    self.t("选择", "Choose").to_string()
                                }
                            }
                            PickMode::File => self.t("选择", "Choose").to_string(),
                            PickMode::Files => {
                                if res.len() > 1 {
                                    if zh {
                                        format!("选择 {} 项", res.len())
                                    } else {
                                        format!("Choose {} items", res.len())
                                    }
                                } else {
                                    self.t("选择", "Choose").to_string()
                                }
                            }
                        };
                        // While another folder is loading, "this folder" would
                        // still be the old one.
                        let ready = !res.is_empty() && !pk.loading();
                        if w::button_sized(ui, Kind::Primary, None, &label, ready, 96.0).clicked() {
                            done = Some(res);
                        }
                        if w::button(ui, Kind::Secondary, None, self.t("取消", "Cancel"), true)
                            .clicked()
                        {
                            cancel = true;
                        }
                        if pk.mode == PickMode::Files {
                            let hint = if zh {
                                "可选文件夹 · Ctrl / Shift 多选"
                            } else {
                                "Folders OK · Ctrl / Shift for several"
                            };
                            let g = w::galley(ui, hint, w::regular(11.5), t.text3);
                            let (r, _) = ui.allocate_exact_size(
                                Vec2::new(g.size().x + 6.0, w::CTRL_H),
                                Sense::hover(),
                            );
                            ui.painter().galley(
                                Pos2::new(r.left(), r.center().y - g.size().y / 2.0),
                                g,
                                t.text3,
                            );
                        }
                    });
                });
            });

        // Always stack above the sheet that opened us (egui raises a modal
        // to the top when it is clicked).
        ctx.move_to_top(modal.response.layer_id);
        if let Some(dir) = nav {
            pk.go(dir);
        }
        if modal.should_close() {
            cancel = true;
        }
        if let Some(paths) = done {
            if pk.mode != PickMode::Folder {
                self.picker_last = Some(pk.cwd.clone());
            }
            let purpose = pk.purpose;
            self.finish_picker(purpose, paths);
        } else if !cancel {
            self.picker = Some(pk);
        }
    }
}
