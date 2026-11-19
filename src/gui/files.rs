//! SFTP file browser for the active session.

use super::widgets::{self as w, Icon, Kind};
use super::*;
use egui::{Align, Color32, CornerRadius, Layout, Pos2, Rect, Sense, Stroke, UiBuilder, Vec2};
use std::future::Future;
use std::pin::Pin;
use std::thread;

type OpFuture = Pin<Box<dyn Future<Output = anyhow::Result<String>> + Send>>;

const ROW_H: f32 = 30.0;

/// Cached visible order of a folder listing (see `SessionSlot::file_view`).
pub(super) struct FileView {
    entries: Arc<Vec<session::RemoteDirEntry>>,
    filter: String,
    show_hidden: bool,
    sort: FileSort,
    asc: bool,
    /// Indices into `entries`, filtered and sorted ("..": never included).
    rows: Vec<usize>,
    /// Name → is a folder, for every entry.
    is_dir: std::collections::HashMap<String, bool>,
}

impl FileView {
    /// Bring `view` up to date; true when it was rebuilt.
    fn current(
        view: &mut Option<FileView>,
        entries: &Arc<Vec<session::RemoteDirEntry>>,
        filter: &str,
        show_hidden: bool,
        sort: FileSort,
        asc: bool,
    ) -> bool {
        if let Some(v) = view {
            if Arc::ptr_eq(&v.entries, entries)
                && v.filter == filter
                && v.show_hidden == show_hidden
                && v.sort == sort
                && v.asc == asc
            {
                return false;
            }
        }
        let is_dir = match view.take() {
            Some(v) if Arc::ptr_eq(&v.entries, entries) => v.is_dir,
            _ => entries.iter().map(|e| (e.name.clone(), e.is_dir)).collect(),
        };
        // Lower-case each name once, not once per comparison.
        let lower: Vec<String> = entries.iter().map(|e| e.name.to_lowercase()).collect();
        let mut rows: Vec<usize> = (0..entries.len())
            .filter(|&i| {
                let e = &entries[i];
                e.name != ".."
                    && (show_hidden || !e.name.starts_with('.'))
                    && (filter.is_empty() || lower[i].contains(filter))
            })
            .collect();
        rows.sort_by(|&a, &b| {
            let (ea, eb) = (&entries[a], &entries[b]);
            let dirs = eb.is_dir.cmp(&ea.is_dir);
            let ord = match sort {
                FileSort::Name => lower[a].cmp(&lower[b]),
                FileSort::Size => ea.size.unwrap_or(0).cmp(&eb.size.unwrap_or(0)),
                FileSort::Modified => ea.mtime.unwrap_or(0).cmp(&eb.mtime.unwrap_or(0)),
            };
            dirs.then(if asc { ord } else { ord.reverse() })
        });
        *view = Some(FileView {
            entries: Arc::clone(entries),
            filter: filter.to_string(),
            show_hidden,
            sort,
            asc,
            rows,
            is_dir,
        });
        true
    }
}

impl GuiApp {
    pub(super) fn slot_handle(&self, idx: usize) -> Option<Arc<session::SshHandle>> {
        let s = self.sessions.get(idx)?.shared.lock().unwrap();
        if s.connected {
            s.handle.clone()
        } else {
            None
        }
    }

    pub(super) fn refresh_files(&mut self, idx: usize, path: Option<String>) {
        let Some(handle) = self.slot_handle(idx) else {
            return;
        };
        let slot = &mut self.sessions[idx];
        slot.file_sel.clear();
        slot.file_anchor = None;
        slot.path_edit = None;
        let shared = Arc::clone(&slot.shared);
        let (target, gen) = {
            let mut s = shared.lock().unwrap();
            let t = path.unwrap_or_else(|| s.files.cwd.clone());
            s.files.gen += 1;
            let gen = s.files.gen;
            s.files.loading = true;
            s.files.error = false;
            s.files.status = format!(
                "{} {t}…",
                if self.lang == LangPref::Zh {
                    "正在读取"
                } else {
                    "Listing"
                }
            );
            (t, gen)
        };
        let runtime = Arc::clone(&self.runtime);
        let ctx = self.ctx.clone();
        let zh = self.zh();
        thread::spawn(move || {
            let result =
                runtime.block_on(async move { session::sftp_list(&handle, &target).await });
            if let Ok(mut s) = shared.lock() {
                if s.files.gen != gen {
                    // The user has navigated again since; this answer is stale.
                    return;
                }
                s.files.loading = false;
                match result {
                    Ok((cwd, entries)) => {
                        s.files.cwd = cwd;
                        s.files.entries = Arc::new(entries);
                        s.files.status = String::new();
                        s.files.error = false;
                    }
                    Err(e) => {
                        s.files.status =
                            format!("{}: {e:#}", if zh { "读取失败" } else { "List failed" });
                        s.files.error = true;
                    }
                }
            }
            ctx.request_repaint();
        });
    }

    /// Run a short SFTP operation, report it, then refresh the listing.
    /// One change runs at a time per tab (a second delete of the same
    /// items would only fail); `busy_msg` is shown while it runs.
    fn sftp_op(
        &mut self,
        idx: usize,
        busy_msg: Option<String>,
        op: Box<dyn FnOnce(Arc<session::SshHandle>) -> OpFuture + Send>,
    ) {
        let Some(handle) = self.slot_handle(idx) else {
            return;
        };
        let shared = Arc::clone(&self.sessions[idx].shared);
        let busy = shared.lock().unwrap().files.busy;
        if busy {
            self.toast_msg(
                self.t(
                    "上一个文件操作还在进行，请稍候",
                    "The previous file operation is still running",
                ),
                StatusKind::Info,
            );
            return;
        }
        let (cwd, gen) = {
            let mut s = shared.lock().unwrap();
            s.files.busy = true;
            if let Some(m) = busy_msg {
                s.files.status = m;
                s.files.error = false;
            }
            (s.files.cwd.clone(), s.files.gen)
        };
        let runtime = Arc::clone(&self.runtime);
        let ctx = self.ctx.clone();
        thread::spawn(move || {
            let shared2 = Arc::clone(&shared);
            let result = runtime.block_on(async move {
                let r = op(Arc::clone(&handle)).await;
                // Refresh even after a partial failure (e.g. some deletes worked).
                if let Ok((next, entries)) = session::sftp_list(&handle, &cwd).await {
                    apply_listing(&shared2, gen, next, entries);
                }
                r
            });
            if let Ok(mut s) = shared.lock() {
                s.files.busy = false;
                match result {
                    Ok(msg) => {
                        s.files.status = msg;
                        s.files.error = false;
                    }
                    Err(e) => {
                        s.files.status = format!("{e:#}");
                        s.files.error = true;
                    }
                }
            }
            ctx.request_repaint();
        });
    }

    pub(super) fn rename_remote(&mut self, idx: usize, cwd: &str, from: &str, to: &str) {
        let to = to.trim().to_string();
        if !safe_remote_name(from) || !safe_remote_name(&to) {
            self.toast_msg(
                self.t(
                    "请使用不含路径分隔符的名称",
                    "Use a simple name without path separators",
                ),
                StatusKind::Error,
            );
            return;
        }
        let (a, b) = (join_remote(cwd, from), join_remote(cwd, &to));
        let zh = self.zh();
        self.sessions[idx].file_sel = vec![to.clone()];
        self.sftp_op(
            idx,
            None,
            Box::new(move |h| {
                Box::pin(async move {
                    session::sftp_rename(&h, &a, &b).await?;
                    Ok(if zh {
                        format!("已重命名为 {to}")
                    } else {
                        format!("Renamed to {to}")
                    })
                })
            }),
        );
    }

    /// Delete entries of `dir` (folders recursively, links as links). Keeps
    /// going past failures and reports them all at the end.
    pub(super) fn delete_items(&mut self, idx: usize, dir: String, items: Vec<(String, bool)>) {
        let items: Vec<(String, bool)> = items
            .into_iter()
            .filter(|(n, _)| safe_remote_name(n))
            .collect();
        if items.is_empty() {
            return;
        }
        let zh = self.zh();
        self.sessions[idx].file_sel.clear();
        let busy = match (zh, items.len()) {
            (true, 1) => format!("正在删除 {}…", items[0].0),
            (true, n) => format!("正在删除 {n} 项…"),
            (false, 1) => format!("Deleting {}…", items[0].0),
            (false, n) => format!("Deleting {n} items…"),
        };
        self.sftp_op(
            idx,
            Some(busy),
            Box::new(move |h| {
                Box::pin(async move {
                    let n = items.len();
                    let paths = items
                        .iter()
                        .map(|(name, _)| join_remote(&dir, name))
                        .collect();
                    let results = session::sftp_remove_many(&h, paths).await?;
                    let failed: Vec<String> = results
                        .iter()
                        .zip(&items)
                        .filter_map(|((_, r), (name, _))| {
                            r.as_ref().err().map(|e| format!("{name}: {e:#}"))
                        })
                        .collect();
                    if !failed.is_empty() {
                        let ok = n - failed.len();
                        anyhow::bail!(
                            "{}{}",
                            if zh {
                                format!("已删除 {ok} 项，{} 项失败 — ", failed.len())
                            } else {
                                format!("Deleted {ok}, {} failed — ", failed.len())
                            },
                            failed
                                .iter()
                                .take(5)
                                .cloned()
                                .collect::<Vec<_>>()
                                .join("; ")
                                + if failed.len() > 5 { " …" } else { "" }
                        );
                    }
                    Ok(match (zh, n) {
                        (true, 1) => format!("已删除 {}", items[0].0),
                        (true, _) => format!("已删除 {n} 项"),
                        (false, 1) => format!("Deleted {}", items[0].0),
                        (false, _) => format!("Deleted {n} items"),
                    })
                })
            }),
        );
    }

    /// Create an empty file and open it in the editor.
    pub(super) fn create_remote_file(&mut self, idx: usize, dir: &str, name: &str) {
        let name = name.trim().to_string();
        if !safe_remote_name(&name) {
            self.toast_msg(
                self.t(
                    "请使用不含路径分隔符的名称",
                    "Use a simple name without path separators",
                ),
                StatusKind::Error,
            );
            return;
        }
        // The editor opens in a "creating…" state and only becomes editable once
        // the server confirms the new file (never over an existing one).
        self.sessions[idx].file_sel = vec![name.clone()];
        self.open_file_editor(idx, dir, &name, true);
    }

    /// Open a remote text file in the built-in editor.
    pub(super) fn open_file_editor(&mut self, idx: usize, cwd: &str, name: &str, is_new: bool) {
        let Some(handle) = self.slot_handle(idx) else {
            return;
        };
        let cwd = cwd.to_string();
        let path = join_remote(&cwd, name);
        let io = Arc::new(Mutex::new(EditorIo::default()));
        if is_new {
            let io2 = Arc::clone(&io);
            let runtime = Arc::clone(&self.runtime);
            let ctx = self.ctx.clone();
            let p = path.clone();
            let shared = Arc::clone(&self.sessions[idx].shared);
            let (dir, gen) = (cwd.clone(), shared.lock().unwrap().files.gen);
            thread::spawn(move || {
                let r = runtime.block_on(async {
                    session::sftp_create_file(&handle, &p).await?;
                    if let Ok((next, entries)) = session::sftp_list(&handle, &dir).await {
                        apply_listing(&shared, gen, next, entries);
                    }
                    // Read it back for its stamp (it's empty).
                    session::sftp_read_text(&handle, &p, 4 * 1024 * 1024).await
                });
                io2.lock().unwrap().loaded = Some(r.map_err(|e| format!("{e:#}")));
                ctx.request_repaint();
            });
        } else {
            self.spawn_editor_load(handle, path.clone(), Arc::clone(&io));
        }
        self.file_editor = Some(FileEditor {
            tab_uid: self.sessions[idx].uid,
            path,
            name: name.to_string(),
            text: String::new(),
            original: String::new(),
            io,
            loading: true,
            saving: false,
            error: String::new(),
            confirm_discard: false,
            focus: true,
            load_failed: false,
            crlf: false,
            dirty: false,
            line_count: 1,
            stamp: None,
            conflict: false,
            saving_text: None,
            find_open: false,
            replace_open: false,
            find_query: String::new(),
            replace_text: String::new(),
            find_case: false,
            find_focus: false,
            find_cur: 0,
            pending_select: None,
        });
    }

    fn spawn_editor_load(
        &self,
        handle: Arc<session::SshHandle>,
        path: String,
        io: Arc<Mutex<EditorIo>>,
    ) {
        let runtime = Arc::clone(&self.runtime);
        let ctx = self.ctx.clone();
        thread::spawn(move || {
            let r = runtime.block_on(async move {
                session::sftp_read_text(&handle, &path, 4 * 1024 * 1024).await
            });
            io.lock().unwrap().loaded = Some(r.map_err(|e| format!("{e:#}")));
            ctx.request_repaint();
        });
    }

    /// Live SSH handle of the editor's tab, or an error message.
    fn editor_handle(&self, tab_uid: u64) -> Result<Arc<session::SshHandle>, String> {
        let zh = self.lang == LangPref::Zh;
        let Some(idx) = self.sessions.iter().position(|s| s.uid == tab_uid) else {
            return Err(if zh {
                "会话已关闭".into()
            } else {
                "session closed".into()
            });
        };
        let s = self.sessions[idx].shared.lock().unwrap();
        match (&s.handle, s.connected) {
            (Some(h), true) => Ok(Arc::clone(h)),
            _ => Err(if zh {
                "会话已断开，无法保存".into()
            } else {
                "Session disconnected".into()
            }),
        }
    }

    /// Save the editor. `force` skips the "changed on the server" check.
    fn save_file_editor(&mut self, force: bool) {
        let Some(tab_uid) = self.file_editor.as_ref().map(|e| e.tab_uid) else {
            return;
        };
        let handle = self.editor_handle(tab_uid);
        let Some(ed) = self.file_editor.as_mut() else {
            return;
        };
        let handle = match handle {
            Ok(h) => h,
            Err(e) => {
                ed.error = e;
                return;
            }
        };
        ed.saving = true;
        ed.conflict = false;
        ed.error.clear();
        let data = if ed.crlf {
            ed.text
                .replace("\r\n", "\n")
                .replace('\n', "\r\n")
                .into_bytes()
        } else {
            ed.text.clone().into_bytes()
        };
        ed.saving_text = Some(ed.text.clone());
        let path = ed.path.clone();
        let expect = if force { None } else { ed.stamp };
        let io = Arc::clone(&ed.io);
        let runtime = Arc::clone(&self.runtime);
        let ctx = self.ctx.clone();
        thread::spawn(move || {
            let r = runtime.block_on(async move {
                session::sftp_write_file(&handle, &path, &data, expect).await
            });
            io.lock().unwrap().saved = Some(r.map_err(|e| {
                (
                    e.downcast_ref::<session::RemoteChanged>().is_some(),
                    format!("{e:#}"),
                )
            }));
            ctx.request_repaint();
        });
    }

    /// Throw away the buffer and load the server's current version.
    fn reload_file_editor(&mut self) {
        let Some(tab_uid) = self.file_editor.as_ref().map(|e| e.tab_uid) else {
            return;
        };
        let handle = self.editor_handle(tab_uid);
        let Some(ed) = self.file_editor.as_mut() else {
            return;
        };
        match handle {
            Ok(h) => {
                ed.loading = true;
                ed.conflict = false;
                ed.error.clear();
                ed.dirty = false;
                ed.confirm_discard = false;
                let (path, io) = (ed.path.clone(), Arc::clone(&ed.io));
                self.spawn_editor_load(h, path, io);
            }
            Err(e) => ed.error = e,
        }
    }

    /// One transfer at a time per tab: a second one would overwrite the
    /// first one's progress. Tell the user instead.
    fn transfer_busy(&mut self, idx: usize) -> bool {
        let busy = self.sessions[idx].shared.lock().unwrap().transfer.active;
        if busy {
            self.toast_msg(
                self.t(
                    "已有传输正在进行，请等它完成",
                    "A transfer is already running — please wait for it to finish",
                ),
                StatusKind::Info,
            );
        }
        busy
    }

    /// Download one or more files/folders into the download folder.
    fn download_many(&mut self, idx: usize, names: Vec<String>) {
        let Some(handle) = self.slot_handle(idx) else {
            return;
        };
        let names: Vec<String> = names.into_iter().filter(|n| safe_remote_name(n)).collect();
        if names.is_empty() {
            return;
        }
        if self.transfer_busy(idx) {
            return;
        }
        let shared = Arc::clone(&self.sessions[idx].shared);
        let cwd = shared.lock().unwrap().files.cwd.clone();
        let dir = self.download_dir();
        let zh = self.zh();
        let count = names.len();
        let label = if count == 1 {
            names[0].clone()
        } else if zh {
            format!("{} 等 {count} 项", names[0])
        } else {
            format!("{} + {} more", names[0], count - 1)
        };
        let runtime = Arc::clone(&self.runtime);
        let ctx = self.ctx.clone();
        let cancel = start_transfer(&shared, &label, zh, true);
        // total size from the listing (best effort; folders start at 0 until enumerated)
        let grand: u64 = {
            let s = shared.lock().unwrap();
            names
                .iter()
                .filter_map(|n| {
                    s.files
                        .entries
                        .iter()
                        .find(|e| &e.name == n)
                        .and_then(|e| e.size)
                })
                .sum()
        };
        let remotes: Vec<String> = names.iter().map(|n| join_remote(&cwd, n)).collect();
        thread::spawn(move || {
            let progress_shared = Arc::clone(&shared);
            let ctx2 = ctx.clone();
            let job_res = runtime.block_on(async {
                let (tx, mut rx) = mpsc::unbounded_channel::<session::TransferProgress>();
                let h = Arc::clone(&handle);
                let d = dir.clone();
                let job = tokio::spawn(async move {
                    session::sftp_download_entries(&h, &remotes, &d, tx, cancel).await
                });
                while let Some(p) = rx.recv().await {
                    if let Ok(mut s) = progress_shared.lock() {
                        let was_scanning = s.transfer.scanning_files.is_some();
                        s.transfer.scanning_files = p.scanning_files;
                        if was_scanning && p.scanning_files.is_none() {
                            s.transfer.started = Some(Instant::now());
                        }
                        s.transfer.done = p.done;
                        s.transfer.total = grand.max(p.total.max(p.done));
                    }
                    ctx2.request_repaint();
                }
                job.await
            });
            match job_res {
                Ok((outcome, Ok(()))) => {
                    finish_transfer(&shared, Ok(Ok(())), zh, outcome.saved_path.clone());
                    if let Ok(mut s) = shared.lock() {
                        if (count > 1 || outcome.files_done > 1) && outcome.files_done > 0 {
                            s.transfer.label = if zh {
                                format!(
                                    "已下载 {} 个文件到 {}",
                                    outcome.files_done,
                                    outcome.saved_path.as_deref().unwrap_or(&dir).display()
                                )
                            } else {
                                format!(
                                    "Downloaded {} files to {}",
                                    outcome.files_done,
                                    outcome.saved_path.as_deref().unwrap_or(&dir).display()
                                )
                            };
                        }
                        if !outcome.skipped.is_empty() {
                            let note = super::skipped_note(&outcome.skipped, zh);
                            s.transfer.label = format!("{} · {note}", s.transfer.label);
                            s.transfer.error = true;
                        }
                    }
                }
                Ok((outcome, Err(e))) => {
                    let partial_dir = (outcome.files_done > 0)
                        .then(|| outcome.saved_path.clone().unwrap_or_else(|| dir.clone()));
                    let cancelled = e.downcast_ref::<session::Cancelled>().is_some();
                    let err_msg = format!("{e:#}");
                    finish_transfer(&shared, Ok(Err(e)), zh, partial_dir.clone());
                    if outcome.files_done > 0 {
                        if let Ok(mut s) = shared.lock() {
                            s.transfer.last_local = partial_dir;
                            s.transfer.label = if cancelled {
                                if zh {
                                    format!(
                                        "已取消（已完成 {}/{} 个文件）",
                                        outcome.files_done, outcome.files_total
                                    )
                                } else {
                                    format!(
                                        "Cancelled ({}/{} files saved)",
                                        outcome.files_done, outcome.files_total
                                    )
                                }
                            } else if zh {
                                format!(
                                    "已下载 {}/{} 个文件 · 失败：{err_msg}",
                                    outcome.files_done, outcome.files_total
                                )
                            } else {
                                format!(
                                    "Downloaded {}/{} files · failed: {err_msg}",
                                    outcome.files_done, outcome.files_total
                                )
                            };
                        }
                    }
                }
                Err(e) => finish_transfer(&shared, Err(e), zh, None),
            }
            ctx.request_repaint();
        });
    }

    pub(super) fn mkdir_remote(&mut self, idx: usize, cwd: &str, name: &str) {
        let name = name.trim().to_string();
        if !safe_remote_name(&name) {
            self.toast_msg(
                self.t(
                    "请使用不含路径分隔符的名称",
                    "Use a simple name without path separators",
                ),
                StatusKind::Error,
            );
            return;
        }
        let path = join_remote(cwd, &name);
        let zh = self.zh();
        self.sftp_op(
            idx,
            None,
            Box::new(move |h| {
                Box::pin(async move {
                    session::sftp_mkdir(&h, &path).await?;
                    Ok(if zh {
                        format!("已创建 {name}")
                    } else {
                        format!("Created {name}")
                    })
                })
            }),
        );
    }

    pub(super) fn chmod_remote(&mut self, idx: usize, dir: &str, name: &str, mode: u32) {
        let path = join_remote(dir, name);
        let msg = if self.zh() {
            format!("已修改权限 {:03o}", mode & 0o7777)
        } else {
            format!("Permissions set to {:03o}", mode & 0o7777)
        };
        self.sftp_op(
            idx,
            Some(self.t("正在修改权限…", "Updating permissions…").into()),
            Box::new(move |h| {
                Box::pin(async move {
                    session::sftp_chmod(&h, &path, mode).await?;
                    Ok(msg)
                })
            }),
        );
    }

    pub(super) fn cd_in_terminal(&mut self, idx: usize, remote_dir: &str) {
        let quoted = shell_single_quote(remote_dir);
        let cmd = format!("cd {quoted}\r");
        self.send_bytes(idx, cmd.into_bytes());
        if let Some(slot) = self.sessions.get_mut(idx) {
            slot.view = CenterView::Terminal;
            slot.term.scroll_to_bottom();
        }
        self.focus_term = true;
    }

    pub(super) fn ui_chmod_prompt(&mut self, ctx: &egui::Context) {
        let Some(mut ch) = self.chmod_prompt.take() else {
            return;
        };
        let t = self.tok();
        let zh = self.zh();
        let mut cancel = false;
        let mut apply = false;

        let resp = egui::Modal::new(egui::Id::new("chmod_prompt"))
            .frame(super::panels::sheet_frame(&t))
            .show(ctx, |ui| {
                ui.set_width(340.0);
                let g = w::galley(
                    ui,
                    if zh { "修改权限" } else { "Permissions" },
                    w::semibold(16.0),
                    t.text,
                );
                let (r, _) =
                    ui.allocate_exact_size(Vec2::new(ui.available_width(), 24.0), Sense::hover());
                ui.painter().galley(
                    Pos2::new(r.left(), r.center().y - g.size().y / 2.0),
                    g,
                    t.text,
                );
                let sub = w::galley(ui, &ch.name, egui::FontId::monospace(12.0), t.text2);
                let (r, _) =
                    ui.allocate_exact_size(Vec2::new(ui.available_width(), 18.0), Sense::hover());
                ui.painter().galley(
                    Pos2::new(r.left(), r.center().y - sub.size().y / 2.0),
                    sub,
                    t.text2,
                );
                ui.add_space(12.0);

                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 6.0;
                    for (lbl, val) in [
                        ("600", 0o600),
                        ("644", 0o644),
                        ("755", 0o755),
                        ("777", 0o777),
                    ] {
                        if w::button_sized(ui, Kind::Secondary, None, lbl, true, 54.0).clicked() {
                            ch.mode = val;
                            ch.octal = format!("{val:03o}");
                        }
                    }
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        let sym = mode_rwx_string(ch.mode);
                        let g = w::galley(ui, &sym, egui::FontId::monospace(12.0), t.text2);
                        let (r, _) = ui.allocate_exact_size(g.size(), Sense::hover());
                        ui.painter().galley(r.min, g, t.text2);
                    });
                });
                ui.add_space(10.0);

                w::caption(
                    ui,
                    if zh {
                        "八进制权限值"
                    } else {
                        "OCTAL MODE"
                    },
                );
                let r_oct = w::Field::new(&mut ch.octal)
                    .mono()
                    .hint("755")
                    .id(egui::Id::new("chmod_octal"))
                    .show(ui);
                if ch.focus {
                    r_oct.request_focus();
                    ch.focus = false;
                }
                if r_oct.changed() {
                    if let Ok(parsed) = u32::from_str_radix(ch.octal.trim(), 8) {
                        ch.mode = parsed & 0o7777;
                    }
                }
                ui.add_space(12.0);

                let rows = [
                    (if zh { "所有者" } else { "Owner" }, 6u32),
                    (if zh { "用户组" } else { "Group" }, 3u32),
                    (if zh { "其他人" } else { "Others" }, 0u32),
                ];
                egui::Grid::new("chmod_matrix")
                    .num_columns(4)
                    .spacing([18.0, 8.0])
                    .show(ui, |ui| {
                        ui.label("");
                        for h in [
                            if zh { "读 (r)" } else { "Read" },
                            if zh { "写 (w)" } else { "Write" },
                            if zh { "执行 (x)" } else { "Exec" },
                        ] {
                            ui.label(egui::RichText::new(h).size(12.0).color(t.text2));
                        }
                        ui.end_row();
                        for (label, shift) in rows {
                            ui.label(egui::RichText::new(label).size(12.5).color(t.text));
                            for bit_mask in [4u32, 2, 1] {
                                let mask = bit_mask << shift;
                                let mut on = (ch.mode & mask) != 0;
                                if ui.checkbox(&mut on, "").changed() {
                                    if on {
                                        ch.mode |= mask;
                                    } else {
                                        ch.mode &= !mask;
                                    }
                                    ch.octal = if ch.mode > 0o777 {
                                        format!("{:04o}", ch.mode)
                                    } else {
                                        format!("{:03o}", ch.mode)
                                    };
                                }
                            }
                            ui.end_row();
                        }
                    });

                let parsed_ok = u32::from_str_radix(ch.octal.trim(), 8)
                    .ok()
                    .filter(|&v| v <= 0o7777);
                if (r_oct.has_focus() || r_oct.lost_focus())
                    && ui.input(|i| i.key_pressed(egui::Key::Enter))
                    && parsed_ok.is_some()
                {
                    apply = true;
                }

                ui.add_space(16.0);
                let bw = (ui.available_width() - 10.0) / 2.0;
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 10.0;
                    if w::button_sized(
                        ui,
                        Kind::Secondary,
                        None,
                        if zh { "取消" } else { "Cancel" },
                        true,
                        bw,
                    )
                    .clicked()
                    {
                        cancel = true;
                    }
                    if w::button_sized(
                        ui,
                        Kind::Primary,
                        None,
                        if zh { "应用" } else { "Apply" },
                        parsed_ok.is_some(),
                        bw,
                    )
                    .clicked()
                    {
                        apply = true;
                    }
                });
            });

        if resp.should_close() || cancel {
            return;
        }
        if apply {
            let final_mode = u32::from_str_radix(ch.octal.trim(), 8)
                .map(|v| v & 0o7777)
                .unwrap_or(ch.mode & 0o7777);
            if let Some(i) = self.slot_index(ch.tab_uid) {
                self.chmod_remote(i, &ch.dir, &ch.name, final_mode);
            }
            return;
        }
        self.chmod_prompt = Some(ch);
    }

    pub(super) fn upload_paths(&mut self, idx: usize, paths: Vec<PathBuf>) {
        let Some(handle) = self.slot_handle(idx) else {
            self.toast_msg(
                self.t("请先连接会话", "Connect a session first"),
                StatusKind::Error,
            );
            return;
        };
        if paths.is_empty() {
            return;
        }
        if self.transfer_busy(idx) {
            return;
        }
        let shared = Arc::clone(&self.sessions[idx].shared);
        let (cwd, gen) = {
            let s = shared.lock().unwrap();
            (s.files.cwd.clone(), s.files.gen)
        };
        let zh = self.zh();
        let runtime = Arc::clone(&self.runtime);
        let ctx = self.ctx.clone();
        let count = paths.len();
        let first = paths[0]
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "file".into());
        let label = if count > 1 {
            if zh {
                format!("{first} 等 {count} 项")
            } else {
                format!("{first} + {} more", count - 1)
            }
        } else {
            first
        };
        let cancel = start_transfer(&shared, &label, zh, false);
        thread::spawn(move || {
            // Walking a big folder can take a moment — do it off the UI thread.
            let super::UploadPlan {
                dirs,
                items,
                skipped,
            } = plan_upload(&paths, &cwd);
            if dirs.is_empty() && items.is_empty() {
                if let Ok(mut s) = shared.lock() {
                    s.transfer.active = false;
                    s.transfer.error = true;
                    s.transfer.label = if !skipped.is_empty() {
                        super::skipped_note(&skipped, zh)
                    } else if zh {
                        "没有可上传的文件".into()
                    } else {
                        "Nothing to upload".into()
                    };
                }
                ctx.request_repaint();
                return;
            }
            let progress_shared = Arc::clone(&shared);
            let ctx2 = ctx.clone();
            let grand: u64 = items.iter().map(|i| i.size).sum();
            if let Ok(mut s) = shared.lock() {
                s.transfer.total = grand;
            }
            let result = runtime.block_on(async move {
                let (tx, mut rx) = mpsc::unbounded_channel::<session::TransferProgress>();
                let h = Arc::clone(&handle);
                let job = tokio::spawn(async move {
                    session::sftp_upload_tree(&h, &dirs, &items, tx, cancel).await
                });
                while let Some(p) = rx.recv().await {
                    if let Ok(mut s) = progress_shared.lock() {
                        s.transfer.done = p.done;
                        s.transfer.total = p.total;
                    }
                    ctx2.request_repaint();
                }
                let r = job.await;
                if let Ok((next, entries)) = session::sftp_list(&handle, &cwd).await {
                    apply_listing(&progress_shared, gen, next, entries);
                }
                r
            });
            finish_transfer(&shared, result, zh, None);
            if !skipped.is_empty() {
                if let Ok(mut s) = shared.lock() {
                    let note = super::skipped_note(&skipped, zh);
                    s.transfer.label = format!("{} · {note}", s.transfer.label);
                    s.transfer.error = true;
                }
            }
            ctx.request_repaint();
        });
    }

    fn pick_upload(&mut self, idx: usize) {
        let uid = self.sessions[idx].uid;
        self.open_picker(super::picker::PickFor::Upload(uid));
    }

    pub(super) fn ui_files(&mut self, ui: &mut egui::Ui) {
        // Whether a text field had focus when this frame started: Enter in a
        // field releases focus during the frame but is not consumed, so the
        // list must not react to that same key press.
        let text_focus_at_start = ui.ctx().memory(|m| m.focused().is_some());
        let t = self.tok();
        let zh = self.zh();
        let idx = self.active.min(self.sessions.len() - 1);
        let (connected, cwd, status, status_err, loading, entries, transfer) = {
            let s = self.sessions[idx].shared.lock().unwrap();
            (
                s.connected,
                s.files.cwd.clone(),
                s.files.status.clone(),
                s.files.error,
                s.files.loading,
                s.files.entries.clone(),
                s.transfer.clone(),
            )
        };

        if !connected {
            ui.add_space(ui.available_height() * 0.32);
            ui.vertical_centered(|ui| {
                let (r, _) = ui.allocate_exact_size(Vec2::splat(34.0), Sense::hover());
                w::paint_icon(ui.painter(), r, Icon::Folder, t.text3);
                ui.add_space(10.0);
                let g = w::galley(
                    ui,
                    self.t(
                        "连接后即可浏览服务器上的文件",
                        "Connect to browse files on the server",
                    ),
                    w::regular(13.0),
                    t.text2,
                );
                let (r, _) = ui.allocate_exact_size(g.size(), Sense::hover());
                ui.painter().galley(r.min, g, t.text2);
            });
            return;
        }
        if entries.is_empty() && !loading && cwd == "~" && status.is_empty() {
            self.refresh_files(idx, None);
        }

        let mut nav: Option<String> = None;
        let mut want_cd: Option<String> = None;
        let mut want_upload = false;
        let mut want_download: Vec<String> = Vec::new();
        let tab_uid = self.sessions[idx].uid;
        let view = {
            let slot = &mut self.sessions[idx];
            let filter = slot.file_filter.trim().to_lowercase();
            let rebuilt = FileView::current(
                &mut slot.file_view,
                &entries,
                &filter,
                slot.show_hidden,
                slot.sort,
                slot.sort_asc,
            );
            // Cheap: the view is moved out for this frame and put back below.
            let view = slot.file_view.take().expect("built above");
            // Only what is on screen stays selected: a filter or the
            // hidden-files switch must never leave unseen entries selected
            // for Delete / Download.
            if rebuilt && !slot.file_sel.is_empty() {
                let shown: std::collections::HashSet<&str> = view
                    .rows
                    .iter()
                    .map(|&i| entries[i].name.as_str())
                    .collect();
                slot.file_sel.retain(|n| shown.contains(n.as_str()));
                if slot
                    .file_anchor
                    .as_deref()
                    .is_some_and(|a| !shown.contains(a))
                {
                    slot.file_anchor = None;
                }
            }
            view
        };
        let is_dir_name = |n: &str| view.is_dir.get(n) == Some(&true);

        // ---------- toolbar ----------
        ui.horizontal(|ui| {
            ui.set_height(w::CTRL_H);
            ui.spacing_mut().item_spacing.x = 2.0;
            if w::icon_button(
                ui,
                Icon::ArrowUp,
                self.t("上一级", "Enclosing folder"),
                !loading && cwd != "/",
            )
            .clicked()
            {
                nav = Some(parent_dir(&cwd));
            }
            if w::icon_button(ui, Icon::Home, self.t("主目录", "Home"), !loading).clicked() {
                nav = Some(".".into());
            }
            if w::icon_button(ui, Icon::Refresh, self.t("刷新", "Refresh"), !loading).clicked() {
                nav = Some(cwd.clone());
            }
            if w::icon_button(
                ui,
                Icon::Terminal,
                self.t("在终端中进入此目录", "Open in terminal"),
                connected,
            )
            .clicked()
            {
                want_cd = Some(cwd.clone());
            }
            ui.add_space(8.0);
            ui.spacing_mut().item_spacing.x = 8.0;

            let right_w = 290.0;
            let path_w = (ui.available_width() - right_w).max(140.0);
            let edit_tip = self.t("输入路径", "Go to path");
            let slot = &mut self.sessions[idx];
            if let Some(edit) = slot.path_edit.as_mut() {
                let id = egui::Id::new(("path_edit", tab_uid));
                let r = w::Field::new(edit).mono().width(path_w).id(id).show(ui);
                if !r.has_focus() && !r.lost_focus() {
                    r.request_focus();
                }
                if r.lost_focus() {
                    if ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                        nav = Some(edit.trim().to_string());
                    } else {
                        slot.path_edit = None;
                    }
                }
            } else {
                // Breadcrumb capsule
                let (pr, presp) =
                    ui.allocate_exact_size(Vec2::new(path_w, w::CTRL_H), Sense::click());
                ui.painter()
                    .rect_filled(pr, CornerRadius::same(7), t.control);
                let mut child = ui.new_child(
                    UiBuilder::new()
                        .max_rect(pr.shrink2(Vec2::new(8.0, 0.0)))
                        .layout(Layout::left_to_right(Align::Center)),
                );
                child.spacing_mut().item_spacing.x = 0.0;
                let parts: Vec<&str> = cwd.split('/').filter(|p| !p.is_empty()).collect();
                // Keep the tail visible when the path is long.
                let max_parts = 6;
                let skip = parts.len().saturating_sub(max_parts);
                let mut acc = String::new();
                if crumb(&mut child, &t, "/", parts.is_empty()).clicked() {
                    nav = Some("/".into());
                }
                for (i, part) in parts.iter().enumerate() {
                    acc.push('/');
                    acc.push_str(part);
                    if i < skip {
                        if i == 0 {
                            crumb_sep(&mut child, &t);
                            let _ = crumb(&mut child, &t, "…", false);
                        }
                        continue;
                    }
                    if i > 0 || skip > 0 {
                        crumb_sep(&mut child, &t);
                    } else {
                        // after the root "/"
                    }
                    let last = i + 1 == parts.len();
                    if crumb(&mut child, &t, part, last).clicked() && !last {
                        nav = Some(acc.clone());
                    }
                }
                if presp.double_clicked() || presp.clicked() {
                    slot.path_edit = Some(cwd.clone());
                }
                let _ = presp.on_hover_text(edit_tip);
            }

            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                let dl: Vec<String> = self.sessions[idx].file_sel.clone();
                let dl_label = if dl.len() > 1 {
                    if zh {
                        format!("下载 {} 项", dl.len())
                    } else {
                        format!("Download {}", dl.len())
                    }
                } else {
                    self.t("下载", "Download").to_string()
                };
                if w::button(
                    ui,
                    Kind::Secondary,
                    Some(Icon::Download),
                    &dl_label,
                    !dl.is_empty() && !transfer.active,
                )
                .clicked()
                {
                    want_download = dl.clone();
                }
                if w::button(
                    ui,
                    Kind::Secondary,
                    Some(Icon::Upload),
                    self.t("上传", "Upload"),
                    !transfer.active,
                )
                .on_hover_text(self.t("也可以直接把文件拖进窗口", "Or drop files onto the window"))
                .clicked()
                {
                    want_upload = true;
                }
                let plus = w::icon_button(ui, Icon::Plus, self.t("新建", "New"), true);
                w::popover(
                    ui,
                    &plus,
                    egui::Id::new(("files_new", tab_uid)),
                    180.0,
                    |ui| {
                        if w::menu_item(ui, if zh { "新建文件…" } else { "New file…" }, "", false)
                        {
                            self.text_prompt = Some(TextPrompt::NewRemoteFile {
                                tab_uid,
                                dir: cwd.clone(),
                                name: String::new(),
                            });
                            ui.memory_mut(|m| m.close_popup());
                        }
                        if w::menu_item(
                            ui,
                            if zh {
                                "新建文件夹…"
                            } else {
                                "New folder…"
                            },
                            "",
                            false,
                        ) {
                            self.text_prompt = Some(TextPrompt::NewRemoteDir {
                                tab_uid,
                                dir: cwd.clone(),
                                name: String::new(),
                            });
                            ui.memory_mut(|m| m.close_popup());
                        }
                    },
                );
            });
        });

        // ---------- filter row ----------
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            ui.set_height(w::CTRL_H);
            let slot = &mut self.sessions[idx];
            w::Field::new(&mut slot.file_filter)
                .icon(Icon::Search)
                .hint(if zh { "筛选" } else { "Filter" })
                .width(220.0)
                .clearable()
                .show(ui);
            ui.add_space(6.0);
            w::switch(
                ui,
                &mut slot.show_hidden,
                if zh { "隐藏文件" } else { "Hidden files" },
            );
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if loading {
                    ui.add(egui::Spinner::new().size(14.0).color(t.text3));
                } else if !status.is_empty() {
                    let avail = ui.available_width();
                    let (r, _) =
                        ui.allocate_exact_size(Vec2::new(avail, w::CTRL_H), Sense::hover());
                    let g = w::galley(
                        ui,
                        &status,
                        w::regular(12.0),
                        if status_err { t.err } else { t.text3 },
                    );
                    let x = (r.right() - g.size().x).max(r.left());
                    w::paint_text_trunc(
                        ui,
                        &ui.painter_at(r),
                        &status,
                        w::regular(12.0),
                        if status_err { t.err } else { t.text3 },
                        x,
                        r.center().y,
                        r.right() - x,
                    );
                }
            });
        });
        ui.add_space(10.0);

        // ---------- transfer ----------
        let mut reveal: Option<PathBuf> = None;
        let mut dismiss = false;
        if !transfer.label.is_empty() {
            let frac_target = if transfer.total > 0 {
                transfer.done as f32 / transfer.total as f32
            } else {
                0.0
            };
            let frac = if transfer.active {
                ui.ctx().animate_value_with_time(
                    egui::Id::new(("xfer", tab_uid)),
                    frac_target,
                    0.25,
                )
            } else {
                1.0
            };
            let color = if transfer.error {
                t.err
            } else if transfer.active {
                t.accent
            } else {
                t.ok
            };
            egui::Frame::NONE
                .fill(if t.dark { t.card } else { t.chrome })
                .corner_radius(CornerRadius::same(10))
                .inner_margin(egui::Margin {
                    left: 12,
                    right: 6,
                    top: 6,
                    bottom: 8,
                })
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.horizontal(|ui| {
                        ui.set_height(w::CTRL_H);
                        let icon = if transfer.error {
                            Icon::Warning
                        } else if !transfer.active {
                            Icon::Check
                        } else if transfer.is_download {
                            Icon::Download
                        } else {
                            Icon::Upload
                        };
                        let (ir, _) = ui.allocate_exact_size(Vec2::splat(16.0), Sense::hover());
                        w::paint_icon(ui.painter(), ir, icon, color);
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            ui.spacing_mut().item_spacing.x = 4.0;
                            if !transfer.active
                                && w::icon_button(ui, Icon::Close, self.t("关闭", "Dismiss"), true)
                                    .clicked()
                            {
                                dismiss = true;
                            }
                            if transfer.active
                                && w::button(ui, Kind::Plain, None, self.t("取消", "Cancel"), true)
                                    .clicked()
                            {
                                transfer
                                    .cancel
                                    .store(true, std::sync::atomic::Ordering::Relaxed);
                            }
                            if let Some(p) = &transfer.last_local {
                                if w::button(
                                    ui,
                                    Kind::Plain,
                                    None,
                                    self.t("在文件夹中显示", "Show in folder"),
                                    true,
                                )
                                .clicked()
                                {
                                    reveal = Some(p.clone());
                                }
                            }
                            if transfer.active {
                                let txt = if let Some(found) = transfer.scanning_files {
                                    if transfer.total > 0 {
                                        if zh {
                                            format!(
                                                "正在扫描目录… 已发现 {found} 个文件 ({})",
                                                format_bytes(transfer.total)
                                            )
                                        } else {
                                            format!(
                                                "Scanning… {found} files ({})",
                                                format_bytes(transfer.total)
                                            )
                                        }
                                    } else if zh {
                                        format!("正在扫描目录… 已发现 {found} 个文件")
                                    } else {
                                        format!("Scanning… {found} files")
                                    }
                                } else {
                                    let speed = transfer
                                        .started
                                        .map(|s| {
                                            transfer.done as f64
                                                / s.elapsed().as_secs_f64().max(0.1)
                                        })
                                        .unwrap_or(0.0);
                                    format!(
                                        "{} / {} · {}",
                                        format_bytes(transfer.done),
                                        format_bytes(transfer.total),
                                        format_rate(speed)
                                    )
                                };
                                let g = w::galley(ui, &txt, egui::FontId::monospace(11.5), t.text2);
                                let mid = w::optical_mid(ui, &g);
                                let (r, _) = ui.allocate_exact_size(
                                    g.size() + Vec2::new(6.0, 0.0),
                                    Sense::hover(),
                                );
                                ui.painter().galley(
                                    Pos2::new(r.left(), r.center().y - mid),
                                    g,
                                    t.text2,
                                );
                            }
                            let avail = ui.available_width();
                            let (r, tr) =
                                ui.allocate_exact_size(Vec2::new(avail, w::CTRL_H), Sense::hover());
                            w::paint_text_trunc(
                                ui,
                                &ui.painter_at(r),
                                &transfer.label,
                                w::regular(12.5),
                                if transfer.error { t.err } else { t.text },
                                r.left() + 2.0,
                                r.center().y,
                                r.width() - 4.0,
                            );
                            tr.on_hover_text(&transfer.label);
                        });
                    });
                    let (r, _) = ui.allocate_exact_size(
                        Vec2::new(ui.available_width() - 6.0, 4.0),
                        Sense::hover(),
                    );
                    ui.painter()
                        .rect_filled(r, CornerRadius::same(2), t.control);
                    if transfer.active && transfer.scanning_files.is_some() {
                        w::paint_indeterminate_bar(ui, r, color);
                    } else {
                        let mut f = r;
                        f.set_width(r.width() * frac.clamp(0.0, 1.0));
                        ui.painter().rect_filled(f, CornerRadius::same(2), color);
                    }
                });
            ui.add_space(10.0);
        }
        if dismiss {
            self.sessions[idx].shared.lock().unwrap().transfer = TransferUi::default();
        }
        if let Some(p) = reveal {
            native::reveal(&p);
        }

        // ---------- table ----------
        let filter = view.filter.clone();
        let sort = view.sort;
        let asc = view.asc;
        let rows = &view.rows;

        let full = ui.available_rect_before_wrap();
        let wdt = full.width();
        let col_size = 96.0;
        let col_time = 150.0;
        let col_mode = 104.0;
        let show_mode = wdt > 660.0;
        let show_time = wdt > 500.0;
        let pad = 12.0;
        let name_w = wdt
            - pad * 2.0
            - col_size
            - if show_time { col_time } else { 0.0 }
            - if show_mode { col_mode } else { 0.0 };

        // Header
        let (hrect, _) = ui.allocate_exact_size(Vec2::new(wdt, 26.0), Sense::hover());
        ui.painter().line_segment(
            [
                Pos2::new(hrect.left(), hrect.bottom()),
                Pos2::new(hrect.right(), hrect.bottom()),
            ],
            Stroke::new(1.0_f32, t.separator),
        );
        let mut header_click: Option<FileSort> = None;
        let mut x = hrect.left() + pad;
        let mut cols: Vec<(f32, f32, &str, Option<FileSort>, bool)> = vec![(
            x,
            name_w,
            if zh { "名称" } else { "Name" },
            Some(FileSort::Name),
            false,
        )];
        x += name_w;
        cols.push((
            x,
            col_size,
            if zh { "大小" } else { "Size" },
            Some(FileSort::Size),
            true,
        ));
        x += col_size;
        if show_time {
            cols.push((
                x + 16.0,
                col_time - 16.0,
                if zh { "修改日期" } else { "Date Modified" },
                Some(FileSort::Modified),
                false,
            ));
            x += col_time;
        }
        if show_mode {
            cols.push((
                x + 16.0,
                col_mode - 16.0,
                if zh { "权限" } else { "Permissions" },
                None,
                false,
            ));
        }
        for (cx, cw, label, key, right) in cols {
            let r = Rect::from_min_size(Pos2::new(cx, hrect.top()), Vec2::new(cw, 26.0));
            let resp = ui.interact(r, ui.id().with(("fhdr", label)), Sense::click());
            let active = key == Some(sort);
            let color = if active || (resp.hovered() && key.is_some()) {
                t.text
            } else {
                t.text2
            };
            let font = if active {
                w::semibold(11.5)
            } else {
                w::regular(11.5)
            };
            let g = w::galley(ui, label, font, color);
            let gx = if right {
                r.right() - g.size().x - (if active { 14.0 } else { 0.0 })
            } else {
                r.left()
            };
            let gw = g.size().x;
            let mid = w::optical_mid(ui, &g);
            ui.painter()
                .galley(Pos2::new(gx, r.center().y - mid), g, color);
            if active {
                let ic = Icon::ChevronDown;
                let c = Pos2::new(gx + gw + 8.0, r.center().y);
                let ir = Rect::from_center_size(c, Vec2::splat(9.0));
                if asc {
                    // chevron up: draw rotated
                    ui.painter().add(egui::Shape::line(
                        vec![
                            c + Vec2::new(-3.0, 1.5),
                            c + Vec2::new(0.0, -1.5),
                            c + Vec2::new(3.0, 1.5),
                        ],
                        Stroke::new(1.3_f32, t.text2),
                    ));
                } else {
                    w::paint_icon(ui.painter(), ir, ic, t.text2);
                }
            }
            if resp.clicked() {
                header_click = key;
            }
        }
        if let Some(k) = header_click {
            let slot = &mut self.sessions[idx];
            if slot.sort == k {
                slot.sort_asc = !slot.sort_asc;
            } else {
                slot.sort = k;
                slot.sort_asc = true;
            }
        }

        let selected = self.sessions[idx].file_sel.clone();
        let sel_set: std::collections::HashSet<&str> =
            selected.iter().map(String::as_str).collect();
        let mods = ui.input(|i| i.modifiers);
        // (name, clicked with secondary button)
        let mut click_sel: Option<(String, bool)> = None;
        let mut open: Option<(String, bool)> = None;
        let mut ctx_action: Option<(u8, String, bool)> = None;
        let mut ctx_perms: Option<u32> = None;
        let multi = selected.len() > 1;

        // Only the rows on screen are laid out (show_rows), so a folder with
        // tens of thousands of entries scrolls as smoothly as a small one.
        let item_spacing = ui.spacing().item_spacing.y;
        ui.spacing_mut().item_spacing.y = 0.0;
        egui::ScrollArea::vertical()
            .id_salt(("files_scroll", tab_uid))
            .auto_shrink([false, false])
            .show_rows(ui, ROW_H, rows.len(), |ui, range| {
                ui.spacing_mut().item_spacing.y = 0.0;
                for k in range {
                    let e = &entries[rows[k]];
                    let (rect, resp) =
                        ui.allocate_exact_size(Vec2::new(wdt, ROW_H), Sense::click());
                    let is_sel = sel_set.contains(e.name.as_str());
                    let h = ui
                        .ctx()
                        .animate_bool_with_time(resp.id.with("h"), resp.hovered(), 0.1);
                    let p = ui.painter();
                    if is_sel {
                        // Merge adjacent selected rows into one rounded block.
                        let prev_sel =
                            k > 0 && sel_set.contains(entries[rows[k - 1]].name.as_str());
                        let next_sel = k + 1 < rows.len()
                            && sel_set.contains(entries[rows[k + 1]].name.as_str());
                        let cr = CornerRadius {
                            nw: if prev_sel { 0 } else { 7 },
                            ne: if prev_sel { 0 } else { 7 },
                            sw: if next_sel { 0 } else { 7 },
                            se: if next_sel { 0 } else { 7 },
                        };
                        p.rect_filled(rect, cr, t.accent);
                        if prev_sel {
                            p.line_segment(
                                [
                                    Pos2::new(rect.left() + 34.0, rect.top()),
                                    Pos2::new(rect.right() - 8.0, rect.top()),
                                ],
                                Stroke::new(
                                    1.0_f32,
                                    Color32::from_rgba_premultiplied(40, 40, 40, 40),
                                ),
                            );
                        }
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
                        p.rect_filled(rect, CornerRadius::same(7), t.row_hover.gamma_multiply(h));
                    }
                    let fg = if is_sel { Color32::WHITE } else { t.text };
                    let fg2 = if is_sel {
                        Color32::from_white_alpha(210)
                    } else {
                        t.text2
                    };
                    let icon = if e.is_dir {
                        Icon::FolderFill
                    } else if e.is_link {
                        Icon::FileLink
                    } else {
                        Icon::File
                    };
                    let icol = if is_sel {
                        Color32::WHITE
                    } else if e.is_dir {
                        t.accent
                    } else {
                        t.text3
                    };
                    let mut x = rect.left() + pad;
                    w::paint_icon(
                        p,
                        Rect::from_center_size(
                            Pos2::new(x + 8.0, rect.center().y),
                            Vec2::splat(16.0),
                        ),
                        icon,
                        icol,
                    );
                    let pr = ui.painter_at(rect);
                    w::paint_text_trunc(
                        ui,
                        &pr,
                        &e.name,
                        w::regular(13.0),
                        fg,
                        x + 26.0,
                        rect.center().y,
                        name_w - 34.0,
                    );
                    x += name_w;
                    let size = if e.is_dir {
                        "—".to_string()
                    } else {
                        e.size.map(format_bytes).unwrap_or_default()
                    };
                    let g = w::galley(ui, &size, w::regular(12.0), fg2);
                    let smid = w::optical_mid(ui, &g);
                    p.galley(
                        Pos2::new(x + col_size - g.size().x, rect.center().y - smid),
                        g,
                        fg2,
                    );
                    x += col_size;
                    if show_time {
                        let g = w::galley(
                            ui,
                            &e.mtime.map(format_mtime).unwrap_or_default(),
                            w::regular(12.0),
                            fg2,
                        );
                        let tmid = w::optical_mid(ui, &g);
                        p.galley(Pos2::new(x + 16.0, rect.center().y - tmid), g, fg2);
                        x += col_time;
                    }
                    if show_mode {
                        let g = w::galley(ui, &e.mode, egui::FontId::monospace(11.5), fg2);
                        let mmid = w::optical_mid(ui, &g);
                        p.galley(Pos2::new(x + 16.0, rect.center().y - mmid), g, fg2);
                    }

                    // While a new listing loads, these rows are about to be
                    // replaced: don't act on them.
                    if loading {
                    } else if resp.double_clicked() {
                        open = Some((e.name.clone(), e.is_dir));
                    } else if resp.clicked() {
                        click_sel = Some((e.name.clone(), false));
                    } else if resp.secondary_clicked() {
                        click_sel = Some((e.name.clone(), true));
                    }
                    let resp = if let Some(target) = &e.link_target {
                        resp.on_hover_text(format!("→ {target}"))
                    } else {
                        resp
                    };
                    let name = e.name.clone();
                    let is_dir = e.is_dir;
                    let entry_perms = e.permissions;
                    let in_multi = multi && is_sel;
                    let n_sel = selected.len();
                    resp.context_menu(|ui| {
                        ui.set_width(210.0);
                        if in_multi {
                            w::menu_caption(
                                ui,
                                &if zh {
                                    format!("已选 {n_sel} 项")
                                } else {
                                    format!("{n_sel} items selected")
                                },
                            );
                            if w::menu_item(
                                ui,
                                if zh {
                                    "下载所选"
                                } else {
                                    "Download selected"
                                },
                                "",
                                false,
                            ) {
                                ctx_action = Some((1, String::new(), false));
                                ui.close_menu();
                            }
                            w::menu_separator(ui);
                            if w::menu_item(
                                ui,
                                if zh {
                                    "删除所选…"
                                } else {
                                    "Delete selected…"
                                },
                                "Del",
                                true,
                            ) {
                                ctx_action = Some((4, String::new(), false));
                                ui.close_menu();
                            }
                            return;
                        }
                        if is_dir {
                            if w::menu_item(ui, if zh { "打开" } else { "Open" }, "", false) {
                                ctx_action = Some((0, name.clone(), is_dir));
                                ui.close_menu();
                            }
                            if w::menu_item(
                                ui,
                                if zh {
                                    "在终端中打开"
                                } else {
                                    "Open in terminal"
                                },
                                "",
                                false,
                            ) {
                                ctx_action = Some((6, name.clone(), is_dir));
                                ui.close_menu();
                            }
                            if w::menu_item(ui, if zh { "下载" } else { "Download" }, "", false) {
                                ctx_action = Some((1, name.clone(), is_dir));
                                ui.close_menu();
                            }
                        } else {
                            if w::menu_item(ui, if zh { "编辑" } else { "Edit" }, "Enter", false)
                            {
                                ctx_action = Some((5, name.clone(), is_dir));
                                ui.close_menu();
                            }
                            if w::menu_item(ui, if zh { "下载" } else { "Download" }, "", false) {
                                ctx_action = Some((1, name.clone(), is_dir));
                                ui.close_menu();
                            }
                        }
                        if w::menu_item(ui, if zh { "重命名…" } else { "Rename…" }, "F2", false)
                        {
                            ctx_action = Some((2, name.clone(), is_dir));
                            ui.close_menu();
                        }
                        if w::menu_item(
                            ui,
                            if zh {
                                "权限 (chmod)…"
                            } else {
                                "Permissions…"
                            },
                            "",
                            false,
                        ) {
                            ctx_action = Some((7, name.clone(), is_dir));
                            ctx_perms = entry_perms;
                            ui.close_menu();
                        }
                        if w::menu_item(ui, if zh { "拷贝路径" } else { "Copy path" }, "", false)
                        {
                            ctx_action = Some((3, name.clone(), is_dir));
                            ui.close_menu();
                        }
                        w::menu_separator(ui);
                        if w::menu_item(ui, if zh { "删除…" } else { "Delete…" }, "Del", true)
                        {
                            ctx_action = Some((4, name.clone(), is_dir));
                            ui.close_menu();
                        }
                    });
                }
            });
        ui.spacing_mut().item_spacing.y = item_spacing;
        if rows.is_empty() && !loading {
            let r = Rect::from_min_size(full.min + Vec2::new(0.0, 66.0), Vec2::new(wdt, 20.0));
            let msg = if !filter.is_empty() {
                if zh {
                    "没有匹配的文件"
                } else {
                    "No matching files"
                }
            } else if zh {
                "文件夹是空的"
            } else {
                "This folder is empty"
            };
            let g = w::galley(ui, msg, w::regular(13.0), t.text3);
            ui.painter().galley(
                Pos2::new(r.center().x - g.size().x / 2.0, r.top()),
                g,
                t.text3,
            );
        }

        // keyboard: Delete / F2 / Enter / Backspace / Ctrl+A / Esc (when no text field has focus)
        let no_text_focus = !text_focus_at_start && ui.ctx().memory(|m| m.focused().is_none());
        let visible_names =
            || -> Vec<String> { rows.iter().map(|&i| entries[i].name.clone()).collect() };
        if no_text_focus && !self.modal_open() {
            let (del, f2, enter, back, all, esc) = ui.input(|i| {
                (
                    i.key_pressed(egui::Key::Delete),
                    i.key_pressed(egui::Key::F2),
                    i.key_pressed(egui::Key::Enter),
                    i.key_pressed(egui::Key::Backspace),
                    i.modifiers.command && i.key_pressed(egui::Key::A),
                    i.key_pressed(egui::Key::Escape),
                )
            });
            if back && cwd != "/" {
                nav = Some(parent_dir(&cwd));
            }
            if all {
                self.sessions[idx].file_sel = visible_names();
            }
            if esc {
                self.sessions[idx].file_sel.clear();
            }
            let sel = self.sessions[idx].file_sel.clone();
            if del && !sel.is_empty() {
                ctx_action = Some((4, String::new(), false));
            } else if sel.len() == 1 && !loading {
                let is_dir = is_dir_name(&sel[0]);
                if f2 {
                    ctx_action = Some((2, sel[0].clone(), is_dir));
                } else if enter {
                    open = Some((sel[0].clone(), is_dir));
                }
            }
        }

        if let Some((name, secondary)) = click_sel {
            let slot = &mut self.sessions[idx];
            if secondary {
                if !slot.file_sel.contains(&name) {
                    slot.file_sel = vec![name.clone()];
                    slot.file_anchor = Some(name);
                }
            } else if mods.shift {
                // Range from the anchor to this row (in the visible order).
                let anchor = slot.file_anchor.clone().unwrap_or_else(|| name.clone());
                let visible_names = visible_names();
                let a = visible_names.iter().position(|n| *n == anchor);
                let b = visible_names.iter().position(|n| *n == name);
                if let (Some(a), Some(b)) = (a, b) {
                    let (lo, hi) = if a <= b { (a, b) } else { (b, a) };
                    let range: Vec<String> = visible_names[lo..=hi].to_vec();
                    if mods.command {
                        for n in range {
                            if !slot.file_sel.contains(&n) {
                                slot.file_sel.push(n);
                            }
                        }
                    } else {
                        slot.file_sel = range;
                    }
                }
            } else if mods.command {
                if let Some(pos) = slot.file_sel.iter().position(|n| *n == name) {
                    slot.file_sel.remove(pos);
                } else {
                    slot.file_sel.push(name.clone());
                }
                slot.file_anchor = Some(name);
            } else {
                slot.file_sel = vec![name.clone()];
                slot.file_anchor = Some(name);
            }
        }
        if let Some((name, is_dir)) = open {
            if is_dir {
                nav = Some(join_remote(&cwd, &name));
            } else {
                let size = entries
                    .iter()
                    .find(|e| e.name == name)
                    .and_then(|e| e.size)
                    .unwrap_or(0);
                if size <= 4 * 1024 * 1024 {
                    self.open_file_editor(idx, &cwd, &name, false);
                } else {
                    want_download = vec![name];
                }
            }
        }
        if let Some((action, name, is_dir)) = ctx_action {
            let sel = self.sessions[idx].file_sel.clone();
            if !name.is_empty() && action != 4 && action != 1 {
                self.sessions[idx].file_sel = vec![name.clone()];
            }
            match action {
                0 => nav = Some(join_remote(&cwd, &name)),
                1 => {
                    want_download = if name.is_empty() || sel.contains(&name) && sel.len() > 1 {
                        sel.clone()
                    } else {
                        vec![name]
                    }
                }
                2 => {
                    self.text_prompt = Some(TextPrompt::RenameRemote {
                        tab_uid,
                        dir: cwd.clone(),
                        from: name.clone(),
                        to: name,
                    })
                }
                3 => {
                    ui.ctx().copy_text(join_remote(&cwd, &name));
                    self.toast_msg(self.t("已拷贝路径", "Path copied"), StatusKind::Ok);
                }
                5 => self.open_file_editor(idx, &cwd, &name, false),
                6 => want_cd = Some(join_remote(&cwd, &name)),
                7 => {
                    let mode = ctx_perms.unwrap_or(if is_dir { 0o755 } else { 0o644 }) & 0o7777;
                    let octal = if mode > 0o777 {
                        format!("{mode:04o}")
                    } else {
                        format!("{mode:03o}")
                    };
                    self.chmod_prompt = Some(ChmodPrompt {
                        tab_uid,
                        dir: cwd.clone(),
                        name,
                        mode,
                        octal,
                        focus: true,
                    });
                }
                _ => {
                    let items: Vec<(String, bool)> =
                        if name.is_empty() || (sel.contains(&name) && sel.len() > 1) {
                            sel.iter().map(|n| (n.clone(), is_dir_name(n))).collect()
                        } else {
                            vec![(name, is_dir)]
                        };
                    if !items.is_empty() {
                        self.confirm = Some(Confirm::DeleteFiles {
                            tab_uid,
                            dir: cwd.clone(),
                            items,
                        });
                    }
                }
            }
        }
        self.sessions[idx].file_view = Some(view);
        if let Some(dir) = want_cd {
            self.cd_in_terminal(idx, &dir);
            return;
        }
        if !want_download.is_empty() && !transfer.active {
            self.download_many(idx, want_download);
        }
        if want_upload {
            self.pick_upload(idx);
        }
        if let Some(p) = nav {
            self.refresh_files(idx, Some(p));
        }
    }
}

/// Breadcrumb segment (plain text button).
fn crumb(ui: &mut egui::Ui, t: &theme::Tokens, text: &str, last: bool) -> egui::Response {
    let font = if last {
        w::semibold(12.5)
    } else {
        w::regular(12.5)
    };
    let g = w::galley(ui, text, font, t.text);
    let (r, resp) = ui.allocate_exact_size(Vec2::new(g.size().x + 8.0, 22.0), Sense::click());
    if resp.hovered() && !last {
        ui.painter()
            .rect_filled(r, CornerRadius::same(5), t.control_hover);
    }
    let c = if last || resp.hovered() {
        t.text
    } else {
        t.text2
    };
    let mid = w::optical_mid(ui, &g);
    ui.painter().galley_with_override_text_color(
        Pos2::new(r.left() + 4.0, r.center().y - mid),
        g,
        c,
    );
    resp
}

fn crumb_sep(ui: &mut egui::Ui, t: &theme::Tokens) {
    let (r, _) = ui.allocate_exact_size(Vec2::new(12.0, 22.0), Sense::hover());
    w::paint_icon(
        ui.painter(),
        Rect::from_center_size(r.center(), Vec2::splat(9.0)),
        Icon::ChevronRight,
        t.text3,
    );
}

/// Returns the transfer's cancel flag.
fn start_transfer(
    shared: &Arc<Mutex<SharedUi>>,
    label: &str,
    zh: bool,
    download: bool,
) -> Arc<std::sync::atomic::AtomicBool> {
    let cancel = Arc::new(std::sync::atomic::AtomicBool::new(false));
    if let Ok(mut s) = shared.lock() {
        s.transfer = TransferUi {
            label: format!(
                "{} {label}",
                match (zh, download) {
                    (true, true) => "下载",
                    (true, false) => "上传",
                    (false, true) => "Downloading",
                    (false, false) => "Uploading",
                }
            ),
            active: true,
            is_download: download,
            started: Some(Instant::now()),
            cancel: Arc::clone(&cancel),
            ..Default::default()
        };
    }
    cancel
}

fn finish_transfer(
    shared: &Arc<Mutex<SharedUi>>,
    result: Result<anyhow::Result<()>, tokio::task::JoinError>,
    zh: bool,
    local: Option<PathBuf>,
) {
    if let Ok(mut s) = shared.lock() {
        let t = &mut s.transfer;
        t.active = false;
        t.scanning_files = None;
        let base = t
            .label
            .split_once(' ')
            .map(|(_, rest)| rest.to_string())
            .unwrap_or_default();
        match result {
            Ok(Ok(())) => {
                t.error = false;
                t.label = match (&local, zh) {
                    (Some(p), true) => format!("已下载到 {}", p.display()),
                    (Some(p), false) => format!("Saved to {}", p.display()),
                    (None, true) => format!("已上传 {base}"),
                    (None, false) => format!("Uploaded {base}"),
                };
                t.last_local = local;
            }
            Ok(Err(e)) if e.downcast_ref::<session::Cancelled>().is_some() => {
                t.error = true;
                t.label = if zh {
                    format!("已取消 {base}")
                } else {
                    format!("Cancelled {base}")
                };
                if let Some(p) = local {
                    session::remove_placeholder(&p);
                }
            }
            Ok(Err(e)) => {
                t.error = true;
                t.label = format!("{base}: {e:#}");
                if let Some(p) = local {
                    session::remove_placeholder(&p);
                }
            }
            Err(e) => {
                t.error = true;
                t.label = format!("{base}: {e}");
            }
        }
    }
}

/// Install a fresh listing, unless the user has since moved to another folder.
fn apply_listing(
    shared: &Arc<Mutex<SharedUi>>,
    gen: u64,
    cwd: String,
    entries: Vec<session::RemoteDirEntry>,
) {
    if let Ok(mut s) = shared.lock() {
        if s.files.gen == gen || (!s.files.loading && (s.files.cwd == cwd || s.files.cwd == "~")) {
            s.files.cwd = cwd;
            s.files.entries = Arc::new(entries);
        }
    }
}

impl GuiApp {
    /// Built-in text editor sheet for remote files.
    pub(super) fn ui_file_editor(&mut self, ctx: &egui::Context) {
        let Some(mut ed) = self.file_editor.take() else {
            return;
        };
        let t = self.tok();
        let zh = self.zh();
        let mut just_saved = false;
        {
            let mut io = ed.io.lock().unwrap();
            if let Some(r) = io.loaded.take() {
                ed.loading = false;
                match r {
                    Ok((text, stamp)) => {
                        ed.stamp = stamp;
                        ed.load_failed = false;
                        // Edit with LF; remember CRLF files and write them back
                        // as CRLF. Only when every line ends in CRLF: a mixed
                        // file is edited as is, so saving never rewrites the
                        // lines that were LF.
                        let crlf = text.matches("\r\n").count();
                        ed.crlf = crlf > 0 && crlf == text.matches('\n').count();
                        let text = if ed.crlf {
                            text.replace("\r\n", "\n")
                        } else {
                            text
                        };
                        ed.line_count = text.lines().count().max(1);
                        ed.original = text.clone();
                        ed.text = text;
                    }
                    Err(e) => {
                        ed.error = e;
                        ed.load_failed = true;
                    }
                }
            }
            if let Some(r) = io.saved.take() {
                ed.saving = false;
                let sent = ed.saving_text.take();
                match r {
                    Ok(stamp) => {
                        ed.stamp = stamp;
                        ed.original = sent.unwrap_or_else(|| ed.text.clone());
                        ed.dirty = ed.text != ed.original;
                        just_saved = true;
                    }
                    Err((true, _)) => {
                        ed.conflict = true;
                        ed.error = if zh {
                            "这个文件在你打开后已被其他人或程序修改。可以覆盖保存，或重新加载（会丢弃你的修改）。".into()
                        } else {
                            "This file was changed on the server after you opened it. Overwrite it, or reload (drops your edits).".into()
                        };
                    }
                    Err((false, e)) => ed.error = e,
                }
            }
        }
        let modified = !ed.loading && !ed.load_failed && ed.dirty;
        let mut save = ctx.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, egui::Key::S));
        let open_find = ctx.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, egui::Key::F));
        let open_replace = ctx.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, egui::Key::H));
        if open_find {
            ed.find_open = true;
            ed.find_focus = true;
        }
        if open_replace {
            ed.find_open = true;
            ed.replace_open = true;
            ed.find_focus = true;
        }
        if ed.find_open
            && ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape))
        {
            ed.find_open = false;
            ed.focus = true;
        }
        let mut close = false;
        let mut force_save = false;
        let mut reload = false;
        let mut replaced_count: Option<usize> = None;

        let screen = ctx.screen_rect();
        let width = (screen.width() * 0.8).clamp(520.0, 1100.0);
        let height = (screen.height() * 0.82).clamp(340.0, 920.0);
        let term_dark = theme::term_palette(self.term_theme_pref());
        let lang = super::highlight::SyntaxLang::detect(&ed.name, &ed.path);
        let resp = egui::Modal::new(egui::Id::new("file_editor"))
            .frame(super::panels::sheet_frame(&t).inner_margin(egui::Margin::same(16)))
            .show(ctx, |ui| {
                ui.set_width(width);
                // Header
                ui.horizontal(|ui| {
                    ui.set_height(w::CTRL_H);
                    let (ir, _) = ui.allocate_exact_size(Vec2::splat(18.0), Sense::hover());
                    w::paint_icon(ui.painter(), ir, Icon::File, t.text2);
                    let g = w::galley(ui, &ed.name, w::semibold(15.0), t.text);
                    let (r, _) = ui.allocate_exact_size(g.size(), Sense::hover());
                    ui.painter().galley(r.min, g, t.text);
                    if modified {
                        let (r, _) = ui.allocate_exact_size(Vec2::new(10.0, 10.0), Sense::hover());
                        ui.painter().circle_filled(r.center(), 3.5, t.warn);
                    }
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if w::button(
                            ui,
                            Kind::Primary,
                            None,
                            if zh { "保存" } else { "Save" },
                            modified && !ed.saving,
                        )
                        .on_hover_text("Ctrl+S")
                        .clicked()
                        {
                            save = true;
                        }
                        let (label, kind) = if ed.confirm_discard {
                            (
                                if zh {
                                    "放弃修改"
                                } else {
                                    "Discard changes"
                                },
                                Kind::Destructive,
                            )
                        } else {
                            (if zh { "关闭" } else { "Close" }, Kind::Plain)
                        };
                        if w::button(ui, kind, None, label, !ed.saving).clicked() {
                            if modified && !ed.confirm_discard {
                                ed.confirm_discard = true;
                            } else {
                                close = true;
                            }
                        }
                        if w::icon_button(
                            ui,
                            Icon::Search,
                            if zh {
                                "查找 / 替换  Ctrl+F"
                            } else {
                                "Find / Replace  Ctrl+F"
                            },
                            !ed.loading && !ed.load_failed,
                        )
                        .clicked()
                        {
                            ed.find_open = !ed.find_open;
                            if ed.find_open {
                                ed.find_focus = true;
                            } else {
                                ed.focus = true;
                            }
                        }
                        let avail = ui.available_width();
                        let (r, _) =
                            ui.allocate_exact_size(Vec2::new(avail, w::CTRL_H), Sense::hover());
                        w::paint_text_trunc(
                            ui,
                            &ui.painter_at(r),
                            &ed.path,
                            egui::FontId::monospace(11.5),
                            t.text3,
                            r.left() + 8.0,
                            r.center().y,
                            r.width() - 12.0,
                        );
                    });
                });
                ui.add_space(10.0);

                if !ed.loading && !ed.load_failed && ed.find_open {
                    let mut find_next = false;
                    let mut find_prev = false;
                    let mut do_replace = false;
                    let mut do_replace_all = false;
                    let mut close_find = false;

                    let mut matches =
                        super::highlight::find_matches(&ed.text, &ed.find_query, ed.find_case);
                    if !matches.is_empty() {
                        ed.find_cur = ed.find_cur.min(matches.len() - 1);
                    } else {
                        ed.find_cur = 0;
                    }

                    egui::Frame::NONE
                        .fill(if t.dark { t.card } else { t.chrome })
                        .stroke(Stroke::new(1.0_f32, t.separator))
                        .corner_radius(CornerRadius::same(8))
                        .inner_margin(egui::Margin::symmetric(8, 5))
                        .show(ui, |ui| {
                            ui.set_width(ui.available_width());
                            ui.horizontal(|ui| {
                                ui.set_height(w::CTRL_H);
                                ui.spacing_mut().item_spacing.x = 6.0;
                                let rf = w::Field::new(&mut ed.find_query)
                                    .icon(Icon::Search)
                                    .hint(if zh { "查找" } else { "Find" })
                                    .mono()
                                    .clearable()
                                    .width(220.0)
                                    .id(egui::Id::new(("fe_find", &ed.path)))
                                    .show(ui);
                                if ed.find_focus {
                                    rf.request_focus();
                                    ed.find_focus = false;
                                }
                                if rf.changed() {
                                    ed.find_cur = 0;
                                    matches = super::highlight::find_matches(
                                        &ed.text,
                                        &ed.find_query,
                                        ed.find_case,
                                    );
                                    if let Some(&(s, e)) = matches.first() {
                                        ed.pending_select =
                                            Some(super::highlight::byte_range_to_char_range(
                                                &ed.text, s, e,
                                            ));
                                    }
                                }
                                if (rf.has_focus() || rf.lost_focus())
                                    && ui.input(|i| i.key_pressed(egui::Key::Enter))
                                {
                                    if ui.input(|i| i.modifiers.shift) {
                                        find_prev = true;
                                    } else {
                                        find_next = true;
                                    }
                                    rf.request_focus();
                                }

                                let cnt_txt = if ed.find_query.is_empty() {
                                    String::new()
                                } else if matches.is_empty() {
                                    (if zh { "无结果" } else { "0 results" }).to_string()
                                } else {
                                    format!("{}/{}", ed.find_cur + 1, matches.len())
                                };
                                let cg =
                                    w::galley(ui, &cnt_txt, egui::FontId::monospace(11.5), t.text2);
                                let (cr, _) = ui.allocate_exact_size(
                                    Vec2::new(cg.size().x.max(44.0), w::CTRL_H),
                                    Sense::hover(),
                                );
                                let mid = w::optical_mid(ui, &cg);
                                ui.painter().galley(
                                    Pos2::new(cr.left(), cr.center().y - mid),
                                    cg,
                                    t.text2,
                                );

                                if w::icon_button(
                                    ui,
                                    Icon::ArrowUp,
                                    if zh { "上一个" } else { "Previous" },
                                    !matches.is_empty(),
                                )
                                .clicked()
                                {
                                    find_prev = true;
                                }
                                if w::icon_button(
                                    ui,
                                    Icon::ArrowDown,
                                    if zh { "下一个" } else { "Next" },
                                    !matches.is_empty(),
                                )
                                .clicked()
                                {
                                    find_next = true;
                                }
                                let mut cs = ed.find_case;
                                if w::toggle_chip(ui, &mut cs, None, "Aa", false, 34.0)
                                    .on_hover_text(if zh {
                                        "区分大小写"
                                    } else {
                                        "Match case"
                                    })
                                    .changed()
                                {
                                    ed.find_case = cs;
                                    ed.find_cur = 0;
                                    matches = super::highlight::find_matches(
                                        &ed.text,
                                        &ed.find_query,
                                        ed.find_case,
                                    );
                                    if let Some(&(s, e)) = matches.first() {
                                        ed.pending_select =
                                            Some(super::highlight::byte_range_to_char_range(
                                                &ed.text, s, e,
                                            ));
                                    }
                                }
                                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                    ui.spacing_mut().item_spacing.x = 6.0;
                                    if w::icon_button(
                                        ui,
                                        Icon::Close,
                                        if zh {
                                            "关闭查找  Esc"
                                        } else {
                                            "Close  Esc"
                                        },
                                        true,
                                    )
                                    .clicked()
                                    {
                                        close_find = true;
                                    }
                                    let mut rep_open = ed.replace_open;
                                    if w::toggle_chip(
                                        ui,
                                        &mut rep_open,
                                        None,
                                        if zh { "替换" } else { "Replace" },
                                        false,
                                        if zh { 52.0 } else { 68.0 },
                                    )
                                    .changed()
                                    {
                                        ed.replace_open = rep_open;
                                    }
                                });
                            });

                            if ed.replace_open {
                                ui.add_space(4.0);
                                ui.horizontal(|ui| {
                                    ui.set_height(w::CTRL_H);
                                    ui.spacing_mut().item_spacing.x = 6.0;
                                    w::Field::new(&mut ed.replace_text)
                                        .hint(if zh { "替换为" } else { "Replace with" })
                                        .mono()
                                        .width(220.0)
                                        .id(egui::Id::new(("fe_replace", &ed.path)))
                                        .show(ui);
                                    if w::button(
                                        ui,
                                        Kind::Secondary,
                                        None,
                                        if zh { "替换" } else { "Replace" },
                                        !matches.is_empty() && !ed.saving,
                                    )
                                    .clicked()
                                    {
                                        do_replace = true;
                                    }
                                    if w::button(
                                        ui,
                                        Kind::Secondary,
                                        None,
                                        if zh { "全部替换" } else { "Replace all" },
                                        !matches.is_empty() && !ed.saving,
                                    )
                                    .clicked()
                                    {
                                        do_replace_all = true;
                                    }
                                });
                            }
                        });
                    ui.add_space(6.0);

                    if close_find {
                        ed.find_open = false;
                        ed.focus = true;
                    } else if do_replace_all {
                        let (next, count) = super::highlight::replace_all_matches(
                            &ed.text,
                            &ed.find_query,
                            &ed.replace_text,
                            ed.find_case,
                        );
                        if count > 0 {
                            ed.text = next;
                            ed.dirty = ed.text != ed.original;
                            ed.confirm_discard = false;
                            ed.line_count = ed.text.lines().count().max(1);
                            ed.find_cur = 0;
                            replaced_count = Some(count);
                        }
                    } else if do_replace {
                        if let Some(&(s, e)) = matches.get(ed.find_cur) {
                            ed.text.replace_range(s..e, &ed.replace_text);
                            ed.dirty = ed.text != ed.original;
                            ed.confirm_discard = false;
                            ed.line_count = ed.text.lines().count().max(1);
                            let next_matches = super::highlight::find_matches(
                                &ed.text,
                                &ed.find_query,
                                ed.find_case,
                            );
                            if !next_matches.is_empty() {
                                ed.find_cur = ed.find_cur.min(next_matches.len() - 1);
                                let (ns, ne) = next_matches[ed.find_cur];
                                ed.pending_select = Some(
                                    super::highlight::byte_range_to_char_range(&ed.text, ns, ne),
                                );
                            } else {
                                ed.find_cur = 0;
                            }
                        }
                    } else if find_next && !matches.is_empty() {
                        ed.find_cur = (ed.find_cur + 1) % matches.len();
                        let (s, e) = matches[ed.find_cur];
                        ed.pending_select =
                            Some(super::highlight::byte_range_to_char_range(&ed.text, s, e));
                    } else if find_prev && !matches.is_empty() {
                        ed.find_cur = (ed.find_cur + matches.len() - 1) % matches.len();
                        let (s, e) = matches[ed.find_cur];
                        ed.pending_select =
                            Some(super::highlight::byte_range_to_char_range(&ed.text, s, e));
                    }
                }

                // Body
                let find_bar_h = if !ed.loading && !ed.load_failed && ed.find_open {
                    if ed.replace_open {
                        78.0
                    } else {
                        44.0
                    }
                } else {
                    0.0
                };
                let body_h = (height - 110.0 - find_bar_h).max(120.0);
                let (body, _) = ui.allocate_exact_size(Vec2::new(width, body_h), Sense::hover());
                ui.painter()
                    .rect_filled(body, CornerRadius::same(10), term_dark.bg);
                let mut child = ui.new_child(
                    UiBuilder::new()
                        .max_rect(body.shrink(2.0))
                        .layout(Layout::top_down(Align::Min)),
                );
                if ed.loading {
                    child.add_space(body_h * 0.4);
                    child.vertical_centered(|ui| {
                        ui.add(egui::Spinner::new().size(18.0).color(t.text3));
                    });
                } else if ed.load_failed {
                    // Nothing to edit: never offer to save an empty buffer over the file.
                    child.add_space(body_h * 0.38);
                    child.vertical_centered(|ui| {
                        let (r, _) = ui.allocate_exact_size(Vec2::splat(26.0), Sense::hover());
                        w::paint_icon(ui.painter(), r, Icon::Warning, t.warn);
                        ui.add_space(8.0);
                        let g = w::galley(
                            ui,
                            if zh {
                                "无法在编辑器中打开这个文件"
                            } else {
                                "This file can't be opened in the editor"
                            },
                            w::semibold(13.5),
                            term_dark.fg,
                        );
                        let (r, _) = ui.allocate_exact_size(g.size(), Sense::hover());
                        ui.painter().galley(r.min, g, term_dark.fg);
                    });
                } else {
                    egui::ScrollArea::both()
                        .id_salt(("editor_scroll", &ed.path))
                        .auto_shrink([false, false])
                        .show(&mut child, |ui| {
                            let gutter = 46.0;
                            let font = egui::FontId::monospace(13.0);
                            let find_open = ed.find_open;
                            let find_query = ed.find_query.clone();
                            let find_case = ed.find_case;
                            let find_cur = ed.find_cur;
                            let mut layouter = |ui: &egui::Ui, text: &str, wrap_width: f32| {
                                let live_matches = if find_open && !find_query.is_empty() {
                                    super::highlight::find_matches(text, &find_query, find_case)
                                } else {
                                    Vec::new()
                                };
                                let active_match = (!live_matches.is_empty())
                                    .then(|| find_cur.min(live_matches.len() - 1));
                                let hl = super::highlight::EditorHighlight {
                                    lang,
                                    pal: &term_dark,
                                    tok: &t,
                                    matches: &live_matches,
                                    active_match,
                                };
                                let job = super::highlight::build_editor_job(
                                    text, &font, wrap_width, &hl,
                                );
                                ui.fonts(|f| f.layout_job(job))
                            };
                            let te = egui::TextEdit::multiline(&mut ed.text)
                                .id(egui::Id::new(("editor_text", &ed.path)))
                                .code_editor()
                                .font(font.clone())
                                .text_color(term_dark.fg)
                                .layouter(&mut layouter)
                                .frame(false)
                                .lock_focus(true)
                                .desired_width(f32::INFINITY)
                                .desired_rows(((body_h - 20.0) / 18.0) as usize)
                                .margin(egui::Margin {
                                    left: gutter as i8,
                                    right: 10,
                                    top: 8,
                                    bottom: 8,
                                });
                            let out = te.show(ui);
                            if out.response.changed() {
                                // Compare only when something was typed, not every frame.
                                ed.dirty = ed.text != ed.original;
                                ed.confirm_discard = false;
                                ed.line_count = ed.text.lines().count().max(1);
                            }
                            if let Some((c0, c1)) = ed.pending_select.take() {
                                let mut state = out.state;
                                let range = egui::text::CCursorRange::two(
                                    egui::text::CCursor::new(c0),
                                    egui::text::CCursor::new(c1),
                                );
                                state.cursor.set_char_range(Some(range));
                                state.store(ui.ctx(), out.response.id);
                                let cursor_rect = out
                                    .galley
                                    .pos_from_cursor(
                                        &out.galley.from_ccursor(egui::text::CCursor::new(c0)),
                                    )
                                    .translate(out.galley_pos.to_vec2());
                                ui.scroll_to_rect(
                                    cursor_rect.expand(24.0),
                                    Some(egui::Align::Center),
                                );
                            }
                            if ed.focus {
                                out.response.request_focus();
                                ed.focus = false;
                            }
                            // Line numbers in the left margin (laid out for visible rows only).
                            let p = ui.painter();
                            let clip = ui.clip_rect();
                            let mut line = 1usize;
                            let mut new_line = true;
                            for row in &out.galley.rows {
                                let y = out.galley_pos.y + row.rect.center().y;
                                if new_line && (y < clip.top() - 20.0 || y > clip.bottom() + 20.0) {
                                    line += 1;
                                } else if new_line {
                                    let g = p.layout_no_wrap(
                                        line.to_string(),
                                        egui::FontId::monospace(11.5),
                                        t.text3,
                                    );
                                    p.galley(
                                        Pos2::new(
                                            out.galley_pos.x - 12.0 - g.size().x,
                                            y - g.size().y / 2.0,
                                        ),
                                        g,
                                        t.text3,
                                    );
                                    line += 1;
                                }
                                new_line = row.ends_with_newline;
                            }
                        });
                }
                ui.add_space(8.0);

                // Footer
                ui.horizontal(|ui| {
                    ui.set_height(20.0);
                    let (msg, color) = if !ed.error.is_empty() {
                        (ed.error.clone(), t.err)
                    } else if ed.saving {
                        (
                            (if zh { "正在保存…" } else { "Saving…" }).to_string(),
                            t.text3,
                        )
                    } else if ed.confirm_discard {
                        (
                            (if zh {
                                "有未保存的修改，再点一次「放弃修改」关闭"
                            } else {
                                "Unsaved changes — click Discard again to close"
                            })
                            .to_string(),
                            t.warn,
                        )
                    } else if modified {
                        ((if zh { "已修改" } else { "Edited" }).to_string(), t.text3)
                    } else {
                        ((if zh { "已是最新" } else { "Saved" }).to_string(), t.text3)
                    };
                    let avail = ui.available_width() - 220.0;
                    let (r, _) =
                        ui.allocate_exact_size(Vec2::new(avail.max(50.0), 20.0), Sense::hover());
                    w::paint_text_trunc(
                        ui,
                        &ui.painter_at(r),
                        &msg,
                        w::regular(12.0),
                        color,
                        r.left() + 2.0,
                        r.center().y,
                        r.width(),
                    );
                    if ed.conflict && !ed.saving {
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if w::button(
                                ui,
                                Kind::Destructive,
                                None,
                                if zh { "覆盖保存" } else { "Overwrite" },
                                true,
                            )
                            .clicked()
                            {
                                force_save = true;
                            }
                            if w::button(
                                ui,
                                Kind::Secondary,
                                None,
                                if zh { "重新加载" } else { "Reload" },
                                true,
                            )
                            .clicked()
                            {
                                reload = true;
                            }
                        });
                        return;
                    }
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        let lines = ed.line_count;
                        let lang_part = lang.label().map(|l| format!(" · {l}")).unwrap_or_default();
                        let g = w::galley(
                            ui,
                            &format!(
                                "{lines} {}{} · UTF-8 · {}",
                                if zh { "行" } else { "lines" },
                                lang_part,
                                if ed.crlf { "CRLF" } else { "LF" }
                            ),
                            w::regular(12.0),
                            t.text3,
                        );
                        let (r, _) = ui.allocate_exact_size(g.size(), Sense::hover());
                        ui.painter().galley(r.min, g, t.text3);
                    });
                });
            });
        if resp.should_close() && !ed.saving {
            if modified && !ed.confirm_discard {
                ed.confirm_discard = true;
            } else {
                close = true;
            }
        }
        if let Some(count) = replaced_count {
            self.toast_msg(
                if zh {
                    format!("已替换 {count} 处")
                } else {
                    format!("Replaced {count} occurrences")
                },
                StatusKind::Ok,
            );
        }
        if just_saved {
            self.toast_msg(
                if zh {
                    format!("已保存 {}", ed.name)
                } else {
                    format!("Saved {}", ed.name)
                },
                StatusKind::Ok,
            );
            if let Some(i) = self.slot_index(ed.tab_uid) {
                self.refresh_files(i, None);
            }
        }
        if reload && !close {
            self.file_editor = Some(ed);
            self.reload_file_editor();
            return;
        }
        if (force_save || (save && modified)) && !ed.saving && !close {
            ed.confirm_discard = false;
            self.file_editor = Some(ed);
            self.save_file_editor(force_save);
            return;
        }
        if !close {
            self.file_editor = Some(ed);
        }
    }
}

/// POSIX shell single-quote escaping for `cd` paths (preserves `~` expansion).
fn shell_single_quote(path: &str) -> String {
    let clean: String = path.chars().filter(|c| !c.is_control()).collect();
    if clean.is_empty() || clean == "~" {
        return "~".into();
    }
    if let Some(rest) = clean.strip_prefix("~/") {
        if rest.is_empty() {
            return "~".into();
        }
        return format!("~/{}", quote_single_raw(rest));
    }
    quote_single_raw(&clean)
}

fn quote_single_raw(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

fn mode_rwx_string(mode: u32) -> String {
    let mut s = String::with_capacity(9);
    for shift in [6u32, 3, 0] {
        let bits = (mode >> shift) & 0o7;
        s.push(if bits & 4 != 0 { 'r' } else { '-' });
        s.push(if bits & 2 != 0 { 'w' } else { '-' });
        s.push(if bits & 1 != 0 { 'x' } else { '-' });
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shell_single_quote_escapes_special_characters() {
        assert_eq!(shell_single_quote("~"), "~");
        assert_eq!(shell_single_quote("~/my dir"), "~/'my dir'");
        assert_eq!(shell_single_quote("/var/log"), "'/var/log'");
        assert_eq!(
            shell_single_quote("/tmp/a b/$HOME/`id`"),
            "'/tmp/a b/$HOME/`id`'"
        );
        assert_eq!(shell_single_quote("/tmp/it's"), "'/tmp/it'\\''s'");
        assert_eq!(shell_single_quote("/tmp/bad\nrm -rf"), "'/tmp/badrm -rf'");
    }

    #[test]
    fn mode_rwx_formats_octal_bits() {
        assert_eq!(mode_rwx_string(0o755), "rwxr-xr-x");
        assert_eq!(mode_rwx_string(0o644), "rw-r--r--");
        assert_eq!(mode_rwx_string(0o600), "rw-------");
    }
}
