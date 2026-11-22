#![cfg_attr(not(feature = "gui"), allow(dead_code))]

//! Saved connection profiles + UI preferences (theme, folders, font).
//!
//! Persist under:
//! - Windows: `%APPDATA%/Tern/profiles.json`
//! - Linux / macOS: `~/.config/tern/profiles.json`

use anyhow::{Context, Result};
use base64::{engine::general_purpose::STANDARD as B64, Engine};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

/// Soft obfuscation key (not encryption — keeps passwords out of plain JSON).
// Kept from the old name on purpose: changing it would make saved passwords unreadable.
const OBFUSCATE_KEY: &[u8] = b"ssh-tun-bypass-v1-profile-pw";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum ThemePref {
    #[default]
    Light,
    Dark,
}

/// Terminal colour scheme (independent of the app theme).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum TermThemePref {
    /// Always dark — easy on the eyes, the default.
    #[default]
    Dark,
    Light,
    /// Follow the app theme.
    System,
}

/// GUI language.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum LangPref {
    #[default]
    Zh,
    En,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum BindModePref {
    #[default]
    Auto,
    Addr,
    Iface,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Profile {
    pub id: String,
    #[serde(default)]
    pub favorite: bool,
    pub name: String,
    pub host: String,
    #[serde(default = "default_port")]
    pub port: u16,
    #[serde(default = "default_user")]
    pub user: String,
    #[serde(default)]
    pub identity: String,
    /// Obfuscated password (base64 of XOR). Only set when remember_password.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub password_obfuscated: Option<String>,
    #[serde(default)]
    pub remember_password: bool,
    #[serde(default)]
    pub bind_mode: BindModePref,
    #[serde(default)]
    pub bind_addr: String,
    #[serde(default)]
    pub bind_iface: String,
    /// Optional folder name for host-tree grouping (empty = root).
    #[serde(default)]
    pub folder: String,
    /// Unix seconds of last successful Connect (for sort / UI).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_connected: Option<u64>,
    /// Optional DNS server used before the bypassed TCP connect.
    #[serde(default)]
    pub dns_server: String,
    /// Obfuscated private-key passphrase.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key_passphrase_obfuscated: Option<String>,
    #[serde(default)]
    pub remember_key_passphrase: bool,
    /// Local listen address for `-L`, e.g. `127.0.0.1:8080`.
    #[serde(default)]
    pub forward_local: String,
    /// Remote target for `-L`, e.g. `127.0.0.1:80`.
    #[serde(default)]
    pub forward_remote: String,
}

fn default_port() -> u16 {
    22
}
fn default_user() -> String {
    "root".into()
}

impl Profile {
    pub fn new_blank(name: impl Into<String>) -> Self {
        Self {
            id: new_id(),
            favorite: false,
            name: name.into(),
            host: String::new(),
            port: 22,
            user: "root".into(),
            identity: String::new(),
            password_obfuscated: None,
            remember_password: false,
            bind_mode: BindModePref::Auto,
            bind_addr: String::new(),
            bind_iface: String::new(),
            folder: String::new(),
            last_connected: None,
            dns_server: String::new(),
            key_passphrase_obfuscated: None,
            remember_key_passphrase: false,
            forward_local: String::new(),
            forward_remote: String::new(),
        }
    }

    pub fn set_password(&mut self, plain: &str) -> Result<(), ()> {
        let remember = self.remember_password && !plain.is_empty();
        self.password_obfuscated = store_secret(&self.id, "password", remember.then_some(plain))?;
        Ok(())
    }

    pub fn get_password(&self) -> Option<String> {
        load_secret(&self.id, "password", self.password_obfuscated.as_deref()).unwrap_or(None)
    }

    /// Like [`Self::get_password`], but tells "nothing saved" apart from
    /// "saved in the credential store, which can't be reached right now"
    /// (`Err`): in that case the saved secret must be left alone.
    pub fn read_password(&self) -> Result<Option<String>, ()> {
        load_secret(&self.id, "password", self.password_obfuscated.as_deref())
    }

    pub fn read_key_passphrase(&self) -> Result<Option<String>, ()> {
        load_secret(
            &self.id,
            "passphrase",
            self.key_passphrase_obfuscated.as_deref(),
        )
    }

    pub fn set_key_passphrase(&mut self, plain: &str) -> Result<(), ()> {
        let remember = self.remember_key_passphrase && !plain.is_empty();
        self.key_passphrase_obfuscated =
            store_secret(&self.id, "passphrase", remember.then_some(plain))?;
        Ok(())
    }

    pub fn get_key_passphrase(&self) -> Option<String> {
        self.read_key_passphrase().unwrap_or(None)
    }

    /// Remove this host's secrets from the OS credential store.
    pub fn forget_secrets(&mut self) {
        let _ = store_secret(&self.id, "password", None);
        let _ = store_secret(&self.id, "passphrase", None);
        self.password_obfuscated = None;
        self.key_passphrase_obfuscated = None;
    }

    /// Move secrets saved by older versions (obfuscated in the JSON) into the
    /// OS credential store. Returns true when something changed.
    pub fn migrate_secrets(&mut self) -> bool {
        let mut changed = false;
        for (kind, field) in [
            ("password", &mut self.password_obfuscated),
            ("passphrase", &mut self.key_passphrase_obfuscated),
        ] {
            if let Some(old) = field.as_deref().filter(|v| *v != KEYRING_MARK) {
                if let Some(plain) = deobfuscate(old) {
                    let next = store_secret(&self.id, kind, Some(&plain)).ok().flatten();
                    if next.as_deref() == Some(KEYRING_MARK) {
                        *field = next;
                        changed = true;
                    }
                }
            }
        }
        changed
    }

    pub fn display_label(&self) -> String {
        if self.name.trim().is_empty() {
            if self.host.trim().is_empty() {
                "(unnamed)".into()
            } else {
                format!("{}@{}", self.user, self.host)
            }
        } else {
            self.name.clone()
        }
    }

    #[allow(dead_code)]
    pub fn session_title(&self) -> String {
        if !self.host.trim().is_empty() {
            format!("{}@{}:{}", self.user, self.host, self.port)
        } else {
            self.display_label()
        }
    }
}

pub fn default_scrollback() -> usize {
    10_000
}

fn default_font_size() -> f32 {
    14.0
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Snippet {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub folder: String,
    pub command: String,
    #[serde(default = "default_true")]
    pub send_enter: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppConfig {
    #[serde(default)]
    pub theme: ThemePref,
    #[serde(default)]
    pub profiles: Vec<Profile>,
    /// Empty or named folders that should appear in the host tree even with no hosts.
    #[serde(default)]
    pub folders: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_profile_id: Option<String>,
    /// Terminal monospace font size (points).
    #[serde(default = "default_font_size")]
    pub term_font_size: f32,
    /// Plain Ctrl+V goes to full-screen apps instead of pasting there.
    #[serde(default)]
    pub ctrl_v_to_apps: bool,
    /// Lines of scrollback kept per terminal (memory grows with it).
    #[serde(default = "default_scrollback")]
    pub scrollback_lines: usize,
    /// GUI language (default Chinese).
    #[serde(default)]
    pub lang: LangPref,
    /// Show the right-hand remote monitor panel.
    #[serde(default = "default_true")]
    pub show_monitor: bool,
    /// Show the left-hand host list.
    #[serde(default = "default_true")]
    pub show_sidebar: bool,
    /// Local folder for SFTP downloads (empty = ~/Downloads).
    #[serde(default)]
    pub download_dir: String,
    /// Terminal colour scheme.
    #[serde(default)]
    pub term_theme: TermThemePref,
    /// Show the command input bar under the terminal.
    #[serde(default = "default_true")]
    pub show_cmdbar: bool,
    /// Recently sent commands from the input bar (newest last).
    /// Kept in history.json; still read from older profiles.json files.
    #[serde(default, skip_serializing)]
    pub cmd_history: Vec<String>,
    /// Set when profiles.json could not be read (shown once by the UI):
    /// `(moved aside, backup file name or the error)`.
    #[serde(skip)]
    pub load_warning: Option<(bool, String)>,
    /// Use the OS title bar instead of Tern's own (Windows).
    #[serde(default)]
    pub system_titlebar: bool,
    /// Saved command snippets for quick execution in single or group terminals.
    #[serde(default)]
    pub snippets: Vec<Snippet>,
    /// Automatically reconnect after an unexpected drop (excludes auth failure / manual disconnect).
    #[serde(default = "default_true")]
    pub auto_reconnect: bool,
}

fn default_true() -> bool {
    true
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            theme: ThemePref::default(),
            profiles: Vec::new(),
            folders: Vec::new(),
            last_profile_id: None,
            term_font_size: default_font_size(),
            scrollback_lines: default_scrollback(),
            ctrl_v_to_apps: false,
            lang: LangPref::default(),
            show_monitor: true,
            show_sidebar: true,
            download_dir: String::new(),
            term_theme: TermThemePref::default(),
            show_cmdbar: true,
            cmd_history: Vec::new(),
            load_warning: None,
            system_titlebar: false,
            snippets: Vec::new(),
            auto_reconnect: true,
        }
    }
}

/// Where profiles / known hosts live. Renamed from `ssh-tun-bypass` to `Tern`
/// in 0.8.0 — an existing old folder is moved over on first use so saved
/// hosts and passwords carry across.
pub fn config_dir() -> PathBuf {
    static DIR: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    DIR.get_or_init(|| {
        let (new, old) = config_dirs();
        if !new.exists() && old.exists() && std::fs::rename(&old, &new).is_err() && !new.exists() {
            // Could not move it (in use / other volume) and nobody else did
            // either: keep using the old folder.
            return old;
        }
        new
    })
    .clone()
}

fn config_dirs() -> (PathBuf, PathBuf) {
    #[cfg(windows)]
    {
        let base = std::env::var("APPDATA")
            .map(PathBuf::from)
            .or_else(|_| {
                std::env::var("USERPROFILE")
                    .map(|h| PathBuf::from(h).join("AppData").join("Roaming"))
            })
            .unwrap_or_else(|_| PathBuf::from("."));
        (base.join("Tern"), base.join("ssh-tun-bypass"))
    }
    #[cfg(not(windows))]
    {
        let base = std::env::var("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|_| std::env::var("HOME").map(|h| PathBuf::from(h).join(".config")))
            .unwrap_or_else(|_| PathBuf::from("."));
        (base.join("tern"), base.join("ssh-tun-bypass"))
    }
}

pub fn config_path() -> PathBuf {
    config_dir().join("profiles.json")
}

/// Load preferences and hosts. A file that exists but cannot be parsed is
/// moved aside (never silently replaced by an empty config); the returned
/// config then carries `load_warning` so the UI can tell the user where the
/// old file went.
pub fn load() -> AppConfig {
    let path = config_path();
    let unreadable = |why: String| -> AppConfig {
        let backup = path.with_file_name(format!("profiles.unreadable-{}.json", now_unix()));
        let moved = fs::rename(&path, &backup).is_ok();
        eprintln!(
            "[config] {} is unreadable ({why}); moved aside: {moved}",
            path.display()
        );
        if !moved {
            // Can't keep a copy: never write over the original this run.
            SAVE_BLOCKED.store(true, std::sync::atomic::Ordering::SeqCst);
        }
        AppConfig {
            load_warning: Some(if moved {
                (
                    true,
                    backup
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_default(),
                )
            } else {
                (false, why)
            }),
            ..AppConfig::default()
        }
    };
    let mut cfg = match fs::read(&path) {
        Ok(bytes) => match std::str::from_utf8(&bytes)
            .map_err(|e| e.to_string())
            .and_then(|t| serde_json::from_str::<AppConfig>(t).map_err(|e| e.to_string()))
        {
            Ok(cfg) => cfg,
            Err(e) => unreadable(e),
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => AppConfig::default(),
        // Exists but can't be read (permissions, I/O error): keep it intact.
        Err(e) => {
            SAVE_BLOCKED.store(true, std::sync::atomic::Ordering::SeqCst);
            eprintln!("[config] cannot read {}: {e}", path.display());
            AppConfig {
                load_warning: Some((false, e.to_string())),
                ..AppConfig::default()
            }
        }
    };
    // Snap to the choices offered in Settings.
    cfg.scrollback_lines = [1_000usize, 5_000, 10_000, 50_000]
        .into_iter()
        .min_by_key(|n| n.abs_diff(cfg.scrollback_lines))
        .unwrap_or(10_000);
    if cfg.term_font_size < 9.0 || cfg.term_font_size > 36.0 {
        cfg.term_font_size = default_font_size();
    }
    // Command history lives in its own file (older versions kept it inline).
    if let Ok(text) = fs::read_to_string(history_path()) {
        if let Ok(h) = serde_json::from_str::<Vec<String>>(&text) {
            cfg.cmd_history = h;
        }
    }
    cfg
}

/// Set when profiles.json exists but could neither be read nor moved aside:
/// saving would destroy it, so nothing is written this run.
static SAVE_BLOCKED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

fn history_path() -> PathBuf {
    config_dir().join("history.json")
}

/// Write a file atomically: a temp file next to it (owner-only on Unix) is
/// flushed to disk and renamed over the target, so a crash can never leave
/// a half-written file behind.
pub fn write_atomic(path: &std::path::Path, data: &[u8]) -> Result<()> {
    use std::io::Write;
    let dir = path.parent().unwrap_or(std::path::Path::new("."));
    fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(dir, fs::Permissions::from_mode(0o700));
    }
    let tmp = path.with_extension(format!("tmp-{}", std::process::id()));
    let mut opts = fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let result = (|| -> Result<()> {
        let mut f = opts
            .open(&tmp)
            .with_context(|| format!("write {}", tmp.display()))?;
        f.write_all(data)?;
        f.sync_all()?;
        drop(f);
        fs::rename(&tmp, path).with_context(|| format!("replace {}", path.display()))?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

static SAVE_LOCK: std::sync::Mutex<u64> = std::sync::Mutex::new(0);
static SAVE_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Save now (blocking). Later snapshots always win over earlier ones, even
/// when a background save is still in flight.
pub fn save(cfg: &AppConfig) -> Result<()> {
    let seq = SAVE_SEQ.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
    write_snapshot(seq, cfg)
}

fn write_snapshot(seq: u64, cfg: &AppConfig) -> Result<()> {
    let mut last = SAVE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    if seq < *last {
        return Ok(()); // a newer snapshot is already on disk
    }
    if !SAVE_BLOCKED.load(std::sync::atomic::Ordering::SeqCst) {
        let text = serde_json::to_string_pretty(cfg).context("serialize profiles")?;
        write_atomic(&config_path(), text.as_bytes())?;
    }
    // History only when it changed (most saves are about something else).
    static LAST_HISTORY: std::sync::Mutex<Option<u64>> = std::sync::Mutex::new(None);
    let hash = {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        cfg.cmd_history.hash(&mut h);
        h.finish()
    };
    let mut last_hist = LAST_HISTORY.lock().unwrap_or_else(|e| e.into_inner());
    if *last_hist != Some(hash) {
        let hist = serde_json::to_string(&cfg.cmd_history).context("serialize history")?;
        write_atomic(&history_path(), hist.as_bytes())?;
        *last_hist = Some(hash);
    }
    *last = seq;
    Ok(())
}

/// Save in the background, coalescing bursts (typing in the command bar,
/// Ctrl+wheel font steps…) into one write ~300 ms after the last change.
pub fn save_later(cfg: &AppConfig) {
    use std::sync::mpsc::{channel, RecvTimeoutError, Sender};
    use std::sync::OnceLock;
    static TX: OnceLock<std::sync::Mutex<Sender<(u64, AppConfig)>>> = OnceLock::new();
    let tx = TX.get_or_init(|| {
        let (tx, rx) = channel::<(u64, AppConfig)>();
        std::thread::spawn(move || {
            while let Ok(mut latest) = rx.recv() {
                loop {
                    match rx.recv_timeout(std::time::Duration::from_millis(300)) {
                        Ok(next) => latest = next,
                        Err(RecvTimeoutError::Timeout) => break,
                        Err(RecvTimeoutError::Disconnected) => break,
                    }
                }
                if let Err(e) = write_snapshot(latest.0, &latest.1) {
                    eprintln!("[config] save failed: {e:#}");
                }
            }
        });
        std::sync::Mutex::new(tx)
    });
    let seq = SAVE_SEQ.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
    let _ = tx
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .send((seq, cfg.clone()));
}

pub fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

pub fn new_id() -> String {
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let pid = std::process::id();
    let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    if seq == 0 {
        format!("{nanos:x}-{pid:x}")
    } else {
        format!("{nanos:x}-{pid:x}-{seq:x}")
    }
}

/// Default location of the user's OpenSSH client config (`~/.ssh/config`).
pub fn default_ssh_config_path() -> Option<PathBuf> {
    std::env::var("USERPROFILE")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .or_else(|| std::env::var("HOME").ok().filter(|s| !s.trim().is_empty()))
        .map(|h| PathBuf::from(h).join(".ssh").join("config"))
}

#[derive(Default, Clone)]
struct SshHostOpts {
    hostname: Option<String>,
    user: Option<String>,
    port: Option<u16>,
    identity: Option<String>,
    forward: Option<(String, String)>,
    bind_addr: Option<String>,
    bind_iface: Option<String>,
}

impl SshHostOpts {
    fn merge_missing_from(&mut self, other: &Self) {
        if self.hostname.is_none() {
            self.hostname = other.hostname.clone();
        }
        if self.user.is_none() {
            self.user = other.user.clone();
        }
        if self.port.is_none() {
            self.port = other.port;
        }
        if self.identity.is_none() {
            self.identity = other.identity.clone();
        }
        if self.forward.is_none() {
            self.forward = other.forward.clone();
        }
        if self.bind_addr.is_none() {
            self.bind_addr = other.bind_addr.clone();
        }
        if self.bind_iface.is_none() {
            self.bind_iface = other.bind_iface.clone();
        }
    }
}

fn split_ssh_config_line(line: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut cur = String::new();
    let mut in_quotes = false;
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '"' => in_quotes = !in_quotes,
            '#' if !in_quotes => break,
            c if !in_quotes && c.is_whitespace() => {
                if !cur.is_empty() {
                    tokens.push(std::mem::take(&mut cur));
                }
            }
            '=' if !in_quotes && tokens.is_empty() && !cur.is_empty() => {
                tokens.push(std::mem::take(&mut cur));
                while chars.peek().is_some_and(|ch| ch.is_whitespace()) {
                    chars.next();
                }
            }
            c => cur.push(c),
        }
    }
    if !cur.is_empty() {
        tokens.push(cur);
    }
    tokens
}

fn expand_tilde_path(raw: &str, home_dir: Option<&std::path::Path>) -> String {
    if let Some(home) = home_dir {
        if let Some(rest) = raw.strip_prefix("~/").or_else(|| raw.strip_prefix("~\\")) {
            return home.join(rest).display().to_string();
        }
        if raw == "~" {
            return home.display().to_string();
        }
    }
    raw.to_string()
}

fn ssh_glob_match(pat: &str, text: &str) -> bool {
    let p: Vec<char> = pat.chars().map(|c| c.to_ascii_lowercase()).collect();
    let t: Vec<char> = text.chars().map(|c| c.to_ascii_lowercase()).collect();
    let (mut pi, mut ti) = (0usize, 0usize);
    let (mut star_pi, mut star_ti) = (None::<usize>, 0usize);
    while ti < t.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == t[ti]) {
            pi += 1;
            ti += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star_pi = Some(pi);
            star_ti = ti;
            pi += 1;
        } else if let Some(sp) = star_pi {
            pi = sp + 1;
            star_ti += 1;
            ti = star_ti;
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == '*' {
        pi += 1;
    }
    pi == p.len()
}

fn ssh_host_patterns_match(pats: &[String], alias: &str) -> bool {
    let mut matched_positive = false;
    for pat in pats {
        if let Some(neg) = pat.strip_prefix('!') {
            if !neg.is_empty() && ssh_glob_match(neg, alias) {
                return false;
            }
        } else if !pat.is_empty() && ssh_glob_match(pat, alias) {
            matched_positive = true;
        }
    }
    matched_positive
}

fn expand_ssh_hostname(raw: &str, alias: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut chars = raw.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '%' {
            match chars.peek().copied() {
                Some('h') => {
                    chars.next();
                    out.push_str(alias);
                    continue;
                }
                Some('%') => {
                    chars.next();
                    out.push('%');
                    continue;
                }
                _ => {}
            }
        }
        out.push(c);
    }
    out
}

fn normalize_forward_listen(raw: &str) -> String {
    let s = raw.trim();
    if let Some(port) = s.strip_prefix("*:") {
        format!("0.0.0.0:{port}")
    } else if let Some(port) = s.strip_prefix(':') {
        format!("127.0.0.1:{port}")
    } else if !s.contains(':') && s.parse::<u16>().is_ok() {
        format!("127.0.0.1:{s}")
    } else {
        s.to_string()
    }
}

fn default_local_user() -> String {
    std::env::var("USER")
        .ok()
        .or_else(|| std::env::var("USERNAME").ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "root".into())
}

fn resolve_include_paths(
    pattern: &str,
    ssh_dir: Option<&std::path::Path>,
    home_dir: Option<&std::path::Path>,
) -> Vec<PathBuf> {
    let expanded = expand_tilde_path(pattern, home_dir);
    let p = PathBuf::from(&expanded);
    let full = if p.is_absolute() {
        p
    } else if let Some(base) = ssh_dir {
        base.join(p)
    } else {
        p
    };
    let Some(file_pat) = full.file_name().map(|f| f.to_string_lossy().to_string()) else {
        return Vec::new();
    };
    if !file_pat.contains(['*', '?']) {
        return if full.is_file() {
            vec![full]
        } else {
            Vec::new()
        };
    }
    let Some(parent) = full.parent() else {
        return Vec::new();
    };
    let Ok(rd) = fs::read_dir(parent) else {
        return Vec::new();
    };
    let mut matches: Vec<PathBuf> = rd
        .flatten()
        .map(|e| e.path())
        .filter(|cand| {
            cand.is_file()
                && cand
                    .file_name()
                    .map(|n| ssh_glob_match(&file_pat, &n.to_string_lossy()))
                    .unwrap_or(false)
        })
        .collect();
    matches.sort();
    matches
}

fn parse_ssh_config_blocks(
    text: &str,
    ssh_dir: Option<&std::path::Path>,
    home_dir: Option<&std::path::Path>,
    outer_patterns: Option<Vec<String>>,
    depth: usize,
    visited: &mut std::collections::HashSet<PathBuf>,
    blocks: &mut Vec<(Vec<String>, SshHostOpts)>,
) {
    let mut current_patterns: Vec<String> = outer_patterns.unwrap_or_else(|| vec!["*".into()]);
    let mut current_opts = SshHostOpts::default();
    let mut in_match = false;

    for raw_line in text.lines() {
        let tokens = split_ssh_config_line(raw_line);
        if tokens.is_empty() {
            continue;
        }
        let key = tokens[0].to_ascii_lowercase();
        let rest = &tokens[1..];
        if key == "host" {
            blocks.push((current_patterns, std::mem::take(&mut current_opts)));
            in_match = false;
            current_patterns = rest.to_vec();
            continue;
        }
        if key == "match" {
            blocks.push((
                std::mem::take(&mut current_patterns),
                std::mem::take(&mut current_opts),
            ));
            in_match = true;
            continue;
        }
        if in_match || rest.is_empty() {
            continue;
        }
        if key == "include" && depth < 8 {
            blocks.push((current_patterns.clone(), std::mem::take(&mut current_opts)));
            for pat in rest {
                for inc_path in resolve_include_paths(pat, ssh_dir, home_dir) {
                    let canon = fs::canonicalize(&inc_path).unwrap_or_else(|_| inc_path.clone());
                    if !visited.insert(canon.clone()) {
                        continue;
                    }
                    if let Ok(inc_text) = fs::read_to_string(&inc_path) {
                        parse_ssh_config_blocks(
                            &inc_text,
                            ssh_dir,
                            home_dir,
                            Some(current_patterns.clone()),
                            depth + 1,
                            visited,
                            blocks,
                        );
                    }
                    visited.remove(&canon);
                }
            }
            continue;
        }

        let val = rest.join(" ");
        match key.as_str() {
            "hostname" if current_opts.hostname.is_none() => current_opts.hostname = Some(val),
            "user" if current_opts.user.is_none() => current_opts.user = Some(val),
            "port" if current_opts.port.is_none() => {
                if let Some(p) = val.parse::<u16>().ok().filter(|&p| p > 0) {
                    current_opts.port = Some(p);
                }
            }
            "identityfile" if current_opts.identity.is_none() => {
                current_opts.identity = Some(expand_tilde_path(&val, home_dir));
            }
            "localforward" if current_opts.forward.is_none() && rest.len() >= 2 => {
                let listen = normalize_forward_listen(&rest[0]);
                let remote = rest[1].trim().to_string();
                if !listen.is_empty() && !remote.is_empty() {
                    current_opts.forward = Some((listen, remote));
                }
            }
            "bindaddress" if current_opts.bind_addr.is_none() => {
                current_opts.bind_addr = Some(val);
            }
            "bindinterface" if current_opts.bind_iface.is_none() => {
                current_opts.bind_iface = Some(val);
            }
            _ => {}
        }
    }
    if !current_patterns.is_empty() {
        blocks.push((current_patterns, current_opts));
    }
}

fn profiles_from_ssh_blocks(blocks: &[(Vec<String>, SshHostOpts)]) -> Vec<Profile> {
    let fallback_user = default_local_user();
    let mut seen_aliases = std::collections::HashSet::new();
    let mut aliases = Vec::new();
    for (pats, _) in blocks {
        for pat in pats {
            if pat.is_empty() || pat.contains(['*', '?', '!']) {
                continue;
            }
            if seen_aliases.insert(pat.to_ascii_lowercase()) {
                aliases.push(pat.clone());
            }
        }
    }

    let mut out = Vec::new();
    for alias in aliases {
        let mut merged = SshHostOpts::default();
        for (pats, opts) in blocks {
            if ssh_host_patterns_match(pats, &alias) {
                merged.merge_missing_from(opts);
            }
        }
        let mut p = Profile::new_blank(&alias);
        p.folder = "SSH Config".into();
        p.host = merged
            .hostname
            .as_deref()
            .map(|h| expand_ssh_hostname(h, &alias))
            .unwrap_or_else(|| alias.clone());
        p.user = merged.user.unwrap_or_else(|| fallback_user.clone());
        p.port = merged.port.unwrap_or(22);
        p.identity = merged.identity.unwrap_or_default();
        if let Some((fl, fr)) = merged.forward {
            p.forward_local = fl;
            p.forward_remote = fr;
        }
        if let Some(iface) = merged.bind_iface.filter(|s| !s.is_empty()) {
            p.bind_mode = BindModePref::Iface;
            p.bind_iface = iface;
        } else if let Some(addr) = merged.bind_addr.filter(|s| !s.is_empty()) {
            p.bind_mode = BindModePref::Addr;
            p.bind_addr = addr;
        }
        out.push(p);
    }
    out
}

/// Load and parse `~/.ssh/config` from disk (including `Include` directives).
pub fn load_ssh_config(path: &std::path::Path) -> Result<Vec<Profile>> {
    let text = fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    let ssh_dir = path.parent();
    let home_dir = ssh_dir.and_then(|p| p.parent());
    let mut visited = std::collections::HashSet::new();
    if let Ok(canon) = fs::canonicalize(path) {
        visited.insert(canon);
    }
    let mut blocks = Vec::new();
    parse_ssh_config_blocks(&text, ssh_dir, home_dir, None, 0, &mut visited, &mut blocks);
    Ok(profiles_from_ssh_blocks(&blocks))
}

/// Parse an OpenSSH `config` string into Tern host profiles under folder `"SSH Config"`.
#[cfg(test)]
fn parse_ssh_config(text: &str, home_dir: Option<&std::path::Path>) -> Vec<Profile> {
    let ssh_dir = home_dir.map(|h| h.join(".ssh"));
    let mut visited = std::collections::HashSet::new();
    let mut blocks = Vec::new();
    parse_ssh_config_blocks(
        text,
        ssh_dir.as_deref(),
        home_dir,
        None,
        0,
        &mut visited,
        &mut blocks,
    );
    profiles_from_ssh_blocks(&blocks)
}

/// Append `imported` profiles into `existing`, skipping duplicates. Returns `(added, skipped)`.
pub fn merge_imported_profiles(
    existing: &mut Vec<Profile>,
    imported: Vec<Profile>,
) -> (usize, usize) {
    let mut added = 0;
    let mut skipped = 0;
    for p in imported {
        let dup = existing.iter().any(|e| {
            (e.host.eq_ignore_ascii_case(&p.host) && e.port == p.port && e.user == p.user)
                || (e.name.eq_ignore_ascii_case(&p.name) && e.host.eq_ignore_ascii_case(&p.host))
        });
        if dup {
            skipped += 1;
        } else {
            existing.push(p);
            added += 1;
        }
    }
    (added, skipped)
}

/// Placeholder stored in the JSON when the real secret lives in the OS
/// credential store.
const KEYRING_MARK: &str = "@keyring";

/// Save (Some) or delete (None) a secret. It goes to the OS credential
/// store — Windows Credential Manager, macOS Keychain, or the Secret Service
/// (GNOME Keyring / KWallet) on Linux — and the JSON only keeps a marker.
/// A failed write preserves the previous value and is reported to the UI.
/// Never persist new secrets using reversible obfuscation.
fn store_secret(id: &str, kind: &str, plain: Option<&str>) -> Result<Option<String>, ()> {
    let key = format!("{id}:{kind}");
    let value = plain.map(str::to_string);
    let k2 = key.clone();
    let stored = keyring_call(move || {
        let e = keyring::Entry::new("Tern", &k2).ok()?;
        match value {
            Some(p) => e.set_password(&p).ok().map(|_| true),
            None => match e.delete_credential() {
                Ok(()) | Err(keyring::Error::NoEntry) => Some(false),
                Err(_) => None,
            },
        }
    });
    if plain.is_some() && stored != Some(true) {
        return Err(());
    }
    {
        let mut cache = secret_cache();
        // A read still waiting on the store must not cache its older value.
        SECRET_GEN.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        cache.remove(&key);
        if let (Some(p), Some(true)) = (plain, stored) {
            cache.insert(key.clone(), Some(p.to_string()));
        }
    }
    match (plain, stored) {
        (Some(_), Some(true)) => {
            cancel_pending_delete(&key);
            Ok(Some(KEYRING_MARK.into()))
        }
        (Some(_), _) => Err(()),
        (None, Some(_)) => Ok(None),
        (None, None) => {
            // Couldn't delete it now: remember to, so it doesn't linger.
            delete_later(&key);
            Ok(None)
        }
    }
}

/// `Ok(None)`: nothing saved. `Err(())`: saved in the credential store but
/// the store can't be reached right now.
fn load_secret(id: &str, kind: &str, stored: Option<&str>) -> Result<Option<String>, ()> {
    match stored {
        None => Ok(None),
        Some(KEYRING_MARK) => {
            let key = format!("{id}:{kind}");
            if let Some(v) = secret_cache().get(&key) {
                return Ok(v.clone());
            }
            let gen = SECRET_GEN.load(std::sync::atomic::Ordering::SeqCst);
            let k2 = key.clone();
            match keyring_call(move || {
                let e = keyring::Entry::new("Tern", &k2).ok()?;
                match e.get_password() {
                    Ok(p) => Some(Some(p)),
                    Err(keyring::Error::NoEntry) => Some(None),
                    Err(_) => None,
                }
            }) {
                Some(v) => {
                    let mut cache = secret_cache();
                    if SECRET_GEN.load(std::sync::atomic::Ordering::SeqCst) == gen {
                        cache.insert(key, v.clone());
                    }
                    Ok(v)
                }
                None => Err(()),
            }
        }
        Some(other) => Ok(deobfuscate(other)),
    }
}

/// Bumped by every secret write (see `load_secret`).
static SECRET_GEN: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Secrets already read from (or written to) the credential store this run,
/// keyed `id:kind`, so the window never waits on the store twice for one
/// secret. Holds `None` for "the store has nothing".
fn secret_cache(
) -> std::sync::MutexGuard<'static, std::collections::HashMap<String, Option<String>>> {
    static CACHE: std::sync::OnceLock<
        std::sync::Mutex<std::collections::HashMap<String, Option<String>>>,
    > = std::sync::OnceLock::new();
    CACHE
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(|e| e.into_inner())
}

/// Read every remembered secret into the cache on the credential worker, so
/// opening a host or connecting doesn't block the window on the credential
/// store. Windows only: there the Credential Manager never asks anything,
/// while a locked Secret Service (Linux) or the Keychain (macOS) may show a
/// prompt, which should only happen when the user actually connects.
pub fn preload_secrets(profiles: &[Profile]) {
    if !cfg!(windows) {
        return;
    }
    let wanted: Vec<(String, &'static str, Option<String>)> = profiles
        .iter()
        .flat_map(|p| {
            [
                ("password", p.password_obfuscated.clone()),
                ("passphrase", p.key_passphrase_obfuscated.clone()),
            ]
            .into_iter()
            .filter(|(_, v)| v.as_deref() == Some(KEYRING_MARK))
            .map(|(k, v)| (p.id.clone(), k, v))
            .collect::<Vec<_>>()
        })
        .collect();
    if wanted.is_empty() {
        return;
    }
    for (id, kind, stored) in wanted {
        if load_secret(&id, kind, stored.as_deref()).is_err() {
            break;
        }
    }
}

/// Called only by the GUI credential worker. Keep native calls in order:
/// abandoning a timed-out write would let it finish after a newer save or
/// delete. The UI stays responsive while an OS unlock prompt is pending.
fn keyring_call<T>(f: impl FnOnce() -> Option<T>) -> Option<T> {
    if cfg!(any(windows, target_os = "linux", target_os = "macos")) {
        f()
    } else {
        None
    }
}

fn pending_deletes_path() -> PathBuf {
    config_dir().join("keyring-pending-deletes.json")
}

/// Guards the pending-deletes file (store, delete and retry may race).
static PENDING_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn delete_later(key: &str) {
    let _g = PENDING_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut keys: Vec<String> = fs::read_to_string(pending_deletes_path())
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default();
    if !keys.iter().any(|k| k == key) {
        keys.push(key.to_string());
        if let Ok(t) = serde_json::to_string(&keys) {
            let _ = write_atomic(&pending_deletes_path(), t.as_bytes());
        }
    }
}

/// Delete credentials that couldn't be deleted earlier (store unreachable at
/// the time). Run in the background at startup.
/// A secret was just saved again under `key`: it must no longer be deleted.
fn cancel_pending_delete(key: &str) {
    let _g = PENDING_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let path = pending_deletes_path();
    let Some(mut keys) = fs::read_to_string(&path)
        .ok()
        .and_then(|t| serde_json::from_str::<Vec<String>>(&t).ok())
    else {
        return;
    };
    let before = keys.len();
    keys.retain(|k| k != key);
    if keys.len() != before {
        if keys.is_empty() {
            let _ = fs::remove_file(&path);
        } else if let Ok(t) = serde_json::to_string(&keys) {
            let _ = write_atomic(&path, t.as_bytes());
        }
    }
}

/// Run at startup, before anything is saved (see `GuiApp::new`).
pub fn retry_pending_deletes() {
    let _g = PENDING_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let path = pending_deletes_path();
    let Some(keys) = fs::read_to_string(&path)
        .ok()
        .and_then(|t| serde_json::from_str::<Vec<String>>(&t).ok())
    else {
        return;
    };
    let mut left = Vec::new();
    for key in keys {
        let k = key.clone();
        let ok = keyring_call(move || {
            let e = keyring::Entry::new("Tern", &k).ok()?;
            match e.delete_credential() {
                Ok(()) | Err(keyring::Error::NoEntry) => Some(()),
                Err(_) => None,
            }
        });
        if ok.is_none() {
            left.push(key);
        }
    }
    if left.is_empty() {
        let _ = fs::remove_file(&path);
    } else if let Ok(t) = serde_json::to_string(&left) {
        let _ = write_atomic(&path, t.as_bytes());
    }
}

#[cfg(test)]
fn obfuscate(plain: &str) -> String {
    let bytes: Vec<u8> = plain
        .as_bytes()
        .iter()
        .enumerate()
        .map(|(i, b)| b ^ OBFUSCATE_KEY[i % OBFUSCATE_KEY.len()])
        .collect();
    B64.encode(bytes)
}

fn deobfuscate(encoded: &str) -> Option<String> {
    let bytes = B64.decode(encoded).ok()?;
    let plain: Vec<u8> = bytes
        .iter()
        .enumerate()
        .map(|(i, b)| b ^ OBFUSCATE_KEY[i % OBFUSCATE_KEY.len()])
        .collect();
    String::from_utf8(plain).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_password() {
        let s = "p@ssw0rd!";
        let enc = obfuscate(s);
        assert_ne!(enc, s);
        assert_eq!(deobfuscate(&enc).as_deref(), Some(s));
    }

    #[test]
    fn older_profile_defaults_to_not_favorite() {
        let mut value = serde_json::to_value(Profile::new_blank("Host")).unwrap();
        value.as_object_mut().unwrap().remove("favorite");
        let restored: Profile = serde_json::from_value(value).unwrap();
        assert!(!restored.favorite);
    }

    #[test]
    fn failed_credential_writes_preserve_existing_secrets_without_fallback() {
        use keyring::credential::{Credential, CredentialBuilderApi, CredentialPersistence};
        struct Unavailable;
        impl CredentialBuilderApi for Unavailable {
            fn build(&self, _: Option<&str>, _: &str, _: &str) -> keyring::Result<Box<Credential>> {
                let mock = keyring::mock::MockCredential::default();
                mock.set_error(keyring::Error::Invalid(
                    "test".into(),
                    "store unavailable".into(),
                ));
                Ok(Box::new(mock))
            }
            fn as_any(&self) -> &dyn std::any::Any {
                self
            }
            fn persistence(&self) -> CredentialPersistence {
                CredentialPersistence::EntryOnly
            }
        }
        // All native credential operations in this test use an in-memory
        // backend; no user credential or configuration file is touched.
        keyring::set_default_credential_builder(Box::new(Unavailable));
        let mut p = Profile::new_blank("test");
        p.remember_password = true;
        p.remember_key_passphrase = true;
        assert!(p.set_password("new secret").is_err());
        assert!(p.password_obfuscated.is_none());
        assert!(p.set_key_passphrase("new passphrase").is_err());
        assert!(p.key_passphrase_obfuscated.is_none());

        p.password_obfuscated = Some(KEYRING_MARK.into());
        let key = format!("{}:password", p.id);
        secret_cache().insert(key.clone(), Some("old cached secret".into()));
        assert!(p.set_password("replacement").is_err());
        assert_eq!(p.password_obfuscated.as_deref(), Some(KEYRING_MARK));
        assert_eq!(p.get_password().as_deref(), Some("old cached secret"));
        secret_cache().remove(&key);

        let legacy = obfuscate("legacy secret");
        p.password_obfuscated = Some(legacy.clone());
        assert!(!p.migrate_secrets());
        assert_eq!(p.password_obfuscated.as_deref(), Some(legacy.as_str()));
        let json = serde_json::to_string(&p).unwrap();
        assert!(!json.contains(&obfuscate("replacement")));
        assert!(!json.contains("replacement"));
    }

    #[test]
    fn parse_ssh_config_handles_aliases_wildcards_defaults_and_quotes() {
        let cfg = r#"
# Global comment
Host prod web-1 *.internal
    HostName 10.0.0.10 # inline comment
    User ubuntu
    Port=2222
    IdentityFile "~/.ssh/my key"

Match host *.internal
    User ignored_match_user

Host bastion
    HostName=bastion.example.com

Host *
    User ops
    Port 2200
    IdentityFile ~/.ssh/id_ed25519
"#;
        let home = std::path::Path::new("/home/tester");
        let parsed = parse_ssh_config(cfg, Some(home));
        assert_eq!(parsed.len(), 3);

        assert_eq!(parsed[0].name, "prod");
        assert_eq!(parsed[0].host, "10.0.0.10");
        assert_eq!(parsed[0].user, "ubuntu");
        assert_eq!(parsed[0].port, 2222);
        assert_eq!(parsed[0].folder, "SSH Config");
        assert_eq!(
            parsed[0].identity,
            home.join(".ssh/my key").display().to_string()
        );

        assert_eq!(parsed[1].name, "web-1");
        assert_eq!(parsed[1].host, "10.0.0.10");

        assert_eq!(parsed[2].name, "bastion");
        assert_eq!(parsed[2].host, "bastion.example.com");
        assert_eq!(parsed[2].user, "ops");
        assert_eq!(parsed[2].port, 2200);
        assert_eq!(
            parsed[2].identity,
            home.join(".ssh/id_ed25519").display().to_string()
        );
    }

    #[test]
    fn ssh_config_supports_includes_percent_h_forwards_and_bind_options() {
        let dir = std::env::temp_dir().join(format!("tern-sshcfg-{}", new_id()));
        let inc_dir = dir.join(".ssh").join("config.d");
        fs::create_dir_all(&inc_dir).unwrap();

        fs::write(
            inc_dir.join("10-corp.conf"),
            r#"
Host app-1
    HostName %h.corp.example.com
    LocalForward 8080 127.0.0.1:80
    BindAddress 192.168.1.50

Host edge-gw
    HostName 203.0.113.20
    BindInterface eth0
"#,
        )
        .unwrap();

        let main_cfg = dir.join(".ssh").join("config");
        fs::write(
            &main_cfg,
            r#"
Include config.d/*.conf

Host *.corp.example.com app-* !app-blocked
    User deploy
    Port 2222
"#,
        )
        .unwrap();

        let parsed = load_ssh_config(&main_cfg).unwrap();
        let _ = fs::remove_dir_all(&dir);

        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].name, "app-1");
        assert_eq!(parsed[0].host, "app-1.corp.example.com");
        assert_eq!(parsed[0].user, "deploy");
        assert_eq!(parsed[0].port, 2222);
        assert_eq!(parsed[0].forward_local, "127.0.0.1:8080");
        assert_eq!(parsed[0].forward_remote, "127.0.0.1:80");
        assert_eq!(parsed[0].bind_mode, BindModePref::Addr);
        assert_eq!(parsed[0].bind_addr, "192.168.1.50");

        assert_eq!(parsed[1].name, "edge-gw");
        assert_eq!(parsed[1].host, "203.0.113.20");
        assert_eq!(parsed[1].bind_mode, BindModePref::Iface);
        assert_eq!(parsed[1].bind_iface, "eth0");
    }

    #[test]
    fn merge_imported_profiles_deduplicates_existing_hosts() {
        let mut cur = Profile::new_blank("my-prod");
        cur.host = "1.2.3.4".into();
        cur.user = "ubuntu".into();
        cur.port = 22;
        let mut existing = vec![cur];

        let mut dup_endpoint = Profile::new_blank("other-name");
        dup_endpoint.host = "1.2.3.4".into();
        dup_endpoint.user = "ubuntu".into();
        dup_endpoint.port = 22;

        let mut dup_name = Profile::new_blank("MY-PROD");
        dup_name.host = "1.2.3.4".into();
        dup_name.user = "root".into();
        dup_name.port = 2222;

        let mut fresh = Profile::new_blank("db");
        fresh.host = "1.2.3.5".into();
        fresh.user = "postgres".into();
        fresh.port = 22;

        let (added, skipped) =
            merge_imported_profiles(&mut existing, vec![dup_endpoint, dup_name, fresh]);
        assert_eq!((added, skipped), (1, 2));
        assert_eq!(existing.len(), 2);
        assert_eq!(existing[1].name, "db");
    }

    #[test]
    fn config_defaults_include_auto_reconnect_and_empty_snippets() {
        let def = AppConfig::default();
        assert!(def.auto_reconnect);
        assert!(def.snippets.is_empty());

        let old: AppConfig = serde_json::from_str("{}").unwrap();
        assert!(old.auto_reconnect);
        assert!(old.snippets.is_empty());
    }
}
