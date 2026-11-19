//! egui / eframe desktop client.
//!
//! Layout: toolbar | host list | session tabs (terminal / files) | remote monitor | status bar.
//! Connection settings live in a modal editor. UI text is Chinese or English (switchable).

mod batch;
mod chrome;
mod conn;
mod credentials;
mod files;
mod group;
mod highlight;
mod hosts;
mod panels;
mod picker;
mod snippets;
mod term;
mod theme;
mod widgets;

use crate::{bypass, profiles, session};
use eframe::egui;
use profiles::{LangPref, ThemePref};
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, LazyLock, Mutex};
use std::time::Instant;
use tokio::runtime::Runtime;
use tokio::sync::mpsc;

#[derive(Clone, Copy, PartialEq, Eq, Default, Debug)]
enum BindMode {
    #[default]
    Auto,
    Addr,
    Iface,
}

impl From<profiles::BindModePref> for BindMode {
    fn from(p: profiles::BindModePref) -> Self {
        match p {
            profiles::BindModePref::Auto => BindMode::Auto,
            profiles::BindModePref::Addr => BindMode::Addr,
            profiles::BindModePref::Iface => BindMode::Iface,
        }
    }
}

impl From<BindMode> for profiles::BindModePref {
    fn from(m: BindMode) -> Self {
        match m {
            BindMode::Auto => profiles::BindModePref::Auto,
            BindMode::Addr => profiles::BindModePref::Addr,
            BindMode::Iface => profiles::BindModePref::Iface,
        }
    }
}

/// Whole-window mode: one session at a time, or 群控 (group control).
#[derive(Clone, Copy, PartialEq, Eq)]
enum ViewMode {
    Single,
    Group,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum CenterView {
    Terminal,
    Files,
}

#[derive(Clone, Copy, PartialEq, Eq, Default, Debug)]
enum StatusKind {
    #[default]
    Info,
    Busy,
    Ok,
    Error,
}

const HIST_LEN: usize = 60;

#[derive(Clone, Default)]
struct MonitorUi {
    cpu_percent: Option<f32>,
    mem_used: Option<u64>,
    mem_total: Option<u64>,
    mem_percent: Option<f32>,
    disk_used: Option<u64>,
    disk_total: Option<u64>,
    disk_percent: Option<f32>,
    disk_mount: Option<String>,
    hostname: Option<String>,
    kernel: Option<String>,
    load_avg: Option<[f32; 3]>,
    uptime_secs: Option<u64>,
    cpu_cores: Option<u32>,
    net_rx_rate: Option<f64>,
    net_tx_rate: Option<f64>,
    cpu_hist: VecDeque<f32>,
    mem_hist: VecDeque<f32>,
    error: Option<String>,
}

#[derive(Clone, Default)]
struct FilesUi {
    cwd: String,
    /// Shared so the UI can read it every frame without copying the list.
    entries: Arc<Vec<session::RemoteDirEntry>>,
    /// Bumped on every navigation; late results for an older one are dropped.
    gen: u64,
    status: String,
    error: bool,
    loading: bool,
    /// A change (delete, rename, …) is running; another one waits for it.
    busy: bool,
}

#[derive(Clone, Default)]
struct TransferUi {
    label: String,
    done: u64,
    total: u64,
    scanning_files: Option<usize>,
    active: bool,
    is_download: bool,
    error: bool,
    /// Set by the "Cancel" button; the transfer stops at the next chunk.
    cancel: Arc<std::sync::atomic::AtomicBool>,
    started: Option<Instant>,
    /// Local path of the last finished download (for "show in folder").
    last_local: Option<PathBuf>,
}

struct SharedUi {
    connected: bool,
    connecting: bool,
    status: String,
    status_kind: StatusKind,
    session_ended: bool,
    /// The last attempt was refused by the server's authentication: a
    /// reconnect asks for the password again instead of resending it.
    auth_failed: bool,
    /// Someone is looking at this session's monitor (the panel is open on
    /// it, or on the group). Otherwise it is polled only now and then.
    monitor_wanted: bool,
    monitor: MonitorUi,
    bound_local: Option<String>,
    remote_addr: Option<String>,
    handle: Option<Arc<session::SshHandle>>,
    files: FilesUi,
    transfer: TransferUi,
    forward: String,
    /// Local notices (connect progress, errors) to print into the terminal.
    notices: Vec<(String, u8)>,
    /// Bumped on every connection attempt: a finished older attempt must not
    /// reset the state of the newer one.
    conn_gen: u64,
    /// Last finished attempt ended unexpectedly without auth or host-key failure.
    auto_reconnect_eligible: bool,
}

impl SharedUi {
    fn new(zh: bool) -> Self {
        Self {
            connected: false,
            connecting: false,
            status: if zh { "就绪".into() } else { "Ready".into() },
            status_kind: StatusKind::Info,
            session_ended: false,
            auth_failed: false,
            monitor_wanted: true,
            monitor: MonitorUi::default(),
            bound_local: None,
            remote_addr: None,
            handle: None,
            files: FilesUi {
                cwd: "~".into(),
                ..Default::default()
            },
            transfer: TransferUi::default(),
            forward: String::new(),
            notices: Vec::new(),
            conn_gen: 0,
            auto_reconnect_eligible: false,
        }
    }
}

/// Parameters needed to (re)connect a session tab.
#[derive(Clone, Default)]
struct ConnParams {
    label: String,
    host: String,
    port: u16,
    user: String,
    identity: String,
    password: String,
    key_passphrase: String,
    dns_server: String,
    forward_local: String,
    forward_remote: String,
    bind_mode: BindMode,
    bind_addr: String,
    bind_iface: String,
    profile_id: Option<String>,
    /// Save `password` with the host once it has signed in (asked for in
    /// the password prompt; a password that turns out wrong is not kept).
    remember_password: bool,
}

impl ConnParams {
    fn title(&self) -> String {
        if !self.label.trim().is_empty() {
            self.label.clone()
        } else if self.host.is_empty() {
            "Session".into()
        } else {
            format!("{}@{}", self.user, self.host)
        }
    }

    fn from_profile(p: &profiles::Profile) -> Self {
        Self {
            label: p.display_label(),
            host: p.host.clone(),
            port: p.port,
            user: p.user.clone(),
            identity: p.identity.clone(),
            password: p.get_password().unwrap_or_default(),
            key_passphrase: p.get_key_passphrase().unwrap_or_default(),
            dns_server: p.dns_server.clone(),
            forward_local: p.forward_local.clone(),
            forward_remote: p.forward_remote.clone(),
            bind_mode: p.bind_mode.into(),
            bind_addr: p.bind_addr.clone(),
            bind_iface: p.bind_iface.clone(),
            profile_id: Some(p.id.clone()),
            remember_password: false,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum FileSort {
    Name,
    Size,
    Modified,
}

/// One session tab with its own PTY / emulator / file browser state.
struct SessionSlot {
    uid: u64,
    title: String,
    params: ConnParams,
    shared: Arc<Mutex<SharedUi>>,
    term: term::TermView,
    input_tx: Option<mpsc::UnboundedSender<session::ChannelInput>>,
    stop_tx: Option<tokio::sync::oneshot::Sender<()>>,
    forward_stop: Option<tokio::sync::watch::Sender<bool>>,
    output_rx: Option<session::OutputSource>,
    disconnect_flag: Arc<AtomicBool>,
    view: CenterView,
    ever_connected: bool,
    /// `conn_gen` of the last connection seen established (each new one
    /// updates the host's "last connected").
    seen_conn_gen: u64,
    /// Output arrived while this tab was in the background.
    unread: bool,
    /// Selected entry names (multi-select with Ctrl / Shift).
    file_sel: Vec<String>,
    /// Anchor for Shift range selection.
    file_anchor: Option<String>,
    file_filter: String,
    show_hidden: bool,
    path_edit: Option<String>,
    sort: FileSort,
    sort_asc: bool,
    /// Visible rows of the file list, rebuilt only when the listing, filter
    /// or sort changes (folders can hold tens of thousands of entries).
    file_view: Option<files::FileView>,
    manual_disconnect: bool,
    auto_reconnect_at: Option<Instant>,
    auto_reconnect_attempts: u32,
}

impl SessionSlot {
    fn new(uid: u64, zh: bool, scrollback: usize) -> Self {
        Self {
            uid,
            title: if zh {
                "新会话".into()
            } else {
                "New session".into()
            },
            params: ConnParams::default(),
            shared: Arc::new(Mutex::new(SharedUi::new(zh))),
            term: term::TermView::new(30, 100, scrollback),
            input_tx: None,
            stop_tx: None,
            forward_stop: None,
            output_rx: None,
            disconnect_flag: Arc::new(AtomicBool::new(false)),
            view: CenterView::Terminal,
            ever_connected: false,
            seen_conn_gen: 0,
            unread: false,
            file_sel: Vec::new(),
            file_anchor: None,
            file_filter: String::new(),
            show_hidden: false,
            path_edit: None,
            sort: FileSort::Name,
            sort_asc: true,
            file_view: None,
            manual_disconnect: false,
            auto_reconnect_at: None,
            auto_reconnect_attempts: 0,
        }
    }

    fn state(&self) -> (bool, bool) {
        let s = self.shared.lock().unwrap();
        (s.connecting, s.connected)
    }

    fn is_busy(&self) -> bool {
        let s = self.shared.lock().unwrap();
        s.connecting || s.connected || s.transfer.active
    }

    /// A never-used tab that can host a new connection.
    fn is_fresh(&self) -> bool {
        !self.is_busy() && !self.ever_connected && self.term.is_empty()
    }

    fn term_id(&self) -> egui::Id {
        egui::Id::new(("ssh_term", self.uid))
    }
}

static HOSTKEY_PROMPT: LazyLock<Mutex<Option<HostKeyPrompt>>> = LazyLock::new(|| Mutex::new(None));

#[derive(Clone)]
struct HostKeyPrompt {
    host: String,
    port: u16,
    fingerprint: String,
    algorithm: String,
    /// Saved fingerprint when the key changed; `None` on first connection.
    previous: Option<String>,
    /// `previous` is a key of another type (the server now shows a new kind
    /// of key): asked like a first connection, not the "changed" alarm.
    other_type: bool,
    decision: Arc<Mutex<Option<bool>>>,
}

/// Host editor modal state.
#[derive(Clone, Copy, PartialEq, Eq)]
enum EditorTab {
    General,
    Auth,
    Network,
}

#[derive(Clone)]
struct HostEditor {
    /// Existing profile id (None = new host).
    id: Option<String>,
    tab: EditorTab,
    name: String,
    folder: String,
    host: String,
    port: String,
    user: String,
    identity: String,
    password: String,
    remember_password: bool,
    key_passphrase: String,
    remember_key_passphrase: bool,
    show_password: bool,
    dns_server: String,
    forward_local: String,
    forward_remote: String,
    bind_mode: BindMode,
    bind_addr: String,
    selected_candidate: usize,
    error: String,
    focus_first: bool,
    /// Keys found in ~/.ssh (looked up once when the editor opens).
    found_keys: Arc<Vec<PathBuf>>,
    /// A saved secret couldn't be read (credential store unreachable): an
    /// empty field then means "keep it", not "forget it".
    password_unreadable: bool,
    passphrase_unreadable: bool,
    credentials_loading: bool,
}

struct PasswordPrompt {
    params: ConnParams,
    password: String,
    remember: bool,
    focus: bool,
    /// Reconnect into this tab instead of picking one.
    tab_uid: Option<u64>,
}

enum Confirm {
    DeleteHost(String),
    DeleteFolder(String),
    DeleteFiles {
        tab_uid: u64,
        /// Folder the items were selected in (not whatever is open later).
        dir: String,
        items: Vec<(String, bool)>,
    },
    CloseTab(u64),
    /// Quit although something would be lost (what, for the user).
    Quit(String),
}

enum TextPrompt {
    NewFolder(String),
    RenameFolder {
        from: String,
        to: String,
    },
    /// `dir`: the folder shown when the dialog opened (the answer applies
    /// there even if the listing changed meanwhile).
    NewRemoteFile {
        tab_uid: u64,
        dir: String,
        name: String,
    },
    NewRemoteDir {
        tab_uid: u64,
        dir: String,
        name: String,
    },
    RenameRemote {
        tab_uid: u64,
        dir: String,
        from: String,
        to: String,
    },
}

#[derive(Default)]
struct EditorIo {
    loaded: Option<Result<(String, Option<session::FileStamp>), String>>,
    /// Err((changed on the server, message))
    saved: Option<Result<Option<session::FileStamp>, (bool, String)>>,
}

/// Built-in editor for a remote text file.
struct FileEditor {
    tab_uid: u64,
    path: String,
    name: String,
    text: String,
    original: String,
    io: Arc<Mutex<EditorIo>>,
    loading: bool,
    saving: bool,
    error: String,
    confirm_discard: bool,
    focus: bool,
    /// Loading failed: the buffer is not the file, so it must not be saved.
    load_failed: bool,
    /// The file used CRLF line endings (restored on save).
    crlf: bool,
    /// Buffer differs from what is on the server (updated on edits only).
    dirty: bool,
    line_count: usize,
    /// Size / mtime when loaded (or last saved): a save is refused if the
    /// server's copy no longer matches.
    stamp: Option<session::FileStamp>,
    /// The last save was refused because the file changed on the server.
    conflict: bool,
    /// The buffer as it was sent by the save in flight: edits typed while
    /// saving stay "unsaved".
    saving_text: Option<String>,
    find_open: bool,
    replace_open: bool,
    find_query: String,
    replace_text: String,
    find_case: bool,
    find_focus: bool,
    find_cur: usize,
    pending_select: Option<(usize, usize)>,
}

struct ChmodPrompt {
    tab_uid: u64,
    dir: String,
    name: String,
    mode: u32,
    octal: String,
    focus: bool,
}

/// (query, tab uid, log generation)
type FindKey = (String, u64, u64);
/// (match count, last 500 matching lines with their line numbers)
/// (match count, last 500 matches: line number, excerpt shown, where the
/// whole line is in the log as (offset counted from the log's very start,
/// length) — see `TermView::log_trimmed`)
type FindHits = (usize, Vec<(usize, String, u64, usize)>);

struct FindBar {
    query: String,
    focus: bool,
    /// Last search: (query, tab uid, log generation) → (match count, last
    /// 500 matching lines). Re-run only when one of the keys changes.
    cache: Option<(FindKey, FindHits)>,
    /// When the cached search ran (egui time).
    searched_at: f64,
}

enum PickResult {
    Upload { tab_uid: u64, paths: Vec<PathBuf> },
    Identity(PathBuf),
    DownloadDir(PathBuf),
    BatchUpload(Vec<PathBuf>),
}

pub struct GuiApp {
    credentials: credentials::CredentialQueue,
    ctx: egui::Context,
    config: profiles::AppConfig,
    theme: ThemePref,
    lang: LangPref,
    applied_theme: Option<ThemePref>,
    term_font_size: f32,
    candidates: Arc<Vec<bypass::IfaceCandidate>>,
    /// Sidebar order / folder list, rebuilt only when the hosts change.
    host_cache: std::cell::RefCell<Option<hosts::HostCache>>,
    sessions: Vec<SessionSlot>,
    active: usize,
    next_uid: u64,
    selected_profile_id: Option<String>,
    host_filter: String,
    quick_connect: String,
    editor: Option<HostEditor>,
    pw_prompt: Option<PasswordPrompt>,
    confirm: Option<Confirm>,
    text_prompt: Option<TextPrompt>,
    hostkey_prompt: Option<HostKeyPrompt>,
    find: Option<FindBar>,
    /// (message, kind, shown at, hidden at)
    toast: Option<(String, StatusKind, f64, f64)>,
    picks: Arc<Mutex<Vec<PickResult>>>,
    /// Move keyboard focus to the active terminal on the next frame.
    focus_term: bool,
    show_shortcuts: bool,
    /// The user confirmed quitting: the next close request goes through.
    quit_confirmed: bool,
    file_editor: Option<FileEditor>,
    chmod_prompt: Option<ChmodPrompt>,
    snippet_filter: String,
    snippet_editor: Option<snippets::SnippetEditor>,
    /// Command input bar text and history cursor.
    cmd_input: String,
    cmd_hist_pos: Option<usize>,
    cmd_composing: bool,
    /// What was typed before browsing history with ↑ (restored by ↓).
    cmd_draft: String,
    /// Scroll the active tab into view on the next frame.
    tab_scroll_pending: bool,
    view_mode: ViewMode,
    mode_changed_at: f64,
    /// Sessions (uid) left out of group broadcast.
    group_excluded: std::collections::HashSet<u64>,
    /// Mirror keystrokes typed in any tile to every selected host.
    group_sync: bool,
    /// Group-mode "fleet files" sheet (batch upload / download).
    batch: batch::BatchXfer,
    /// In-app file / folder chooser.
    picker: Option<picker::LocalPicker>,
    picker_last: Option<PathBuf>,
    /// Password prompts waiting their turn (e.g. "connect all").
    pw_queue: VecDeque<(ConnParams, Option<u64>)>,
    /// Theme last pushed to the native (Windows) title bar.
    titlebar_theme: Option<ThemePref>,
    /// Custom chrome state last pushed to the window (None = not yet).
    applied_chrome: Option<bool>,
    /// Terminal size flash after a resize: (cols, rows, time).
    size_hud: Option<(u16, u16, f64)>,
    window_title: String,
    runtime: Arc<Runtime>,
    cjk_font: Option<String>,
    /// Where the next frame's output round starts (fair share between tabs).
    output_rr: usize,
}

impl GuiApp {
    pub fn new(
        cc: &eframe::CreationContext<'_>,
        config: profiles::AppConfig,
        prefill_host: Option<String>,
        prefill_user: Option<String>,
        prefill_port: u16,
    ) -> Self {
        let scrollback = config.scrollback_lines;
        let theme = config.theme;
        let lang = config.lang;
        let cjk_font = theme::install_fonts(&cc.egui_ctx);
        theme::apply(&cc.egui_ctx, theme);
        // Ctrl +/- must reach the terminal; terminal font size has its own shortcuts.
        cc.egui_ctx.options_mut(|o| o.zoom_with_keyboard = false);

        let candidates = bypass::list_bind_candidates().unwrap_or_default();
        let term_font_size = config.term_font_size.clamp(9.0, 32.0);
        let selected_profile_id = config.last_profile_id.clone();
        let zh = lang == LangPref::Zh;

        let mut app = Self {
            credentials: credentials::CredentialQueue::new(cc.egui_ctx.clone()),
            ctx: cc.egui_ctx.clone(),
            config,
            theme,
            lang,
            applied_theme: Some(theme),
            term_font_size,
            candidates: Arc::new(candidates),
            host_cache: Default::default(),
            sessions: vec![SessionSlot::new(1, zh, scrollback)],
            active: 0,
            next_uid: 2,
            selected_profile_id,
            host_filter: String::new(),
            quick_connect: String::new(),
            editor: None,
            pw_prompt: None,
            confirm: None,
            text_prompt: None,
            hostkey_prompt: None,
            find: None,
            toast: None,
            picks: Arc::new(Mutex::new(Vec::new())),
            focus_term: false,
            show_shortcuts: false,
            quit_confirmed: false,
            file_editor: None,
            chmod_prompt: None,
            snippet_filter: String::new(),
            snippet_editor: None,
            cmd_input: String::new(),
            cmd_hist_pos: None,
            cmd_composing: false,
            cmd_draft: String::new(),
            tab_scroll_pending: false,
            titlebar_theme: None,
            applied_chrome: None,
            view_mode: ViewMode::Single,
            mode_changed_at: 0.0,
            group_excluded: Default::default(),
            group_sync: false,
            batch: batch::BatchXfer::default(),
            picker: None,
            picker_last: None,
            pw_queue: VecDeque::new(),
            size_hud: None,
            window_title: String::new(),
            runtime: Arc::new(Runtime::new().expect("tokio runtime")),
            cjk_font,
            output_rr: 0,
        };

        // profiles.json could not be read: it was moved aside, say where.
        if let Some((moved, w)) = app.config.load_warning.take() {
            let msg = if moved {
                format!(
                    "{} {w}",
                    app.t(
                        "主机配置已损坏，原文件已备份为",
                        "Saved hosts were unreadable; the old file was kept as"
                    )
                )
            } else {
                format!(
                    "{} {w} — {}",
                    app.t("无法读取主机配置：", "Couldn't read saved hosts:"),
                    app.t(
                        "为免覆盖原文件，本次的修改不会保存",
                        "changes won't be saved this time, so the file isn't overwritten"
                    )
                )
            };
            app.toast_msg(msg, StatusKind::Error);
            app.toast = app.toast.take().map(|(m, k, t0, t1)| (m, k, t0, t1 + 8.0));
        }
        term::CTRL_V_TO_APPS.store(
            app.config.ctrl_v_to_apps,
            std::sync::atomic::Ordering::Relaxed,
        );
        app.init_credentials();
        // Partial downloads left by a crashed / killed run.
        let dl = app.download_dir();
        std::thread::spawn(move || session::clean_stale_parts(&dl));

        // `tern --gui user@host`: open the editor prefilled.
        if let Some(host) = prefill_host.filter(|h| !h.trim().is_empty()) {
            let mut ed = app.blank_editor();
            ed.host = host;
            ed.user = prefill_user.unwrap_or_else(|| "root".into());
            ed.port = prefill_port.to_string();
            ed.tab = EditorTab::Auth;
            app.editor = Some(ed);
        }
        app
    }

    fn zh(&self) -> bool {
        self.lang == LangPref::Zh
    }

    /// Pick the Chinese or English string.
    fn t(&self, zh: &'static str, en: &'static str) -> &'static str {
        if self.zh() {
            zh
        } else {
            en
        }
    }

    fn tok(&self) -> theme::Tokens {
        theme::tokens(self.theme)
    }

    fn active_slot(&self) -> &SessionSlot {
        &self.sessions[self.active.min(self.sessions.len() - 1)]
    }

    fn active_slot_mut(&mut self) -> &mut SessionSlot {
        let i = self.active.min(self.sessions.len() - 1);
        self.active = i;
        &mut self.sessions[i]
    }

    fn slot_index(&self, uid: u64) -> Option<usize> {
        self.sessions.iter().position(|s| s.uid == uid)
    }

    fn sync_config(&mut self) {
        self.config.theme = self.theme;
        self.config.lang = self.lang;
        self.config.last_profile_id = self.selected_profile_id.clone();
        self.config.term_font_size = self.term_font_size;
    }

    /// Save settings in the background (bursts of changes become one write).
    fn persist(&mut self) {
        self.sync_config();
        profiles::save_later(&self.config);
    }

    fn toast_msg(&mut self, msg: impl Into<String>, kind: StatusKind) {
        let now = self.ctx.input(|i| i.time);
        self.toast = Some((msg.into(), kind, now, now + 2.6));
        self.ctx.request_repaint();
    }

    fn set_font_size(&mut self, size: f32) {
        let s = size.clamp(9.0, 32.0);
        if (s - self.term_font_size).abs() > f32::EPSILON {
            self.term_font_size = s;
            self.persist();
        }
    }

    fn new_tab(&mut self) -> usize {
        let uid = self.next_uid;
        self.next_uid += 1;
        self.sessions.push(SessionSlot::new(
            uid,
            self.zh(),
            self.config.scrollback_lines,
        ));
        self.active = self.sessions.len() - 1;
        self.tab_scroll_pending = true;
        self.find = None;
        self.active
    }

    /// Close a tab; asks first when it is live.
    fn request_close_tab(&mut self, idx: usize) {
        let Some(slot) = self.sessions.get(idx) else {
            return;
        };
        if slot.is_busy() {
            self.confirm = Some(Confirm::CloseTab(slot.uid));
        } else {
            self.close_tab(idx);
        }
    }

    fn close_tab(&mut self, idx: usize) {
        if idx >= self.sessions.len() {
            return;
        }
        let old_uid = self.sessions[idx].uid;
        self.group_excluded.remove(&old_uid);
        self.pw_queue.retain(|(_, u)| *u != Some(old_uid));
        self.disconnect_at(idx);
        self.find = None;
        if self.sessions.len() == 1 {
            let uid = self.next_uid;
            self.next_uid += 1;
            self.sessions[0] = SessionSlot::new(uid, self.zh(), self.config.scrollback_lines);
            self.active = 0;
            return;
        }
        self.sessions.remove(idx);
        if self.active > idx || self.active >= self.sessions.len() {
            self.active = self.active.saturating_sub(1);
        }
    }

    fn switch_tab(&mut self, idx: usize) {
        if idx < self.sessions.len() && idx != self.active {
            self.active = idx;
            self.sessions[idx].unread = false;
            self.find = None;
            self.focus_term = true;
            self.tab_scroll_pending = true;
        }
    }

    fn refresh_ifaces(&mut self) {
        if let Ok(list) = bypass::list_bind_candidates() {
            self.candidates = Arc::new(list);
        }
    }

    fn download_dir(&self) -> PathBuf {
        if !self.config.download_dir.trim().is_empty() {
            return PathBuf::from(self.config.download_dir.trim());
        }
        default_download_dir()
    }

    fn handle_picks(&mut self) {
        let picks: Vec<PickResult> = std::mem::take(&mut *self.picks.lock().unwrap());
        for p in picks {
            match p {
                PickResult::Upload { tab_uid, paths } => {
                    if let Some(i) = self.slot_index(tab_uid) {
                        self.upload_paths(i, paths);
                    }
                }
                PickResult::BatchUpload(paths) => {
                    self.batch.add_files(paths, &self.ctx);
                    self.batch.open = true;
                }
                PickResult::Identity(path) => {
                    if let Some(ed) = self.editor.as_mut() {
                        ed.identity = path.display().to_string();
                    }
                }
                PickResult::DownloadDir(path) => {
                    self.config.download_dir = path.display().to_string();
                    self.persist();
                    let msg = format!(
                        "{} {}",
                        self.t("下载目录：", "Download folder:"),
                        path.display()
                    );
                    self.toast_msg(msg, StatusKind::Ok);
                }
            }
        }
    }

    /// Global keyboard shortcuts (consumed before the terminal sees them).
    fn handle_shortcuts(&mut self, ctx: &egui::Context) {
        use egui::{Key, Modifiers};
        let cs = Modifiers::COMMAND | Modifiers::SHIFT;
        let modal_open = self.modal_open();
        let group_key = ctx.input_mut(|i| i.consume_key(cs, Key::G));
        let (new_tab, close_tab, next, prev, find, bigger, smaller, new_host) =
            ctx.input_mut(|i| {
                (
                    i.consume_key(cs, Key::T),
                    i.consume_key(cs, Key::W),
                    !i.modifiers.shift && i.consume_key(Modifiers::CTRL, Key::Tab),
                    i.consume_key(Modifiers::CTRL | Modifiers::SHIFT, Key::Tab),
                    i.consume_key(cs, Key::F),
                    i.consume_key(cs, Key::Equals) || i.consume_key(cs, Key::Plus),
                    i.consume_key(cs, Key::Minus),
                    i.consume_key(cs, Key::N),
                )
            });
        if modal_open {
            return;
        }
        if group_key {
            self.set_view_mode(if self.view_mode == ViewMode::Group {
                ViewMode::Single
            } else {
                ViewMode::Group
            });
        }
        if new_tab {
            self.new_tab();
        }
        if close_tab {
            self.request_close_tab(self.active);
        }
        let n = self.sessions.len();
        if next && n > 1 {
            self.switch_tab((self.active + 1) % n);
        }
        if prev && n > 1 {
            self.switch_tab((self.active + n - 1) % n);
        }
        if find {
            self.active_slot_mut().view = CenterView::Terminal;
            if let Some(f) = self.find.as_mut() {
                f.focus = true;
            } else {
                self.find = Some(FindBar {
                    query: String::new(),
                    focus: true,
                    cache: None,
                    searched_at: 0.0,
                });
            }
        }
        if bigger {
            self.set_font_size(self.term_font_size + 1.0);
        }
        if smaller {
            self.set_font_size(self.term_font_size - 1.0);
        }
        if new_host {
            self.editor = Some(self.blank_editor());
        }
    }

    /// Colour scheme used by the terminal (independent of the app theme).
    fn term_theme_pref(&self) -> ThemePref {
        match self.config.term_theme {
            profiles::TermThemePref::Dark => ThemePref::Dark,
            profiles::TermThemePref::Light => ThemePref::Light,
            profiles::TermThemePref::System => self.theme,
        }
    }

    /// What quitting now would throw away, or `None` when nothing is.
    fn quit_blockers(&self) -> Option<String> {
        let zh = self.zh();
        let mut what: Vec<String> = Vec::new();
        if self.credentials.is_pending() {
            what.push(
                self.t(
                    "凭据操作尚未完成",
                    "credential operations are still pending",
                )
                .into(),
            );
        }
        if let Some(ed) = &self.file_editor {
            if ed.saving || (!ed.loading && !ed.load_failed && ed.dirty) {
                what.push(if zh {
                    format!("「{}」有未保存的修改", ed.name)
                } else {
                    format!("unsaved changes in \"{}\"", ed.name)
                });
            }
        }
        let live = self.sessions.iter().filter(|s| s.is_busy()).count();
        if live > 0 {
            what.push(if zh {
                format!("{live} 个会话仍在连接")
            } else if live == 1 {
                "a session is still connected".into()
            } else {
                format!("{live} sessions are still connected")
            });
        }
        let transfers = self
            .sessions
            .iter()
            .any(|s| s.shared.lock().is_ok_and(|s| s.transfer.active))
            || self.batch.running();
        if transfers {
            what.push(if zh {
                "还有文件在传输".into()
            } else {
                "files are still being transferred".into()
            });
        }
        (!what.is_empty()).then(|| what.join(if zh { "，" } else { "; " }))
    }

    fn modal_open(&self) -> bool {
        self.editor.is_some()
            || self.pw_prompt.is_some()
            || self.confirm.is_some()
            || self.text_prompt.is_some()
            || self.hostkey_prompt.is_some()
            || self.show_shortcuts
            || self.file_editor.is_some()
            || self.chmod_prompt.is_some()
            || self.snippet_editor.is_some()
            || self.batch.open
            || self.picker.is_some()
    }
}

impl eframe::App for GuiApp {
    fn update(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        if self.applied_theme != Some(self.theme) {
            theme::apply(ctx, self.theme);
            self.applied_theme = Some(self.theme);
        }
        let custom = self.custom_chrome();
        if self.applied_chrome != Some(custom) {
            if self.applied_chrome.is_some() {
                ctx.send_viewport_cmd(egui::ViewportCommand::Decorations(!custom));
            }
            if custom {
                theme::style_borderless(frame);
            }
            self.applied_chrome = Some(custom);
            self.titlebar_theme = None;
        }
        if self.titlebar_theme != Some(self.theme) {
            // The OS theme of the window itself (Windows: system title bar,
            // scroll bars of native dialogs) follows the app's.
            ctx.send_viewport_cmd(egui::ViewportCommand::SetTheme(match self.theme {
                ThemePref::Dark => egui::SystemTheme::Dark,
                ThemePref::Light => egui::SystemTheme::Light,
            }));
            theme::style_title_bar(frame, &self.tok());
            self.titlebar_theme = Some(self.theme);
        }

        for update in self.credentials.drain() {
            update(self);
        }
        self.poll_sessions();
        // Closing the window (title bar X, Alt+F4, our own close button)
        // while it would lose something asks first.
        if ctx.input(|i| i.viewport().close_requested()) && !self.quit_confirmed {
            if let Some(what) = self.quit_blockers() {
                ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
                self.confirm = Some(Confirm::Quit(what));
                ctx.request_repaint();
            }
        }
        self.handle_picks();
        // A question withdrawn by its connection (cancelled / tab closed)
        // closes its dialog too.
        if let Some(shown) = &self.hostkey_prompt {
            let still_open = HOSTKEY_PROMPT
                .lock()
                .unwrap()
                .as_ref()
                .is_some_and(|p| Arc::ptr_eq(&p.decision, &shown.decision));
            if !still_open {
                self.hostkey_prompt = None;
            }
        }
        if self.hostkey_prompt.is_none() {
            if let Some(prompt) = HOSTKEY_PROMPT
                .lock()
                .unwrap()
                .clone()
                .filter(|p| p.decision.lock().unwrap().is_none())
            {
                self.hostkey_prompt = Some(prompt);
            }
        }
        self.handle_shortcuts(ctx);

        // Tell each session's monitor task whether anyone looks at it. (No
        // timed repaint: new stats, output and status changes wake the UI
        // themselves.)
        let group = self.view_mode == ViewMode::Group;
        for (i, slot) in self.sessions.iter().enumerate() {
            let wanted = self.config.show_monitor && (group || i == self.active);
            if let Ok(mut s) = slot.shared.lock() {
                s.monitor_wanted = wanted;
            }
        }

        let tok = self.tok();
        if custom {
            egui::TopBottomPanel::top("caption")
                .exact_height(chrome::CAPTION_H)
                .show_separator_line(false)
                .frame(egui::Frame::NONE.fill(tok.chrome))
                .show(ctx, |ui| self.ui_caption(ui));
        }
        egui::TopBottomPanel::top("toolbar")
            .exact_height(if custom { 46.0 } else { 48.0 })
            .show_separator_line(true)
            .frame(
                egui::Frame::NONE
                    .fill(tok.chrome)
                    .inner_margin(egui::Margin::symmetric(12, 0)),
            )
            .show(ctx, |ui| self.ui_toolbar(ui));

        egui::SidePanel::left("hosts_panel")
            .resizable(true)
            .default_width(264.0)
            .width_range(220.0..=420.0)
            .frame(egui::Frame::NONE.fill(tok.chrome))
            .show_animated(ctx, self.config.show_sidebar, |ui| {
                egui::Frame::NONE
                    .inner_margin(egui::Margin {
                        left: 12,
                        right: 12,
                        top: 14,
                        bottom: 10,
                    })
                    .show(ui, |ui| {
                        ui.set_min_size(ui.available_size());
                        self.ui_sidebar(ui);
                    });
            });

        egui::SidePanel::right("monitor_panel")
            .resizable(true)
            .default_width(268.0)
            .width_range(230.0..=380.0)
            .frame(egui::Frame::NONE.fill(tok.chrome))
            .show_animated(ctx, self.config.show_monitor, |ui| {
                egui::Frame::NONE
                    .inner_margin(egui::Margin {
                        left: 14,
                        right: 14,
                        top: 14,
                        bottom: 10,
                    })
                    .show(ui, |ui| {
                        ui.set_min_size(ui.available_size());
                        if self.view_mode == ViewMode::Group {
                            self.ui_group_monitor(ui);
                        } else {
                            self.ui_monitor(ui);
                        }
                    });
            });

        egui::CentralPanel::default()
            .frame(egui::Frame::NONE.fill(tok.bg).inner_margin(egui::Margin {
                left: 14,
                right: 14,
                top: 10,
                bottom: 14,
            }))
            .show(ctx, |ui| {
                // Soft cross-fade when switching between 会话 / 群控. (Only the
                // opacity animates: moving the layout would resize the
                // terminals mid-transition and send the server a size that
                // only existed for a moment.)
                let now = ui.input(|i| i.time);
                let k = ((now - self.mode_changed_at) / 0.22).clamp(0.0, 1.0) as f32;
                if k < 1.0 {
                    let e = 1.0 - (1.0 - k).powi(3);
                    ui.multiply_opacity(e);
                    ui.ctx().request_repaint();
                }
                if self.view_mode == ViewMode::Group {
                    self.ui_group(ui);
                } else {
                    self.ui_tab_bar(ui);
                    ui.add_space(10.0);
                    self.ui_center(ui);
                }
            });

        self.ui_dialogs(ctx);
        self.ui_toast(ctx);
        if custom {
            self.chrome_edges(ctx);
        }

        // Window title follows the active session.
        let slot = self.active_slot();
        let title = if slot.params.host.is_empty() {
            "Tern".to_string()
        } else {
            format!("{} — Tern", slot.title)
        };
        if title != self.window_title {
            ctx.send_viewport_cmd(egui::ViewportCommand::Title(title.clone()));
            self.window_title = title;
        }
    }

    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        self.tok().bg.to_normalized_gamma_f32()
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        for i in 0..self.sessions.len() {
            self.disconnect_at(i);
        }
        // Final save is synchronous so nothing pending is lost on quit.
        self.sync_config();
        if let Err(e) = profiles::save(&self.config) {
            eprintln!("[config] save failed: {e:#}");
        }
    }
}

/// The user's Downloads folder: on Windows where the system says it is
/// (it can be moved, e.g. to another drive), else `~/Downloads`. Looked up
/// once.
fn default_download_dir() -> PathBuf {
    static DIR: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    DIR.get_or_init(|| {
        #[cfg(windows)]
        if let Some(p) = windows_downloads_dir() {
            return p;
        }
        std::env::var("USERPROFILE")
            .or_else(|_| std::env::var("HOME"))
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from("."))
            .join("Downloads")
    })
    .clone()
}

/// `FOLDERID_Downloads` from the shell (follows a moved Downloads folder).
#[cfg(windows)]
fn windows_downloads_dir() -> Option<PathBuf> {
    use std::os::windows::ffi::OsStringExt;
    use windows_sys::Win32::System::Com::CoTaskMemFree;
    use windows_sys::Win32::UI::Shell::{FOLDERID_Downloads, SHGetKnownFolderPath};
    // SAFETY: SHGetKnownFolderPath fills `p` with a NUL-terminated string
    // allocated by the shell (or leaves it null); it is read up to the NUL
    // and freed with CoTaskMemFree, as documented, whatever the result.
    unsafe {
        let mut p: windows_sys::core::PWSTR = std::ptr::null_mut();
        let hr = SHGetKnownFolderPath(&FOLDERID_Downloads, 0, std::ptr::null_mut(), &mut p);
        let out = if hr >= 0 && !p.is_null() {
            let mut n = 0usize;
            while *p.add(n) != 0 {
                n += 1;
            }
            let wide = std::slice::from_raw_parts(p, n);
            Some(PathBuf::from(std::ffi::OsString::from_wide(wide)))
        } else {
            None
        };
        if !p.is_null() {
            CoTaskMemFree(p as *const core::ffi::c_void);
        }
        out.filter(|d| !d.as_os_str().is_empty())
    }
}

fn format_bytes(n: u64) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = KB * 1024.0;
    const GB: f64 = MB * 1024.0;
    const TB: f64 = GB * 1024.0;
    let x = n as f64;
    if x >= TB {
        format!("{:.2} TB", x / TB)
    } else if x >= GB {
        format!("{:.2} GB", x / GB)
    } else if x >= MB {
        format!("{:.1} MB", x / MB)
    } else if x >= KB {
        format!("{:.0} KB", x / KB)
    } else {
        format!("{n} B")
    }
}

fn format_rate(bps: f64) -> String {
    format!("{}/s", format_bytes(bps.max(0.0) as u64))
}

fn format_duration(secs: u64, zh: bool) -> String {
    let d = secs / 86_400;
    let h = (secs % 86_400) / 3600;
    let m = (secs % 3600) / 60;
    match (zh, d > 0) {
        (true, true) => format!("{d} 天 {h} 小时"),
        (true, false) => format!("{h} 小时 {m} 分"),
        (false, true) => format!("{d}d {h}h"),
        (false, false) => format!("{h}h {m}m"),
    }
}

fn format_mtime(secs: u64) -> String {
    use chrono::TimeZone;
    match chrono::Local.timestamp_opt(secs as i64, 0) {
        chrono::LocalResult::Single(t) => t.format("%Y-%m-%d %H:%M").to_string(),
        _ => String::new(),
    }
}

/// Parse `[user@]host[:port]` for quick connect.
fn parse_quick(s: &str) -> Option<(Option<String>, String, Option<u16>)> {
    let s = s.trim().trim_start_matches("ssh ").trim();
    if s.is_empty() || s.contains(' ') {
        return None;
    }
    let (user, rest) = match s.rsplit_once('@') {
        Some((u, r)) if !u.is_empty() => (Some(u.to_string()), r),
        _ => (None, s),
    };
    // [v6]:port, host:port, or bare host / IPv6
    let (host, port) = if let Some(r) = rest.strip_prefix('[') {
        let (h, tail) = r.split_once(']')?;
        let port = if tail.is_empty() {
            None
        } else {
            let p = tail
                .strip_prefix(':')?
                .parse::<u16>()
                .ok()
                .filter(|&p| p > 0)?;
            Some(p)
        };
        (h.to_string(), port)
    } else if rest.matches(':').count() == 1 {
        let (h, p) = rest.split_once(':')?;
        let port = p.parse::<u16>().ok().filter(|&p| p > 0)?;
        (h.to_string(), Some(port))
    } else {
        (rest.to_string(), None)
    };
    if host.is_empty() {
        return None;
    }
    Some((user, host, port))
}

fn parse_forward(local: &str, remote: &str) -> Option<(std::net::SocketAddr, String, u16)> {
    let local = local.trim();
    // Accept a bare port for the local side: "8080" → 127.0.0.1:8080
    let listen: std::net::SocketAddr = match local.parse::<u16>() {
        Ok(p) => std::net::SocketAddr::from(([127, 0, 0, 1], p)),
        Err(_) => local.parse().ok()?,
    };
    let (host, port) = remote.trim().rsplit_once(':')?;
    let port: u16 = port.parse().ok()?;
    let host = host.trim_start_matches('[').trim_end_matches(']');
    if host.is_empty() {
        return None;
    }
    Some((listen, host.to_string(), port))
}

/// Expand local files *and folders* into an upload plan under `base`:
/// remote folders to create (parent-first) and files to send. Folder
/// symlinks are skipped to avoid loops; file symlinks are followed.
/// What an upload will do: remote folders to create, files to send, and
/// local entries that can't be sent (shown to the user, never dropped silently).
#[derive(Default)]
struct UploadPlan {
    dirs: Vec<String>,
    items: Vec<session::UploadItem>,
    /// (local path, reason)
    skipped: Vec<(String, String)>,
}

fn plan_upload(paths: &[PathBuf], base: &str) -> UploadPlan {
    fn skip(plan: &mut UploadPlan, p: &std::path::Path, why: &str) {
        plan.skipped
            .push((p.display().to_string(), why.to_string()));
    }
    fn add(
        plan: &mut UploadPlan,
        seen_files: &mut std::collections::HashSet<String>,
        path: &std::path::Path,
        remote: &str,
    ) {
        let Some(name) = path.file_name() else {
            return;
        };
        // A name that isn't valid UTF-8 can't be sent as-is; a lossy copy
        // could collide with another file, so leave it out and say so.
        let Some(name) = name.to_str() else {
            skip(plan, path, "file name is not valid UTF-8");
            return;
        };
        let target = join_remote(remote, name);
        let lmd = match std::fs::symlink_metadata(path) {
            Ok(m) => m,
            Err(e) => return skip(plan, path, &e.to_string()),
        };
        if lmd.is_dir() {
            let rd = match std::fs::read_dir(path) {
                Ok(rd) => rd,
                Err(e) => return skip(plan, path, &e.to_string()),
            };
            plan.dirs.push(target.clone());
            let mut entries = Vec::new();
            for e in rd {
                match e {
                    Ok(e) => entries.push(e),
                    Err(err) => skip(plan, path, &err.to_string()),
                }
            }
            entries.sort_by_key(|e| e.file_name());
            for e in entries {
                add(plan, seen_files, &e.path(), &target);
            }
            return;
        }
        // Files, and links to files (sent as the file they point to).
        match std::fs::metadata(path) {
            Ok(md) if md.is_file() => {
                let key = session::remote_home_relative(&target);
                if !seen_files.insert(key) {
                    skip(plan, path, "duplicate target name");
                    return;
                }
                match std::fs::File::open(path) {
                    Ok(_) => plan.items.push(session::UploadItem {
                        local: path.to_path_buf(),
                        remote: target,
                        size: md.len(),
                    }),
                    Err(e) => skip(plan, path, &e.to_string()),
                }
            }
            Ok(md) if md.is_dir() => skip(plan, path, "link to a folder"),
            Ok(_) => skip(plan, path, "not a regular file"),
            Err(e) => skip(plan, path, &e.to_string()),
        }
    }
    let mut plan = UploadPlan::default();
    let mut seen_files = std::collections::HashSet::new();
    for p in paths {
        add(&mut plan, &mut seen_files, p, base);
    }
    if !plan.items.is_empty() || !plan.dirs.is_empty() {
        let base_dir = session::remote_home_relative(base);
        let base_dir = if base_dir == "/" {
            "/"
        } else {
            base_dir.trim_end_matches('/')
        };
        if !base_dir.is_empty() && base_dir != "." && base_dir != "/" {
            plan.dirs.insert(0, base_dir.to_string());
        }
    }
    plan
}

/// "3 items skipped: a, b, c …" for the user.
fn skipped_note(skipped: &[(String, String)], zh: bool) -> String {
    let names: Vec<String> = skipped
        .iter()
        .take(3)
        .map(|(p, why)| {
            let short = std::path::Path::new(p)
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| p.clone());
            let why = match (zh, why.as_str()) {
                (true, "file name is not valid UTF-8") => "文件名不是有效的 UTF-8",
                (true, "not a regular file") => "不是普通文件",
                (true, "link to a folder") => "指向文件夹的链接",
                (true, "duplicate target name") => "同名文件冲突",
                (true, "unsafe file name") => "不安全的文件名",
                (_, w) => w,
            };
            format!("{short} ({why})")
        })
        .collect();
    let more = if skipped.len() > 3 { " …" } else { "" };
    if zh {
        format!("跳过 {} 项：{}{more}", skipped.len(), names.join("，"))
    } else {
        format!("skipped {}: {}{more}", skipped.len(), names.join(", "))
    }
}

/// (total bytes, file count) of a local file or folder tree.
fn local_summary(p: &std::path::Path) -> (u64, usize) {
    let plan = plan_upload(&[p.to_path_buf()], "");
    (plan.items.iter().map(|i| i.size).sum(), plan.items.len())
}

/// A single remote path component (any name the server allows, but never a
/// path or a reference to the folder itself / its parent). Names that are
/// not valid locally (e.g. `a:b` on Windows) are fixed by `local_safe_name`
/// when downloaded.
fn safe_remote_name(name: &str) -> bool {
    !name.is_empty() && name != "." && name != ".." && !name.contains(['/', '\0'])
}

fn join_remote(cwd: &str, name: &str) -> String {
    if cwd.is_empty() || cwd == "." || cwd == "~" {
        name.to_string()
    } else if cwd == "/" {
        format!("/{name}")
    } else {
        format!("{}/{}", cwd.trim_end_matches('/'), name)
    }
}

fn parent_dir(cwd: &str) -> String {
    let p = cwd.trim_end_matches('/');
    match p.rfind('/') {
        Some(0) | None => "/".into(),
        Some(i) => p[..i].to_string(),
    }
}

/// Run a native file dialog / opener without blocking the UI thread.
mod native {
    use std::path::Path;
    use std::process::Command;

    fn command(program: &str) -> Command {
        #[allow(unused_mut)]
        let mut c = Command::new(program);
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            c.creation_flags(CREATE_NO_WINDOW);
        }
        c
    }

    /// Open a folder (or reveal a file) in the system file manager.
    pub fn reveal(path: &Path) {
        #[cfg(windows)]
        {
            if path.is_file() {
                // explorer parses its own command line: the path must be quoted
                // inside the switch (`/select,"C:\a b\x"`), which `.arg()`
                // would wrap in a second pair of quotes.
                use std::os::windows::process::CommandExt;
                let _ = command("explorer")
                    .raw_arg(format!("/select,\"{}\"", path.display()))
                    .spawn();
            } else {
                let _ = command("explorer").arg(path).spawn();
            }
        }
        #[cfg(target_os = "macos")]
        {
            let _ = command("open").arg("-R").arg(path).spawn();
        }
        #[cfg(all(unix, not(target_os = "macos")))]
        {
            let dir = if path.is_file() {
                path.parent().unwrap_or(path)
            } else {
                path
            };
            let _ = command("xdg-open").arg(dir).spawn();
        }
    }

    /// Open an external `http://` or `https://` URL in the user's browser.
    pub fn open_url(url: &str) {
        if !(url.starts_with("http://") || url.starts_with("https://"))
            || url.chars().any(|c| c.is_control() || c.is_whitespace())
        {
            return;
        }
        #[cfg(windows)]
        {
            let _ = command("rundll32")
                .args(["url.dll,FileProtocolHandler", url])
                .spawn();
        }
        #[cfg(target_os = "macos")]
        {
            let _ = command("open").arg(url).spawn();
        }
        #[cfg(all(unix, not(target_os = "macos")))]
        {
            let _ = command("xdg-open").arg(url).spawn();
        }
    }
}

/// 256 px window / taskbar icon (source: assets/tern.svg).
const LOGO_PNG: &[u8] = include_bytes!("../../assets/tern-256.png");

pub fn run_gui(
    prefill_host: Option<String>,
    prefill_user: Option<String>,
    prefill_port: u16,
) -> Result<(), eframe::Error> {
    // Load once: a damaged file is moved aside on load, so a second load
    // would lose the warning that tells the user where it went.
    let config = crate::profiles::load();
    let custom_chrome = !config.system_titlebar && chrome::custom_chrome_available();
    let mut viewport = egui::ViewportBuilder::default()
        .with_inner_size([1320.0, 820.0])
        .with_min_inner_size([900.0, 560.0])
        .with_title("Tern")
        .with_app_id("tern")
        .with_drag_and_drop(true)
        .with_decorations(!custom_chrome);
    if let Ok(icon) = eframe::icon_data::from_png_bytes(LOGO_PNG) {
        viewport = viewport.with_icon(icon);
    }
    let options = eframe::NativeOptions {
        viewport,
        renderer: eframe::Renderer::Glow,
        hardware_acceleration: eframe::HardwareAcceleration::Preferred,
        vsync: true,
        depth_buffer: 0,
        stencil_buffer: 0,
        multisampling: 0,
        // Window size / position and panel layout come back next launch.
        persist_window: true,
        ..Default::default()
    };
    eframe::run_native(
        "Tern",
        options,
        Box::new(move |cc| {
            Ok(Box::new(GuiApp::new(
                cc,
                config,
                prefill_host,
                prefill_user,
                prefill_port,
            )))
        }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remote_file_actions_reject_parent_and_path_separators() {
        assert!(safe_remote_name("report.txt"));
        for name in ["", ".", "..", "../secret", "a/b", "a\0b"] {
            assert!(!safe_remote_name(name), "{name:?}");
        }
        // Legal on a Unix server; made harmless locally when downloaded.
        for name in ["..\\secret", "a\\b", "C:secret"] {
            assert!(safe_remote_name(name), "{name:?}");
            let local = session::local_safe_name(name);
            assert!(!local.contains('/'));
            if cfg!(windows) {
                assert!(!local.contains(['\\', ':']), "{local:?}");
            }
        }
    }

    #[test]
    fn plan_upload_skips_empty_base_dir_and_duplicate_targets() {
        let tmp = std::env::temp_dir().join(format!("tern-plan-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(tmp.join("d1")).unwrap();
        std::fs::create_dir_all(tmp.join("d2")).unwrap();
        let f1 = tmp.join("d1/conf.yml");
        let f2 = tmp.join("d2/conf.yml");
        std::fs::write(&f1, b"first").unwrap();
        std::fs::write(&f2, b"second").unwrap();

        let missing = tmp.join("does_not_exist.txt");
        let empty = plan_upload(&[missing], "new_empty_dir");
        assert!(empty.dirs.is_empty());
        assert!(empty.items.is_empty());
        assert_eq!(empty.skipped.len(), 1);

        let dup = plan_upload(&[f1, f2], "~/deploy");
        assert_eq!(dup.dirs, vec!["deploy".to_string()]);
        assert_eq!(dup.items.len(), 1);
        assert_eq!(dup.items[0].remote, "~/deploy/conf.yml");
        assert_eq!(dup.skipped.len(), 1);
        assert_eq!(dup.skipped[0].1, "duplicate target name");

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn reserve_local_path_and_dir_avoid_collisions() {
        let tmp = std::env::temp_dir().join(format!("tern-resv-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(tmp.join("collide")).unwrap();

        let p1 = session::reserve_local_path(&tmp, "collide").unwrap();
        assert_eq!(p1.file_name().unwrap().to_string_lossy(), "collide (1)");

        let d1 = session::reserve_local_dir(&tmp, "collide").unwrap();
        assert_eq!(d1.file_name().unwrap().to_string_lossy(), "collide (2)");

        if cfg!(windows) {
            assert_eq!(session::local_safe_name("CON"), "_CON");
        }
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn quick_connect_parsing() {
        assert_eq!(
            parse_quick("root@1.2.3.4:2222"),
            Some((Some("root".into()), "1.2.3.4".into(), Some(2222)))
        );
        assert_eq!(
            parse_quick("vps.example.com"),
            Some((None, "vps.example.com".into(), None))
        );
        assert_eq!(
            parse_quick("ssh u@h"),
            Some((Some("u".into()), "h".into(), None))
        );
        assert_eq!(
            parse_quick("[::1]:22"),
            Some((None, "::1".into(), Some(22)))
        );
        assert_eq!(parse_quick("[::1]"), Some((None, "::1".into(), None)));
        assert_eq!(parse_quick("[::1]:notaport"), None);
        assert_eq!(parse_quick("[::1]extra"), None);
        assert_eq!(parse_quick("[::1]:0"), None);
        assert_eq!(parse_quick("host:0"), None);
        assert_eq!(parse_quick("a@b:notaport"), None);
        assert_eq!(parse_quick(""), None);
    }

    #[test]
    fn forward_parsing() {
        let (l, h, p) = parse_forward("8080", "127.0.0.1:80").unwrap();
        assert_eq!(l.to_string(), "127.0.0.1:8080");
        assert_eq!((h.as_str(), p), ("127.0.0.1", 80));
        assert!(parse_forward("127.0.0.1:1", "nohost").is_none());
    }

    #[test]
    fn parent_paths() {
        assert_eq!(parent_dir("/root/a"), "/root");
        assert_eq!(parent_dir("/root"), "/");
        assert_eq!(parent_dir("/"), "/");
    }
}
