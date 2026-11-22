//! Connection lifecycle: validate → (password prompt) → connect thread → PTY I/O.

use super::*;
use std::net::IpAddr;
use std::sync::atomic::Ordering;

pub(super) const MAX_AUTO_RECONNECT: u32 = 5;

pub(super) fn auto_reconnect_delay_secs(attempt: u32) -> Option<u64> {
    const DELAYS: [u64; 5] = [2, 4, 8, 16, 30];
    DELAYS.get(attempt as usize).copied()
}

impl GuiApp {
    /// Connect a saved host (double-click, context menu, welcome list…).
    pub(super) fn connect_profile(&mut self, id: &str, force_new_tab: bool) {
        if self.credentials_busy(id) {
            return;
        }
        let Some(p) = self.config.profiles.iter().find(|p| p.id == id).cloned() else {
            return;
        };
        self.selected_profile_id = Some(id.to_string());
        self.credentials.submit(
            id.to_string(),
            move || ConnParams::from_profile(&p),
            move |app, params| {
                if app
                    .config
                    .profiles
                    .iter()
                    .any(|p| Some(&p.id) == params.profile_id.as_ref())
                {
                    app.connect_params(params, force_new_tab, None);
                }
            },
        );
    }

    /// Validate parameters, ask for a password when nothing else can authenticate,
    /// then start the connection.
    pub(super) fn connect_params(
        &mut self,
        params: ConnParams,
        force_new_tab: bool,
        tab_uid: Option<u64>,
    ) {
        self.connect_params_inner(params, force_new_tab, tab_uid, true);
    }

    fn connect_params_inner(
        &mut self,
        params: ConnParams,
        force_new_tab: bool,
        tab_uid: Option<u64>,
        focus_tab: bool,
    ) {
        if let Err(e) = self.validate(&params) {
            self.toast_msg(e, StatusKind::Error);
            return;
        }
        if params.password.is_empty() && params.identity.trim().is_empty() {
            self.ask_password(params, tab_uid);
            return;
        }
        let idx = match tab_uid.and_then(|u| self.slot_index(u)) {
            Some(i) => i,
            None => self.target_tab(force_new_tab),
        };
        self.start_connect_in(idx, params, focus_tab);
    }

    /// Ask for the password, then connect (see `ui_password_prompt`).
    fn ask_password(&mut self, params: ConnParams, tab_uid: Option<u64>) {
        if self.pw_prompt.is_some() {
            // Another prompt is open (e.g. "connect all"): ask afterwards.
            self.pw_queue.push_back((params, tab_uid));
            return;
        }
        self.pw_prompt = Some(PasswordPrompt {
            params,
            password: String::new(),
            remember: false,
            focus: true,
            tab_uid,
        });
    }

    fn validate(&self, p: &ConnParams) -> Result<(), String> {
        if p.host.trim().is_empty() {
            return Err(self.t("请填写主机地址", "Enter a host address").into());
        }
        if p.user.trim().is_empty() {
            return Err(self.t("请填写用户名", "Enter an SSH username").into());
        }
        if p.port == 0 {
            return Err(self
                .t(
                    "端口必须在 1–65535 之间",
                    "Port must be between 1 and 65535",
                )
                .into());
        }
        if (!p.forward_local.trim().is_empty() || !p.forward_remote.trim().is_empty())
            && parse_forward(&p.forward_local, &p.forward_remote).is_none()
        {
            return Err(self
                .t(
                    "端口转发格式：本地 127.0.0.1:8080 → 远端 host:port",
                    "Forward needs local IP:port and remote host:port",
                )
                .into());
        }
        self.resolve_bind(p).map(|_| ())
    }

    /// Reuse the active tab when it has never been used, otherwise open a new one.
    fn target_tab(&mut self, force_new: bool) -> usize {
        if !force_new && self.active_slot().is_fresh() {
            return self.active;
        }
        if !force_new {
            if let Some(i) = self.sessions.iter().position(|s| s.is_fresh()) {
                self.switch_tab(i);
                return i;
            }
        }
        self.new_tab()
    }

    pub(super) fn reconnect(&mut self, idx: usize) {
        self.reconnect_with_focus(idx, true);
    }

    pub(super) fn reconnect_with_focus(&mut self, idx: usize, focus_tab: bool) {
        let Some(slot) = self.sessions.get_mut(idx) else {
            return;
        };
        if slot.is_busy() || slot.params.host.is_empty() {
            return;
        }
        if focus_tab {
            slot.auto_reconnect_attempts = 0;
            slot.auto_reconnect_at = None;
        }
        // A password the server refused is not sent again: ask for it.
        let auth_failed = slot.shared.lock().unwrap().auth_failed;
        let uid = slot.uid;
        let previous = slot.params.clone();
        let profile = previous
            .profile_id
            .as_ref()
            .and_then(|id| self.config.profiles.iter().find(|p| &p.id == id))
            .cloned();
        if let Some(p) = profile {
            if self.credentials_busy(&p.id) {
                return;
            }
            self.credentials.submit(
                p.id.clone(),
                move || {
                    let mut params = ConnParams::from_profile(&p);
                    if params.password.is_empty() {
                        params.password = previous.password;
                    }
                    if params.key_passphrase.is_empty() {
                        params.key_passphrase = previous.key_passphrase;
                    }
                    params
                },
                move |app, params| app.finish_reconnect(uid, params, auth_failed, focus_tab),
            );
        } else {
            self.finish_reconnect(uid, previous, auth_failed, focus_tab);
        }
    }

    fn finish_reconnect(
        &mut self,
        uid: u64,
        mut params: ConnParams,
        auth_failed: bool,
        focus_tab: bool,
    ) {
        let Some(i) = self.slot_index(uid) else {
            return;
        };
        if self.sessions[i].is_busy() {
            return;
        }
        let had_password = auth_failed && !params.password.is_empty();
        if auth_failed {
            params.password.clear();
        }
        // With a key as well, connecting would quietly retry the key alone.
        if had_password && !params.identity.trim().is_empty() {
            if let Err(e) = self.validate(&params) {
                self.toast_msg(e, StatusKind::Error);
                return;
            }
            self.ask_password(params, Some(uid));
            return;
        }
        self.connect_params_inner(params, false, Some(uid), focus_tab);
    }

    fn resolve_bind(&self, p: &ConnParams) -> Result<(Option<IpAddr>, Option<String>), String> {
        match p.bind_mode {
            BindMode::Auto => Ok((None, None)),
            BindMode::Addr => p
                .bind_addr
                .trim()
                .parse::<IpAddr>()
                .map(|ip| (Some(ip), None))
                .map_err(|_| {
                    self.t("请填写有效的本地绑定 IP", "Enter a valid local bind IP")
                        .into()
                }),
            BindMode::Iface => {
                if p.bind_iface.is_empty() && p.bind_addr.is_empty() {
                    return Err(self.t("请选择网卡", "Select a network interface").into());
                }
                #[cfg(target_os = "linux")]
                {
                    if p.bind_iface.is_empty() {
                        return Err(self.t("请选择网卡", "Select a network interface").into());
                    }
                    Ok((None, Some(p.bind_iface.clone())))
                }
                #[cfg(not(target_os = "linux"))]
                {
                    // Bind to the adapter's address *now*: the one saved with
                    // the host goes stale when DHCP / another Wi-Fi hands out
                    // a new one. The saved address is only the fallback.
                    if !p.bind_iface.is_empty() {
                        if let Some(c) = bypass::list_bind_candidates()
                            .unwrap_or_default()
                            .into_iter()
                            .find(|c| c.name == p.bind_iface)
                        {
                            return Ok((Some(IpAddr::V4(c.ip)), None));
                        }
                    }
                    let ip = p.bind_addr.parse::<IpAddr>().map_err(|_| {
                        self.t(
                            "保存的网卡地址已失效，请重新选择网卡",
                            "Saved interface IPv4 is unavailable; choose an interface again",
                        )
                        .to_string()
                    })?;
                    Ok((Some(ip), None))
                }
            }
        }
    }

    fn start_connect_in(&mut self, idx: usize, params: ConnParams, focus_tab: bool) {
        let (bind_addr, bind_iface) = match self.resolve_bind(&params) {
            Ok(b) => b,
            Err(e) => {
                self.toast_msg(e, StatusKind::Error);
                return;
            }
        };
        let zh = self.zh();
        let ctx = self.ctx.clone();
        let runtime = Arc::clone(&self.runtime);
        let profile_id = params.profile_id.clone();

        // Disambiguate several tabs connected to the same host: "web", "web (2)", …
        let base = params.title();
        let mut title = base.clone();
        let mut n = 2;
        while self
            .sessions
            .iter()
            .enumerate()
            .any(|(i, s)| i != idx && s.title == title)
        {
            title = format!("{base} ({n})");
            n += 1;
        }

        let slot = &mut self.sessions[idx];
        if slot.is_busy() {
            return;
        }
        slot.manual_disconnect = false;
        slot.auto_reconnect_at = None;
        let is_reconnect = slot.ever_connected;
        if !is_reconnect {
            slot.term.reset(self.config.scrollback_lines);
        }
        let my_gen = {
            let mut s = slot.shared.lock().unwrap();
            s.conn_gen += 1;
            s.connecting = true;
            s.connected = false;
            s.session_ended = false;
            s.auth_failed = false;
            s.auto_reconnect_eligible = false;
            s.status = if zh {
                "正在连接…".into()
            } else {
                "Connecting…".into()
            };
            s.status_kind = StatusKind::Busy;
            s.monitor = MonitorUi::default();
            s.bound_local = None;
            s.remote_addr = None;
            s.handle = None;
            s.forward.clear();
            // Keep `gen` increasing: a listing still in flight from the old
            // connection must not land in the new one.
            s.files = FilesUi {
                cwd: "~".into(),
                gen: s.files.gen + 1,
                ..Default::default()
            };
            s.notices.clear();
            s.closing_notice = None;
            s.closing = false;
            s.conn_gen
        };
        let (fwd_tx, fwd_rx) = tokio::sync::watch::channel(false);
        let (input_tx, input_rx) = mpsc::unbounded_channel::<session::ChannelInput>();
        let (ui_tx, ui_rx) = session::output_channel();
        let (stop_tx, stop_rx) = tokio::sync::oneshot::channel::<()>();
        slot.forward_stop = Some(fwd_tx.clone());
        slot.input_tx = Some(input_tx);
        slot.stop_tx = Some(stop_tx);
        slot.output_rx = Some(ui_rx);
        slot.params = params.clone();
        slot.title = title;
        if focus_tab {
            slot.view = CenterView::Terminal;
        }
        slot.file_sel.clear();
        let (cols, rows) = slot.term.size();
        let shared = Arc::clone(&slot.shared);
        if focus_tab {
            self.active = idx;
            self.tab_scroll_pending = true;
            self.find = None;
        }

        if let Some(id) = &profile_id {
            self.selected_profile_id = Some(id.clone());
        }
        self.persist();

        let notice = {
            let shared = Arc::clone(&shared);
            let ctx = ctx.clone();
            move |text: String, color: u8| {
                if let Ok(mut s) = shared.lock() {
                    if s.conn_gen == my_gen {
                        s.notices.push((text, color));
                    }
                }
                ctx.request_repaint();
            }
        };

        // Runs as a task on the shared runtime (no OS thread per connection).
        runtime.spawn(async move {
            let shared_after = Arc::clone(&shared);
            let ctx_after = ctx.clone();
            let host = params.host.clone();
            let user = params.user.clone();
            let port = params.port;
            let key_rejected = Arc::new(std::sync::atomic::AtomicBool::new(false));
            let key_rejected_after = Arc::clone(&key_rejected);
            let result = async move {
                let mut stop_rx = stop_rx;
                let cancelled = Arc::new(std::sync::atomic::AtomicBool::new(false));
                let key_rejected_setup = Arc::clone(&key_rejected);
                // Everything up to an open shell can be cancelled with
                // "Disconnect" (or by closing the tab).
                let setup = async {
                notice(
                    format!(
                        "{} {user}@{host}:{port}{}",
                        if zh { "▶ 连接" } else { "▶ Connecting to" },
                        match (&bind_addr, &bind_iface) {
                            (_, Some(i)) => format!("  (bind iface {i})"),
                            (Some(a), _) => format!("  (bind {a})"),
                            _ => "  (bind auto)".into(),
                        }
                    ),
                    90,
                );
                let remote = bypass::resolve_host(
                    &host,
                    port,
                    Some(&params.dns_server),
                    bind_addr,
                    bind_iface.as_deref(),
                )
                .await
                .map_err(|e| format!("{}: {e:#}", if zh { "DNS 解析失败" } else { "DNS lookup failed" }))?;
                notice(format!("  {} {remote}", if zh { "解析为" } else { "resolved" }), 90);

                let stream = bypass::connect_bypassing_tun(remote, bind_addr, bind_iface.as_deref())
                    .await
                    .map_err(|e| format!("{}: {e:#}", if zh { "TCP 连接失败" } else { "Connection failed" }))?;
                if let Ok(local) = stream.local_addr() {
                    notice(format!("  {} {local}", if zh { "本地出口" } else { "local socket" }), 90);
                    if let Ok(mut s) = shared.lock() {
                        s.bound_local = Some(local.to_string());
                        s.remote_addr = Some(remote.to_string());
                    }
                }

                let verify_host = host.clone();
                let verify_shared = Arc::clone(&shared);
                let verify_ctx = ctx.clone();
                let verify_cancel = Arc::clone(&cancelled);
                // Why the server key was refused, reported instead of russh's
                // generic "Unknown server key".
                let key_err = Arc::new(Mutex::new(None::<String>));
                let verify_err = Arc::clone(&key_err);
                let verifier: session::HostKeyVerifier = Arc::new(move |key| {
                    let host = verify_host.clone();
                    let shared = Arc::clone(&verify_shared);
                    let ctx = verify_ctx.clone();
                    let key_err = Arc::clone(&verify_err);
                    let cancelled = Arc::clone(&verify_cancel);
                    let key_rejected = Arc::clone(&key_rejected_setup);
                    Box::pin(async move {
                        let set_err = |msg: String| {
                            key_rejected.store(true, Ordering::Relaxed);
                            *key_err.lock().unwrap() = Some(msg);
                        };
                        // File I/O stays off the async workers.
                        let (h, k) = (host.clone(), key.clone());
                        let verdict = match tokio::task::spawn_blocking(move || {
                            crate::hostkeys::check(&h, port, &k)
                        })
                        .await
                        {
                            Ok(Ok(v)) => v,
                            Ok(Err(e)) => {
                                set_err(format!(
                                    "{}: {e:#}",
                                    if zh { "无法校验主机密钥" } else { "Host key check failed" }
                                ));
                                return false;
                            }
                            Err(_) => return false,
                        };
                        let (previous, other_type) = match verdict {
                            crate::hostkeys::Verdict::Match => return true,
                            crate::hostkeys::Verdict::New => (None, false),
                            crate::hostkeys::Verdict::Changed(old) => (Some(old.fingerprint), false),
                            // Another type of key is saved: shown for
                            // reference, asked like a new host.
                            crate::hostkeys::Verdict::OtherType(old) => (
                                Some(format!("{}  {}", old.algorithm, old.fingerprint)),
                                true,
                            ),
                        };
                        let changed = previous.is_some() && !other_type;
                        let decision = Arc::new(Mutex::new(None::<bool>));
                        if cancelled.load(Ordering::Relaxed) {
                            return false;
                        }
                        if let Ok(mut s) = shared.lock().map_err(|_| ()).and_then(|s| {
                            if s.conn_gen == my_gen { Ok(s) } else { Err(()) }
                        }) {
                            s.status = match (changed, zh) {
                                _ if other_type && zh => "服务器出示了新的密钥类型，请核对指纹".into(),
                                _ if other_type => "New host key type — check the fingerprint".into(),
                                (true, true) => "主机密钥已变化，请确认".into(),
                                (true, false) => "Host key changed — confirm before sign-in".into(),
                                (false, true) => "首次连接，请核对主机指纹".into(),
                                (false, false) => "First connection — check the host fingerprint".into(),
                            };
                            s.status_kind = StatusKind::Busy;
                        }
                        let prompt = HostKeyPrompt {
                            host: host.clone(),
                            port,
                            fingerprint: crate::hostkeys::fingerprint(&key),
                            algorithm: key.algorithm().to_string(),
                            previous,
                            other_type,
                            decision: Arc::clone(&decision),
                        };
                        let mine = |p: &Option<HostKeyPrompt>| {
                            p.as_ref().is_some_and(|p| Arc::ptr_eq(&p.decision, &decision))
                        };
                        // One host-key dialog at a time; wait our turn. A
                        // cancelled attempt gives up (and its TCP connection
                        // closes) instead of waiting forever.
                        let mut waited = false;
                        loop {
                            if cancelled.load(Ordering::Relaxed) {
                                return false;
                            }
                            let free = HOSTKEY_PROMPT.lock().unwrap().is_none();
                            if free && waited {
                                // While we waited, the user may have answered the
                                // same question for another tab ("connect all").
                                let (h, k) = (host.clone(), key.clone());
                                if let Ok(Ok(crate::hostkeys::Verdict::Match)) =
                                    tokio::task::spawn_blocking(move || {
                                        crate::hostkeys::check(&h, port, &k)
                                    })
                                    .await
                                {
                                    return true;
                                }
                            }
                            if free {
                                let mut pending = HOSTKEY_PROMPT.lock().unwrap();
                                if pending.is_none() {
                                    *pending = Some(prompt.clone());
                                    break;
                                }
                            }
                            waited = true;
                            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                        }
                        ctx.request_repaint();
                        let accepted = loop {
                            let answer = *decision.lock().unwrap();
                            if let Some(a) = answer {
                                break a;
                            }
                            if cancelled.load(Ordering::Relaxed) {
                                break false;
                            }
                            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                        };
                        // Save the answer before giving up the dialog slot, so
                        // a tab waiting for it finds the key already trusted
                        // instead of asking the same question again.
                        let trusted = if accepted {
                            let (h, k) = (host.clone(), key.clone());
                            match tokio::task::spawn_blocking(move || {
                                crate::hostkeys::trust(&h, port, &k)
                            })
                            .await
                            {
                                Ok(Ok(())) => true,
                                Ok(Err(e)) => {
                                    set_err(format!("{e:#}"));
                                    false
                                }
                                Err(_) => false,
                            }
                        } else {
                            false
                        };
                        {
                            // Remove only our own question, never another tab's.
                            let mut pending = HOSTKEY_PROMPT.lock().unwrap();
                            if mine(&pending) {
                                pending.take();
                            }
                        }
                        ctx.request_repaint();
                        if !accepted && !cancelled.load(Ordering::Relaxed) {
                            set_err(match (changed, zh) {
                                (true, true) => "已拒绝变化后的主机密钥，未登录".into(),
                                (true, false) => "Changed host key rejected — not signed in".into(),
                                (false, true) => "未信任该主机，已断开".into(),
                                (false, false) => "Host not trusted — disconnected".into(),
                            });
                        }
                        trusted
                    })
                });

                let identity = Some(params.identity.trim())
                    .filter(|s| !s.is_empty())
                    .map(PathBuf::from);
                let (handle, _server_key) = session::connect_and_auth(
                    stream,
                    session::SessionOptions {
                        user: &user,
                        host: &host,
                        identity: identity.as_deref(),
                        key_passphrase: Some(params.key_passphrase.as_str()).filter(|s| !s.is_empty()),
                        password: Some(params.password.as_str()).filter(|s| !s.is_empty()),
                        ask_password: false,
                        host_key_verifier: verifier,
                    },
                )
                .await
                .map_err(|e| {
                    if let Some(why) = key_err.lock().unwrap().take() {
                        return why;
                    }
                    let detail = format!("{e:#}");
                    if detail.starts_with("SSH handshake failed") {
                        detail
                    } else {
                        if let Ok(mut s) = shared.lock() {
                            s.auth_failed = true;
                        }
                        if zh {
                        format!("认证失败：{detail}")
                    } else {
                        format!("Authentication failed: {detail}")
                    }
                    }
                })?;
                notice(format!("  {}", if zh { "认证成功，正在打开终端…" } else { "authenticated, opening shell…" }), 90);

                let handle = Arc::new(handle);
                let channel = session::open_pty_shell(&handle, cols as u32, rows as u32, "xterm-256color")
                    .await
                    .map_err(|e| format!("{}: {e:#}", if zh { "打开 PTY 失败" } else { "Failed to open PTY" }))?;
                Ok::<_, String>((handle, channel))
                };
                let (handle, channel) = tokio::select! {
                    r = setup => r?,
                    _ = &mut stop_rx => {
                        // The handshake runs in russh's own task: tell its
                        // host-key check to give up (closing the connection)
                        // and to withdraw its dialog, if it has one.
                        cancelled.store(true, Ordering::Relaxed);
                        return Err(if zh { "已取消连接".to_string() } else { "Connection cancelled".to_string() });
                    }
                };

                let fwd_join = if let Some((listen, remote_host, remote_port)) =
                    parse_forward(&params.forward_local, &params.forward_remote)
                {
                    let fwd_handle = Arc::clone(&handle);
                    let shared_fwd = Arc::clone(&shared);
                    let fwd_notice = notice.clone();
                    if let Ok(mut s) = shared.lock() {
                        s.forward = format!("{listen} → {remote_host}:{remote_port}");
                    }
                    fwd_notice(format!("  -L {listen} → {remote_host}:{remote_port}"), 90);
                    Some(tokio::spawn(async move {
                        if let Err(e) =
                            session::local_forward(fwd_handle, listen, remote_host, remote_port, fwd_rx).await
                        {
                            if let Ok(mut s) = shared_fwd.lock() {
                                s.forward = format!("forward failed: {e:#}");
                            }
                            fwd_notice(format!("port forward failed: {e:#}"), 31);
                        }
                    }))
                } else {
                    None
                };

                {
                    let mut s = shared.lock().unwrap();
                    s.connected = true;
                    s.connecting = false;
                    s.authed_gen = my_gen;
                    s.status = if zh {
                        format!("已连接 {user}@{host}:{port}")
                    } else {
                        format!("Connected {user}@{host}:{port}")
                    };
                    s.status_kind = StatusKind::Ok;
                    s.handle = Some(Arc::clone(&handle));
                }
                ctx.request_repaint();

                // Remote monitor loop.
                let handle_mon = Arc::clone(&handle);
                let shared_mon = Arc::clone(&shared);
                let ctx_mon = ctx.clone();
                let (mon_stop_tx, mut mon_stop_rx) = tokio::sync::oneshot::channel::<()>();
                let mon_join = tokio::spawn(async move {
                    let mut prev = session::RemoteStats::default();
                    let mut prev_net: Option<(u64, u64, Instant)> = None;
                    let mut interval = tokio::time::interval(std::time::Duration::from_secs(2));
                    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
                    let mut last_poll: Option<Instant> = None;
                    let mut have_static = false;
                    loop {
                        tokio::select! {
                            _ = &mut mon_stop_rx => break,
                            _ = interval.tick() => {
                                // Every 2 s while the monitor is shown, every
                                // 30 s otherwise (keeps history going cheaply),
                                // every 5 min where commands don't run at all
                                // (SFTP-only accounts).
                                let wanted = shared_mon.lock().map(|s| s.monitor_wanted).unwrap_or(true);
                                let every = if !session::exec_works(&handle_mon) {
                                    300
                                } else if wanted {
                                    0
                                } else {
                                    30
                                };
                                if last_poll.is_some_and(|t| t.elapsed() < std::time::Duration::from_secs(every)) {
                                    continue;
                                }
                                last_poll = Some(Instant::now());
                                // Host name, kernel, cores: until we have them.
                                let first = !have_static;
                                let stats = tokio::select! {
                                    _ = &mut mon_stop_rx => break,
                                    s = session::poll_remote_stats(&handle_mon, &mut prev, first) => s,
                                };
                                if first
                                    && (stats.hostname.is_some()
                                        || stats.kernel.is_some()
                                        || stats.cpu_cores.is_some())
                                {
                                    have_static = true;
                                }
                                prev.prev_cpu_total = stats.prev_cpu_total;
                                prev.prev_cpu_idle = stats.prev_cpu_idle;
                                let now = Instant::now();
                                let (rx_rate, tx_rate) = match (stats.net_rx_bytes, stats.net_tx_bytes, prev_net) {
                                    (Some(rx), Some(tx), Some((prx, ptx, pt))) => {
                                        let dt = now.duration_since(pt).as_secs_f64().max(0.001);
                                        (
                                            Some(rx.saturating_sub(prx) as f64 / dt),
                                            Some(tx.saturating_sub(ptx) as f64 / dt),
                                        )
                                    }
                                    _ => (None, None),
                                };
                                if let (Some(rx), Some(tx)) = (stats.net_rx_bytes, stats.net_tx_bytes) {
                                    prev_net = Some((rx, tx, now));
                                }
                                if let Ok(mut s) = shared_mon.lock() {
                                    let m = &mut s.monitor;
                                    m.cpu_percent = stats.cpu_percent;
                                    m.mem_used = stats.mem_used_bytes();
                                    m.mem_total = stats.mem_total_bytes;
                                    m.mem_percent = stats.mem_percent();
                                    m.disk_used = stats.disk_used_bytes;
                                    m.disk_total = stats.disk_total_bytes;
                                    m.disk_percent = stats.disk_percent();
                                    m.disk_mount = stats.disk_mount.clone();
                                    if first {
                                        if stats.hostname.is_some() {
                                            m.hostname = stats.hostname.clone();
                                        }
                                        if stats.kernel.is_some() {
                                            m.kernel = stats.kernel.clone();
                                        }
                                        if stats.cpu_cores.is_some() {
                                            m.cpu_cores = stats.cpu_cores;
                                        }
                                    }
                                    m.load_avg = stats.load_avg;
                                    m.uptime_secs = stats.uptime_secs;
                                    m.net_rx_rate = rx_rate;
                                    m.net_tx_rate = tx_rate;
                                    m.error = stats.raw_error.clone();
                                    if let Some(c) = stats.cpu_percent {
                                        m.cpu_hist.push_back(c);
                                        if m.cpu_hist.len() > HIST_LEN { m.cpu_hist.pop_front(); }
                                    }
                                    if let Some(p) = stats.mem_percent() {
                                        m.mem_hist.push_back(p);
                                        if m.mem_hist.len() > HIST_LEN { m.mem_hist.pop_front(); }
                                    }
                                }
                                ctx_mon.request_repaint();
                            }
                        }
                    }
                });

                // PTY output goes straight to the UI, which is woken at once.
                let ctx_out = ctx.clone();
                let io_res = session::drive_channel_io(
                    channel,
                    Arc::clone(&handle),
                    input_rx,
                    ui_tx,
                    stop_rx,
                    move || ctx_out.request_repaint(),
                )
                .await;
                let _ = mon_stop_tx.send(());
                let _ = fwd_tx.send(true);
                let _ = mon_join.await;
                if let Some(fj) = fwd_join {
                    let _ = fj.await;
                }
                let code = io_res
                    .map_err(|e| format!("{}: {e:#}", if zh { "会话错误" } else { "Session error" }))?;
                Ok::<u32, String>(code)
            }
            .await;

            // A newer attempt owns this tab now: leave its state alone. The
            // check and the updates happen under one lock, so a reconnect
            // can't slip in between.
            let (msg, kind, line, color) = match &result {
                Ok(code) => {
                    let msg = if zh {
                        format!("会话已结束（退出码 {code}）")
                    } else {
                        format!("Session ended (exit {code})")
                    };
                    (msg.clone(), StatusKind::Info, format!("■ {msg}"), 33)
                }
                Err(e) => (e.clone(), StatusKind::Error, format!("✖ {e}"), 31),
            };
            if let Ok(mut s) = shared_after.lock() {
                if s.conn_gen == my_gen {
                    s.closing_notice = Some((line, color));
                    s.closing = true;
                    s.status = msg;
                    s.status_kind = kind;
                    s.connected = false;
                    s.connecting = false;
                    s.session_ended = true;
                    s.auto_reconnect_eligible = result.is_err()
                        && !s.auth_failed
                        && !key_rejected_after.load(Ordering::Relaxed);
                    s.monitor = MonitorUi::default();
                    s.handle = None;
                    s.forward.clear();
                }
            }
            ctx_after.request_repaint();
        });
    }

    pub(super) fn disconnect_at(&mut self, idx: usize) {
        let zh = self.zh();
        if let Some(slot) = self.sessions.get_mut(idx) {
            slot.manual_disconnect = true;
            slot.auto_reconnect_at = None;
            slot.auto_reconnect_attempts = 0;
            if let Some(tx) = slot.stop_tx.take() {
                let _ = tx.send(());
            }
            if let Some(tx) = slot.forward_stop.take() {
                let _ = tx.send(true);
            }
            slot.input_tx = None;
            if let Ok(mut s) = slot.shared.lock() {
                s.transfer
                    .cancel
                    .store(true, std::sync::atomic::Ordering::Relaxed);
                if s.connected || s.connecting {
                    s.status = if zh {
                        "正在断开…".into()
                    } else {
                        "Disconnecting…".into()
                    };
                    s.status_kind = StatusKind::Busy;
                }
            }
        }
    }

    pub(super) fn send_bytes(&self, idx: usize, data: Vec<u8>) {
        if data.is_empty() {
            return;
        }
        if let Some(tx) = self.sessions.get(idx).and_then(|s| s.input_tx.as_ref()) {
            let _ = tx.send(session::ChannelInput::Data(data));
        }
    }

    pub(super) fn send_resize(&self, idx: usize, cols: u16, rows: u16) {
        if let Some(tx) = self.sessions.get(idx).and_then(|s| s.input_tx.as_ref()) {
            let _ = tx.send(session::ChannelInput::Resize {
                cols: cols as u32,
                rows: rows as u32,
            });
        }
    }

    /// Drain PTY output / notices for every tab and track connection transitions.
    pub(super) fn poll_sessions(&mut self) {
        /// Output parsed per frame across all tabs, and per tab: a flood
        /// (cat bigfile, in several tabs) can't freeze the UI; the rest is
        /// picked up next frame.
        const FRAME_BUDGET: usize = 1024 * 1024;
        const TAB_BUDGET: usize = 512 * 1024;
        let frame_started = Instant::now();
        let frame_time = std::time::Duration::from_millis(4);
        /// What a closed connection may still leave to show at once.
        const CLOSE_BUDGET: usize = 1024 * 1024;
        // (host id, password to remember now that it worked)
        let mut newly_connected: Vec<(String, Option<String>)> = Vec::new();
        let mut focus_active = false;
        let active = self.active;
        // In group mode every tile is on screen, so nothing is "unread".
        let group = self.view_mode == ViewMode::Group;
        let zh = self.zh();
        let gap_notice = |skipped: u64| {
            if zh {
                format!(
                    "[Tern] 界面繁忙，这里跳过了 {} 的输出",
                    format_bytes(skipped)
                )
            } else {
                format!(
                    "[Tern] UI was busy: skipped {} of output here",
                    format_bytes(skipped)
                )
            }
        };
        let mut more_output = false;
        let n = self.sessions.len();
        // Rotate the first tab too: an expensive active terminal must not
        // consume every frame's time budget before background tabs run.
        let start = self.output_rr % n.max(1);
        let mut frame_left = FRAME_BUDGET;
        let mut due_reconnects = Vec::new();
        for i in (0..n).map(|k| (start + k) % n) {
            let slot = &mut self.sessions[i];
            let (notices, closing, closing_notice, eligible) = {
                let mut s = slot.shared.lock().unwrap();
                if s.authed_gen != 0 && slot.seen_conn_gen != s.authed_gen {
                    slot.seen_conn_gen = s.authed_gen;
                    slot.ever_connected = true;
                    slot.auto_reconnect_attempts = 0;
                    slot.auto_reconnect_at = None;
                    if i == active && s.connected {
                        focus_active = true;
                    }
                    if let Some(id) = &slot.params.profile_id {
                        let remember = std::mem::take(&mut slot.params.remember_password)
                            .then(|| slot.params.password.clone())
                            .filter(|p| !p.is_empty());
                        newly_connected.push((id.clone(), remember));
                    }
                }
                (
                    std::mem::take(&mut s.notices),
                    std::mem::take(&mut s.closing),
                    s.closing_notice.take(),
                    s.auto_reconnect_eligible,
                )
            };
            // Connection notices ("认证成功…") precede the shell's first output;
            // a closing notice ("已断开") follows the last of it.
            for (text, color) in &notices {
                slot.term.feed_notice(text, *color);
            }
            let mut got = false;
            if closing {
                slot.input_tx = None;
                slot.stop_tx = None;
                slot.forward_stop = None;
                // Show what arrived just before the channel closed (up to a
                // limit: the rest is summarised), then the closing notice.
                if let Some(mut rx) = slot.output_rx.take() {
                    let mut left = CLOSE_BUDGET;
                    let mut skipped = 0u64;
                    while let Some(item) = rx.try_recv() {
                        match item {
                            session::OutputItem::Data(data)
                                if left > 0
                                    && frame_left > 0
                                    && frame_started.elapsed() < frame_time =>
                            {
                                left = left.saturating_sub(data.len());
                                frame_left = frame_left.saturating_sub(data.len());
                                slot.term.feed(&data);
                                got = true;
                            }
                            session::OutputItem::Data(data) => skipped += data.len() as u64,
                            session::OutputItem::Gap(n) => skipped += n,
                        }
                    }
                    if skipped > 0 {
                        slot.term.feed_notice(&gap_notice(skipped), 33);
                    }
                }
                if let Some((text, color)) = closing_notice {
                    slot.term.feed_notice(&text, color);
                }
                let has_creds =
                    !slot.params.password.is_empty() || !slot.params.identity.trim().is_empty();
                if self.config.auto_reconnect
                    && slot.ever_connected
                    && !slot.manual_disconnect
                    && eligible
                    && has_creds
                {
                    if let Some(delay) = auto_reconnect_delay_secs(slot.auto_reconnect_attempts) {
                        slot.auto_reconnect_at =
                            Some(Instant::now() + std::time::Duration::from_secs(delay));
                        slot.auto_reconnect_attempts += 1;
                    }
                }
            } else if let Some(rx) = slot.output_rx.as_mut() {
                let budget = TAB_BUDGET.min(frame_left);
                let mut used = 0usize;
                while used < budget && frame_started.elapsed() < frame_time {
                    match rx.try_recv() {
                        None => break,
                        Some(session::OutputItem::Data(data)) => {
                            used += data.len();
                            slot.term.feed(&data);
                            got = true;
                        }
                        Some(session::OutputItem::Gap(skipped)) => {
                            slot.term.feed_notice(&gap_notice(skipped), 33);
                        }
                    }
                }
                frame_left = frame_left.saturating_sub(used);
                if rx.has_pending() {
                    more_output = true;
                }
            }
            if got && i != active && !group {
                slot.unread = true;
            }
            let _ = slot.term.take_bell();
            if let Some(at) = slot.auto_reconnect_at {
                if !self.config.auto_reconnect || slot.is_busy() {
                    slot.auto_reconnect_at = None;
                } else if Instant::now() >= at {
                    slot.auto_reconnect_at = None;
                    due_reconnects.push(i);
                } else {
                    self.ctx
                        .request_repaint_after(std::time::Duration::from_millis(250));
                }
            }
        }
        self.output_rr = (start + 1) % n.max(1);
        for i in due_reconnects {
            self.reconnect_with_focus(i, false);
        }
        if focus_active {
            self.focus_term = true;
        }
        if more_output {
            self.ctx.request_repaint();
        }
        if !newly_connected.is_empty() {
            let now = profiles::now_unix();
            for (id, remember) in newly_connected {
                if let Some(p) = self.config.profiles.iter_mut().find(|p| p.id == id) {
                    p.last_connected = Some(now);
                    if let Some(pw) = remember {
                        let mut profile = p.clone();
                        self.credentials.submit(
                            id.clone(),
                            move || {
                                profile.remember_password = true;
                                let saved = profile.set_password(&pw).is_ok();
                                (profile, saved)
                            },
                            |app, (profile, saved)| {
                                if let Some(p) =
                                    app.config.profiles.iter_mut().find(|p| p.id == profile.id)
                                {
                                    if saved {
                                        p.remember_password = true;
                                        p.password_obfuscated = profile.password_obfuscated;
                                        app.persist();
                                    } else {
                                        app.credential_warning();
                                    }
                                }
                            },
                        );
                    }
                }
            }
            self.persist();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auto_reconnect_backoff_schedule_caps_at_five_attempts() {
        assert_eq!(auto_reconnect_delay_secs(0), Some(2));
        assert_eq!(auto_reconnect_delay_secs(1), Some(4));
        assert_eq!(auto_reconnect_delay_secs(2), Some(8));
        assert_eq!(auto_reconnect_delay_secs(3), Some(16));
        assert_eq!(auto_reconnect_delay_secs(4), Some(30));
        assert_eq!(auto_reconnect_delay_secs(5), None);
        assert_eq!(MAX_AUTO_RECONNECT, 5);
    }
}
