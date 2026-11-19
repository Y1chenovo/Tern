//! Group mode: send files to / fetch files from every selected host at once.

use super::panels::sheet_frame;
use super::widgets::{self as w, Icon, Kind};
use super::*;
use egui::{Align, Align2, Color32, CornerRadius, Layout, Pos2, Rect, Sense, Stroke, Vec2};
use std::thread;

const MAX_BATCH_TRANSFER_HOSTS: usize = 4;

async fn acquire_batch_slot(
    sem: &Arc<tokio::sync::Semaphore>,
    cancel: &std::sync::atomic::AtomicBool,
) -> Option<tokio::sync::OwnedSemaphorePermit> {
    use std::sync::atomic::Ordering;
    let acq = Arc::clone(sem).acquire_owned();
    tokio::pin!(acq);
    loop {
        if cancel.load(Ordering::Relaxed) {
            return None;
        }
        tokio::select! {
            permit = &mut acq => {
                let permit = permit.ok()?;
                if cancel.load(Ordering::Relaxed) {
                    return None;
                }
                return Some(permit);
            }
            _ = tokio::time::sleep(std::time::Duration::from_millis(100)) => {}
        }
    }
}

#[derive(Clone, PartialEq)]
pub(super) enum JobState {
    Waiting,
    Running,
    Done,
    Failed(String),
}

#[derive(Clone)]
pub(super) struct BatchJob {
    uid: u64,
    name: String,
    done: u64,
    total: u64,
    scanning_files: Option<usize>,
    state: JobState,
}

/// A file or folder queued for upload. Its size is counted in the background
/// so dropping a huge folder never stalls the UI.
pub(super) struct LocalItem {
    path: PathBuf,
    is_dir: bool,
    /// (total bytes, file count) once counted.
    summary: Arc<Mutex<Option<(u64, usize)>>>,
}

/// State of the "group files" sheet.
pub(super) struct BatchXfer {
    pub open: bool,
    /// 0 = upload, 1 = download
    tab: usize,
    /// Local files / folders to send.
    files: Vec<LocalItem>,
    remote_dir: String,
    remote_path: String,
    jobs: Arc<Mutex<Vec<BatchJob>>>,
    /// Which tab the current job list belongs to.
    jobs_tab: usize,
    out_dir: Option<PathBuf>,
    /// Stops the current run (every host) at the next chunk.
    cancel: Arc<std::sync::atomic::AtomicBool>,
    /// Local entries the last upload had to leave out: (path, reason).
    skipped: Arc<Mutex<Vec<(String, String)>>>,
}

impl Default for BatchXfer {
    fn default() -> Self {
        Self {
            open: false,
            tab: 0,
            files: Vec::new(),
            remote_dir: "~".into(),
            remote_path: String::new(),
            jobs: Arc::new(Mutex::new(Vec::new())),
            jobs_tab: 0,
            out_dir: None,
            cancel: Default::default(),
            skipped: Default::default(),
        }
    }
}

impl BatchXfer {
    pub(super) fn running(&self) -> bool {
        self.jobs
            .lock()
            .unwrap()
            .iter()
            .any(|j| matches!(j.state, JobState::Waiting | JobState::Running))
    }

    pub(super) fn add_files(&mut self, paths: Vec<PathBuf>, ctx: &egui::Context) {
        for p in paths {
            if (p.is_file() || p.is_dir()) && !self.files.iter().any(|f| f.path == p) {
                let summary = Arc::new(Mutex::new(None));
                let (s2, p2, ctx) = (Arc::clone(&summary), p.clone(), ctx.clone());
                thread::spawn(move || {
                    let r = local_summary(&p2);
                    *s2.lock().unwrap() = Some(r);
                    ctx.request_repaint();
                });
                self.files.push(LocalItem {
                    is_dir: p.is_dir(),
                    path: p,
                    summary,
                });
            }
        }
    }
}

/// `~`, `~/x`, `/abs` → a path SFTP understands (relative = home).
fn remote_dir(input: &str) -> String {
    let d = input.trim();
    if d.is_empty() || d == "~" {
        String::new()
    } else if let Some(rest) = d.strip_prefix("~/") {
        rest.trim_end_matches('/').to_string()
    } else {
        d.to_string()
    }
}

/// Make a host title usable as a local folder name.
fn folder_name(title: &str) -> String {
    let s: String = title
        .chars()
        .map(|c| {
            if c.is_control() || r#"<>:"/\|?*"#.contains(c) {
                '_'
            } else {
                c
            }
        })
        .collect();
    let trimmed = s.trim().trim_matches('.');
    if trimmed.is_empty() {
        return "host".into();
    }
    let safe = session::local_safe_name(trimmed);
    if safe == "file" && trimmed != "file" {
        "host".into()
    } else {
        safe
    }
}

fn job_error(e: &anyhow::Error, zh: bool) -> String {
    if e.downcast_ref::<session::Cancelled>().is_some() {
        (if zh { "已取消" } else { "Cancelled" }).into()
    } else {
        format!("{e:#}")
    }
}

fn set_job(jobs: &Arc<Mutex<Vec<BatchJob>>>, uid: u64, f: impl FnOnce(&mut BatchJob)) {
    if let Ok(mut j) = jobs.lock() {
        if let Some(job) = j.iter_mut().find(|j| j.uid == uid) {
            f(job);
        }
    }
}

impl GuiApp {
    pub(super) fn open_batch(&mut self, tab: usize) {
        self.batch.open = true;
        if !self.batch.running() {
            self.batch.tab = tab;
        }
    }

    fn pick_batch_files(&mut self) {
        self.open_picker(super::picker::PickFor::BatchUpload);
    }

    fn start_batch_upload(&mut self) {
        let targets = self.group_targets();
        if targets.is_empty() || self.batch.files.is_empty() {
            return;
        }
        let dir = remote_dir(&self.batch.remote_dir);
        let zh = self.zh();
        let paths: Vec<PathBuf> = self.batch.files.iter().map(|f| f.path.clone()).collect();
        let grand: u64 = self
            .batch
            .files
            .iter()
            .filter_map(|f| f.summary.lock().unwrap().map(|s| s.0))
            .sum();
        let jobs = Arc::clone(&self.batch.jobs);
        *jobs.lock().unwrap() = targets
            .iter()
            .map(|&i| BatchJob {
                uid: self.sessions[i].uid,
                name: self.sessions[i].title.clone(),
                done: 0,
                total: grand,
                scanning_files: None,
                state: JobState::Waiting,
            })
            .collect();
        self.batch.jobs_tab = 0;
        self.batch.out_dir = None;
        self.batch.cancel = Default::default();
        self.batch.skipped.lock().unwrap().clear();
        let cancel = Arc::clone(&self.batch.cancel);
        let skipped_out = Arc::clone(&self.batch.skipped);
        let mut hosts = Vec::new();
        for &i in &targets {
            let uid = self.sessions[i].uid;
            match self.slot_handle(i) {
                Some(h) => hosts.push((uid, h)),
                None => set_job(&jobs, uid, |j| {
                    j.state = JobState::Failed(self.t("未连接", "Not connected").into())
                }),
            }
        }
        let ctx = self.ctx.clone();
        let rt = Arc::clone(&self.runtime);
        self.runtime.spawn(async move {
            // Expand folders once (off the UI thread), then send to every host
            // in parallel, each over its own single SFTP session.
            let plan = tokio::task::spawn_blocking(move || plan_upload(&paths, &dir)).await;
            let super::UploadPlan {
                dirs,
                items,
                skipped,
            } = match plan {
                Ok(p) => p,
                Err(e) => {
                    let msg = e.to_string();
                    for (uid, _) in hosts {
                        set_job(&jobs, uid, |j| j.state = JobState::Failed(msg.clone()));
                    }
                    ctx.request_repaint();
                    return;
                }
            };
            *skipped_out.lock().unwrap() = skipped.clone();
            if dirs.is_empty() && items.is_empty() {
                let msg = if !skipped.is_empty() {
                    super::skipped_note(&skipped, zh)
                } else if zh {
                    "没有可上传的文件".into()
                } else {
                    "Nothing to upload".into()
                };
                for (uid, _) in hosts {
                    set_job(&jobs, uid, |j| j.state = JobState::Failed(msg.clone()));
                }
                ctx.request_repaint();
                return;
            }
            ctx.request_repaint();
            if cancel.load(std::sync::atomic::Ordering::Relaxed) {
                let msg = if zh { "已取消" } else { "Cancelled" }.to_string();
                for (uid, _) in hosts {
                    set_job(&jobs, uid, |j| j.state = JobState::Failed(msg.clone()));
                }
                ctx.request_repaint();
                return;
            }
            let dirs = Arc::new(dirs);
            let items = Arc::new(items);
            let sem = Arc::new(tokio::sync::Semaphore::new(MAX_BATCH_TRANSFER_HOSTS));
            for (uid, handle) in hosts {
                let jobs = Arc::clone(&jobs);
                let dirs = Arc::clone(&dirs);
                let items = Arc::clone(&items);
                let ctx = ctx.clone();
                let cancel = Arc::clone(&cancel);
                let sem = Arc::clone(&sem);
                rt.spawn(async move {
                    let Some(_permit) = acquire_batch_slot(&sem, &cancel).await else {
                        set_job(&jobs, uid, |j| {
                            j.state =
                                JobState::Failed((if zh { "已取消" } else { "Cancelled" }).into());
                        });
                        ctx.request_repaint();
                        return;
                    };
                    set_job(&jobs, uid, |j| j.state = JobState::Running);
                    ctx.request_repaint();
                    let (tx, mut rx) = mpsc::unbounded_channel::<session::TransferProgress>();
                    let job = tokio::spawn(async move {
                        session::sftp_upload_tree(&handle, &dirs, &items, tx, cancel).await
                    });
                    while let Some(p) = rx.recv().await {
                        set_job(&jobs, uid, |j| {
                            j.done = p.done;
                            j.total = p.total;
                        });
                        ctx.request_repaint();
                    }
                    let state = match job.await {
                        Ok(Ok(())) => JobState::Done,
                        Ok(Err(e)) => JobState::Failed(job_error(&e, zh)),
                        Err(e) => JobState::Failed(e.to_string()),
                    };
                    set_job(&jobs, uid, |j| {
                        if state == JobState::Done {
                            j.done = j.total;
                        }
                        j.state = state;
                    });
                    ctx.request_repaint();
                });
            }
        });
    }

    fn start_batch_download(&mut self) {
        let targets = self.group_targets();
        if targets.is_empty() {
            return;
        }
        let remote = self.batch.remote_path.trim().to_string();
        let name = remote
            .trim_end_matches('/')
            .rsplit('/')
            .next()
            .unwrap_or("")
            .to_string();
        if !safe_remote_name(&name) {
            self.toast_msg(
                self.t(
                    "请输入有效的远端文件或文件夹路径",
                    "Enter a valid remote file or folder path",
                ),
                StatusKind::Error,
            );
            return;
        }
        let base_dir = self.download_dir();
        // One folder per host; hosts that share a name get " (2)", " (3)" …
        // so their downloads can never land in the same file.
        // (A host literally named "web (2)" can't collide either.)
        let mut taken: std::collections::HashSet<String> = Default::default();
        let folders: Vec<String> = targets
            .iter()
            .map(|&i| {
                let base = folder_name(&self.sessions[i].title);
                let mut name = base.clone();
                let mut n = 1;
                while !taken.insert(name.to_lowercase()) {
                    n += 1;
                    name = format!("{base} ({n})");
                }
                name
            })
            .collect();
        let jobs = Arc::clone(&self.batch.jobs);
        *jobs.lock().unwrap() = targets
            .iter()
            .map(|&i| BatchJob {
                uid: self.sessions[i].uid,
                name: self.sessions[i].title.clone(),
                done: 0,
                total: 0,
                scanning_files: None,
                state: JobState::Waiting,
            })
            .collect();
        self.batch.jobs_tab = 1;
        self.batch.out_dir = Some(base_dir.clone());
        self.batch.cancel = Default::default();
        self.batch.skipped.lock().unwrap().clear();
        let zh = self.zh();
        let sem = Arc::new(tokio::sync::Semaphore::new(MAX_BATCH_TRANSFER_HOSTS));
        for (k, &i) in targets.iter().enumerate() {
            let uid = self.sessions[i].uid;
            let Some(handle) = self.slot_handle(i) else {
                set_job(&jobs, uid, |j| {
                    j.state = JobState::Failed(self.t("未连接", "Not connected").into())
                });
                continue;
            };
            let dir = base_dir.join(&folders[k]);
            let jobs = Arc::clone(&jobs);
            let remote = remote.clone();
            let ctx = self.ctx.clone();
            let cancel = Arc::clone(&self.batch.cancel);
            let sem = Arc::clone(&sem);
            self.runtime.spawn(async move {
                let Some(_permit) = acquire_batch_slot(&sem, &cancel).await else {
                    set_job(&jobs, uid, |j| {
                        j.scanning_files = None;
                        j.state =
                            JobState::Failed((if zh { "已取消" } else { "Cancelled" }).into());
                    });
                    ctx.request_repaint();
                    return;
                };
                set_job(&jobs, uid, |j| j.state = JobState::Running);
                ctx.request_repaint();
                if let Err(e) = tokio::fs::create_dir_all(&dir).await {
                    set_job(&jobs, uid, |j| {
                        j.scanning_files = None;
                        j.state = JobState::Failed(e.to_string());
                    });
                    ctx.request_repaint();
                    return;
                }
                let (tx, mut rx) = mpsc::unbounded_channel::<session::TransferProgress>();
                let d2 = dir.clone();
                let r2 = remote.clone();
                let job = tokio::spawn(async move {
                    session::sftp_download_entries(&handle, &[r2], &d2, tx, cancel).await
                });
                while let Some(p) = rx.recv().await {
                    set_job(&jobs, uid, |j| {
                        j.done = p.done;
                        j.total = p.total.max(p.done);
                        j.scanning_files = p.scanning_files;
                    });
                    ctx.request_repaint();
                }
                let state = match job.await {
                    Ok((outcome, Ok(()))) => {
                        if outcome.files_done == 0 && !outcome.skipped.is_empty() {
                            JobState::Failed(super::skipped_note(&outcome.skipped, zh))
                        } else {
                            JobState::Done
                        }
                    }
                    Ok((outcome, Err(e))) => {
                        if outcome.files_done == 0 {
                            let _ = tokio::fs::remove_dir(&dir).await;
                        }
                        if outcome.files_done > 0 {
                            let msg = job_error(&e, zh);
                            JobState::Failed(format!(
                                "{}/{} · {msg}",
                                outcome.files_done, outcome.files_total
                            ))
                        } else {
                            JobState::Failed(job_error(&e, zh))
                        }
                    }
                    Err(e) => {
                        let _ = tokio::fs::remove_dir(&dir).await;
                        JobState::Failed(e.to_string())
                    }
                };
                set_job(&jobs, uid, |j| {
                    j.scanning_files = None;
                    if state == JobState::Done {
                        j.total = j.total.max(j.done);
                        j.done = j.total;
                    }
                    j.state = state;
                });
                ctx.request_repaint();
            });
        }
    }

    /// Files dragged onto the window in group mode go to the batch sheet.
    pub(super) fn handle_group_drop(&mut self, ctx: &egui::Context) {
        if self.picker.is_some()
            || self.editor.is_some()
            || self.confirm.is_some()
            || self.text_prompt.is_some()
            || self.pw_prompt.is_some()
            || self.hostkey_prompt.is_some()
            || self.file_editor.is_some()
            || self.chmod_prompt.is_some()
            || self.snippet_editor.is_some()
        {
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
        let n = self.group_targets().len();
        if hovering {
            let t = self.tok();
            let screen = ctx.screen_rect();
            let painter = ctx.layer_painter(egui::LayerId::new(
                egui::Order::Foreground,
                egui::Id::new("group_drop_overlay"),
            ));
            painter.rect_filled(screen, 0.0, Color32::from_black_alpha(90));
            let card = Rect::from_center_size(screen.center(), Vec2::new(380.0, 150.0));
            painter.rect_filled(card, CornerRadius::same(18), t.card);
            let c = if n > 0 { t.accent } else { t.warn };
            painter.rect_stroke(
                card.shrink(10.0),
                CornerRadius::same(12),
                Stroke::new(1.5_f32, c),
                egui::StrokeKind::Inside,
            );
            w::paint_icon(
                &painter,
                Rect::from_center_size(card.center() - Vec2::new(0.0, 22.0), Vec2::splat(30.0)),
                Icon::Upload,
                c,
            );
            let msg = if n > 0 {
                if self.zh() {
                    format!("松开以分发到 {n} 台主机")
                } else {
                    format!("Drop to send to {n} hosts")
                }
            } else {
                self.t(
                    "请先选中至少一台在线主机",
                    "Select at least one online host",
                )
                .to_string()
            };
            painter.text(
                card.center() + Vec2::new(0.0, 24.0),
                Align2::CENTER_CENTER,
                msg,
                w::regular(14.0),
                t.text,
            );
        }
        if !dropped.is_empty() {
            self.batch.add_files(dropped, ctx);
            self.open_batch(0);
        }
    }

    pub(super) fn ui_batch(&mut self, ctx: &egui::Context) {
        if !self.batch.open {
            return;
        }
        let t = self.tok();
        let zh = self.zh();
        let targets = self.group_targets();
        let n = targets.len();
        let running = self.batch.running();
        let mut close = false;
        let mut go = false;
        let mut pick = false;
        let mut reveal: Option<PathBuf> = None;
        let resp = egui::Modal::new(egui::Id::new("batch_xfer"))
            .frame(sheet_frame(&t))
            .show(ctx, |ui| {
                ui.set_width(460.0);
                // Title
                let (r, _) =
                    ui.allocate_exact_size(Vec2::new(ui.available_width(), 26.0), Sense::hover());
                let g = w::galley(
                    ui,
                    if zh { "群发文件" } else { "Fleet files" },
                    w::semibold(16.0),
                    t.text,
                );
                let tw = g.size().x;
                ui.painter().galley(
                    Pos2::new(r.left(), r.center().y - g.size().y / 2.0),
                    g,
                    t.text,
                );
                let sub = if zh {
                    format!("{n} 台已选主机")
                } else {
                    format!("{n} selected hosts")
                };
                let g = w::galley(ui, &sub, w::regular(12.5), t.text2);
                ui.painter().galley(
                    Pos2::new(r.left() + tw + 8.0, r.center().y - g.size().y / 2.0 + 1.0),
                    g,
                    t.text2,
                );
                ui.add_space(12.0);

                let mut tab = self.batch.tab;
                ui.add_enabled_ui(!running, |ui| {
                    if w::segmented(
                        ui,
                        &[
                            (Some(Icon::Upload), if zh { "上传到主机" } else { "Upload" }),
                            (
                                Some(Icon::Download),
                                if zh { "从主机下载" } else { "Download" },
                            ),
                        ],
                        &mut tab,
                        (ui.available_width() - 4.0) / 2.0,
                    ) {
                        self.batch.tab = tab;
                    }
                });
                ui.add_space(14.0);

                if self.batch.tab == 0 {
                    // ---- local files ----
                    w::caption(ui, if zh { "本地文件" } else { "LOCAL FILES" });
                    let mut remove: Option<usize> = None;
                    let list_h = (self.batch.files.len().max(1) as f32 * 32.0 + 8.0).min(170.0);
                    let (box_r, _) = ui.allocate_exact_size(
                        Vec2::new(ui.available_width(), list_h),
                        Sense::hover(),
                    );
                    ui.painter()
                        .rect_filled(box_r, CornerRadius::same(10), t.control);
                    if self.batch.files.is_empty() {
                        let resp = ui.interact(box_r, ui.id().with("pick_zone"), Sense::click());
                        let c = if resp.hovered() { t.accent } else { t.text3 };
                        let g = w::galley(
                            ui,
                            if zh {
                                "拖入文件或文件夹，或点击选择"
                            } else {
                                "Drop files or folders, or click to choose"
                            },
                            w::regular(12.5),
                            c,
                        );
                        let total = 20.0 + g.size().x;
                        let x0 = box_r.center().x - total / 2.0;
                        w::paint_icon(
                            ui.painter(),
                            Rect::from_center_size(
                                Pos2::new(x0 + 7.0, box_r.center().y),
                                Vec2::splat(14.0),
                            ),
                            Icon::Upload,
                            c,
                        );
                        ui.painter().galley(
                            Pos2::new(x0 + 20.0, box_r.center().y - g.size().y / 2.0),
                            g,
                            c,
                        );
                        if resp.clicked() {
                            pick = true;
                        }
                    } else {
                        let mut cu = ui.new_child(
                            egui::UiBuilder::new()
                                .max_rect(box_r.shrink(4.0))
                                .layout(Layout::top_down(Align::Min)),
                        );
                        egui::ScrollArea::vertical()
                            .id_salt("batch_files")
                            .auto_shrink([false, false])
                            .show(&mut cu, |ui| {
                                ui.spacing_mut().item_spacing.y = 0.0;
                                for (k, p) in self.batch.files.iter().enumerate() {
                                    let (rr, rresp) = ui.allocate_exact_size(
                                        Vec2::new(ui.available_width(), 32.0),
                                        Sense::hover(),
                                    );
                                    if rresp.hovered() {
                                        ui.painter().rect_filled(
                                            rr,
                                            CornerRadius::same(7),
                                            t.control_hover,
                                        );
                                    }
                                    w::paint_icon(
                                        ui.painter(),
                                        Rect::from_center_size(
                                            Pos2::new(rr.left() + 16.0, rr.center().y),
                                            Vec2::splat(14.0),
                                        ),
                                        if p.is_dir {
                                            Icon::FolderFill
                                        } else {
                                            Icon::File
                                        },
                                        if p.is_dir { t.accent } else { t.text2 },
                                    );
                                    let name = p
                                        .path
                                        .file_name()
                                        .map(|s| s.to_string_lossy().to_string())
                                        .unwrap_or_default();
                                    let summary = *p.summary.lock().unwrap();
                                    let size = match (summary, p.is_dir) {
                                        (None, _) => {
                                            (if zh { "计算中…" } else { "Counting…" }).to_string()
                                        }
                                        (Some((b, n)), true) => {
                                            if zh {
                                                format!("{n} 个文件 · {}", format_bytes(b))
                                            } else {
                                                format!("{n} files · {}", format_bytes(b))
                                            }
                                        }
                                        (Some((b, _)), false) => format_bytes(b),
                                    };
                                    let sg = w::galley(ui, &size, w::regular(11.5), t.text3);
                                    let sw = sg.size().x;
                                    ui.painter().galley(
                                        Pos2::new(
                                            rr.right() - 34.0 - sw,
                                            rr.center().y - sg.size().y / 2.0,
                                        ),
                                        sg,
                                        t.text3,
                                    );
                                    let pr = ui.painter_at(rr);
                                    w::paint_text_trunc(
                                        ui,
                                        &pr,
                                        &name,
                                        w::regular(13.0),
                                        t.text,
                                        rr.left() + 32.0,
                                        rr.center().y,
                                        rr.width() - 80.0 - sw,
                                    );
                                    let xr = Rect::from_center_size(
                                        Pos2::new(rr.right() - 16.0, rr.center().y),
                                        Vec2::splat(20.0),
                                    );
                                    let xresp =
                                        ui.interact(xr, ui.id().with(("bf_rm", k)), Sense::click());
                                    if !running && (rresp.hovered() || xresp.hovered()) {
                                        if xresp.hovered() {
                                            ui.painter().rect_filled(
                                                xr,
                                                CornerRadius::same(5),
                                                t.control_press,
                                            );
                                        }
                                        w::paint_icon(
                                            ui.painter(),
                                            Rect::from_center_size(xr.center(), Vec2::splat(9.0)),
                                            Icon::Close,
                                            t.text2,
                                        );
                                        if xresp.clicked() {
                                            remove = Some(k);
                                        }
                                    }
                                }
                            });
                    }
                    if let Some(k) = remove {
                        self.batch.files.remove(k);
                    }
                    if !self.batch.files.is_empty() {
                        ui.add_space(4.0);
                        if w::button(
                            ui,
                            Kind::Plain,
                            Some(Icon::Plus),
                            if zh {
                                "添加文件或文件夹…"
                            } else {
                                "Add files or folders…"
                            },
                            !running,
                        )
                        .clicked()
                        {
                            pick = true;
                        }
                    }
                    ui.add_space(8.0);
                    w::caption(ui, if zh { "远端目录" } else { "REMOTE FOLDER" });
                    w::Field::new(&mut self.batch.remote_dir)
                        .icon(Icon::Folder)
                        .hint(if zh {
                            "~ 表示用户主目录，例如 /opt/app"
                        } else {
                            "~ is the home folder, e.g. /opt/app"
                        })
                        .mono()
                        .id(egui::Id::new("batch_remote_dir"))
                        .show(ui);
                    ui.add_space(4.0);
                    let g = w::galley(
                        ui,
                        if zh {
                            "同名文件会被覆盖"
                        } else {
                            "Files with the same name are overwritten"
                        },
                        w::regular(11.5),
                        t.text3,
                    );
                    let (r, _) = ui
                        .allocate_exact_size(Vec2::new(ui.available_width(), 18.0), Sense::hover());
                    ui.painter().galley(
                        Pos2::new(r.left() + 6.0, r.center().y - g.size().y / 2.0),
                        g,
                        t.text3,
                    );
                } else {
                    w::caption(
                        ui,
                        if zh {
                            "远端文件或文件夹"
                        } else {
                            "REMOTE FILE OR FOLDER"
                        },
                    );
                    w::Field::new(&mut self.batch.remote_path)
                        .icon(Icon::File)
                        .hint(if zh {
                            "例如 /var/log/syslog 或 ~/logs"
                        } else {
                            "e.g. /var/log/syslog or ~/logs"
                        })
                        .mono()
                        .id(egui::Id::new("batch_remote_path"))
                        .show(ui);
                    ui.add_space(4.0);
                    let note = format!(
                        "{} {}",
                        if zh {
                            "每台主机各存一份："
                        } else {
                            "One copy per host:"
                        },
                        self.download_dir()
                            .join(if zh { "<主机名>" } else { "<host>" })
                            .display()
                    );
                    let g = w::galley(ui, &note, w::regular(11.5), t.text3);
                    let (r, _) = ui
                        .allocate_exact_size(Vec2::new(ui.available_width(), 18.0), Sense::hover());
                    let pr = ui.painter_at(r);
                    pr.galley(
                        Pos2::new(r.left() + 6.0, r.center().y - g.size().y / 2.0),
                        g,
                        t.text3,
                    );
                }

                // ---- per-host progress ----
                let jobs = self.batch.jobs.lock().unwrap().clone();
                if !jobs.is_empty() && self.batch.jobs_tab == self.batch.tab {
                    ui.add_space(10.0);
                    let ok = jobs.iter().filter(|j| j.state == JobState::Done).count();
                    let fail = jobs
                        .iter()
                        .filter(|j| matches!(j.state, JobState::Failed(_)))
                        .count();
                    let head = if zh {
                        format!("进度　{ok} 完成 · {fail} 失败 · 共 {}", jobs.len())
                    } else {
                        format!("PROGRESS   {ok} done · {fail} failed · {}", jobs.len())
                    };
                    w::caption(ui, &head);
                    let h = (jobs.len() as f32 * 40.0).min(200.0);
                    egui::ScrollArea::vertical()
                        .id_salt("batch_jobs")
                        .max_height(h)
                        .auto_shrink([false, true])
                        .show(ui, |ui| {
                            ui.spacing_mut().item_spacing.y = 0.0;
                            for j in &jobs {
                                job_row(ui, &t, j, zh);
                            }
                        });
                    if self.batch.jobs_tab == 1 && !running && ok > 0 {
                        reveal = self.batch.out_dir.clone();
                    }
                }
                let skipped = self.batch.skipped.lock().unwrap().clone();
                if !skipped.is_empty() && self.batch.jobs_tab == 0 && self.batch.tab == 0 {
                    ui.add_space(6.0);
                    ui.add(
                        egui::Label::new(
                            egui::RichText::new(super::skipped_note(&skipped, zh))
                                .size(12.0)
                                .color(t.warn),
                        )
                        .wrap(),
                    );
                }

                // ---- footer ----
                ui.add_space(16.0);
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 8.0;
                    if let Some(dir) = reveal.take() {
                        if w::button(
                            ui,
                            Kind::Plain,
                            Some(Icon::Folder),
                            if zh {
                                "在文件夹中显示"
                            } else {
                                "Show in folder"
                            },
                            true,
                        )
                        .clicked()
                        {
                            native::reveal(&dir);
                        }
                    }
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        let can = n > 0
                            && !running
                            && if self.batch.tab == 0 {
                                !self.batch.files.is_empty()
                            } else {
                                !self.batch.remote_path.trim().is_empty()
                            };
                        let label = match (self.batch.tab, zh, running) {
                            (_, true, true) => "传输中…".to_string(),
                            (_, false, true) => "Transferring…".to_string(),
                            (0, true, _) => format!("上传到 {n} 台"),
                            (0, false, _) => format!("Upload to {n}"),
                            (_, true, _) => format!("从 {n} 台下载"),
                            (_, false, _) => format!("Download from {n}"),
                        };
                        if running {
                            if w::button_sized(
                                ui,
                                Kind::Destructive,
                                None,
                                if zh { "全部停止" } else { "Stop all" },
                                true,
                                130.0,
                            )
                            .clicked()
                            {
                                self.batch
                                    .cancel
                                    .store(true, std::sync::atomic::Ordering::Relaxed);
                            }
                        } else if w::button_sized(ui, Kind::Primary, None, &label, can, 130.0)
                            .clicked()
                        {
                            go = true;
                        }
                        if w::button(
                            ui,
                            Kind::Secondary,
                            None,
                            if zh { "关闭" } else { "Close" },
                            true,
                        )
                        .clicked()
                        {
                            close = true;
                        }
                    });
                });
            });
        if go {
            if self.batch.tab == 0 {
                self.start_batch_upload();
            } else {
                self.start_batch_download();
            }
        }
        if pick {
            self.pick_batch_files();
        }
        if resp.should_close() || close {
            // Transfers keep running in the background; reopening shows them.
            self.batch.open = false;
        }
    }
}

fn job_row(ui: &mut egui::Ui, t: &theme::Tokens, j: &BatchJob, zh: bool) {
    let (r, resp) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 40.0), Sense::hover());
    if let JobState::Failed(e) = &j.state {
        resp.on_hover_text(e.as_str());
    }
    let r = Rect::from_min_max(r.min, Pos2::new(r.right() - 10.0, r.bottom()));
    let p = ui.painter();
    let (dot, dstate) = match j.state {
        JobState::Done => (t.ok, w::DotState::Steady),
        JobState::Failed(_) => (t.err, w::DotState::Steady),
        JobState::Running => (t.accent, w::DotState::Busy),
        JobState::Waiting => (t.text3, w::DotState::Steady),
    };
    w::status_dot(
        ui.ctx(),
        p,
        Pos2::new(r.left() + 10.0, r.top() + 13.0),
        dot,
        dstate,
    );
    let pr = ui.painter_at(r);
    w::paint_text_trunc(
        ui,
        &pr,
        &j.name,
        w::semibold(12.5),
        t.text,
        r.left() + 22.0,
        r.top() + 13.0,
        r.width() * 0.45,
    );
    let status = match &j.state {
        JobState::Waiting => (if zh { "等待" } else { "Waiting" }).to_string(),
        JobState::Running if j.scanning_files.is_some() => {
            let found = j.scanning_files.unwrap_or(0);
            if j.total > 0 {
                if zh {
                    format!("扫描中… {found} 个 · {}", format_bytes(j.total))
                } else {
                    format!("Scanning… {found} · {}", format_bytes(j.total))
                }
            } else if zh {
                format!("扫描中… {found} 个")
            } else {
                format!("Scanning… {found}")
            }
        }
        JobState::Running if j.total > 0 => {
            format!("{} / {}", format_bytes(j.done), format_bytes(j.total))
        }
        JobState::Running => format_bytes(j.done),
        JobState::Done => (if zh { "完成" } else { "Done" }).to_string(),
        JobState::Failed(e) => e.clone(),
    };
    let sc = if matches!(j.state, JobState::Failed(_)) {
        t.err
    } else {
        t.text2
    };
    let sg = w::galley(ui, &status, w::regular(11.5), sc);
    let sw = sg.size().x.min(r.width() * 0.5);
    let sx = r.right() - sw;
    let spr = ui.painter_at(Rect::from_min_max(
        Pos2::new(r.left() + r.width() * 0.5, r.top()),
        r.max,
    ));
    w::paint_text_trunc(
        ui,
        &spr,
        &status,
        w::regular(11.5),
        sc,
        sx,
        r.top() + 13.0,
        r.width() * 0.5,
    );
    // progress track
    let track = Rect::from_min_size(
        Pos2::new(r.left() + 22.0, r.top() + 27.0),
        Vec2::new(r.width() - 22.0, 4.0),
    );
    p.rect_filled(track, CornerRadius::same(2), t.control);
    if j.state == JobState::Running && j.scanning_files.is_some() {
        w::paint_indeterminate_bar(ui, track, t.accent);
    } else {
        let frac = match j.state {
            JobState::Done => 1.0,
            _ if j.total > 0 => (j.done as f32 / j.total as f32).clamp(0.0, 1.0),
            _ => 0.0,
        };
        let k = ui
            .ctx()
            .animate_value_with_time(egui::Id::new(("batch_bar", j.uid)), frac, 0.25);
        if k > 0.0 {
            let fill = Rect::from_min_size(track.min, Vec2::new(track.width() * k, track.height()));
            let c = match j.state {
                JobState::Done => t.ok,
                JobState::Failed(_) => t.err,
                _ => t.accent,
            };
            p.rect_filled(fill, CornerRadius::same(2), c);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn acquire_batch_slot_caps_concurrency_and_honors_cancel() {
        let sem = Arc::new(tokio::sync::Semaphore::new(MAX_BATCH_TRANSFER_HOSTS));
        let cancel = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let mut held = Vec::new();
        for _ in 0..MAX_BATCH_TRANSFER_HOSTS {
            let permit = acquire_batch_slot(&sem, &cancel).await.unwrap();
            held.push(permit);
        }

        // Fifth slot must wait while all permits are held.
        let sem2 = Arc::clone(&sem);
        let cancel2 = Arc::clone(&cancel);
        let blocked = tokio::spawn(async move { acquire_batch_slot(&sem2, &cancel2).await });
        tokio::time::sleep(std::time::Duration::from_millis(60)).await;
        assert!(!blocked.is_finished());

        // Cancelling wakes the blocked waiter promptly with None.
        cancel.store(true, std::sync::atomic::Ordering::Relaxed);
        let res = tokio::time::timeout(std::time::Duration::from_millis(400), blocked)
            .await
            .expect("cancelled waiter did not wake")
            .unwrap();
        assert!(res.is_none());

        // Dropping one permit while not cancelled grants the freed slot cleanly.
        cancel.store(false, std::sync::atomic::Ordering::Relaxed);
        drop(held.pop());
        let permit = tokio::time::timeout(
            std::time::Duration::from_millis(200),
            acquire_batch_slot(&sem, &cancel),
        )
        .await
        .expect("freed permit was not acquired")
        .unwrap();
        drop(permit);
    }
}
