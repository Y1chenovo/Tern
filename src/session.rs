#![cfg_attr(not(feature = "gui"), allow(dead_code))]

//! Interactive SSH session: auth, PTY, bidirectional I/O, window-size updates.

use anyhow::{bail, Context, Result};
use crossterm::terminal::{disable_raw_mode, enable_raw_mode, size as term_size};
use russh::client::{self, KeyboardInteractiveAuthResponse, Msg};
use russh::keys::{load_secret_key, PrivateKeyWithHashAlg, PublicKey, PublicKeyOrCertificate};
use russh::{Channel, ChannelMsg, Disconnect, Pty};
use std::future::Future;
use std::io::{self, Write};
use std::path::Path;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::mpsc;

pub type HostKeyVerifier =
    Arc<dyn Fn(PublicKey) -> Pin<Box<dyn Future<Output = bool> + Send>> + Send + Sync>;

/// Verify the server key during the SSH handshake, before any credentials are sent.
pub struct ClientHandler {
    server_key: Arc<std::sync::Mutex<Option<PublicKey>>>,
    host_key_verifier: HostKeyVerifier,
}

impl client::Handler for ClientHandler {
    type Error = russh::Error;

    async fn check_server_key(
        &mut self,
        server_public_key: &PublicKeyOrCertificate,
    ) -> Result<bool, Self::Error> {
        let key = server_public_key.public_key();
        if !(self.host_key_verifier)(key.clone()).await {
            return Ok(false);
        }
        if let Ok(mut slot) = self.server_key.lock() {
            *slot = Some(key.clone());
        }
        Ok(true)
    }
}

/// Convenience alias for an authenticated SSH client handle (GUI / helpers).
#[cfg(feature = "gui")]
pub type SshHandle = client::Handle<ClientHandler>;

pub struct SessionOptions<'a> {
    pub user: &'a str,
    /// Server name as the user typed it (only for prompts).
    pub host: &'a str,
    pub identity: Option<&'a Path>,
    pub key_passphrase: Option<&'a str>,
    pub password: Option<&'a str>,
    pub ask_password: bool,
    pub host_key_verifier: HostKeyVerifier,
}

/// How long a server may take to send its SSH identification line.
const BANNER_TIMEOUT: Duration = Duration::from_secs(15);

/// A stream whose first read fails with `TimedOut` if nothing arrives in
/// time; afterwards it is the plain stream.
struct FirstByteDeadline<S> {
    inner: S,
    deadline: Option<Pin<Box<tokio::time::Sleep>>>,
    limit: Duration,
}

impl<S> FirstByteDeadline<S> {
    fn new(inner: S, limit: Duration) -> Self {
        Self {
            inner,
            deadline: Some(Box::pin(tokio::time::sleep(limit))),
            limit,
        }
    }
}

impl<S: tokio::io::AsyncRead + Unpin> tokio::io::AsyncRead for FirstByteDeadline<S> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<io::Result<()>> {
        use std::task::Poll;
        let before = buf.filled().len();
        let r = Pin::new(&mut self.inner).poll_read(cx, buf);
        match r {
            Poll::Ready(Ok(())) => {
                if buf.filled().len() > before {
                    self.deadline = None;
                }
                Poll::Ready(Ok(()))
            }
            Poll::Pending => {
                let limit = self.limit;
                if let Some(d) = self.deadline.as_mut() {
                    if d.as_mut().poll(cx).is_ready() {
                        return Poll::Ready(Err(io::Error::new(
                            io::ErrorKind::TimedOut,
                            format!(
                                "no answer from the server within {} s — is this an SSH port?",
                                limit.as_secs()
                            ),
                        )));
                    }
                }
                Poll::Pending
            }
            other => other,
        }
    }
}

impl<S: tokio::io::AsyncWrite + Unpin> tokio::io::AsyncWrite for FirstByteDeadline<S> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> std::task::Poll<io::Result<usize>> {
        Pin::new(&mut self.inner).poll_write(cx, buf)
    }
    fn poll_flush(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_flush(cx)
    }
    fn poll_shutdown(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }
    fn poll_write_vectored(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        bufs: &[io::IoSlice<'_>],
    ) -> std::task::Poll<io::Result<usize>> {
        Pin::new(&mut self.inner).poll_write_vectored(cx, bufs)
    }
    fn is_write_vectored(&self) -> bool {
        self.inner.is_write_vectored()
    }
}

/// Load a private key off the async workers (encrypted keys run a slow KDF).
async fn load_key(
    path: &Path,
    passphrase: Option<String>,
) -> Result<russh::keys::PrivateKey, russh::keys::Error> {
    let path = path.to_path_buf();
    tokio::task::spawn_blocking(move || load_secret_key(&path, passphrase.as_deref()))
        .await
        .unwrap_or(Err(russh::keys::Error::KeyIsCorrupt))
}

/// Perform SSH handshake + auth over an already-connected (bypass-bound) stream.
pub async fn connect_and_auth(
    stream: TcpStream,
    opts: SessionOptions<'_>,
) -> Result<(client::Handle<ClientHandler>, Option<PublicKey>)> {
    let config = client::Config {
        inactivity_timeout: Some(Duration::from_secs(300)),
        keepalive_interval: Some(Duration::from_secs(30)),
        // A larger receive window keeps fast / far links busy (downloads).
        window_size: 16 * 1024 * 1024,
        ..Default::default()
    };
    let config = Arc::new(config);
    let server_key = Arc::new(std::sync::Mutex::new(None));
    let handler = ClientHandler {
        server_key: Arc::clone(&server_key),
        host_key_verifier: opts.host_key_verifier,
    };

    // A wrong port (or a middlebox that accepts TCP and stays silent) would
    // otherwise hang the handshake forever: the first bytes from the server
    // must come in time. (Only after we have sent our own greeting, so
    // protocol multiplexers that wait for the client still work; and not a
    // limit on the whole handshake, which includes the host-key question.)
    let stream = FirstByteDeadline::new(stream, BANNER_TIMEOUT);
    let mut handle = client::connect_stream(config, stream, handler)
        .await
        .context("SSH handshake failed")?;

    let mut authed = false;
    let mut key_error: Option<String> = None;
    if let Some(id_path) = opts.identity {
        let mut pass = opts.key_passphrase.map(str::to_string);
        let mut tries = 0;
        let key = loop {
            match load_key(id_path, pass.clone()).await {
                Ok(k) => break Some(k),
                // CLI: ask for the passphrase (again, if it was wrong).
                Err(e)
                    if opts.ask_password
                        && tries < 3
                        && key_may_need_passphrase(&e, pass.is_some()) =>
                {
                    tries += 1;
                    let prompt = format!("passphrase for {}: ", id_path.display());
                    let p = tokio::task::spawn_blocking(move || rpassword::prompt_password(prompt))
                        .await
                        .map_err(|e| anyhow::anyhow!("{e}"))?
                        .context("read passphrase")?;
                    pass = Some(p);
                }
                Err(e) => {
                    // Don't give up on the connection: password / keyboard-
                    // interactive may still work. Remember why the key failed.
                    let why = match e {
                        russh::keys::Error::KeyIsEncrypted => {
                            "the key is encrypted — enter its passphrase".to_string()
                        }
                        other => other.to_string(),
                    };
                    eprintln!("[auth] can't use key {}: {why}", id_path.display());
                    key_error = Some(format!("private key {}: {why}", id_path.display()));
                    break None;
                }
            }
        };
        if let Some(key) = key {
            let hash = handle.best_supported_rsa_hash().await?.flatten();
            let auth_res = handle
                .authenticate_publickey(opts.user, PrivateKeyWithHashAlg::new(Arc::new(key), hash))
                .await
                .context("publickey auth")?;
            if auth_res.success() {
                authed = true;
                eprintln!("[auth] publickey OK ({})", id_path.display());
            } else {
                eprintln!(
                    "[auth] publickey rejected for {} — will try password if available",
                    id_path.display()
                );
            }
        }
    }

    // Resolve password once: GUI passes it; CLI may prompt.
    let provided_password = opts
        .password
        .map(|s| s.to_string())
        .filter(|s| !s.is_empty());
    let can_try_password = provided_password.is_some() || opts.ask_password;
    let mut unanswered = false;

    if !authed && can_try_password {
        // Typed at the prompt (CLI): a wrong one may be typed again.
        let typed = provided_password.is_none();
        let mut tries = 0;
        // The server takes passwords and said no to this one.
        let mut rejected = false;
        let password = loop {
            let password = if let Some(p) = provided_password.clone() {
                p
            } else {
                let prompt = format!("{}@{}'s password: ", opts.user, opts.host);
                tokio::task::spawn_blocking(move || rpassword::prompt_password(prompt))
                    .await
                    .map_err(|e| anyhow::anyhow!("{e}"))?
                    .context("read password")?
            };
            if password.is_empty() {
                break password;
            }
            tries += 1;
            match handle
                .authenticate_password(opts.user, &password)
                .await
                .context("password auth")?
            {
                client::AuthResult::Success => {
                    authed = true;
                    eprintln!("[auth] password OK");
                    break password;
                }
                client::AuthResult::Failure {
                    remaining_methods, ..
                } => {
                    rejected = remaining_methods.contains(&russh::MethodKind::Password);
                    if rejected && typed && tries < 3 {
                        eprintln!("Permission denied, please try again.");
                        continue;
                    }
                    eprintln!("[auth] password rejected — will try keyboard-interactive");
                    break password;
                }
            }
        };

        // keyboard-interactive (PAM; also one-time codes / 2FA). An empty
        // password at the prompt means "skip password, go interactive".
        if !authed {
            // The password answers the first hidden prompt only; any further
            // hidden prompt (a verification code…) is asked for. A password
            // the user typed and the server already refused is not sent
            // again: the prompt asks instead.
            let mut password_used = password.is_empty() || (rejected && typed);
            let mut kbd = handle
                .authenticate_keyboard_interactive_start(opts.user, None)
                .await
                .context("keyboard-interactive start")?;
            loop {
                match kbd {
                    KeyboardInteractiveAuthResponse::Success => {
                        authed = true;
                        eprintln!("[auth] keyboard-interactive OK");
                        break;
                    }
                    KeyboardInteractiveAuthResponse::Failure { .. } => break,
                    KeyboardInteractiveAuthResponse::InfoRequest {
                        name,
                        instructions,
                        prompts,
                    } => {
                        if !name.is_empty() {
                            eprintln!("[auth] {name}");
                        }
                        if !instructions.is_empty() {
                            eprintln!("{instructions}");
                        }
                        let mut answers = Vec::with_capacity(prompts.len());
                        for prompt in &prompts {
                            let ans = if !prompt.echo && !password_used {
                                password_used = true;
                                password.clone()
                            } else if opts.ask_password {
                                let (text, echo) = (prompt.prompt.clone(), prompt.echo);
                                tokio::task::spawn_blocking(move || -> io::Result<String> {
                                    if echo {
                                        eprint!("{text}");
                                        let _ = io::stderr().flush();
                                        let mut line = String::new();
                                        io::stdin().read_line(&mut line)?;
                                        Ok(line.trim_end_matches(['\r', '\n']).to_string())
                                    } else {
                                        rpassword::prompt_password(&text)
                                    }
                                })
                                .await
                                .map_err(|e| anyhow::anyhow!("{e}"))??
                            } else {
                                // GUI: nothing to ask with (yet).
                                unanswered = true;
                                String::new()
                            };
                            answers.push(ans);
                        }
                        kbd = handle
                            .authenticate_keyboard_interactive_respond(answers)
                            .await?;
                    }
                }
            }
        }
    }

    if !authed {
        if unanswered {
            bail!(
                "the server asked for more than a password (e.g. a verification code); \
                 use a key, or the command-line client which can ask for it"
            );
        }
        match key_error {
            Some(k) => {
                bail!("authentication failed ({k}; password / keyboard-interactive also failed)")
            }
            None => {
                bail!("authentication failed (tried publickey / password / keyboard-interactive)")
            }
        }
    }

    let key = server_key.lock().ok().and_then(|g| g.clone());
    Ok((handle, key))
}

/// Could asking for a (new) passphrase help with this key error?
fn key_may_need_passphrase(e: &russh::keys::Error, had_passphrase: bool) -> bool {
    match e {
        russh::keys::Error::KeyIsEncrypted => true,
        // A wrong passphrase shows up as a decoding failure.
        russh::keys::Error::IO(_) => false,
        _ => had_passphrase,
    }
}

/// Open a PTY + login shell channel (no local TTY coupling) — used by GUI.
pub async fn open_pty_shell(
    handle: &client::Handle<ClientHandler>,
    cols: u32,
    rows: u32,
    term: &str,
) -> Result<Channel<Msg>> {
    let channel = handle.channel_open_session().await?;

    channel
        .request_pty(
            false,
            term,
            cols,
            rows,
            0,
            0,
            &[
                (Pty::ECHO, 1),
                (Pty::ICANON, 1),
                (Pty::ISIG, 1),
                (Pty::ICRNL, 1),
                (Pty::OPOST, 1),
                (Pty::ONLCR, 1),
                (Pty::CS8, 1),
            ],
        )
        .await
        .context("request_pty")?;

    channel.request_shell(true).await.context("request_shell")?;
    Ok(channel)
}

/// GUI / non-TTY input to the remote PTY channel.
#[derive(Debug)]
pub enum ChannelInput {
    /// Raw bytes (character mode): Ctrl+C=0x03, Ctrl+D=0x04, CSI sequences, UTF-8 text.
    Data(Vec<u8>),
    /// SSH window-change (cols × rows).
    Resize { cols: u32, rows: u32 },
}

/// How long to keep reading after the remote side reported its exit (or
/// EOF) while waiting for the channel to close: the last output may still
/// be in flight.
const DRAIN_AFTER_EXIT: Duration = Duration::from_secs(2);

/// Terminal output on its way to the UI. Never blocks the reader: russh
/// processes every channel of a connection in one loop, so a reader waiting
/// on a busy UI would stall SFTP, port forwards and keepalives too. Instead
/// the amount queued is capped; beyond it output is skipped (and counted, so
/// the UI can say so) — that only happens if the UI stalls for a long time
/// while a lot of output arrives.
pub struct OutputSink {
    tx: mpsc::UnboundedSender<bytes::Bytes>,
    queued: Arc<std::sync::atomic::AtomicUsize>,
    dropped: Arc<std::sync::atomic::AtomicU64>,
    /// A gap marker is queued and not yet seen by the UI.
    gap_open: Arc<std::sync::atomic::AtomicBool>,
}

/// UI side of [`OutputSink`].
pub struct OutputSource {
    rx: mpsc::UnboundedReceiver<bytes::Bytes>,
    queued: Arc<std::sync::atomic::AtomicUsize>,
    dropped: Arc<std::sync::atomic::AtomicU64>,
    gap_open: Arc<std::sync::atomic::AtomicBool>,
}

/// One item from [`OutputSource::try_recv`].
pub enum OutputItem {
    Data(bytes::Bytes),
    /// Output was skipped here (this many bytes): the UI fell too far behind.
    Gap(u64),
}

/// Most output kept waiting for the UI.
const OUTPUT_QUEUE_CAP: usize = 64 * 1024 * 1024;

pub fn output_channel() -> (OutputSink, OutputSource) {
    let (tx, rx) = mpsc::unbounded_channel();
    let queued = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let dropped = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let gap_open = Arc::new(std::sync::atomic::AtomicBool::new(false));
    (
        OutputSink {
            tx,
            queued: Arc::clone(&queued),
            dropped: Arc::clone(&dropped),
            gap_open: Arc::clone(&gap_open),
        },
        OutputSource {
            rx,
            queued,
            dropped,
            gap_open,
        },
    )
}

impl OutputSink {
    /// Queue output; false once the UI side is gone.
    fn push(&self, data: &bytes::Bytes) -> bool {
        use std::sync::atomic::Ordering;
        if data.is_empty() {
            // (An empty item is the gap marker; empty packets carry nothing.)
            return true;
        }
        if self.queued.load(Ordering::Relaxed) + data.len() > OUTPUT_QUEUE_CAP {
            self.dropped.fetch_add(data.len() as u64, Ordering::Relaxed);
            // Mark where the gap is (once per gap): an empty item.
            if !self.gap_open.swap(true, Ordering::Relaxed) {
                return self.tx.send(bytes::Bytes::new()).is_ok();
            }
            return !self.tx.is_closed();
        }
        self.queued.fetch_add(data.len(), Ordering::Relaxed);
        // `Bytes` is reference counted: no copy of the packet.
        self.tx.send(data.clone()).is_ok()
    }
}

impl OutputSource {
    pub fn has_pending(&self) -> bool {
        !self.rx.is_empty()
    }
    pub fn try_recv(&mut self) -> Option<OutputItem> {
        use std::sync::atomic::Ordering;
        let d = self.rx.try_recv().ok()?;
        if d.is_empty() {
            // Everything dropped up to now belongs to this gap.
            self.gap_open.store(false, Ordering::Relaxed);
            return Some(OutputItem::Gap(self.dropped.swap(0, Ordering::Relaxed)));
        }
        self.queued.fetch_sub(d.len(), Ordering::Relaxed);
        Some(OutputItem::Data(d))
    }
}

/// Drive a channel with mpsc I/O (GUI / non-TTY). Returns exit status.
///
/// Input is written by a separate task, so a large paste waiting for the
/// server's window never stops output from being read; output goes to an
/// [`OutputSink`], which never blocks either.
pub async fn drive_channel_io(
    channel: Channel<Msg>,
    handle: Arc<client::Handle<ClientHandler>>,
    mut input_rx: mpsc::UnboundedReceiver<ChannelInput>,
    output: OutputSink,
    mut stop_rx: tokio::sync::oneshot::Receiver<()>,
    wake: impl Fn() + Send + 'static,
) -> Result<u32> {
    let (mut read, write) = channel.split();
    let write = Arc::new(write);
    let writer = {
        let write = Arc::clone(&write);
        tokio::spawn(async move {
            while let Some(m) = input_rx.recv().await {
                match m {
                    ChannelInput::Data(data) => {
                        if write.data_bytes(data).await.is_err() {
                            return;
                        }
                    }
                    ChannelInput::Resize { cols, rows } => {
                        let _ = write.window_change(cols, rows, 0, 0).await;
                    }
                }
            }
            let _ = write.eof().await;
        })
    };

    let mut exit_status = 0u32;
    // Set once the remote reported exit / EOF: read on until Close or this.
    let mut drain_until: Option<tokio::time::Instant> = None;
    loop {
        let deadline = async {
            match drain_until {
                Some(t) => tokio::time::sleep_until(t).await,
                None => std::future::pending::<()>().await,
            }
        };
        tokio::select! {
            _ = &mut stop_rx => {
                // Stop the writer first, so no data can follow our EOF.
                writer.abort();
                let _ = tokio::time::timeout(Duration::from_secs(2), write.eof()).await;
                break;
            }
            _ = deadline => break,
            msg = read.wait() => {
                match msg {
                    Some(ChannelMsg::Data { ref data })
                    | Some(ChannelMsg::ExtendedData { ref data, .. }) => {
                        if !output.push(data) {
                            break;
                        }
                        wake();
                    }
                    Some(ChannelMsg::ExitStatus { exit_status: code }) => {
                        exit_status = code;
                        drain_until.get_or_insert(tokio::time::Instant::now() + DRAIN_AFTER_EXIT);
                    }
                    Some(ChannelMsg::ExitSignal { .. }) => {
                        if exit_status == 0 {
                            exit_status = 255;
                        }
                        drain_until.get_or_insert(tokio::time::Instant::now() + DRAIN_AFTER_EXIT);
                    }
                    Some(ChannelMsg::Eof) => {
                        drain_until.get_or_insert(tokio::time::Instant::now() + DRAIN_AFTER_EXIT);
                    }
                    Some(ChannelMsg::Close) | None => break,
                    _ => {}
                }
            }
        }
    }
    writer.abort();
    let _ = tokio::time::timeout(
        Duration::from_secs(2),
        handle.disconnect(Disconnect::ByApplication, "bye", "en"),
    )
    .await;
    Ok(exit_status)
}

/// Snapshot of remote host resource usage (Linux /proc + df).
#[derive(Debug, Clone, Default)]
pub struct RemoteStats {
    /// CPU busy percent 0..=100 (None if unknown / first sample).
    pub cpu_percent: Option<f32>,
    pub mem_total_bytes: Option<u64>,
    pub mem_available_bytes: Option<u64>,
    pub disk_total_bytes: Option<u64>,
    pub disk_used_bytes: Option<u64>,
    pub disk_mount: Option<String>,
    pub hostname: Option<String>,
    pub kernel: Option<String>,
    pub load_avg: Option<[f32; 3]>,
    pub uptime_secs: Option<u64>,
    pub cpu_cores: Option<u32>,
    /// Cumulative bytes received / sent on non-loopback interfaces.
    pub net_rx_bytes: Option<u64>,
    pub net_tx_bytes: Option<u64>,
    pub raw_error: Option<String>,
    /// Internal: previous /proc/stat totals for delta CPU.
    pub(crate) prev_cpu_total: Option<u64>,
    pub(crate) prev_cpu_idle: Option<u64>,
}

impl RemoteStats {
    pub fn mem_used_bytes(&self) -> Option<u64> {
        match (self.mem_total_bytes, self.mem_available_bytes) {
            (Some(t), Some(a)) if t >= a => Some(t - a),
            _ => None,
        }
    }

    pub fn mem_percent(&self) -> Option<f32> {
        match (self.mem_used_bytes(), self.mem_total_bytes) {
            (Some(u), Some(t)) if t > 0 => Some((u as f32 / t as f32) * 100.0),
            _ => None,
        }
    }

    pub fn disk_percent(&self) -> Option<f32> {
        match (self.disk_used_bytes, self.disk_total_bytes) {
            (Some(u), Some(t)) if t > 0 => Some((u as f32 / t as f32) * 100.0),
            _ => None,
        }
    }
}

/// Run a remote command via a short-lived exec channel; return combined stdout/stderr.
pub async fn exec_capture(
    handle: &Arc<client::Handle<ClientHandler>>,
    command: &str,
) -> Result<String> {
    /// Small helper commands only: never wait or buffer without bound.
    const LIMIT: usize = 1024 * 1024;
    const TIMEOUT: Duration = Duration::from_secs(15);
    let mut channel = match tokio::time::timeout(TIMEOUT, handle.channel_open_session()).await {
        Ok(Ok(c)) => c,
        Ok(Err(e)) => {
            note_exec(handle, ExecOutcome::Refused);
            return Err(anyhow::Error::new(e).context("open session for exec"));
        }
        Err(_) => {
            note_exec(handle, ExecOutcome::TimedOut);
            bail!("remote command timed out");
        }
    };
    // Whether the server has closed the channel (else we must, or it stays
    // open on the server and counts against its session limit).
    let mut closed = false;
    let run = async {
        channel
            .exec(true, command)
            .await
            .context("exec remote command")?;
        // Nothing to send: a forced command that waits for input (an
        // SFTP-only account runs the SFTP server here) ends at once instead
        // of hanging until the timeout.
        let _ = channel.eof().await;
        let mut out = Vec::new();
        loop {
            match channel.wait().await {
                Some(ChannelMsg::Data { ref data })
                | Some(ChannelMsg::ExtendedData { ref data, .. }) => {
                    let room = LIMIT.saturating_sub(out.len());
                    out.extend_from_slice(&data[..data.len().min(room)]);
                    if out.len() >= LIMIT {
                        break;
                    }
                }
                // The server refused to run it.
                Some(ChannelMsg::Failure) => bail!("the server refused to run the command"),
                Some(ChannelMsg::Close) | None => {
                    closed = true;
                    break;
                }
                // EOF ends output, but does not close the SSH channel. Some
                // servers wait for our Close before releasing the session.
                Some(ChannelMsg::Eof) => {
                    break;
                }
                _ => {}
            }
        }
        Ok(String::from_utf8_lossy(&out).into_owned())
    };
    let r = tokio::time::timeout(TIMEOUT, run).await;
    if !closed {
        let _ = tokio::time::timeout(Duration::from_secs(2), channel.close()).await;
    }
    let r = match r {
        Ok(r) => r,
        Err(_) => {
            note_exec(handle, ExecOutcome::TimedOut);
            bail!("remote command timed out");
        }
    };
    note_exec(
        handle,
        if r.is_ok() {
            ExecOutcome::Ran
        } else {
            ExecOutcome::Refused
        },
    );
    r
}

/// How running a command on a connection went (see [`exec_works`]).
#[derive(Clone, Copy, PartialEq, Eq)]
enum ExecOutcome {
    Ran,
    /// Didn't answer in time: maybe just a busy server, so it takes two in
    /// a row to count as broken.
    TimedOut,
    /// Refused, or not a shell at all (an SFTP-only account, a forced
    /// command): broken right away.
    Refused,
}

/// Strikes against running commands, per connection. Held by a weak
/// reference: an entry never outlives its connection, and a later
/// connection can never inherit it.
#[allow(clippy::type_complexity)]
static EXEC_STRIKES: std::sync::Mutex<Vec<(std::sync::Weak<client::Handle<ClientHandler>>, u8)>> =
    std::sync::Mutex::new(Vec::new());

fn note_exec(handle: &Arc<client::Handle<ClientHandler>>, outcome: ExecOutcome) {
    let mut v = EXEC_STRIKES.lock().unwrap_or_else(|e| e.into_inner());
    v.retain(|(w, _)| w.strong_count() > 0);
    let pos = v
        .iter()
        .position(|(w, _)| std::ptr::eq(w.as_ptr(), Arc::as_ptr(handle)));
    match (outcome, pos) {
        // It works (again): helpers may use it, the monitor polls normally.
        (ExecOutcome::Ran, Some(i)) => {
            v.swap_remove(i);
        }
        (ExecOutcome::Ran, None) => {}
        (o, Some(i)) => {
            v[i].1 = if o == ExecOutcome::Refused {
                2
            } else {
                v[i].1.saturating_add(1)
            };
        }
        (o, None) => v.push((
            Arc::downgrade(handle),
            if o == ExecOutcome::Refused { 2 } else { 1 },
        )),
    }
}

/// `false` while commands don't run on this connection (refused, not a
/// shell, or timed out twice in a row). The monitor keeps trying now and
/// then, and the first command that works clears it.
pub fn exec_works(handle: &Arc<client::Handle<ClientHandler>>) -> bool {
    let mut v = EXEC_STRIKES.lock().unwrap_or_else(|e| e.into_inner());
    v.retain(|(w, _)| w.strong_count() > 0);
    !v.iter()
        .any(|(w, n)| std::ptr::eq(w.as_ptr(), Arc::as_ptr(handle)) && *n >= 2)
}

/// Probe remote Linux host for CPU / memory / disk. Updates `prev` CPU counters in-place.
/// One round trip for the monitor. `with_static`: also host name, kernel
/// and core count (they don't change, so only the first poll asks).
pub async fn poll_remote_stats(
    handle: &Arc<client::Handle<ClientHandler>>,
    prev: &mut RemoteStats,
    with_static: bool,
) -> RemoteStats {
    const CMD: &str = "echo '__CPU__'; head -n1 /proc/stat 2>/dev/null; \
echo '__MEM__'; grep -E '^(MemTotal|MemAvailable|MemFree|Buffers|Cached):' /proc/meminfo 2>/dev/null; \
echo '__DISK__'; df -Pk / 2>/dev/null | awk 'NR==2 {print}'; \
echo '__SYS__'; \
echo \"load=$(cut -d' ' -f1-3 /proc/loadavg 2>/dev/null)\"; echo \"up=$(cut -d' ' -f1 /proc/uptime 2>/dev/null)\"; \
echo '__NET__'; tail -n +3 /proc/net/dev 2>/dev/null";
    const STATIC: &str = "echo '__SYS__'; \
echo \"host=$(hostname 2>/dev/null || uname -n 2>/dev/null || cat /proc/sys/kernel/hostname 2>/dev/null)\"; \
echo \"kern=$(uname -sr 2>/dev/null)\"; \
echo \"cores=$(nproc 2>/dev/null || getconf _NPROCESSORS_ONLN 2>/dev/null)\"; ";
    let cmd = if with_static {
        format!("{STATIC}{CMD}")
    } else {
        CMD.to_string()
    };

    let mut next = RemoteStats {
        prev_cpu_total: prev.prev_cpu_total,
        prev_cpu_idle: prev.prev_cpu_idle,
        ..Default::default()
    };

    let text = match exec_capture(handle, &cmd).await {
        Ok(t) => t,
        Err(e) => {
            next.raw_error = Some(format!("{e:#}"));
            return next;
        }
    };
    if !text.contains("__CPU__") {
        // Our own `echo` didn't come back: the account doesn't run commands
        // (a forced command / SFTP only). Poll rarely from now on.
        note_exec(handle, ExecOutcome::Refused);
        next.raw_error = Some("the server doesn't run commands for this account".into());
        return next;
    }

    let mut section = "";
    let mut cpu_line = None::<String>;
    let mut mem_total = None::<u64>;
    let mut mem_avail = None::<u64>;
    let mut mem_free = None::<u64>;
    let mut mem_buffers = None::<u64>;
    let mut mem_cached = None::<u64>;
    let mut disk_line = None::<String>;

    for line in text.lines() {
        let line = line.trim();
        if line == "__CPU__" {
            section = "cpu";
            continue;
        }
        if line == "__MEM__" {
            section = "mem";
            continue;
        }
        if line == "__DISK__" {
            section = "disk";
            continue;
        }
        if line == "__SYS__" {
            section = "sys";
            continue;
        }
        if line == "__NET__" {
            section = "net";
            continue;
        }
        match section {
            "cpu" if cpu_line.is_none() && line.starts_with("cpu ") => {
                cpu_line = Some(line.to_string());
            }
            "mem" => {
                if let Some((k, rest)) = line.split_once(':') {
                    let num: u64 = rest
                        .split_whitespace()
                        .next()
                        .and_then(|s| s.parse::<u64>().ok())
                        .unwrap_or(0)
                        .saturating_mul(1024); // kB → bytes
                    match k {
                        "MemTotal" => mem_total = Some(num),
                        "MemAvailable" => mem_avail = Some(num),
                        "MemFree" => mem_free = Some(num),
                        "Buffers" => mem_buffers = Some(num),
                        "Cached" => mem_cached = Some(num),
                        _ => {}
                    }
                }
            }
            "disk" if disk_line.is_none() && !line.is_empty() => {
                disk_line = Some(line.to_string());
            }
            "sys" => {
                if let Some((k, v)) = line.split_once('=') {
                    let v = v.trim();
                    if v.is_empty() {
                        continue;
                    }
                    match k {
                        "host" => next.hostname = Some(v.to_string()),
                        "kern" => next.kernel = Some(v.to_string()),
                        "load" => {
                            let l: Vec<f32> = v
                                .split_whitespace()
                                .filter_map(|x| x.parse().ok())
                                .collect();
                            if l.len() == 3 {
                                next.load_avg = Some([l[0], l[1], l[2]]);
                            }
                        }
                        "up" => next.uptime_secs = v.parse::<f64>().ok().map(|x| x as u64),
                        "cores" => next.cpu_cores = v.parse().ok(),
                        _ => {}
                    }
                }
            }
            "net" => {
                if let Some((iface, rest)) = line.split_once(':') {
                    let iface = iface.trim();
                    if iface == "lo" {
                        continue;
                    }
                    let nums: Vec<u64> = rest
                        .split_whitespace()
                        .filter_map(|x| x.parse().ok())
                        .collect();
                    if nums.len() >= 9 {
                        // Untrusted numbers from the server: never overflow.
                        let rx = next.net_rx_bytes.get_or_insert(0);
                        *rx = rx.saturating_add(nums[0]);
                        let tx = next.net_tx_bytes.get_or_insert(0);
                        *tx = tx.saturating_add(nums[8]);
                    }
                }
            }
            _ => {}
        }
    }

    // CPU delta
    if let Some(ref cl) = cpu_line {
        let parts: Vec<u64> = cl
            .split_whitespace()
            .skip(1)
            .filter_map(|s| s.parse().ok())
            .collect();
        if parts.len() >= 4 {
            let idle = parts[3].saturating_add(parts.get(4).copied().unwrap_or(0)); // idle + iowait
            let total: u64 = parts.iter().fold(0u64, |a, b| a.saturating_add(*b));
            if let (Some(pt), Some(pi)) = (prev.prev_cpu_total, prev.prev_cpu_idle) {
                let dt = total.saturating_sub(pt);
                let di = idle.saturating_sub(pi);
                if dt > 0 {
                    let busy = 1.0 - (di as f32 / dt as f32);
                    next.cpu_percent = Some((busy * 100.0).clamp(0.0, 100.0));
                }
            }
            next.prev_cpu_total = Some(total);
            next.prev_cpu_idle = Some(idle);
        }
    }

    next.mem_total_bytes = mem_total;
    next.mem_available_bytes = mem_avail.or_else(|| match (mem_free, mem_buffers, mem_cached) {
        (Some(f), b, c) => Some(
            f.saturating_add(b.unwrap_or(0))
                .saturating_add(c.unwrap_or(0)),
        ),
        _ => None,
    });

    if let Some(ref dl) = disk_line {
        // Filesystem 1024-blocks Used Available Capacity Mounted
        let cols: Vec<&str> = dl.split_whitespace().collect();
        if cols.len() >= 6 {
            next.disk_total_bytes = cols[1].parse::<u64>().ok().map(|k| k.saturating_mul(1024));
            next.disk_used_bytes = cols[2].parse::<u64>().ok().map(|k| k.saturating_mul(1024));
            next.disk_mount = Some(cols[5].to_string());
        }
    }

    if next.cpu_percent.is_none()
        && next.mem_total_bytes.is_none()
        && next.disk_total_bytes.is_none()
    {
        next.raw_error = Some("could not parse remote stats (non-Linux host?)".into());
    }

    next
}

/// One entry from a remote directory listing.
#[cfg(feature = "gui")]
#[derive(Debug, Clone)]
pub struct RemoteDirEntry {
    pub name: String,
    pub is_dir: bool,
    pub is_link: bool,
    pub size: Option<u64>,
    pub mode: String,
    pub permissions: Option<u32>,
    pub link_target: Option<String>,
    /// Modification time (unix seconds) when the server reports it.
    pub mtime: Option<u64>,
}

#[cfg(feature = "gui")]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TransferProgress {
    pub done: u64,
    pub total: u64,
    /// When `Some(n)`, remote folders are still being enumerated and `n`
    /// files (`total` bytes) have been discovered so far.
    pub scanning_files: Option<usize>,
}

// ---------------------------------------------------------------------------
// SFTP
//
// One SFTP session is kept per SSH connection and shared by every listing /
// file operation (opening a subsystem costs several round trips). It is
// dropped and re-opened when it fails at the transport level.
// ---------------------------------------------------------------------------

#[cfg(feature = "gui")]
type Sftp = russh_sftp::client::SftpSession;

#[cfg(feature = "gui")]
#[allow(clippy::type_complexity)]
static SFTP_CACHE: std::sync::Mutex<
    Vec<(
        std::sync::Weak<SshHandle>,
        Arc<tokio::sync::OnceCell<Arc<Sftp>>>,
    )>,
> = std::sync::Mutex::new(Vec::new());

#[cfg(feature = "gui")]
async fn open_subsystem(handle: &SshHandle) -> Result<russh::ChannelStream<client::Msg>> {
    let channel = handle.channel_open_session().await?;
    channel.request_subsystem(true, "sftp").await?;
    Ok(channel.into_stream())
}

#[cfg(feature = "gui")]
async fn sftp_session(handle: &Arc<SshHandle>) -> Result<Arc<Sftp>> {
    let cell = {
        let mut cache = SFTP_CACHE.lock().unwrap_or_else(|e| e.into_inner());
        cache.retain(|(w, _)| w.strong_count() > 0);
        if let Some((_, s)) = cache
            .iter()
            .find(|(w, _)| std::ptr::eq(w.as_ptr(), Arc::as_ptr(handle)))
        {
            Arc::clone(s)
        } else {
            let cell = Arc::new(tokio::sync::OnceCell::new());
            cache.push((Arc::downgrade(handle), Arc::clone(&cell)));
            cell
        }
    };
    cell.get_or_try_init(|| async {
        let cfg = russh_sftp::client::Config {
            // More writes in flight = faster uploads on high-latency links.
            max_concurrent_writes: 32,
            // With many requests in flight a slow link needs time to answer
            // them all (dead connections are caught by SSH keepalives).
            request_timeout_secs: SFTP_TIMEOUT_SECS,
            ..Default::default()
        };
        let sftp = Arc::new(
            Sftp::new_with_config(open_subsystem(handle).await?, cfg)
                .await
                .context("start SFTP subsystem")?,
        );
        Ok(sftp)
    })
    .await
    .cloned()
}

#[cfg(feature = "gui")]
fn sftp_forget(handle: &Arc<SshHandle>) {
    SFTP_CACHE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .retain(|(w, _)| !std::ptr::eq(w.as_ptr(), Arc::as_ptr(handle)));
}

/// True when an error means the SFTP channel itself is broken (as opposed to
/// a normal server answer such as "no such file").
#[cfg(feature = "gui")]
fn is_transport_error(e: &anyhow::Error) -> bool {
    match e.downcast_ref::<russh_sftp::client::error::Error>() {
        Some(russh_sftp::client::error::Error::Status(_)) => false,
        Some(_) => true,
        None => e.downcast_ref::<russh::Error>().is_some(),
    }
}

/// Run `op` on the shared SFTP session; if the session turned out to be dead,
/// open a fresh one and try once more.
#[cfg(feature = "gui")]
async fn with_sftp<T, F, Fut>(handle: &Arc<SshHandle>, op: F) -> Result<T>
where
    F: Fn(Arc<Sftp>) -> Fut,
    Fut: std::future::Future<Output = Result<T>>,
{
    let sftp = sftp_session(handle).await?;
    match op(sftp).await {
        Err(e) if is_transport_error(&e) => {
            sftp_forget(handle);
            let sftp = sftp_session(handle).await?;
            op(sftp).await
        }
        r => r,
    }
}

/// Like [`with_sftp`] for operations that change something (create, rename,
/// delete, mkdir): those are never sent twice — a retry after a dropped
/// connection could report a misleading "already exists" for what the first
/// attempt did. Instead the cached session is checked with a harmless request
/// first and replaced if it has gone stale.
#[cfg(feature = "gui")]
async fn with_sftp_once<T, F, Fut>(handle: &Arc<SshHandle>, op: F) -> Result<T>
where
    F: FnOnce(Arc<Sftp>) -> Fut,
    Fut: std::future::Future<Output = Result<T>>,
{
    let mut sftp = sftp_session(handle).await?;
    if let Err(e) = sftp.canonicalize(".").await {
        if is_transport_error(&anyhow::Error::new(e)) {
            sftp_forget(handle);
            sftp = sftp_session(handle).await?;
        }
    }
    let r = op(sftp).await;
    if r.as_ref().is_err_and(is_transport_error) {
        sftp_forget(handle);
    }
    r
}

/// A dedicated low-level session (pipelined reads, protocol extensions).
#[cfg(feature = "gui")]
async fn raw_sftp(
    handle: &SshHandle,
) -> Result<(
    russh_sftp::client::RawSftpSession,
    russh_sftp::protocol::Version,
)> {
    let raw = russh_sftp::client::RawSftpSession::new(open_subsystem(handle).await?);
    // 16 reads of 64 KB in flight take a while on a slow link.
    raw.set_timeout(SFTP_TIMEOUT_SECS);
    let ver = raw.init().await.context("start SFTP subsystem")?;
    Ok((raw, ver))
}

/// Per-request SFTP timeout.
#[cfg(feature = "gui")]
const SFTP_TIMEOUT_SECS: u64 = 120;

#[cfg(feature = "gui")]
fn file_is_dir(meta: &russh_sftp::protocol::FileAttributes) -> bool {
    matches!(meta.file_type(), russh_sftp::protocol::FileType::Dir)
}

/// `~`, `~/x` → paths relative to the login directory (what SFTP expects).
#[cfg(feature = "gui")]
pub fn remote_home_relative(path: &str) -> String {
    // These paths also come from directory entries: whitespace is part of
    // a valid remote filename, not necessarily user input to clean up.
    let p = path;
    if p.is_empty() || p == "~" {
        ".".to_string()
    } else if let Some(rest) = p.strip_prefix("~/") {
        if rest.is_empty() {
            ".".to_string()
        } else {
            rest.to_string()
        }
    } else {
        p.to_string()
    }
}

#[cfg(feature = "gui")]
fn child_path(dir: &str, name: &str) -> String {
    if dir == "/" {
        format!("/{name}")
    } else {
        format!("{}/{name}", dir.trim_end_matches('/'))
    }
}

/// Throttle for progress messages (one per ~50 ms is plenty for a UI).
#[cfg(feature = "gui")]
struct Ticker(std::time::Instant);

#[cfg(feature = "gui")]
impl Ticker {
    fn new() -> Self {
        Self(std::time::Instant::now() - Duration::from_secs(1))
    }
    fn delayed() -> Self {
        Self(std::time::Instant::now())
    }
    fn due(&mut self) -> bool {
        if self.0.elapsed() >= Duration::from_millis(50) {
            self.0 = std::time::Instant::now();
            true
        } else {
            false
        }
    }
}

#[cfg(feature = "gui")]
pub async fn sftp_list(
    handle: &Arc<SshHandle>,
    path: &str,
) -> Result<(String, Vec<RemoteDirEntry>)> {
    let raw = remote_home_relative(path);
    with_sftp(handle, |sftp| {
        let raw = raw.clone();
        async move {
            let path = match sftp.canonicalize(raw.as_str()).await {
                Ok(p) => p,
                Err(_) if raw == "." => "/".to_string(),
                Err(e) => {
                    return Err(anyhow::Error::new(e))
                        .with_context(|| format!("sftp resolve {raw}"))
                }
            };
            let dir = sftp
                .read_dir(path.as_str())
                .await
                .with_context(|| format!("sftp list {path}"))?;
            let mut entries = Vec::new();
            if path != "/" {
                entries.push(RemoteDirEntry {
                    name: "..".into(),
                    is_dir: true,
                    is_link: false,
                    size: None,
                    mode: String::new(),
                    permissions: None,
                    link_target: None,
                    mtime: None,
                });
            }
            // Resolve symlinks concurrently instead of one round trip each.
            use futures_util::{stream, StreamExt};
            let mut items = Vec::new();
            for item in dir {
                let name = item.file_name();
                if name == "." || name == ".." {
                    continue;
                }
                let meta = item.metadata();
                items.push((name, meta));
            }
            let link_names: Vec<String> = items
                .iter()
                .filter(|(_, meta)| {
                    matches!(meta.file_type(), russh_sftp::protocol::FileType::Symlink)
                })
                .map(|(name, _)| name.clone())
                .collect();
            let mut links = stream::iter(link_names.into_iter().map(|name| {
                let s = Arc::clone(&sftp);
                let target = child_path(&path, &name);
                async move {
                    let (meta, link_target) =
                        tokio::join!(s.metadata(target.as_str()), s.read_link(target.as_str()),);
                    (name, (meta.ok(), link_target.ok()))
                }
            }))
            .buffer_unordered(16);
            let mut resolved = std::collections::HashMap::new();
            while let Some((n, pair)) = links.next().await {
                resolved.insert(n, pair);
            }
            drop(links);
            for (name, meta) in items {
                let is_link = matches!(meta.file_type(), russh_sftp::protocol::FileType::Symlink);
                let target = resolved.get(&name).and_then(|(m, _)| m.as_ref());
                let link_target = resolved.get(&name).and_then(|(_, l)| l.clone());
                let is_dir = match target {
                    Some(m) => file_is_dir(m),
                    None => file_is_dir(&meta),
                };
                // For links, show the size of what they point to.
                let size = if is_dir {
                    None
                } else {
                    Some(target.and_then(|m| m.size).or(meta.size).unwrap_or(0))
                };
                entries.push(RemoteDirEntry {
                    is_dir,
                    is_link,
                    size,
                    mode: meta
                        .permissions
                        .map(|m| format_mode(m, is_dir, is_link))
                        .unwrap_or_default(),
                    permissions: target.and_then(|m| m.permissions).or(meta.permissions),
                    link_target,
                    mtime: meta.mtime.map(u64::from),
                    name,
                });
            }
            entries.sort_by(|a, b| match (a.is_dir, b.is_dir) {
                (true, false) => std::cmp::Ordering::Less,
                (false, true) => std::cmp::Ordering::Greater,
                _ => a
                    .name
                    .to_ascii_lowercase()
                    .cmp(&b.name.to_ascii_lowercase()),
            });
            Ok((path, entries))
        }
    })
    .await
}

/// `rwxr-xr-x` style permission string.
#[cfg(feature = "gui")]
fn format_mode(mode: u32, is_dir: bool, is_link: bool) -> String {
    let mut s = String::with_capacity(10);
    s.push(if is_link {
        'l'
    } else if is_dir {
        'd'
    } else {
        '-'
    });
    for shift in [6u32, 3, 0] {
        let bits = (mode >> shift) & 0o7;
        s.push(if bits & 4 != 0 { 'r' } else { '-' });
        s.push(if bits & 2 != 0 { 'w' } else { '-' });
        s.push(if bits & 1 != 0 { 'x' } else { '-' });
    }
    s
}

#[cfg(feature = "gui")]
async fn or_cancel_any<F: std::future::Future>(
    f: F,
    a: &std::sync::atomic::AtomicBool,
    b: &std::sync::atomic::AtomicBool,
) -> Result<F::Output> {
    use std::sync::atomic::Ordering;
    tokio::pin!(f);
    loop {
        if a.load(Ordering::Relaxed) || b.load(Ordering::Relaxed) {
            return Err(anyhow::Error::new(Cancelled));
        }
        tokio::select! {
            v = &mut f => return Ok(v),
            _ = tokio::time::sleep(Duration::from_millis(100)) => {}
        }
    }
}

/// Run `f`, but give up with [`Cancelled`] soon after `cancel` is set: a
/// request stuck on a dead link would otherwise keep a cancelled transfer
/// alive until its timeout.
#[cfg(feature = "gui")]
async fn or_cancel<F: std::future::Future>(
    f: F,
    cancel: &std::sync::atomic::AtomicBool,
) -> Result<F::Output> {
    static NEVER: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    or_cancel_any(f, cancel, &NEVER).await
}

/// Progress shared by the files of one transfer (they may run in parallel).
#[cfg(feature = "gui")]
struct SharedProgress {
    done: std::sync::atomic::AtomicU64,
    total: std::sync::atomic::AtomicU64,
    tx: mpsc::UnboundedSender<TransferProgress>,
    tick: std::sync::Mutex<Ticker>,
}

#[cfg(feature = "gui")]
impl SharedProgress {
    fn new(tx: mpsc::UnboundedSender<TransferProgress>, total: u64) -> Self {
        Self {
            done: 0.into(),
            total: total.into(),
            tx,
            tick: std::sync::Mutex::new(Ticker::new()),
        }
    }
    fn add_total(&self, n: u64) {
        self.total
            .fetch_add(n, std::sync::atomic::Ordering::Relaxed);
    }
    fn add(&self, n: u64) {
        self.done.fetch_add(n, std::sync::atomic::Ordering::Relaxed);
        let due = self.tick.lock().map(|mut t| t.due()).unwrap_or(true);
        if due {
            self.send();
        }
    }
    fn send(&self) {
        use std::sync::atomic::Ordering;
        let done = self.done.load(Ordering::Relaxed);
        let total = self.total.load(Ordering::Relaxed);
        let _ = self.tx.send(TransferProgress {
            done,
            total: total.max(done),
            scanning_files: None,
        });
    }
}

/// Live discovery counter emitted while enumerating remote folders before bytes move.
#[cfg(feature = "gui")]
struct ScanProgress {
    files: std::sync::atomic::AtomicUsize,
    bytes: std::sync::atomic::AtomicU64,
    tx: mpsc::UnboundedSender<TransferProgress>,
    tick: std::sync::Mutex<Ticker>,
}

#[cfg(feature = "gui")]
impl ScanProgress {
    fn new(tx: mpsc::UnboundedSender<TransferProgress>) -> Self {
        Self {
            files: 0.into(),
            bytes: 0.into(),
            tx,
            tick: std::sync::Mutex::new(Ticker::delayed()),
        }
    }
    fn add_quiet(&self, files: usize, bytes: u64) {
        use std::sync::atomic::Ordering;
        self.files.fetch_add(files, Ordering::Relaxed);
        self.bytes.fetch_add(bytes, Ordering::Relaxed);
    }
    fn add(&self, files: usize, bytes: u64) {
        self.add_quiet(files, bytes);
        if self.tick.lock().map(|mut t| t.due()).unwrap_or(true) {
            self.send();
        }
    }
    fn send(&self) {
        use std::sync::atomic::Ordering;
        let _ = self.tx.send(TransferProgress {
            done: 0,
            total: self.bytes.load(Ordering::Relaxed),
            scanning_files: Some(self.files.load(Ordering::Relaxed)),
        });
    }
}

/// A download session: one low-level SFTP channel and the read size the
/// server allows.
#[cfg(feature = "gui")]
struct Downloader {
    raw: Arc<russh_sftp::client::RawSftpSession>,
    chunk: u32,
    depth: usize,
    /// Reads in flight on the whole session (all files together). Each
    /// request's timeout starts when it is sent, and its answer comes only
    /// after everything sent before it: too much in flight would time out
    /// on a slow link.
    inflight: Arc<tokio::sync::Semaphore>,
}

#[cfg(feature = "gui")]
impl Downloader {
    async fn open(handle: &SshHandle) -> Result<Self> {
        let (raw, ver) = raw_sftp(handle).await?;
        // Ask OpenSSH how much one read may return (usually 255 KB); bigger
        // reads mean fewer round trips. Without the extension, stay at 64 KB.
        let mut chunk = 64 * 1024u32;
        if ver.extensions.contains_key(russh_sftp::extensions::LIMITS) {
            if let Ok(l) = raw.limits().await {
                if l.max_read_len > 0 {
                    chunk = l.max_read_len.clamp(16 * 1024, 256 * 1024) as u32;
                }
            }
        }
        // About 4 MB in flight per session: fills a fast, distant link (the
        // SSH window is 16 MB) and still answers the last request within
        // the timeout down to ~15 KB/s.
        raw.set_timeout(DOWNLOAD_TIMEOUT_SECS);
        let depth = ((4 * 1024 * 1024) / chunk as usize).clamp(8, 64);
        Ok(Self {
            raw: Arc::new(raw),
            chunk,
            depth,
            inflight: Arc::new(tokio::sync::Semaphore::new(depth)),
        })
    }
}

/// Per-request timeout for download reads (see `Downloader::inflight`).
#[cfg(feature = "gui")]
const DOWNLOAD_TIMEOUT_SECS: u64 = 300;

/// Reads in flight for one file; aborted if the download ends early.
#[cfg(feature = "gui")]
type ReadJob =
    tokio::task::JoinHandle<std::result::Result<Option<Vec<u8>>, russh_sftp::client::error::Error>>;

#[cfg(feature = "gui")]
struct Inflight(std::collections::VecDeque<(u64, u32, ReadJob)>);

#[cfg(feature = "gui")]
impl Drop for Inflight {
    fn drop(&mut self) {
        for (_, _, j) in self.0.drain(..) {
            j.abort();
        }
    }
}

/// Download one regular file over `dl`. Many reads are kept in flight (much
/// faster on high-latency links); data goes to a hidden `.part` file that is
/// renamed into place only once complete, so a failed transfer never leaves
/// a truncated file.
#[cfg(feature = "gui")]
async fn download_one(
    dl: &Downloader,
    remote: &str,
    local: &Path,
    known_size: Option<u64>,
    prog: &SharedProgress,
    cancel: &std::sync::atomic::AtomicBool,
    abort: &std::sync::atomic::AtomicBool,
) -> Result<()> {
    use russh_sftp::client::error::Error as SErr;
    use russh_sftp::protocol::{FileAttributes, FileType, OpenFlags, StatusCode};

    let raw = &dl.raw;
    let remote = remote_home_relative(remote);
    let total = match known_size {
        Some(sz) => sz,
        None => {
            let attrs = or_cancel_any(raw.stat(remote.as_str()), cancel, abort)
                .await?
                .with_context(|| format!("stat {remote}"))?
                .attrs;
            match attrs.file_type() {
                FileType::File => {}
                FileType::Dir => bail!("{remote} is a folder"),
                _ => bail!("{remote} is not a regular file"),
            }
            let sz = attrs.size.unwrap_or(0);
            prog.add_total(sz);
            sz
        }
    };
    let fh = or_cancel_any(
        raw.open(remote.as_str(), OpenFlags::READ, FileAttributes::empty()),
        cancel,
        abort,
    )
    .await?
    .with_context(|| format!("open {remote}"))?
    .handle;
    // A name no other download (tab, host, process) can pick at the same time.
    let part = local.with_file_name(local_part_name(local));
    let out_file = tokio::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&part)
        .await
        .with_context(|| format!("create {}", part.display()))?;
    // Removes the partial file however this ends: error, cancel, abort or panic.
    let guard = PartFile(Some(part.clone()));

    let result: Result<()> = async {
        let mut out = out_file;
        let run: Result<()> = async {
            let read = |off: u64, len: u32| -> ReadJob {
                let raw = Arc::clone(raw);
                let fh = fh.clone();
                let gate = Arc::clone(&dl.inflight);
                tokio::spawn(async move {
                    let _slot = gate.acquire_owned().await;
                    match raw.read(fh, off, len).await {
                        Ok(d) => Ok(Some(d.data)),
                        Err(SErr::Status(s)) if s.status_code == StatusCode::Eof => Ok(None),
                        Err(e) => Err(e),
                    }
                })
            };
            let chunk = dl.chunk as u64;
            let mut queue = Inflight(std::collections::VecDeque::new());
            let mut next = 0u64;
            let mut done = 0u64;
            let mut eof = false;
            loop {
                while !eof && queue.0.len() < dl.depth && next < total {
                    let len = chunk.min(total - next) as u32;
                    queue.0.push_back((next, len, read(next, len)));
                    next += len as u64;
                }
                let Some((off, len, job)) = queue.0.pop_front() else {
                    break;
                };
                let mut data = match or_cancel_any(job, cancel, abort).await?? {
                    Ok(Some(d)) => d,
                    Ok(None) => {
                        eof = true;
                        Vec::new()
                    }
                    Err(e) => return Err(anyhow::Error::new(e)),
                };
                // A server may return less than asked: fetch the rest of this chunk.
                while !eof && (data.len() as u64) < len as u64 {
                    let got = data.len() as u64;
                    match or_cancel_any(read(off + got, (len as u64 - got) as u32), cancel, abort)
                        .await??
                    {
                        Ok(Some(d)) if !d.is_empty() => data.extend_from_slice(&d),
                        Ok(_) => eof = true,
                        Err(e) => return Err(anyhow::Error::new(e)),
                    }
                }
                out.write_all(&data).await?;
                done += data.len() as u64;
                prog.add(data.len() as u64);
                if eof {
                    break;
                }
            }
            drop(queue);
            // The file may have grown since we looked: read on until EOF.
            while !eof {
                match or_cancel_any(read(done, dl.chunk), cancel, abort).await?? {
                    Ok(Some(d)) if !d.is_empty() => {
                        out.write_all(&d).await?;
                        done += d.len() as u64;
                        if done > total {
                            prog.add_total(d.len().min((done - total) as usize) as u64);
                        }
                        prog.add(d.len() as u64);
                    }
                    Ok(_) => eof = true,
                    Err(e) => return Err(anyhow::Error::new(e)),
                }
            }
            out.flush().await?;
            Ok(())
        }
        .await;
        // Close the OS handle synchronously before rename or PartFile cleanup on Windows;
        // drop on the blocking pool so close-time antivirus scans do not stall async workers.
        let std_file = out.into_std().await;
        let _ = tokio::task::spawn_blocking(move || drop(std_file)).await;
        run
    }
    .await;
    // Don't let a dead link hold up a cancelled / failed download.
    let _ = tokio::time::timeout(Duration::from_secs(5), raw.close(fh)).await;
    if let Err(e) = result {
        let _ = tokio::fs::remove_file(&part).await;
        guard.disarm();
        return Err(e);
    }
    // Windows: a virus scanner / indexer may briefly hold the placeholder
    // open ("used by another process"); retry for a few seconds rather than
    // throw the finished download away.
    let mut tries = 0;
    loop {
        match tokio::fs::rename(&part, local).await {
            Ok(()) => break,
            Err(e) if tries < 20 && cfg!(windows) && e.kind() != std::io::ErrorKind::NotFound => {
                tries += 1;
                tokio::time::sleep(Duration::from_millis(250)).await;
            }
            Err(e) => {
                let _ = tokio::fs::remove_file(&part).await;
                guard.disarm();
                return Err(anyhow::Error::new(e))
                    .with_context(|| format!("save {}", local.display()));
            }
        }
    }
    guard.disarm();
    Ok(())
}

#[cfg(feature = "gui")]
struct DownloadJob {
    remote: String,
    local: std::path::PathBuf,
    known_size: Option<u64>,
    has_placeholder: bool,
}

#[cfg(feature = "gui")]
async fn download_jobs(
    dl: &Downloader,
    jobs: &[DownloadJob],
    prog: &SharedProgress,
    cancel: &std::sync::atomic::AtomicBool,
) -> (Vec<bool>, Result<()>) {
    use futures_util::stream::{FuturesUnordered, StreamExt};
    use std::sync::atomic::{AtomicBool, Ordering};
    const PARALLEL: usize = 4;
    let mut completed = vec![false; jobs.len()];
    let abort = AtomicBool::new(false);
    let multi = jobs.len() > 1;
    let mut result = Ok(());
    {
        let mut pending = jobs.iter().enumerate();
        let mut running = FuturesUnordered::new();
        loop {
            while !abort.load(Ordering::Relaxed)
                && !cancel.load(Ordering::Relaxed)
                && running.len() < PARALLEL
            {
                let Some((i, job)) = pending.next() else {
                    break;
                };
                let abort_ref = &abort;
                let cancel_ref = cancel;
                running.push(async move {
                    let r = download_one(
                        dl,
                        &job.remote,
                        &job.local,
                        job.known_size,
                        prog,
                        cancel_ref,
                        abort_ref,
                    )
                    .await;
                    (i, r)
                });
            }
            match running.next().await {
                None => break,
                Some((i, Ok(()))) => completed[i] = true,
                Some((i, Err(e))) => {
                    abort.store(true, Ordering::Relaxed);
                    if result.is_ok() {
                        result = Err(if multi && e.downcast_ref::<Cancelled>().is_none() {
                            e.context(format!("download {}", jobs[i].remote))
                        } else {
                            e
                        });
                    }
                }
            }
        }
    }
    (completed, result)
}

/// Download several files (`remote`, `local`) over one SFTP channel, a few
/// at a time. Stops at the first error (or cancel); returns which files
/// were completed alongside the outcome.
#[cfg(all(feature = "gui", test))]
pub async fn sftp_download_files(
    handle: &Arc<SshHandle>,
    items: Vec<(String, std::path::PathBuf)>,
    progress: mpsc::UnboundedSender<TransferProgress>,
    cancel: Arc<std::sync::atomic::AtomicBool>,
) -> (Vec<bool>, Result<()>) {
    let dl = match or_cancel(Downloader::open(handle), &cancel).await {
        Ok(Ok(d)) => d,
        Ok(Err(e)) | Err(e) => return (vec![false; items.len()], Err(e)),
    };
    let jobs: Vec<DownloadJob> = items
        .into_iter()
        .map(|(remote, local)| DownloadJob {
            remote,
            local,
            known_size: None,
            has_placeholder: false,
        })
        .collect();
    let prog = SharedProgress::new(progress, 0);
    let (completed, result) = download_jobs(&dl, &jobs, &prog, &cancel).await;
    prog.send();
    let _ = dl.raw.close_session();
    (completed, result)
}

#[cfg(feature = "gui")]
fn local_name_key(name: &str) -> String {
    if cfg!(any(windows, target_os = "macos")) {
        name.to_lowercase()
    } else {
        name.to_string()
    }
}

#[cfg(feature = "gui")]
fn claim_unique_name(
    taken: &mut std::collections::HashSet<String>,
    base: String,
    ext: &str,
) -> String {
    let first = format!("{base}{ext}");
    if taken.insert(local_name_key(&first)) {
        return first;
    }
    for i in 1usize.. {
        let cand = format!("{base} ({i}){ext}");
        if taken.insert(local_name_key(&cand)) {
            return cand;
        }
    }
    unreachable!()
}

#[cfg(feature = "gui")]
fn alloc_unique_file_name(taken: &mut std::collections::HashSet<String>, name: &str) -> String {
    let safe = local_safe_name(name);
    let (stem, ext) = match safe.rsplit_once('.') {
        Some((s, e)) if !s.is_empty() => (s.to_string(), format!(".{e}")),
        _ => (safe, String::new()),
    };
    claim_unique_name(taken, stem, &ext)
}

#[cfg(feature = "gui")]
fn alloc_unique_dir_name(taken: &mut std::collections::HashSet<String>, name: &str) -> String {
    let safe = local_safe_name(name);
    let safe = if safe == "file" && name != "file" {
        "folder".to_string()
    } else {
        safe
    };
    claim_unique_name(taken, safe, "")
}

#[cfg(feature = "gui")]
async fn reserve_local_path_async(
    dir: std::path::PathBuf,
    name: String,
) -> Result<std::path::PathBuf> {
    Ok(tokio::task::spawn_blocking(move || reserve_local_path(&dir, &name)).await??)
}

#[cfg(feature = "gui")]
async fn reserve_local_dir_async(
    dir: std::path::PathBuf,
    name: String,
) -> Result<std::path::PathBuf> {
    Ok(tokio::task::spawn_blocking(move || reserve_local_dir(&dir, &name)).await??)
}

#[cfg(feature = "gui")]
async fn cleanup_download_artifacts(
    placeholders: Vec<std::path::PathBuf>,
    dirs: Vec<std::path::PathBuf>,
) {
    if placeholders.is_empty() && dirs.is_empty() {
        return;
    }
    let _ = tokio::task::spawn_blocking(move || {
        for p in &placeholders {
            remove_placeholder(p);
        }
        for d in dirs.iter().rev() {
            let _ = std::fs::remove_dir(d);
        }
    })
    .await;
}

#[cfg(feature = "gui")]
async fn read_remote_dir(
    raw: Arc<russh_sftp::client::RawSftpSession>,
    cur_remote: String,
    cur_local: std::path::PathBuf,
    scan: Arc<ScanProgress>,
    cancel: Arc<std::sync::atomic::AtomicBool>,
) -> Result<(
    String,
    std::path::PathBuf,
    Vec<(String, russh_sftp::protocol::FileAttributes)>,
)> {
    use russh_sftp::client::error::Error as SErr;
    use russh_sftp::protocol::{FileType, StatusCode};

    if cancel.load(std::sync::atomic::Ordering::Relaxed) {
        return Err(anyhow::Error::new(Cancelled));
    }
    let dh = or_cancel(raw.opendir(cur_remote.as_str()), &cancel)
        .await?
        .with_context(|| format!("opendir {cur_remote}"))?
        .handle;
    let mut dir_entries = Vec::new();
    let read_res: Result<()> = async {
        loop {
            match or_cancel(raw.readdir(dh.as_str()), &cancel).await? {
                Ok(batch) => {
                    let mut batch_files = 0usize;
                    let mut batch_bytes = 0u64;
                    for f in batch.files {
                        if f.filename != "."
                            && f.filename != ".."
                            && !f.filename.contains(['/', '\0'])
                            && f.attrs.file_type() == FileType::File
                        {
                            batch_files += 1;
                            batch_bytes += f.attrs.size.unwrap_or(0);
                        }
                        dir_entries.push((f.filename, f.attrs));
                    }
                    if batch_files > 0 {
                        scan.add(batch_files, batch_bytes);
                    }
                }
                Err(SErr::Status(s)) if s.status_code == StatusCode::Eof => break,
                Err(e) => {
                    return Err(anyhow::Error::new(e))
                        .with_context(|| format!("readdir {cur_remote}"))
                }
            }
        }
        Ok(())
    }
    .await;
    let _ = tokio::time::timeout(Duration::from_secs(5), raw.close(dh)).await;
    read_res?;
    dir_entries.sort_by(|a, b| a.0.cmp(&b.0));
    Ok((cur_remote, cur_local, dir_entries))
}

/// Result summary for [`sftp_download_entries`].
#[cfg(feature = "gui")]
#[derive(Default, Debug)]
pub struct DownloadOutcome {
    pub saved_path: Option<std::path::PathBuf>,
    pub files_done: usize,
    pub files_total: usize,
    pub skipped: Vec<(String, String)>,
}

/// Download any mix of remote regular files and folders recursively into `dest_dir`.
/// Safe local filenames and collision-free top-level targets/folders are
/// reserved before transfer (nested targets are collision-checked in memory and
/// materialized as `.part` files when downloaded); on total failure or
/// cancellation, partial files and newly created empty folders are cleaned up.
#[cfg(feature = "gui")]
pub async fn sftp_download_entries(
    handle: &Arc<SshHandle>,
    remotes: &[String],
    dest_dir: &Path,
    progress: mpsc::UnboundedSender<TransferProgress>,
    cancel: Arc<std::sync::atomic::AtomicBool>,
) -> (DownloadOutcome, Result<()>) {
    use futures_util::stream::{FuturesUnordered, StreamExt};
    use russh_sftp::protocol::FileType;
    use std::collections::{HashSet, VecDeque};
    use std::sync::atomic::Ordering;

    const SCAN_PARALLEL: usize = 4;

    let mut outcome = DownloadOutcome::default();
    if let Err(e) = tokio::fs::create_dir_all(dest_dir).await {
        return (
            outcome,
            Err(anyhow::Error::new(e)).with_context(|| format!("create {}", dest_dir.display())),
        );
    }
    let dl = match or_cancel(Downloader::open(handle), &cancel).await {
        Ok(Ok(d)) => d,
        Ok(Err(e)) | Err(e) => return (outcome, Err(e)),
    };

    let scan = Arc::new(ScanProgress::new(progress.clone()));
    let mut jobs: Vec<DownloadJob> = Vec::new();
    let mut created_dirs: Vec<std::path::PathBuf> = Vec::new();
    let mut top_locals: Vec<(std::path::PathBuf, bool)> = Vec::new();
    let mut planned_total = 0u64;

    let walk_res: Result<()> = async {
        for raw_remote in remotes {
            if cancel.load(Ordering::Relaxed) {
                return Err(anyhow::Error::new(Cancelled));
            }
            let remote = remote_home_relative(raw_remote);
            let trimmed = remote.trim_end_matches('/');
            let name = trimmed.rsplit('/').next().unwrap_or("");
            if name.is_empty() || name == "." || name == ".." {
                bail!("invalid remote path: {raw_remote}");
            }
            let attrs = or_cancel(dl.raw.stat(remote.as_str()), &cancel)
                .await?
                .with_context(|| format!("stat {remote}"))?
                .attrs;
            match attrs.file_type() {
                FileType::File => {
                    let local =
                        reserve_local_path_async(dest_dir.to_path_buf(), name.to_string()).await?;
                    top_locals.push((local.clone(), false));
                    let sz = attrs.size.unwrap_or(0);
                    planned_total += sz;
                    if remotes.len() > 1 {
                        scan.add(1, sz);
                    } else {
                        scan.add_quiet(1, sz);
                    }
                    jobs.push(DownloadJob {
                        remote,
                        local,
                        known_size: attrs.size,
                        has_placeholder: true,
                    });
                }
                FileType::Dir => {
                    scan.send();
                    let root_local =
                        reserve_local_dir_async(dest_dir.to_path_buf(), name.to_string()).await?;
                    created_dirs.push(root_local.clone());
                    top_locals.push((root_local.clone(), true));
                    let mut queue = VecDeque::from([(remote, root_local)]);
                    let mut running = FuturesUnordered::new();
                    while !queue.is_empty() || !running.is_empty() {
                        if cancel.load(Ordering::Relaxed) {
                            return Err(anyhow::Error::new(Cancelled));
                        }
                        while running.len() < SCAN_PARALLEL {
                            let Some((cur_remote, cur_local)) = queue.pop_front() else {
                                break;
                            };
                            running.push(read_remote_dir(
                                Arc::clone(&dl.raw),
                                cur_remote,
                                cur_local,
                                Arc::clone(&scan),
                                Arc::clone(&cancel),
                            ));
                        }
                        let Some(res) = running.next().await else {
                            break;
                        };
                        let (cur_remote, cur_local, dir_entries) = res?;
                        let mut taken: HashSet<String> = HashSet::new();
                        for (child_name, cattrs) in dir_entries {
                            if cancel.load(Ordering::Relaxed) {
                                return Err(anyhow::Error::new(Cancelled));
                            }
                            if child_name == "." || child_name == ".." {
                                continue;
                            }
                            let child_remote = child_path(&cur_remote, &child_name);
                            if child_name.contains(['/', '\0']) {
                                outcome
                                    .skipped
                                    .push((child_remote, "unsafe file name".into()));
                                continue;
                            }
                            match cattrs.file_type() {
                                FileType::Dir => {
                                    let unique = alloc_unique_dir_name(&mut taken, &child_name);
                                    let cand = cur_local.join(&unique);
                                    let child_local = match tokio::fs::create_dir(&cand).await {
                                        Ok(()) => cand,
                                        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                                            let resv = reserve_local_dir_async(
                                                cur_local.clone(),
                                                child_name.clone(),
                                            )
                                            .await?;
                                            if let Some(n) =
                                                resv.file_name().and_then(|s| s.to_str())
                                            {
                                                taken.insert(local_name_key(n));
                                            }
                                            resv
                                        }
                                        Err(e) => {
                                            return Err(anyhow::Error::new(e)).with_context(|| {
                                                format!("create {}", cand.display())
                                            });
                                        }
                                    };
                                    created_dirs.push(child_local.clone());
                                    queue.push_back((child_remote, child_local));
                                }
                                FileType::File => {
                                    let unique = alloc_unique_file_name(&mut taken, &child_name);
                                    planned_total += cattrs.size.unwrap_or(0);
                                    jobs.push(DownloadJob {
                                        remote: child_remote,
                                        local: cur_local.join(unique),
                                        known_size: cattrs.size,
                                        has_placeholder: false,
                                    });
                                }
                                FileType::Symlink => {
                                    match or_cancel(dl.raw.stat(child_remote.as_str()), &cancel)
                                        .await?
                                    {
                                        Ok(target) => match target.attrs.file_type() {
                                            FileType::File => {
                                                let unique =
                                                    alloc_unique_file_name(&mut taken, &child_name);
                                                let sz = target.attrs.size.unwrap_or(0);
                                                planned_total += sz;
                                                scan.add(1, sz);
                                                jobs.push(DownloadJob {
                                                    remote: child_remote,
                                                    local: cur_local.join(unique),
                                                    known_size: target.attrs.size,
                                                    has_placeholder: false,
                                                });
                                            }
                                            FileType::Dir => {
                                                outcome.skipped.push((
                                                    child_remote,
                                                    "link to a folder".into(),
                                                ));
                                            }
                                            _ => {
                                                outcome.skipped.push((
                                                    child_remote,
                                                    "not a regular file".into(),
                                                ));
                                            }
                                        },
                                        Err(e) => {
                                            outcome.skipped.push((child_remote, e.to_string()));
                                        }
                                    }
                                }
                                _ => {
                                    outcome
                                        .skipped
                                        .push((child_remote, "not a regular file".into()));
                                }
                            }
                        }
                    }
                }
                _ => bail!("{remote} is not a regular file or folder"),
            }
        }
        Ok(())
    }
    .await;

    drop(scan);
    outcome.files_total = jobs.len();
    if let Err(e) = walk_res {
        let placeholders = jobs
            .iter()
            .filter(|j| j.has_placeholder)
            .map(|j| j.local.clone())
            .collect();
        cleanup_download_artifacts(placeholders, created_dirs).await;
        let _ = dl.raw.close_session();
        return (outcome, Err(e));
    }

    let prog = SharedProgress::new(progress, planned_total);
    prog.send();
    let (completed, result) = download_jobs(&dl, &jobs, &prog, &cancel).await;
    prog.send();
    let _ = dl.raw.close_session();

    let mut placeholders = Vec::new();
    for (k, ok) in completed.iter().enumerate() {
        if *ok {
            outcome.files_done += 1;
        } else if jobs[k].has_placeholder {
            placeholders.push(jobs[k].local.clone());
        }
    }
    let has_created_dirs = !created_dirs.is_empty();
    let dirs_to_remove = if result.is_err() && outcome.files_done == 0 {
        created_dirs
    } else {
        Vec::new()
    };
    cleanup_download_artifacts(placeholders, dirs_to_remove).await;

    outcome.saved_path = if remotes.len() == 1 && top_locals.len() == 1 {
        let (p, is_dir) = &top_locals[0];
        if (*is_dir && (result.is_ok() || outcome.files_done > 0))
            || (!*is_dir && outcome.files_done > 0)
        {
            Some(p.clone())
        } else {
            None
        }
    } else if outcome.files_done > 0 || (result.is_ok() && has_created_dirs) {
        Some(dest_dir.to_path_buf())
    } else {
        None
    };

    (outcome, result)
}

/// Remove a reserved download target that never received its content.
#[cfg(feature = "gui")]
pub fn remove_placeholder(p: &Path) {
    if std::fs::metadata(p).is_ok_and(|m| m.is_file() && m.len() == 0) {
        let _ = std::fs::remove_file(p);
    }
}

/// A remote name made safe as a local file/folder name: never a path (`/`, and on
/// Windows `\`), no characters Windows rejects (`<>:"|?*`, control
/// characters — replaced by `_`), no reserved device names, no trailing dots
/// or spaces, and at most 200 bytes.
#[cfg(feature = "gui")]
pub fn local_safe_name(name: &str) -> String {
    let mut out: String = name
        .chars()
        .map(|c| {
            let bad = c == '/'
                || c == '\0'
                || (cfg!(windows) && (c.is_control() || r#"<>:"\|?*"#.contains(c)));
            if bad {
                '_'
            } else {
                c
            }
        })
        .collect();
    if cfg!(windows) {
        let stem = out
            .split('.')
            .next()
            .unwrap_or("")
            .trim_end()
            .to_ascii_uppercase();
        let reserved = matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
            || ((stem.starts_with("COM") || stem.starts_with("LPT"))
                && stem.len() == 4
                && stem.as_bytes()[3].is_ascii_digit());
        if reserved {
            out.insert(0, '_');
        }
        while out.ends_with('.') || out.ends_with(' ') {
            out.pop();
        }
    }
    if out.len() > 200 {
        let mut cut = 200;
        while !out.is_char_boundary(cut) {
            cut -= 1;
        }
        // keep the extension
        let ext = out
            .rfind('.')
            .filter(|i| out.len() - i <= 16)
            .map(|i| out[i..].to_string())
            .unwrap_or_default();
        let keep = cut.saturating_sub(ext.len());
        let mut k = keep;
        while !out.is_char_boundary(k) {
            k -= 1;
        }
        out = format!("{}{ext}", &out[..k]);
    }
    if cfg!(windows) {
        while out.ends_with('.') || out.ends_with(' ') {
            out.pop();
        }
        let stem = out
            .split('.')
            .next()
            .unwrap_or("")
            .trim_end()
            .to_ascii_uppercase();
        let reserved = matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
            || ((stem.starts_with("COM") || stem.starts_with("LPT"))
                && stem.len() == 4
                && stem.as_bytes()[3].is_ascii_digit());
        if reserved && !out.starts_with('_') {
            out.insert(0, '_');
        }
    }
    if out.is_empty() || out == "." || out == ".." {
        out = "file".into();
    }
    out
}

/// Claim a local file name for a download: `name`, or `name (1).ext` …
/// when taken (including collision with an existing local folder on Windows).
#[cfg(feature = "gui")]
pub fn reserve_local_path(dir: &Path, name: &str) -> std::io::Result<std::path::PathBuf> {
    let name = local_safe_name(name);
    let (stem, ext) = match name.rsplit_once('.') {
        Some((s, e)) if !s.is_empty() => (s.to_string(), format!(".{e}")),
        _ => (name.clone(), String::new()),
    };
    let claim = |p: std::path::PathBuf| -> std::io::Result<Option<std::path::PathBuf>> {
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&p)
        {
            Ok(_) => Ok(Some(p)),
            Err(e)
                if e.kind() == std::io::ErrorKind::AlreadyExists
                    || (cfg!(windows)
                        && e.kind() == std::io::ErrorKind::PermissionDenied
                        && p.is_dir()) =>
            {
                Ok(None)
            }
            Err(e) => Err(e),
        }
    };
    if let Some(p) = claim(dir.join(&name))? {
        return Ok(p);
    }
    for i in 1..1000 {
        if let Some(p) = claim(dir.join(format!("{stem} ({i}){ext}")))? {
            return Ok(p);
        }
    }
    // Never overwrite: fall back to a timestamped name.
    loop {
        let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S%.6f");
        if let Some(p) = claim(dir.join(format!("{stem} ({stamp}){ext}")))? {
            return Ok(p);
        }
    }
}

/// Claim a local folder name for a folder download: `name`, or `name (1)` …
/// when taken — never merges into or overwrites an existing folder or file.
#[cfg(feature = "gui")]
pub fn reserve_local_dir(dir: &Path, name: &str) -> std::io::Result<std::path::PathBuf> {
    let safe = local_safe_name(name);
    let name = if safe == "file" && name != "file" {
        "folder".to_string()
    } else {
        safe
    };
    let claim = |p: std::path::PathBuf| -> std::io::Result<Option<std::path::PathBuf>> {
        match std::fs::create_dir(&p) {
            Ok(()) => Ok(Some(p)),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Ok(None),
            Err(e) => Err(e),
        }
    };
    if let Some(p) = claim(dir.join(&name))? {
        return Ok(p);
    }
    for i in 1..1000 {
        if let Some(p) = claim(dir.join(format!("{name} ({i})")))? {
            return Ok(p);
        }
    }
    loop {
        let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S%.6f");
        if let Some(p) = claim(dir.join(format!("{name} ({stamp})")))? {
            return Ok(p);
        }
    }
}

/// Download one regular file (see [`sftp_download_files`]).
#[cfg(all(feature = "gui", test))]
pub async fn sftp_download(
    handle: &Arc<SshHandle>,
    remote: &str,
    local: &Path,
    progress: mpsc::UnboundedSender<TransferProgress>,
    cancel: Arc<std::sync::atomic::AtomicBool>,
) -> Result<()> {
    sftp_download_files(
        handle,
        vec![(remote.to_string(), local.to_path_buf())],
        progress,
        cancel,
    )
    .await
    .1
}

/// Hidden partial-download name next to `local`: `.<name>.tern-<tag>.part`,
/// unique across tabs and processes, within the 255-byte name limit.
#[cfg(feature = "gui")]
fn local_part_name(local: &Path) -> std::ffi::OsString {
    let name = local
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    temp_name(&name, "part").into()
}

/// Remove partial downloads left in `dir` by Tern runs that ended without
/// cleaning up (crash, killed process) — only our own `.…​.tern-….part`
/// names, and only when older than a day.
#[cfg(feature = "gui")]
pub fn clean_stale_parts(dir: &Path) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    let day = std::time::Duration::from_secs(24 * 3600);
    for e in rd.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        // Group downloads use one folder per host: look one level down.
        if e.file_type().is_ok_and(|t| t.is_dir()) {
            if let Ok(sub) = std::fs::read_dir(e.path()) {
                for f in sub.flatten() {
                    let n = f.file_name().to_string_lossy().into_owned();
                    let old = f
                        .metadata()
                        .and_then(|m| m.modified())
                        .ok()
                        .and_then(|t| t.elapsed().ok())
                        .is_some_and(|age| age > day);
                    if n.starts_with('.') && n.contains(".tern-") && n.ends_with(".part") && old {
                        let _ = std::fs::remove_file(f.path());
                    }
                }
            }
            continue;
        }
        if !(name.starts_with('.') && name.contains(".tern-") && name.ends_with(".part")) {
            continue;
        }
        let old = e
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.elapsed().ok())
            .is_some_and(|age| age > day);
        if old {
            let _ = std::fs::remove_file(e.path());
        }
    }
}

/// The transfer was cancelled by the user.
#[cfg(feature = "gui")]
#[derive(Debug)]
pub struct Cancelled;

#[cfg(feature = "gui")]
impl std::fmt::Display for Cancelled {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("cancelled")
    }
}

#[cfg(feature = "gui")]
impl std::error::Error for Cancelled {}

/// Deletes a local partial file when dropped (unless disarmed), so even a
/// panicking or aborted transfer task doesn't leave it behind.
#[cfg(feature = "gui")]
struct PartFile(Option<std::path::PathBuf>);

#[cfg(feature = "gui")]
impl PartFile {
    fn disarm(mut self) {
        self.0 = None;
    }
}

#[cfg(feature = "gui")]
impl Drop for PartFile {
    fn drop(&mut self) {
        if let Some(p) = self.0.take() {
            let _ = std::fs::remove_file(p);
        }
    }
}

/// One file of an upload plan: local source and its remote destination.
#[cfg(feature = "gui")]
#[derive(Clone)]
pub struct UploadItem {
    pub local: std::path::PathBuf,
    pub remote: String,
    pub size: u64,
}

#[cfg(feature = "gui")]
async fn ensure_remote_dir(
    w: &RemoteWriter<'_>,
    dir: &str,
    cancel: &std::sync::atomic::AtomicBool,
    fresh: &mut std::collections::HashSet<String>,
) -> Result<()> {
    let dir = if dir == "/" {
        "/"
    } else {
        dir.trim_end_matches('/')
    };
    if dir.is_empty() || dir == "." || dir == "/" || fresh.contains(dir) {
        return Ok(());
    }
    let first_err = match or_cancel(w.sftp.create_dir(dir), cancel).await? {
        Ok(()) => {
            fresh.insert(dir.to_string());
            return Ok(());
        }
        Err(e) => e,
    };
    if let Ok(Some(m)) = or_cancel(stat_opt(&w.sftp, dir, true), cancel).await? {
        if file_is_dir(&m) {
            return Ok(());
        }
        w.heal().await;
        bail!("mkdir {dir}: not a directory");
    }
    let mut cur = if dir.starts_with('/') {
        String::from("/")
    } else {
        String::new()
    };
    let parts: Vec<&str> = dir.split('/').filter(|p| !p.is_empty()).collect();
    for (idx, part) in parts.iter().enumerate() {
        if !cur.is_empty() && cur != "/" {
            cur.push('/');
        }
        cur.push_str(part);
        if fresh.contains(&cur) {
            continue;
        }
        match or_cancel(w.sftp.create_dir(cur.as_str()), cancel).await? {
            Ok(()) => {
                fresh.insert(cur.clone());
            }
            Err(e) => {
                if let Ok(Some(m)) = or_cancel(stat_opt(&w.sftp, &cur, true), cancel).await? {
                    if file_is_dir(&m) {
                        continue;
                    }
                    w.heal().await;
                    bail!("mkdir {cur}: not a directory");
                }
                if idx + 1 == parts.len() {
                    w.heal().await;
                    bail!("mkdir {dir}: {e}");
                }
            }
        }
    }
    if !fresh.contains(dir)
        && !matches!(
            or_cancel(stat_opt(&w.sftp, dir, true), cancel).await?,
            Ok(Some(m)) if file_is_dir(&m)
        )
    {
        w.heal().await;
        bail!("mkdir {dir}: {first_err}");
    }
    Ok(())
}

/// Upload a whole plan (folders first, parent-first order) over the shared
/// SFTP session, several files at a time. Folders that already exist are
/// fine. Each file goes through [`RemoteWriter`]: a failed or cancelled
/// upload never truncates the file it would replace.
#[cfg(feature = "gui")]
pub async fn sftp_upload_tree<'h>(
    handle: &'h Arc<SshHandle>,
    dirs: &[String],
    items: &[UploadItem],
    progress: mpsc::UnboundedSender<TransferProgress>,
    cancel: Arc<std::sync::atomic::AtomicBool>,
) -> Result<()> {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    const PARALLEL: usize = 4;
    let w = or_cancel(RemoteWriter::new(handle), &cancel).await??;
    // Folders this upload created: nothing can be in them yet, so their
    // files need no "what's there now" look-up first.
    let mut fresh = std::collections::HashSet::new();
    for d in dirs {
        let d = remote_home_relative(d);
        ensure_remote_dir(&w, &d, &cancel, &mut fresh).await?;
    }
    let total: u64 = items.iter().map(|i| i.size).sum();
    let prog = SharedProgress::new(progress, total);
    let next = AtomicUsize::new(0);
    let stop = AtomicBool::new(false);
    let first_err: std::sync::Mutex<Option<anyhow::Error>> = std::sync::Mutex::new(None);
    let worker = |mut w: RemoteWriter<'h>| {
        let (prog, next, stop, first_err, fresh, cancel) =
            (&prog, &next, &stop, &first_err, &fresh, &*cancel);
        async move {
            loop {
                if stop.load(Ordering::Relaxed) || cancel.load(Ordering::Relaxed) {
                    break;
                }
                let i = next.fetch_add(1, Ordering::Relaxed);
                let Some(it) = items.get(i) else {
                    break;
                };
                let remote = remote_home_relative(&it.remote);
                let parent = remote.rsplit_once('/').map(|(d, _)| d);
                let target = match parent.filter(|d| fresh.contains(*d)) {
                    Some(_) => Ok(WriteTarget::new_file(&remote)),
                    None => match or_cancel(w.prepare(&remote, None), cancel).await {
                        Ok(r) => r,
                        Err(e) => Err(e),
                    },
                };
                let mut sent = 0u64;
                let r = match target {
                    Ok(t) => {
                        w.put(&t, Source::File(&it.local), cancel, &mut |n| {
                            prog.add(n.saturating_sub(sent));
                            sent = sent.max(n);
                        })
                        .await
                    }
                    Err(e) => Err(e),
                };
                match r {
                    Ok(()) => prog.add(it.size.saturating_sub(sent)),
                    Err(e) => {
                        if e.downcast_ref::<Cancelled>().is_none() {
                            let _ = tokio::time::timeout(Duration::from_secs(5), w.heal()).await;
                        }
                        stop.store(true, Ordering::Relaxed);
                        let mut slot = first_err.lock().unwrap_or_else(|p| p.into_inner());
                        if slot.is_none() {
                            *slot = Some(e);
                        }
                        break;
                    }
                }
            }
        }
    };
    let mut writers = vec![w];
    for _ in 1..PARALLEL.min(items.len()) {
        writers.push(RemoteWriter::new_sharing(&writers[0]));
    }
    futures_util::future::join_all(writers.into_iter().map(worker)).await;
    prog.send();
    if let Some(e) = first_err.into_inner().unwrap_or_else(|p| p.into_inner()) {
        return Err(e);
    }
    if cancel.load(Ordering::Relaxed) {
        return Err(anyhow::Error::new(Cancelled));
    }
    Ok(())
}

/// Delete a remote entry. Links are removed as links (never followed);
/// folders are removed recursively, many requests at a time, using the
/// types the listing already reports (no extra stat per entry).
#[cfg(all(feature = "gui", test))]
pub async fn sftp_remove(handle: &Arc<SshHandle>, path: &str) -> Result<()> {
    let mut r = sftp_remove_many(handle, vec![path.to_string()]).await?;
    r.pop().map(|(_, r)| r).unwrap_or(Ok(()))
}

/// Delete several entries (see [`sftp_remove`]) over one SFTP session, many
/// at a time: each item's own result, in the order given. `Err` only when
/// no session could be opened.
#[cfg(feature = "gui")]
pub async fn sftp_remove_many(
    handle: &Arc<SshHandle>,
    paths: Vec<String>,
) -> Result<Vec<(String, Result<()>)>> {
    use futures_util::stream::{self, StreamExt};
    let sftp = {
        // A stale cached session is replaced first (deletes are never sent
        // twice, see `with_sftp_once`).
        let mut sftp = sftp_session(handle).await?;
        if let Err(e) = sftp.canonicalize(".").await {
            if is_transport_error(&anyhow::Error::new(e)) {
                sftp_forget(handle);
                sftp = sftp_session(handle).await?;
            }
        }
        sftp
    };
    let sem = Arc::new(tokio::sync::Semaphore::new(REMOVE_PARALLEL));
    let results: Vec<(String, Result<()>)> = stream::iter(paths.into_iter().map(|path| {
        let (sftp, sem) = (Arc::clone(&sftp), Arc::clone(&sem));
        async move {
            let normalized = remote_home_relative(&path);
            let r = remove_path(sftp, sem, normalized).await;
            (path, r)
        }
    }))
    .buffered(REMOVE_PARALLEL)
    .collect()
    .await;
    if results
        .iter()
        .any(|(_, r)| r.as_ref().is_err_and(is_transport_error))
    {
        sftp_forget(handle);
    }
    Ok(results)
}

/// Remove a folder's contents (concurrently), then the folder.
#[cfg(feature = "gui")]
fn remove_dir_tree(
    sftp: Arc<Sftp>,
    sem: Arc<tokio::sync::Semaphore>,
    path: String,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<()>> + Send>> {
    use futures_util::stream::{self, TryStreamExt};
    use russh_sftp::protocol::FileType;
    Box::pin(async move {
        let entries = {
            let _p = sem.acquire().await;
            sftp.read_dir(path.as_str())
                .await
                .with_context(|| format!("list {path}"))?
        };
        let children: Vec<(String, bool)> = entries
            .into_iter()
            .filter_map(|e| {
                let name = e.file_name();
                if name == "." || name == ".." {
                    return None;
                }
                // Listing attributes are lstat-like: a link to a folder
                // reports as a link and is removed as one.
                let is_dir = matches!(e.metadata().file_type(), FileType::Dir);
                Some((child_path(&path, &name), is_dir))
            })
            .collect();
        stream::iter(children.into_iter().map(Ok::<_, anyhow::Error>))
            .try_for_each_concurrent(REMOVE_PARALLEL, |(child, is_dir)| {
                let (sftp, sem) = (Arc::clone(&sftp), Arc::clone(&sem));
                async move {
                    if is_dir {
                        return remove_dir_tree(sftp, sem, child).await;
                    }
                    let _p = sem.acquire().await;
                    sftp.remove_file(child.as_str())
                        .await
                        .with_context(|| format!("delete {child}"))
                }
            })
            .await?;
        let _p = sem.acquire().await;
        sftp.remove_dir(path.as_str())
            .await
            .with_context(|| format!("delete {path}"))?;
        Ok(())
    })
}

/// Requests in flight across one whole delete.
#[cfg(feature = "gui")]
const REMOVE_PARALLEL: usize = 32;

/// Remove one entry: a link or file as itself, a folder with its contents.
#[cfg(feature = "gui")]
async fn remove_path(
    sftp: Arc<Sftp>,
    sem: Arc<tokio::sync::Semaphore>,
    path: String,
) -> Result<()> {
    let meta = {
        let _p = sem.acquire().await;
        sftp.symlink_metadata(path.as_str())
            .await
            .with_context(|| format!("stat {path}"))?
    };
    if !file_is_dir(&meta) {
        let _p = sem.acquire().await;
        sftp.remove_file(path.as_str())
            .await
            .with_context(|| format!("delete {path}"))?;
        return Ok(());
    }
    remove_dir_tree(sftp, sem, path).await
}

#[cfg(feature = "gui")]
pub async fn sftp_rename(handle: &Arc<SshHandle>, from: &str, to: &str) -> Result<()> {
    let from = remote_home_relative(from);
    let to = remote_home_relative(to);
    with_sftp_once(handle, |s| async move {
        s.rename(from.as_str(), to.as_str()).await?;
        Ok(())
    })
    .await
}

/// Read a remote text file (UTF-8, at most `max_bytes`). Only regular files
/// are opened, and never more than `max_bytes + 1` bytes are read, so device
/// files or files that grow while reading cannot exhaust memory.
#[cfg(feature = "gui")]
pub async fn sftp_read_text(
    handle: &Arc<SshHandle>,
    path: &str,
    max_bytes: u64,
) -> Result<(String, Option<FileStamp>)> {
    let path = remote_home_relative(path);
    let data = with_sftp(handle, |s| {
        let path = path.clone();
        async move {
            let meta = s
                .metadata(path.as_str())
                .await
                .with_context(|| format!("stat {path}"))?;
            if !matches!(meta.file_type(), russh_sftp::protocol::FileType::File) {
                bail!("not a regular file");
            }
            if let Some(size) = meta.size {
                if size > max_bytes {
                    bail!("file is too large to edit ({size} bytes)");
                }
            }
            let file = s
                .open(path.as_str())
                .await
                .with_context(|| format!("open {path}"))?;
            let mut data = Vec::with_capacity(meta.size.unwrap_or(0).min(max_bytes) as usize + 1);
            file.take(max_bytes + 1)
                .read_to_end(&mut data)
                .await
                .with_context(|| format!("read {path}"))?;
            if data.len() as u64 > max_bytes {
                bail!("file is too large to edit");
            }
            Ok((data, FileStamp::of(&meta)))
        }
    })
    .await?;
    let (data, stamp) = data;
    if data.iter().take(8192).any(|b| *b == 0) {
        bail!("binary file");
    }
    let text = String::from_utf8(data).map_err(|_| anyhow::anyhow!("not UTF-8 text"))?;
    Ok((text, stamp))
}

/// Size + modification time of a remote file, used to notice that someone
/// else changed it between opening and saving.
#[cfg(feature = "gui")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FileStamp {
    pub size: u64,
    pub mtime: u32,
}

#[cfg(feature = "gui")]
impl FileStamp {
    fn of(m: &russh_sftp::protocol::FileAttributes) -> Option<Self> {
        Some(Self {
            size: m.size?,
            mtime: m.mtime?,
        })
    }
}

/// The file changed on the server since it was opened (save refused).
#[cfg(feature = "gui")]
#[derive(Debug)]
pub struct RemoteChanged;

#[cfg(feature = "gui")]
impl std::fmt::Display for RemoteChanged {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("the file was changed on the server since it was opened")
    }
}

#[cfg(feature = "gui")]
impl std::error::Error for RemoteChanged {}

/// Where new content for one remote file goes (see [`RemoteWriter::prepare`]).
#[cfg(feature = "gui")]
struct WriteTarget {
    /// The file actually written (symbolic links resolved).
    path: String,
    dir: String,
    name: String,
    /// Its attributes if it exists (`None`: a new file).
    existing: Option<russh_sftp::protocol::FileAttributes>,
    /// Editor's expected version; checked again immediately before commit.
    expect: Option<FileStamp>,
}

#[cfg(feature = "gui")]
impl WriteTarget {
    /// A file known not to exist yet (its folder was just created).
    fn new_file(path: &str) -> Self {
        let (dir, name) = split_remote(path);
        Self {
            path: path.to_string(),
            dir,
            name,
            existing: None,
            expect: None,
        }
    }
}

/// `dir/name` → (`dir`, `name`); `/name` → (`/`, `name`); `name` → (`.`, `name`).
#[cfg(feature = "gui")]
fn split_remote(path: &str) -> (String, String) {
    match path.rsplit_once('/') {
        Some((d, n)) => (
            if d.is_empty() {
                "/".to_string()
            } else {
                d.to_string()
            },
            n.to_string(),
        ),
        None => (".".to_string(), path.to_string()),
    }
}

#[cfg(feature = "gui")]
enum Source<'a> {
    Bytes(&'a [u8]),
    File(&'a std::path::Path),
}

/// Writes remote files without ever leaving one truncated or half written.
///
/// New content always goes completely into a temporary file next to the
/// target first. Then:
/// * new file, or a plain file we own with one link: the temporary file is
///   renamed over it atomically (`posix-rename@openssh.com`), keeping its
///   permissions;
/// * owned by someone else / hard-linked: the content is copied into the
///   existing file in place (server-side `copy-data` when available), so
///   owner, links and ACLs stay; if that fails the temporary file is kept
///   and named in the error.
///
/// Symbolic links are followed (the file they point to is updated). One
/// helper SFTP session and one directory listing per folder are reused for
/// a whole upload.
#[cfg(feature = "gui")]
pub(crate) struct RemoteWriter<'h> {
    handle: &'h Arc<SshHandle>,
    sftp: Arc<Sftp>,
    /// Low-level session for extensions, opened on first use and shared by
    /// writers made with [`RemoteWriter::new_sharing`] (one channel, not
    /// one per parallel upload).
    raw: Arc<
        tokio::sync::OnceCell<(
            russh_sftp::client::RawSftpSession,
            russh_sftp::protocol::Version,
        )>,
    >,
    /// dir → (name → link count), from one `ls -l` style listing each.
    nlinks: std::collections::HashMap<String, std::collections::HashMap<String, u64>>,
    /// Link counts asked with a remote `stat` command so far (`None` once
    /// that failed: no shell, or not a Unix server).
    exec_stats: Option<u32>,
}

#[cfg(feature = "gui")]
impl<'h> RemoteWriter<'h> {
    async fn new(handle: &'h Arc<SshHandle>) -> Result<Self> {
        let mut sftp = sftp_session(handle).await?;
        if let Err(e) = sftp.canonicalize(".").await {
            if is_transport_error(&anyhow::Error::new(e)) {
                sftp_forget(handle);
                sftp = sftp_session(handle).await?;
            }
        }
        Ok(Self {
            handle,
            sftp,
            raw: Default::default(),
            nlinks: Default::default(),
            exec_stats: Some(0),
        })
    }

    /// Another writer on the same SFTP session (for parallel uploads).
    fn new_sharing(other: &Self) -> Self {
        Self {
            handle: other.handle,
            sftp: Arc::clone(&other.sftp),
            raw: Arc::clone(&other.raw),
            nlinks: Default::default(),
            exec_stats: other.exec_stats,
        }
    }

    async fn raw(
        &mut self,
    ) -> Result<&(
        russh_sftp::client::RawSftpSession,
        russh_sftp::protocol::Version,
    )> {
        let handle = self.handle;
        self.raw.get_or_try_init(|| raw_sftp(handle)).await
    }

    /// After an error: drop the cached SFTP session if the channel is broken
    /// (file-level errors come back as plain I/O errors, so ask the server).
    async fn heal(&self) {
        if let Err(e) = self.sftp.canonicalize(".").await {
            if !matches!(e, russh_sftp::client::error::Error::Status(_)) {
                sftp_forget(self.handle);
            }
        }
    }

    /// Resolve `path` (following symlinks) and look at what is there now.
    /// With `expect`, refuse with [`RemoteChanged`] if the file changed (or
    /// disappeared) since it was loaded. A failing stat is an error, never
    /// mistaken for "new file".
    async fn prepare(&mut self, path: &str, expect: Option<FileStamp>) -> Result<WriteTarget> {
        let link = stat_opt(&self.sftp, path, false).await?;
        let real = match &link {
            Some(m) if m.is_symlink() => match self.sftp.canonicalize(path).await {
                Ok(real) => real,
                Err(_) => {
                    // Dangling link: write where it points.
                    let dest = self
                        .sftp
                        .read_link(path)
                        .await
                        .with_context(|| format!("readlink {path}"))?;
                    if dest.starts_with('/') {
                        dest
                    } else {
                        let dir = path.rsplit_once('/').map(|(d, _)| d).unwrap_or(".");
                        child_path(if dir.is_empty() { "/" } else { dir }, &dest)
                    }
                }
            },
            _ => path.to_string(),
        };
        let existing = if real == path {
            link
        } else {
            stat_opt(&self.sftp, &real, true).await?
        };
        if let Some(want) = expect {
            if existing.as_ref().and_then(FileStamp::of) != Some(want) {
                return Err(anyhow::Error::new(RemoteChanged));
            }
        }
        if existing.as_ref().is_some_and(file_is_dir) {
            bail!("{real} is a folder");
        }
        let (dir, name) = split_remote(&real);
        Ok(WriteTarget {
            path: real,
            dir,
            name,
            existing,
            expect,
        })
    }

    /// Link count of `dir/name` from the folder's long listing (SFTP v3
    /// attributes don't carry it). `None` when it can't be told.
    /// `server_now`: the server's clock (mtime of a file we just created);
    /// leftover temporary files are only tidied when it is known.
    async fn link_count(&mut self, dir: &str, name: &str, server_now: Option<u32>) -> Option<u64> {
        // A few files: ask `stat` for just this one rather than list a
        // folder that may hold many thousands of entries. Many files: one
        // listing per folder is cheaper.
        if !self.nlinks.contains_key(dir) {
            if let Some(n) = self
                .exec_stats
                .filter(|n| *n < 8 && exec_works(self.handle))
            {
                let path = child_path(dir, name);
                let q = sh_quote(&path);
                let cmd = format!("stat -c %h -- {q} 2>/dev/null || stat -f %l -- {q}");
                let r = exec_capture(self.handle, &cmd).await;
                match r.ok().and_then(|o| o.trim().parse::<u64>().ok()) {
                    Some(links) => {
                        self.exec_stats = Some(n + 1);
                        return Some(links);
                    }
                    None => self.exec_stats = None,
                }
            }
        }
        if !self.nlinks.contains_key(dir) {
            let mut map = std::collections::HashMap::new();
            let mut stale = Vec::new();
            if let Ok((raw, _)) = self.raw().await {
                if let Ok(dh) = raw.opendir(dir).await {
                    while let Ok(batch) = raw.readdir(dh.handle.as_str()).await {
                        for f in batch.files {
                            // Our own temporary files left by an earlier run
                            // that lost its connection: tidy up after a day.
                            let ours = f.filename.starts_with('.')
                                && f.filename.contains(".tern-")
                                && (f.filename.ends_with(".tmp") || f.filename.ends_with(".part"));
                            let old = match (server_now, f.attrs.mtime) {
                                (Some(now), Some(m)) => (m as u64) + 24 * 3600 < now as u64,
                                _ => false,
                            };
                            if ours && old {
                                stale.push(child_path(dir, &f.filename));
                            }
                            if let Some(n) = f
                                .longname
                                .split_whitespace()
                                .nth(1)
                                .and_then(|n| n.parse::<u64>().ok())
                            {
                                map.insert(f.filename, n);
                            }
                        }
                    }
                    let _ = raw.close(dh.handle).await;
                }
            }
            for p in stale {
                let _ = self.sftp.remove_file(p.as_str()).await;
            }
            self.nlinks.insert(dir.to_string(), map);
        }
        self.nlinks.get(dir)?.get(name).copied()
    }

    /// Can a new file (`tmp`, created by us) replace the target without
    /// changing its owner or breaking hard links? Tries to give `tmp` the
    /// original owner (works as root).
    async fn can_swap_in(&mut self, tmp: &str, t: &WriteTarget) -> bool {
        let Some(orig) = &t.existing else {
            return true;
        };
        let tmp_meta = self.sftp.metadata(tmp).await.ok();
        let server_now = tmp_meta.as_ref().and_then(|m| m.mtime);
        let mut same_owner = tmp_meta
            .as_ref()
            .is_some_and(|m| m.uid == orig.uid && m.gid == orig.gid);
        if !same_owner && orig.uid.is_some() {
            let mut own = russh_sftp::protocol::FileAttributes::empty();
            own.uid = orig.uid;
            own.gid = orig.gid;
            same_owner = self.sftp.set_metadata(tmp, own).await.is_ok();
        }
        same_owner && self.link_count(&t.dir, &t.name, server_now).await == Some(1)
    }

    /// Atomically replace `to` with `from`. Without `posix-rename` the old
    /// file is moved aside first and put back if the swap fails.
    async fn replace(&mut self, from: &str, to: &str, to_exists: bool) -> Result<()> {
        let (raw, ver) = self.raw().await?;
        if ver.extensions.contains_key("posix-rename@openssh.com") {
            let reply = raw
                .extended("posix-rename@openssh.com", ssh_strings(&[from, to]))
                .await;
            return expect_ok(reply).with_context(|| format!("save {to}"));
        }
        if !to_exists {
            return self
                .sftp
                .rename(from, to)
                .await
                .with_context(|| format!("save {to}"));
        }
        let aside = format!("{to}.tern-old-{}", unique_tag());
        self.sftp
            .rename(to, aside.as_str())
            .await
            .with_context(|| format!("save {to}"))?;
        if let Err(e) = self.sftp.rename(from, to).await {
            if self.sftp.rename(aside.as_str(), to).await.is_err() {
                return Err(anyhow::Error::new(e)).with_context(|| {
                    format!("save {to} failed; the previous version is in {aside}")
                });
            }
            return Err(anyhow::Error::new(e)).with_context(|| format!("save {to}"));
        }
        let _ = self.sftp.remove_file(aside.as_str()).await;
        Ok(())
    }

    /// Copy `tmp` into the existing target in place: server-side when the
    /// server offers `copy-data`, else by streaming the remote snapshot.
    async fn copy_in_place(&mut self, tmp: &str, t: &WriteTarget) -> Result<()> {
        use russh_sftp::protocol::{FileAttributes, OpenFlags};
        let has_copy = self
            .raw()
            .await
            .is_ok_and(|(_, ver)| ver.extensions.contains_key("copy-data"));
        if has_copy {
            let (raw, _) = raw_sftp(self.handle).await?;
            let rh = raw
                .open(tmp, OpenFlags::READ, FileAttributes::empty())
                .await
                .with_context(|| format!("open {tmp}"))?
                .handle;
            let wh = match raw
                .open(
                    t.path.as_str(),
                    OpenFlags::WRITE | OpenFlags::TRUNCATE,
                    FileAttributes::empty(),
                )
                .await
            {
                Ok(h) => h.handle,
                Err(e) => {
                    let _ = raw.close(rh).await;
                    let _ = raw.close_session();
                    return Err(anyhow::Error::new(e)).with_context(|| format!("open {}", t.path));
                }
            };
            let mut payload = ssh_strings(&[rh.as_str()]);
            payload.extend_from_slice(&0u64.to_be_bytes()); // from offset
            payload.extend_from_slice(&0u64.to_be_bytes()); // length: to EOF
            payload.extend(ssh_strings(&[wh.as_str()]));
            payload.extend_from_slice(&0u64.to_be_bytes()); // to offset
                                                            // Dedicated channel: won't race the shared helper's timeout across parallel uploads.
            raw.set_timeout(3600);
            let r = expect_ok(raw.extended("copy-data", payload).await);
            let _ = raw.close(rh).await;
            let closed = raw.close(wh).await;
            let _ = raw.close_session();
            r.with_context(|| format!("write {}", t.path))?;
            closed
                .map_err(anyhow::Error::new)
                .with_context(|| format!("write {}", t.path))?;
            return Ok(());
        }
        // Copy the completed remote snapshot, not a local file that might
        // have changed or disappeared since the temporary upload finished.
        let mut input = self
            .sftp
            .open(tmp)
            .await
            .with_context(|| format!("open saved content {tmp}"))?;
        let mut f = self
            .sftp
            .open_with_flags(t.path.as_str(), OpenFlags::WRITE | OpenFlags::TRUNCATE)
            .await
            .with_context(|| format!("open {}", t.path))?;
        tokio::io::copy(&mut input, &mut f)
            .await
            .with_context(|| format!("write {}", t.path))?;
        f.shutdown()
            .await
            .with_context(|| format!("write {}", t.path))?;
        Ok(())
    }

    /// Put `src` into the target (see the type's description). `cancel`
    /// interrupts only the copy into the temporary file: once the old file
    /// is being replaced, the step runs to completion.
    async fn put(
        &mut self,
        t: &WriteTarget,
        src: Source<'_>,
        cancel: &std::sync::atomic::AtomicBool,
        progress: &mut (dyn FnMut(u64) + Send),
    ) -> Result<()> {
        use russh_sftp::protocol::{FileAttributes, OpenFlags};
        let tmp = child_path(&t.dir, &temp_name(&t.name, "tmp"));
        let perm = t
            .existing
            .as_ref()
            .and_then(|m| m.permissions)
            .map(|p| p & 0o7777);
        let mut attrs = FileAttributes::empty();
        attrs.permissions = perm;
        let file = or_cancel(
            self.sftp.open_with_flags_and_attributes(
                tmp.as_str(),
                OpenFlags::CREATE | OpenFlags::EXCLUDE | OpenFlags::WRITE,
                attrs,
            ),
            cancel,
        )
        .await?;
        // Never truncate the original as a fallback when its directory
        // cannot hold a recovery copy (permissions, quota, or full disk).
        let mut file = file.with_context(|| {
            format!(
            "cannot safely save {}: could not create a temporary file in {}; original unchanged",
            t.path, t.dir
        )
        })?;

        // 1) The full new content into the temporary file.
        let written: Result<()> = async {
            write_source(&mut file, &src, Some(cancel), progress).await?;
            or_cancel(file.shutdown(), cancel).await??;
            Ok(())
        }
        .await;
        drop(file);
        if let Err(e) = written {
            // Bounded: after a cancel the link may be the very thing stuck.
            let _ =
                tokio::time::timeout(Duration::from_secs(5), self.sftp.remove_file(tmp.as_str()))
                    .await;
            if e.downcast_ref::<Cancelled>().is_some() {
                return Err(e);
            }
            return Err(e.context(format!("write {}", t.path)));
        }

        // 2) Commit.
        let can_swap = self.can_swap_in(&tmp, t).await;
        let commit_check: Result<()> = async {
            if let Some(want) = t.expect {
                let current = stat_opt(&self.sftp, &t.path, true).await?;
                if current.as_ref().and_then(FileStamp::of) != Some(want) {
                    return Err(anyhow::Error::new(RemoteChanged));
                }
            }
            Ok(())
        }
        .await;
        if let Err(e) = commit_check {
            let _ = self.sftp.remove_file(tmp.as_str()).await;
            return Err(e);
        }
        if can_swap {
            if let Some(perm) = perm {
                // umask may have trimmed the mode at create time.
                let mut a = FileAttributes::empty();
                a.permissions = Some(perm);
                let _ = self.sftp.set_metadata(tmp.as_str(), a).await;
            }
            if let Err(e) = self.replace(&tmp, &t.path, t.existing.is_some()).await {
                let _ = self.sftp.remove_file(tmp.as_str()).await;
                return Err(e);
            }
            return Ok(());
        }
        match self.copy_in_place(&tmp, t).await {
            Ok(()) => {
                let _ = self.sftp.remove_file(tmp.as_str()).await;
                Ok(())
            }
            // The target may be partly overwritten: keep the full copy,
            // under a visible name that temp-file cleanup never touches.
            Err(e) => {
                let kept = child_path(&t.dir, &format!("{}.tern-saved-{}", t.name, unique_tag()));
                let at = match self.sftp.rename(tmp.as_str(), kept.as_str()).await {
                    Ok(()) => kept,
                    Err(_) => tmp,
                };
                Err(e.context(format!("the new content was kept in {at}")))
            }
        }
    }
}

/// `Ok(None)` only when the server says the path doesn't exist.
#[cfg(feature = "gui")]
async fn stat_opt(
    sftp: &Sftp,
    path: &str,
    follow: bool,
) -> Result<Option<russh_sftp::protocol::FileAttributes>> {
    use russh_sftp::protocol::StatusCode;
    let r = if follow {
        sftp.metadata(path).await
    } else {
        sftp.symlink_metadata(path).await
    };
    match r {
        Ok(m) => Ok(Some(m)),
        Err(russh_sftp::client::error::Error::Status(st))
            if st.status_code == StatusCode::NoSuchFile =>
        {
            Ok(None)
        }
        Err(e) => Err(anyhow::Error::new(e)).with_context(|| format!("stat {path}")),
    }
}

/// Stream `src` into `f`. Stops with [`Cancelled`] when `cancel` is set.
#[cfg(feature = "gui")]
async fn write_source(
    f: &mut russh_sftp::client::fs::File,
    src: &Source<'_>,
    cancel: Option<&std::sync::atomic::AtomicBool>,
    progress: &mut (dyn FnMut(u64) + Send),
) -> Result<()> {
    use std::sync::atomic::Ordering;
    let cancelled = || cancel.is_some_and(|c| c.load(Ordering::Relaxed));
    match src {
        Source::Bytes(data) => {
            for chunk in data.chunks(256 * 1024) {
                if cancelled() {
                    return Err(anyhow::Error::new(Cancelled));
                }
                match cancel {
                    Some(c) => or_cancel(f.write_all(chunk), c).await??,
                    None => f.write_all(chunk).await?,
                }
            }
            progress(data.len() as u64);
        }
        Source::File(path) => {
            let mut input = tokio::fs::File::open(path)
                .await
                .with_context(|| format!("open {}", path.display()))?;
            let mut buf = vec![0u8; 128 * 1024];
            let mut n_total = 0u64;
            loop {
                if cancelled() {
                    return Err(anyhow::Error::new(Cancelled));
                }
                let n = input.read(&mut buf).await?;
                if n == 0 {
                    break;
                }
                match cancel {
                    Some(c) => or_cancel(f.write_all(&buf[..n]), c).await??,
                    None => f.write_all(&buf[..n]).await?,
                }
                n_total += n as u64;
                progress(n_total);
            }
        }
    }
    Ok(())
}

/// A tag no other temporary name of this process or another client can
/// share: process id, a counter and random bits.
#[cfg(feature = "gui")]
fn unique_tag() -> String {
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let mut r = [0u8; 4];
    let _ = getrandom::getrandom(&mut r);
    format!(
        "{:x}-{n:x}-{:08x}",
        std::process::id(),
        u32::from_be_bytes(r)
    )
}

/// `.<name>.tern-<tag>.<ext>`, shortening `name` so the whole thing stays
/// within the usual 255-byte file name limit.
#[cfg(feature = "gui")]
fn temp_name(name: &str, ext: &str) -> String {
    let tag = unique_tag();
    let room = 255usize.saturating_sub(tag.len() + ext.len() + 9);
    let mut short = name;
    if short.len() > room {
        let mut cut = room;
        while cut > 0 && !short.is_char_boundary(cut) {
            cut -= 1;
        }
        short = &short[..cut];
    }
    format!(".{short}.tern-{tag}.{ext}")
}

/// Quote for a POSIX shell: `'…'`, with embedded quotes escaped.
#[cfg(feature = "gui")]
fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// SSH wire strings (u32 length + bytes), concatenated.
#[cfg(feature = "gui")]
fn ssh_strings(parts: &[&str]) -> Vec<u8> {
    let mut out = Vec::new();
    for s in parts {
        out.extend_from_slice(&(s.len() as u32).to_be_bytes());
        out.extend_from_slice(s.as_bytes());
    }
    out
}

#[cfg(feature = "gui")]
fn expect_ok(
    reply: std::result::Result<russh_sftp::protocol::Packet, russh_sftp::client::error::Error>,
) -> Result<()> {
    use russh_sftp::protocol::{Packet, StatusCode};
    match reply {
        Ok(Packet::Status(s)) if s.status_code == StatusCode::Ok => Ok(()),
        Ok(Packet::Status(s)) => Err(anyhow::anyhow!("{}", s.error_message)),
        Ok(_) => Err(anyhow::anyhow!("unexpected reply")),
        Err(e) => Err(anyhow::Error::new(e)),
    }
}

/// Replace a remote file's contents without ever losing data (see
/// [`RemoteWriter`]). With `expect`, the save is refused with
/// [`RemoteChanged`] when the file changed or vanished since it was loaded.
/// Returns the new stamp.
#[cfg(feature = "gui")]
pub async fn sftp_write_file(
    handle: &Arc<SshHandle>,
    path: &str,
    data: &[u8],
    expect: Option<FileStamp>,
) -> Result<Option<FileStamp>> {
    let path = remote_home_relative(path);
    let mut w = RemoteWriter::new(handle).await?;
    let never = std::sync::atomic::AtomicBool::new(false);
    let result = async {
        let t = w.prepare(&path, expect).await?;
        w.put(&t, Source::Bytes(data), &never, &mut |_| {}).await?;
        Ok::<_, anyhow::Error>(t.path)
    }
    .await;
    match result {
        Ok(real) => Ok(w
            .sftp
            .metadata(real.as_str())
            .await
            .ok()
            .as_ref()
            .and_then(FileStamp::of)),
        Err(e) => {
            if e.downcast_ref::<RemoteChanged>().is_none() {
                w.heal().await;
            }
            Err(e)
        }
    }
}

/// Create a new empty file; fails if it already exists (atomic check).
#[cfg(feature = "gui")]
pub async fn sftp_create_file(handle: &Arc<SshHandle>, path: &str) -> Result<()> {
    use russh_sftp::protocol::OpenFlags;
    let path = remote_home_relative(path);
    with_sftp_once(handle, |s| {
        let path = path.clone();
        async move {
            let mut file = s
                .open_with_flags(
                    path.as_str(),
                    OpenFlags::CREATE | OpenFlags::EXCLUDE | OpenFlags::WRITE,
                )
                .await
                .map_err(|e| match &e {
                    russh_sftp::client::error::Error::Status(st)
                        if st.status_code == russh_sftp::protocol::StatusCode::Failure =>
                    {
                        anyhow::anyhow!("{path} already exists")
                    }
                    _ => anyhow::Error::new(e).context(format!("create {path}")),
                })?;
            file.shutdown().await?;
            Ok(())
        }
    })
    .await
}

#[cfg(feature = "gui")]
pub async fn sftp_mkdir(handle: &Arc<SshHandle>, path: &str) -> Result<()> {
    let path = remote_home_relative(path);
    with_sftp_once(handle, |s| async move {
        s.create_dir(path.as_str()).await?;
        Ok(())
    })
    .await
}

/// Update POSIX permissions (lower 12 mode bits) on a remote file or folder.
#[cfg(feature = "gui")]
pub async fn sftp_chmod(handle: &Arc<SshHandle>, path: &str, mode: u32) -> Result<()> {
    let raw = remote_home_relative(path);
    with_sftp(handle, |sftp| {
        let raw = raw.clone();
        async move {
            let mut attrs = russh_sftp::protocol::FileAttributes::empty();
            attrs.permissions = Some(mode & 0o7777);
            sftp.set_metadata(raw.as_str(), attrs)
                .await
                .with_context(|| format!("chmod {raw}"))?;
            Ok(())
        }
    })
    .await
}

/// Local port forward (`ssh -L listen:remote_host:remote_port`). Stops when
/// told to, or when its owner goes away (the stop sender is dropped); open
/// tunnels are closed with it.
#[cfg(feature = "gui")]
pub async fn local_forward(
    handle: Arc<SshHandle>,
    listen: std::net::SocketAddr,
    remote_host: String,
    remote_port: u16,
    mut stop: tokio::sync::watch::Receiver<bool>,
) -> Result<()> {
    let listener = tokio::net::TcpListener::bind(listen)
        .await
        .with_context(|| format!("bind local forward {listen}"))?;
    let mut tunnels = tokio::task::JoinSet::new();
    loop {
        tokio::select! {
            changed = stop.changed() => {
                if changed.is_err() || *stop.borrow() {
                    break;
                }
            }
            Some(_) = tunnels.join_next(), if !tunnels.is_empty() => {}
            accepted = listener.accept() => {
                let inbound = match accepted {
                    Ok((s, _)) => s,
                    Err(e) => {
                        // Transient (e.g. too many open files): keep serving.
                        eprintln!("[forward] accept failed: {e}");
                        tokio::time::sleep(Duration::from_millis(200)).await;
                        continue;
                    }
                };
                let handle = Arc::clone(&handle);
                let host = remote_host.clone();
                tunnels.spawn(async move {
                    let channel = match handle
                        .channel_open_direct_tcpip(&host, remote_port as u32, "127.0.0.1", 0)
                        .await
                    {
                        Ok(c) => c,
                        Err(_) => return,
                    };
                    pump_tunnel(channel, inbound).await;
                });
            }
        }
    }
    tunnels.abort_all();
    Ok(())
}

/// Most bytes a forwarded connection may buffer for a slow local reader
/// before it is dropped. Without this queue an unread channel fills russh's
/// per-channel buffer and stalls the whole session (every tab and tunnel).
const TUNNEL_QUEUE_CAP: usize = 16 * 1024 * 1024;

/// Copy one forwarded connection both ways. Data from the server is always
/// taken off the channel right away (into a capped queue), so a local client
/// that stops reading only ever hurts its own connection.
async fn pump_tunnel(channel: Channel<client::Msg>, inbound: tokio::net::TcpStream) {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let _ = inbound.set_nodelay(true);
    let (mut rd, wr) = channel.split();
    let (mut lr, mut lw) = inbound.into_split();
    let queued = Arc::new(AtomicUsize::new(0));
    let (overflow_tx, mut overflow_rx) = tokio::sync::watch::channel(false);
    let (tx, mut rx) = mpsc::unbounded_channel::<bytes::Bytes>();
    let wr = &wr;

    // Server → queue. Never waits on the local side.
    let from_server = {
        let queued = Arc::clone(&queued);
        async move {
            let mut tx = Some(tx);
            while let Some(msg) = rd.wait().await {
                match msg {
                    ChannelMsg::Data { data } => {
                        let n = data.len();
                        if queued.fetch_add(n, Ordering::AcqRel) + n > TUNNEL_QUEUE_CAP {
                            let _ = overflow_tx.send(true);
                            let _ = wr.close().await;
                            break;
                        }
                        match &tx {
                            Some(t) if t.send(data).is_ok() => {}
                            _ => break,
                        }
                    }
                    // Half close: the server has sent everything. Dropping
                    // `tx` lets the writer pass on what is queued and then
                    // end the client's read side; the client may still be
                    // sending, so keep the channel until the server closes.
                    ChannelMsg::Eof => tx = None,
                    ChannelMsg::Close => break,
                    _ => {}
                }
            }
        }
    };

    // Queue → local client.
    let to_local = {
        let queued = Arc::clone(&queued);
        async move {
            async fn wait_overflow(rx: &mut tokio::sync::watch::Receiver<bool>) {
                while !*rx.borrow_and_update() {
                    if rx.changed().await.is_err() {
                        std::future::pending::<()>().await;
                    }
                }
            }
            loop {
                let data = tokio::select! {
                    biased;
                    _ = wait_overflow(&mut overflow_rx) => break,
                    msg = rx.recv() => match msg {
                        Some(d) => d,
                        None => break,
                    }
                };
                let n = data.len();
                let write_ok = tokio::select! {
                    biased;
                    _ = wait_overflow(&mut overflow_rx) => false,
                    res = lw.write_all(&data) => res.is_ok(),
                };
                if !write_ok {
                    // Client went away or queue overflowed: tell the server,
                    // which ends the reader.
                    let _ = wr.close().await;
                    break;
                }
                queued.fetch_sub(n, Ordering::AcqRel);
            }
            drop(rx);
            let _ = lw.shutdown().await;
        }
    };

    // Local client → server.
    let to_server = async {
        let mut buf = vec![0u8; 64 * 1024];
        loop {
            match lr.read(&mut buf).await {
                Ok(0) | Err(_) => {
                    let _ = wr.eof().await;
                    break;
                }
                Ok(n) => {
                    if wr.data(&buf[..n]).await.is_err() {
                        break;
                    }
                }
            }
        }
    };

    // The connection is over once the server has closed the channel and all
    // of its data has reached the client (or the client went away, or
    // overflowed). The client's own EOF only half-closes it.
    let down = async {
        tokio::join!(from_server, to_local);
    };
    tokio::pin!(to_server);
    tokio::select! {
        _ = down => {}
        _ = async { (&mut to_server).await; std::future::pending::<()>().await } => {}
    }
    let _ = wr.close().await;
}

/// Open an interactive login shell with PTY and run until exit (CLI / local TTY).
// `&out[..]` is needed on Windows, where `out` is an owned Vec.
#[cfg_attr(not(windows), allow(clippy::redundant_slicing))]
pub async fn interactive_shell(handle: &client::Handle<ClientHandler>) -> Result<u32> {
    let (cols, rows) = term_size().unwrap_or((80, 24));
    let term = std::env::var("TERM").unwrap_or_else(|_| "xterm-256color".into());
    let channel = open_pty_shell(handle, cols as u32, rows as u32, &term).await?;

    enable_raw_mode().context("enable_raw_mode")?;
    struct RawGuard;
    impl Drop for RawGuard {
        fn drop(&mut self) {
            let _ = disable_raw_mode();
        }
    }
    let _guard = RawGuard;
    // Windows console: keys as VT sequences (arrows, F-keys, Alt), UTF-8 both
    // ways, and VT output processing. Restored when this returns.
    #[cfg(windows)]
    let _console = win_console::VtConsole::enable();

    let (mut read, write) = channel.split();
    let write = Arc::new(write);

    // Keyboard → server in its own task, so a big paste waiting for the
    // server's window never stops output from being shown.
    let stdin_task = {
        let write = Arc::clone(&write);
        tokio::spawn(async move {
            let mut stdin = tokio::io::stdin();
            let mut buf = vec![0u8; 32 * 1024];
            loop {
                match stdin.read(&mut buf).await {
                    // Windows: the console strips Ctrl+Z (0x1A) and reports
                    // an empty read. A console never ends, so it's a key
                    // press for the remote side (suspend in bash / vim).
                    #[cfg(windows)]
                    Ok(0) if std::io::IsTerminal::is_terminal(&std::io::stdin()) => {
                        if write.data(&[0x1Au8][..]).await.is_err() {
                            return;
                        }
                    }
                    Ok(0) => {
                        let _ = write.eof().await;
                        return;
                    }
                    Ok(n) => {
                        if write.data(&buf[..n]).await.is_err() {
                            return;
                        }
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                    Err(_) => return,
                }
            }
        })
    };

    // Window size: SIGWINCH on Unix; polled elsewhere (no resize signal).
    let resize_task = {
        let write = Arc::clone(&write);
        tokio::spawn(async move {
            let mut last = term_size().ok();
            #[cfg(unix)]
            {
                use tokio::signal::unix::{signal, SignalKind};
                let Ok(mut winch) = signal(SignalKind::window_change()) else {
                    return;
                };
                while winch.recv().await.is_some() {
                    if let Ok((c, r)) = term_size() {
                        if last != Some((c, r)) {
                            last = Some((c, r));
                            let _ = write.window_change(c as u32, r as u32, 0, 0).await;
                        }
                    }
                }
            }
            #[cfg(not(unix))]
            loop {
                tokio::time::sleep(Duration::from_millis(250)).await;
                if let Ok((c, r)) = term_size() {
                    if last != Some((c, r)) {
                        last = Some((c, r));
                        let _ = write.window_change(c as u32, r as u32, 0, 0).await;
                    }
                }
            }
        })
    };

    let mut stdout = tokio::io::stdout();
    // Windows: the console only accepts valid UTF-8. Remote output may be
    // split mid-character or not be UTF-8 at all (binary, GBK): carry the
    // incomplete tail over and replace invalid bytes instead of failing.
    #[cfg(windows)]
    let mut carry: Vec<u8> = Vec::new();
    let mut exit_status = 0u32;
    let mut drain_until: Option<tokio::time::Instant> = None;
    loop {
        let deadline = async {
            match drain_until {
                Some(t) => tokio::time::sleep_until(t).await,
                None => std::future::pending::<()>().await,
            }
        };
        tokio::select! {
            _ = deadline => break,
            msg = read.wait() => {
                match msg {
                    Some(ChannelMsg::Data { ref data })
                    | Some(ChannelMsg::ExtendedData { ref data, .. }) => {
                        #[cfg(windows)]
                        let out = {
                            carry.extend_from_slice(data);
                            utf8_lossy_prefix(&mut carry)
                        };
                        #[cfg(not(windows))]
                        let out: &[u8] = data;
                        stdout.write_all(&out[..]).await?;
                        stdout.flush().await?;
                    }
                    Some(ChannelMsg::ExitStatus { exit_status: code }) => {
                        exit_status = code;
                        drain_until.get_or_insert(tokio::time::Instant::now() + DRAIN_AFTER_EXIT);
                    }
                    Some(ChannelMsg::ExitSignal {
                        signal_name,
                        core_dumped,
                        error_message,
                        ..
                    }) => {
                        eprintln!(
                            "\r\n[ssh] remote exit-signal {signal_name:?} \
                             core_dumped={core_dumped} {error_message}\r"
                        );
                        if exit_status == 0 {
                            exit_status = 255;
                        }
                        drain_until.get_or_insert(tokio::time::Instant::now() + DRAIN_AFTER_EXIT);
                    }
                    Some(ChannelMsg::Eof) => {
                        drain_until.get_or_insert(tokio::time::Instant::now() + DRAIN_AFTER_EXIT);
                    }
                    Some(ChannelMsg::Close) | None => break,
                    _ => {}
                }
            }
        }
    }
    stdin_task.abort();
    resize_task.abort();

    let _ = handle
        .disconnect(Disconnect::ByApplication, "bye", "en")
        .await;

    Ok(exit_status)
}

/// Take the longest valid-UTF-8 prefix out of `buf` (invalid bytes become
/// U+FFFD), leaving at most an incomplete trailing character behind.
#[cfg_attr(not(windows), allow(dead_code))]
fn utf8_lossy_prefix(buf: &mut Vec<u8>) -> Vec<u8> {
    let mut out = String::new();
    let mut rest: &[u8] = buf;
    loop {
        match std::str::from_utf8(rest) {
            Ok(s) => {
                out.push_str(s);
                rest = &[];
                break;
            }
            Err(e) => {
                let (ok, after) = rest.split_at(e.valid_up_to());
                out.push_str(std::str::from_utf8(ok).unwrap_or_default());
                match e.error_len() {
                    Some(n) => {
                        out.push('\u{FFFD}');
                        rest = &after[n..];
                    }
                    // Incomplete character at the end: wait for more.
                    None => {
                        rest = after;
                        break;
                    }
                }
            }
        }
    }
    let keep = rest.to_vec();
    *buf = keep;
    out.into_bytes()
}

#[cfg(windows)]
mod win_console {
    use windows_sys::Win32::System::Console::{
        GetConsoleCP, GetConsoleMode, GetConsoleOutputCP, GetStdHandle, SetConsoleCP,
        SetConsoleMode, SetConsoleOutputCP, CONSOLE_MODE, DISABLE_NEWLINE_AUTO_RETURN,
        ENABLE_VIRTUAL_TERMINAL_INPUT, ENABLE_VIRTUAL_TERMINAL_PROCESSING, STD_INPUT_HANDLE,
        STD_OUTPUT_HANDLE,
    };

    /// Console modes / code pages before we changed them.
    pub struct VtConsole {
        input: Option<CONSOLE_MODE>,
        output: Option<CONSOLE_MODE>,
        cp_in: u32,
        cp_out: u32,
    }

    impl VtConsole {
        pub fn enable() -> Self {
            // SAFETY: plain Win32 console calls on this process's std handles;
            // a failing call just leaves that mode unchanged.
            unsafe {
                let hin = GetStdHandle(STD_INPUT_HANDLE);
                let hout = GetStdHandle(STD_OUTPUT_HANDLE);
                let mut input = None;
                let mut m: CONSOLE_MODE = 0;
                if GetConsoleMode(hin, &mut m) != 0 {
                    input = Some(m);
                    SetConsoleMode(hin, m | ENABLE_VIRTUAL_TERMINAL_INPUT);
                }
                let mut output = None;
                let mut m: CONSOLE_MODE = 0;
                if GetConsoleMode(hout, &mut m) != 0 {
                    output = Some(m);
                    // DISABLE_NEWLINE_AUTO_RETURN keeps a bare LF from also
                    // returning the cursor, matching a real VT. Older consoles
                    // reject it, so fall back to plain VT processing.
                    if SetConsoleMode(
                        hout,
                        m | ENABLE_VIRTUAL_TERMINAL_PROCESSING | DISABLE_NEWLINE_AUTO_RETURN,
                    ) == 0
                    {
                        SetConsoleMode(hout, m | ENABLE_VIRTUAL_TERMINAL_PROCESSING);
                    }
                }
                let (cp_in, cp_out) = (GetConsoleCP(), GetConsoleOutputCP());
                SetConsoleCP(65001);
                SetConsoleOutputCP(65001);
                Self {
                    input,
                    output,
                    cp_in,
                    cp_out,
                }
            }
        }
    }

    impl Drop for VtConsole {
        fn drop(&mut self) {
            // SAFETY: as above, restoring what we read in `enable`.
            unsafe {
                if let Some(m) = self.input {
                    SetConsoleMode(GetStdHandle(STD_INPUT_HANDLE), m);
                }
                if let Some(m) = self.output {
                    SetConsoleMode(GetStdHandle(STD_OUTPUT_HANDLE), m);
                }
                if self.cp_in != 0 {
                    SetConsoleCP(self.cp_in);
                }
                if self.cp_out != 0 {
                    SetConsoleOutputCP(self.cp_out);
                }
            }
        }
    }
}

/// Live SFTP checks against a throwaway local sshd. Run with
/// `TERN_TEST_SSH=user:pass@127.0.0.1:2222 cargo test -- --ignored sftp_live`.
#[cfg(test)]
mod utf8_tests {
    #[test]
    fn lossy_prefix_keeps_split_chars_and_replaces_garbage() {
        let mut b = "中".as_bytes()[..2].to_vec();
        assert!(super::utf8_lossy_prefix(&mut b).is_empty());
        b.extend_from_slice(&"中".as_bytes()[2..]);
        b.extend_from_slice(b"a\xffb");
        assert_eq!(super::utf8_lossy_prefix(&mut b), "中a\u{FFFD}b".as_bytes());
        assert!(b.is_empty());
    }
}

#[cfg(all(test, feature = "gui"))]
#[path = "session_tests.rs"]
mod regression_tests;

#[cfg(all(test, feature = "gui"))]
mod live_tests {
    use super::*;

    async fn open() -> Option<Arc<SshHandle>> {
        let spec = std::env::var("TERN_TEST_SSH").ok()?;
        let (cred, addr) = spec.rsplit_once('@')?;
        let (user, pass) = cred.split_once(':')?;
        let stream = TcpStream::connect(addr).await.expect("tcp connect");
        let verifier: HostKeyVerifier = Arc::new(|_| Box::pin(async { true }));
        let (h, _) = connect_and_auth(
            stream,
            SessionOptions {
                user,
                host: addr,
                identity: None,
                key_passphrase: None,
                password: Some(pass),
                ask_password: false,
                host_key_verifier: verifier,
            },
        )
        .await
        .expect("ssh auth");
        Some(Arc::new(h))
    }

    /// A forwarded connection whose local client stops reading must not
    /// stall the session (other tunnels, commands) — it is dropped instead.
    #[tokio::test]
    #[ignore]
    async fn forward_live() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let Some(h) = open().await else {
            eprintln!("TERN_TEST_SSH not set; skipping");
            return;
        };
        // A local "server" that floods whoever connects.
        let flood = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let flood_port = flood.local_addr().unwrap().port();
        tokio::spawn(async move {
            while let Ok((mut s, _)) = flood.accept().await {
                tokio::spawn(async move {
                    let chunk = vec![b'x'; 64 * 1024];
                    for _ in 0..1024 {
                        if s.write_all(&chunk).await.is_err() {
                            break;
                        }
                    }
                });
            }
        });
        let pick = || {
            let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            l.local_addr().unwrap()
        };
        let (fa, fb) = (pick(), pick());
        let (_stop_tx, stop) = tokio::sync::watch::channel(false);
        tokio::spawn(local_forward(
            Arc::clone(&h),
            fa,
            "127.0.0.1".into(),
            flood_port,
            stop.clone(),
        ));
        tokio::spawn(local_forward(
            Arc::clone(&h),
            fb,
            "127.0.0.1".into(),
            2222,
            stop,
        ));
        tokio::time::sleep(Duration::from_millis(200)).await;

        // Client A connects to the flood and never reads.
        let _stuck = TcpStream::connect(fa).await.unwrap();
        tokio::time::sleep(Duration::from_secs(2)).await;

        // The session still serves everything else promptly.
        let t0 = std::time::Instant::now();
        let mut b = TcpStream::connect(fb).await.unwrap();
        let mut banner = [0u8; 7];
        tokio::time::timeout(Duration::from_secs(5), b.read_exact(&mut banner))
            .await
            .expect("second tunnel stalled")
            .unwrap();
        assert_eq!(&banner, b"SSH-2.0");
        let echo = tokio::time::timeout(Duration::from_secs(5), exec_capture(&h, "echo ok"))
            .await
            .expect("exec stalled")
            .unwrap();
        assert_eq!(echo.trim(), "ok");
        assert!(t0.elapsed() < Duration::from_secs(5));

        // Half close: the far end finishes sending first; what the client
        // sends afterwards must still arrive.
        let half = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let half_port = half.local_addr().unwrap().port();
        let (got_tx, got_rx) = tokio::sync::oneshot::channel::<usize>();
        tokio::spawn(async move {
            let (mut s, _) = half.accept().await.unwrap();
            s.write_all(b"hi").await.unwrap();
            s.shutdown().await.unwrap();
            let mut all = Vec::new();
            let _ = s.read_to_end(&mut all).await;
            let _ = got_tx.send(all.len());
        });
        let fc = pick();
        let (_stop2, stop2) = tokio::sync::watch::channel(false);
        tokio::spawn(local_forward(
            Arc::clone(&h),
            fc,
            "127.0.0.1".into(),
            half_port,
            stop2,
        ));
        tokio::time::sleep(Duration::from_millis(200)).await;
        let mut c = TcpStream::connect(fc).await.unwrap();
        let mut first = Vec::new();
        tokio::time::timeout(Duration::from_secs(5), c.read_to_end(&mut first))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(first, b"hi");
        c.write_all(&vec![7u8; 1 << 20]).await.unwrap();
        c.shutdown().await.unwrap();
        let n = tokio::time::timeout(Duration::from_secs(10), got_rx)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(n, 1 << 20);
    }

    #[tokio::test]
    #[ignore]
    async fn sftp_live() {
        let Some(h) = open().await else {
            eprintln!("TERN_TEST_SSH not set; skipping");
            return;
        };
        let base = format!("/tmp/tern-remote-{}", std::process::id());
        let tmp = std::env::temp_dir().join(format!("tern-live-{}", std::process::id()));
        std::fs::create_dir_all(tmp.join("tree/sub")).unwrap();
        // 5 MB of pseudo-random data
        let mut big = vec![0u8; 5 * 1024 * 1024 + 123];
        let mut x: u32 = 1;
        for b in big.iter_mut() {
            x = x.wrapping_mul(1664525).wrapping_add(1013904223);
            *b = (x >> 24) as u8;
        }
        std::fs::write(tmp.join("tree/big.bin"), &big).unwrap();
        std::fs::write(tmp.join("tree/sub/a.txt"), b"hello").unwrap();

        // upload a tree
        let (tx, _rx) = mpsc::unbounded_channel();
        let dirs = vec![base.clone(), format!("{base}/sub")];
        let items = vec![
            UploadItem {
                local: tmp.join("tree/big.bin"),
                remote: format!("{base}/big.bin"),
                size: big.len() as u64,
            },
            UploadItem {
                local: tmp.join("tree/sub/a.txt"),
                remote: format!("{base}/sub/a.txt"),
                size: 5,
            },
        ];
        sftp_upload_tree(&h, &dirs, &items, tx.clone(), Default::default())
            .await
            .unwrap();

        // pipelined download round-trips byte for byte, no .part left behind
        let dl = tmp.join("dl.bin");
        sftp_download(
            &h,
            &format!("{base}/big.bin"),
            &dl,
            tx.clone(),
            Default::default(),
        )
        .await
        .unwrap();
        assert_eq!(std::fs::read(&dl).unwrap(), big);
        let leftovers: Vec<_> = std::fs::read_dir(&tmp)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().ends_with(".part"))
            .collect();
        assert!(leftovers.is_empty(), "partial files left behind");

        // a cancelled download leaves nothing behind
        let cancel = Arc::new(std::sync::atomic::AtomicBool::new(true));
        let dl2 = tmp.join("dl2.bin");
        let r = sftp_download(&h, &format!("{base}/big.bin"), &dl2, tx.clone(), cancel).await;
        assert!(r.unwrap_err().downcast_ref::<Cancelled>().is_some());
        assert!(!dl2.exists());
        assert_eq!(
            std::fs::read_dir(&tmp)
                .unwrap()
                .filter_map(|e| e.ok())
                .filter(|e| e.file_name().to_string_lossy().ends_with(".part"))
                .count(),
            0
        );

        // re-uploading over an existing file keeps its mode; a cancelled
        // upload leaves the old file untouched and no temp files
        exec_capture(&h, &format!("chmod 640 {base}/sub/a.txt"))
            .await
            .unwrap();
        std::fs::write(tmp.join("tree/sub/a.txt"), b"hello again").unwrap();
        let again = vec![UploadItem {
            local: tmp.join("tree/sub/a.txt"),
            remote: format!("{base}/sub/a.txt"),
            size: 11,
        }];
        sftp_upload_tree(&h, &[], &again, tx.clone(), Default::default())
            .await
            .unwrap();
        let mode = exec_capture(&h, &format!("stat -c %a {base}/sub/a.txt"))
            .await
            .unwrap();
        assert_eq!(mode.trim(), "640");
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(true));
        assert!(sftp_upload_tree(&h, &[], &again, tx.clone(), stop)
            .await
            .is_err());
        let ls = exec_capture(&h, &format!("ls -A {base}/sub"))
            .await
            .unwrap();
        assert_eq!(ls.trim(), "a.txt");

        // listing
        let names: Vec<String> = sftp_list(&h, &base)
            .await
            .unwrap()
            .1
            .into_iter()
            .map(|e| e.name)
            .collect();
        assert!(names.contains(&"big.bin".to_string()) && names.contains(&"sub".to_string()));

        // create refuses to clobber
        sftp_create_file(&h, &format!("{base}/new.txt"))
            .await
            .unwrap();
        assert!(sftp_create_file(&h, &format!("{base}/sub/a.txt"))
            .await
            .is_err());
        assert_eq!(
            sftp_read_text(&h, &format!("{base}/sub/a.txt"), 1 << 20)
                .await
                .unwrap()
                .0,
            "hello again"
        );

        // atomic save keeps the file mode and content
        let f = format!("{base}/mode.txt");
        sftp_write_file(&h, &f, b"one\r\ntwo\r\n", None)
            .await
            .unwrap();
        exec_capture(&h, &format!("chmod 751 {f}")).await.unwrap();
        sftp_write_file(&h, &f, b"three\r\n", None).await.unwrap();
        let mode = exec_capture(&h, &format!("stat -c %a {f}")).await.unwrap();
        assert_eq!(mode.trim(), "751");
        assert_eq!(
            sftp_read_text(&h, &f, 1 << 20).await.unwrap().0,
            "three\r\n"
        );

        // conflict: refuse to save over a file changed since it was loaded
        let (_, stamp) = sftp_read_text(&h, &f, 1 << 20).await.unwrap();
        exec_capture(&h, &format!("sleep 1; echo changed >> {f}"))
            .await
            .unwrap();
        let e = sftp_write_file(&h, &f, b"mine\n", stamp).await.unwrap_err();
        assert!(e.downcast_ref::<RemoteChanged>().is_some());
        sftp_write_file(&h, &f, b"mine\n", None).await.unwrap();

        // a symlink stays a symlink; its target gets the content
        let target = format!("{base}/real.txt");
        let link = format!("{base}/link.txt");
        sftp_write_file(&h, &target, b"v1\n", None).await.unwrap();
        exec_capture(&h, &format!("ln -s real.txt {link}"))
            .await
            .unwrap();
        sftp_write_file(&h, &link, b"v2\n", None).await.unwrap();
        let kind = exec_capture(&h, &format!("stat -c %F {link}"))
            .await
            .unwrap();
        assert_eq!(kind.trim(), "symbolic link");
        assert_eq!(
            sftp_read_text(&h, &target, 1 << 20).await.unwrap().0,
            "v2\n"
        );

        // a hard-linked file is written in place: both names see the change
        let hard = format!("{base}/hard.txt");
        exec_capture(&h, &format!("ln {target} {hard}"))
            .await
            .unwrap();
        sftp_write_file(&h, &target, b"v3\n", None).await.unwrap();
        assert_eq!(sftp_read_text(&h, &hard, 1 << 20).await.unwrap().0, "v3\n");
        let ls = exec_capture(&h, &format!("ls -A {base}")).await.unwrap();
        assert!(!ls.contains(".tmp"), "temp file left: {ls}");

        // someone else's (writable) file: written in place, owner kept
        if let Ok(foreign) = std::env::var("TERN_TEST_FOREIGN") {
            let before = exec_capture(&h, &format!("stat -c %U {foreign}"))
                .await
                .unwrap();
            sftp_write_file(&h, &foreign, b"edited\n", None)
                .await
                .unwrap();
            let after = exec_capture(&h, &format!("stat -c %U {foreign}"))
                .await
                .unwrap();
            assert_eq!(before, after);
            assert_eq!(
                sftp_read_text(&h, &foreign, 1 << 20).await.unwrap().0,
                "edited\n"
            );
        }

        // read cap: /dev/zero must not hang or eat memory
        assert!(sftp_read_text(&h, "/dev/zero", 1 << 20).await.is_err());

        // several files over one session; a failure reports what completed
        // and leaves no partial files
        let got = sftp_download_files(
            &h,
            vec![
                (format!("{base}/big.bin"), tmp.join("m1.bin")),
                (format!("{base}/sub/a.txt"), tmp.join("m2.txt")),
            ],
            tx.clone(),
            Default::default(),
        )
        .await;
        got.1.unwrap();
        assert_eq!(got.0, vec![true, true]);
        assert_eq!(std::fs::read(tmp.join("m1.bin")).unwrap(), big);
        assert_eq!(
            std::fs::read_to_string(tmp.join("m2.txt")).unwrap(),
            sftp_read_text(&h, &format!("{base}/sub/a.txt"), 1 << 20)
                .await
                .unwrap()
                .0
        );
        let got = sftp_download_files(
            &h,
            vec![
                (format!("{base}/nope.bin"), tmp.join("m3.bin")),
                (format!("{base}/sub/a.txt"), tmp.join("m4.txt")),
            ],
            tx.clone(),
            Default::default(),
        )
        .await;
        assert!(got.1.is_err());
        assert!(!got.0[0]);
        assert!(!tmp.join("m3.bin").exists());

        // a folder is never "saved over"
        let e = sftp_write_file(&h, &format!("{base}/sub"), b"x", None)
            .await
            .unwrap_err();
        assert!(format!("{e:#}").contains("folder"), "{e:#}");

        // many small files into new folders, in parallel
        std::fs::create_dir_all(tmp.join("many")).unwrap();
        let mut items = Vec::new();
        for i in 0..40 {
            let p = tmp.join(format!("many/f{i}.txt"));
            std::fs::write(&p, format!("file {i}")).unwrap();
            items.push(UploadItem {
                local: p,
                remote: format!("{base}/many/d{}/f{i}.txt", i % 3),
                size: format!("file {i}").len() as u64,
            });
        }
        let dirs: Vec<String> = std::iter::once(format!("{base}/many"))
            .chain((0..3).map(|d| format!("{base}/many/d{d}")))
            .collect();
        sftp_upload_tree(&h, &dirs, &items, tx.clone(), Default::default())
            .await
            .unwrap();
        let n = exec_capture(&h, &format!("cat {base}/many/*/f*.txt | wc -c"))
            .await
            .unwrap();
        let want: usize = (0..40).map(|i| format!("file {i}").len()).sum();
        assert_eq!(n.trim(), want.to_string());

        // recursive delete never follows a link to a folder
        let keep = format!("{base}-keep");
        exec_capture(
            &h,
            &format!("mkdir -p {keep} && echo k > {keep}/k && ln -s {keep} {base}/many/d1/link"),
        )
        .await
        .unwrap();

        // recursive delete
        sftp_remove(&h, &base).await.unwrap();
        let kept = exec_capture(&h, &format!("cat {keep}/k; rm -rf {keep}"))
            .await
            .unwrap();
        assert_eq!(kept.trim(), "k");
        let gone = exec_capture(&h, &format!("test -e {base} && echo yes || echo no"))
            .await
            .unwrap();
        assert_eq!(gone.trim(), "no");
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
