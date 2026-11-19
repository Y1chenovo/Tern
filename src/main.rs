//! Tern — interactive SSH client that keeps the TCP path off the local TUN/VPN.
//!
//! See README.md (中文) for motivation, bypass details, and usage.

// GUI build: WINDOWS subsystem so double-click does not flash a console and exit.
// CLI-only build (--no-default-features) keeps the default CONSOLE subsystem.
#![cfg_attr(all(windows, feature = "gui"), windows_subsystem = "windows")]

mod bypass;
#[cfg(feature = "gui")]
mod gui;
mod hostkeys;
mod profiles;
mod session;

use anyhow::{bail, Result};
use clap::Parser;
use std::net::IpAddr;
use std::path::PathBuf;

/// Interactive SSH client that binds out via a non-TUN interface.
#[derive(Debug, Parser)]
#[command(
    name = "tern",
    version,
    about = "SSH remote terminal that bypasses local TUN/VPN by binding to a physical interface",
    after_help = "Examples:\n  \
        tern\n  \
        tern --gui\n  \
        tern user@host\n  \
        tern -i ~/.ssh/id_ed25519 -p 22 --bind-iface eth0 user@vps\n  \
        tern --bind-addr 192.168.1.10 user@vps"
)]
struct Cli {
    /// Destination as [user@]host (omit to open GUI when built with gui feature)
    destination: Option<String>,

    /// Launch egui desktop GUI
    #[arg(long)]
    gui: bool,

    /// List local IPv4 adapters and show the automatic bind choice
    #[arg(long)]
    list_interfaces: bool,

    /// SSH port
    #[arg(short = 'p', long, default_value_t = 22)]
    port: u16,

    /// Remote username (overrides user@host)
    #[arg(short = 'l', long)]
    user: Option<String>,

    /// OpenSSH private key file (ed25519 / rsa / …)
    #[arg(short = 'i', long = "identity", value_name = "FILE")]
    identity: Option<PathBuf>,

    /// Bind outbound socket to this local IP (Option A)
    #[arg(long = "bind-addr", value_name = "IP")]
    bind_addr: Option<IpAddr>,

    /// Bind outbound socket to this interface via SO_BINDTODEVICE (Option B; Linux, needs CAP_NET_RAW)
    #[arg(long = "bind-iface", value_name = "IFACE")]
    bind_iface: Option<String>,

    /// Skip password / keyboard-interactive prompts (key-only)
    #[arg(long)]
    no_password: bool,

    /// Trust a host's key on first connection without asking (scripts).
    /// A key that differs from the saved one is still refused.
    #[arg(long)]
    accept_new_hostkey: bool,
}

/// Ask on the terminal; anything but "y"/"yes" (or no terminal) is a no.
fn ask_yes_no(question: &str) -> bool {
    use std::io::{IsTerminal, Write};
    if !std::io::stdin().is_terminal() {
        eprintln!(
            "{}\n(no terminal to ask on; pass --accept-new-hostkey to trust new hosts)",
            question.trim_end()
        );
        return false;
    }
    eprint!("{question}");
    let _ = std::io::stderr().flush();
    let mut line = String::new();
    if std::io::stdin().read_line(&mut line).is_err() {
        return false;
    }
    matches!(line.trim().to_ascii_lowercase().as_str(), "y" | "yes")
}

/// Password for scripting, read only from the TERN_PASSWORD environment
/// variable (never a command-line flag, which other users could see in the
/// process list).
fn env_password() -> Option<String> {
    std::env::var("TERN_PASSWORD")
        .ok()
        .filter(|p| !p.is_empty())
}

fn parse_destination(dest: &str, user_flag: Option<String>) -> Result<(String, String)> {
    let (user_part, raw_host) = if let Some((u, h)) = dest.rsplit_once('@') {
        if u.is_empty() {
            bail!("empty username in destination");
        }
        (Some(u.to_string()), h)
    } else {
        (None, dest)
    };
    let host_part = raw_host
        .strip_prefix('[')
        .and_then(|s| s.strip_suffix(']'))
        .unwrap_or(raw_host)
        .to_string();
    if host_part.is_empty() {
        bail!("empty host in destination");
    }
    let user = user_flag
        .or(user_part)
        .or_else(|| {
            std::env::var("USER")
                .ok()
                .or_else(|| std::env::var("USERNAME").ok())
        })
        .unwrap_or_else(|| "root".into());
    Ok((user, host_part))
}

/// Crash / fatal-error log path: %TEMP%\tern-crash.log on Windows (a
/// per-user folder); on Unix inside Tern's own config folder (owner-only),
/// never a fixed name in the shared /tmp.
fn crash_log_path() -> PathBuf {
    #[cfg(windows)]
    {
        if let Ok(temp) = std::env::var("TEMP").or_else(|_| std::env::var("TMP")) {
            return PathBuf::from(temp).join("tern-crash.log");
        }
        PathBuf::from("tern-crash.log")
    }
    #[cfg(not(windows))]
    {
        profiles::config_dir().join("tern-crash.log")
    }
}

fn write_crash_log(kind: &str, msg: &str) -> PathBuf {
    use std::io::Write;
    let path = crash_log_path();
    let body = format!(
        "Tern crash log\ntime: {}\nkind: {kind}\n----\n{msg}\n",
        chrono_like_now()
    );
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        // Never follow a planted symlink; readable by the owner only.
        opts.mode(0o600).custom_flags(nix_o_nofollow());
    }
    if let Ok(mut f) = opts.open(&path) {
        let _ = f.write_all(body.as_bytes());
    }
    path
}

#[cfg(unix)]
fn nix_o_nofollow() -> i32 {
    #[cfg(target_os = "linux")]
    {
        nix::libc::O_NOFOLLOW
    }
    #[cfg(target_os = "macos")]
    {
        0x0100
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        0
    }
}

fn chrono_like_now() -> String {
    // Avoid extra chrono dep; local time via system.
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format!("unix_ts={secs}")
}

#[cfg(all(windows, feature = "gui"))]
mod win_msg {
    #[link(name = "user32")]
    extern "system" {
        pub fn MessageBoxW(
            hwnd: *mut core::ffi::c_void,
            text: *const u16,
            caption: *const u16,
            utype: u32,
        ) -> i32;
    }
}

#[cfg(all(windows, feature = "gui"))]
fn show_windows_error(title: &str, message: &str) {
    use std::ffi::OsStr;
    use std::os::windows::ffi::OsStrExt;
    fn wide(s: &str) -> Vec<u16> {
        OsStr::new(s)
            .encode_wide()
            .chain(std::iter::once(0))
            .collect()
    }
    let title_w = wide(title);
    let msg_w = wide(message);
    const MB_OK: u32 = 0x0000_0000;
    const MB_ICONERROR: u32 = 0x0000_0010;
    unsafe {
        win_msg::MessageBoxW(
            std::ptr::null_mut(),
            msg_w.as_ptr(),
            title_w.as_ptr(),
            MB_OK | MB_ICONERROR,
        );
    }
}

fn install_panic_hook() {
    std::panic::set_hook(Box::new(|info| {
        let msg = format!("{info}");
        let path = write_crash_log("panic", &msg);
        #[cfg(all(windows, feature = "gui"))]
        {
            show_windows_error(
                "Tern crashed",
                &format!(
                    "The application panicked and could not continue.\n\n\
                     Details written to:\n{}\n\n{msg}",
                    path.display()
                ),
            );
        }
        // Best-effort stderr (no-op for WINDOWS subsystem with no console).
        eprintln!("panic: {msg}\nlog: {}", path.display());
    }));
}

fn main() {
    install_panic_hook();
    if let Err(e) = real_main() {
        let msg = format!("{e:#}");
        let path = write_crash_log("fatal", &msg);
        #[cfg(all(windows, feature = "gui"))]
        {
            show_windows_error(
                "Tern failed",
                &format!(
                    "Startup / run failed.\n\n\
                     Details written to:\n{}\n\n{msg}\n\n\
                     Tip: try running tern-cli.exe",
                    path.display()
                ),
            );
        }
        eprintln!("error: {msg}\nlog: {}", path.display());
        std::process::exit(1);
    }
}

fn real_main() -> Result<()> {
    let cli = Cli::parse();

    if cli.list_interfaces {
        for candidate in bypass::list_bind_candidates()? {
            let status = if candidate.is_tunnel {
                "virtual/VPN"
            } else {
                "physical"
            };
            println!("{}\t{}\t{status}", candidate.name, candidate.ip);
        }
        match bypass::auto_detect_non_tun_ipv4() {
            Ok(ip) => println!("Auto bind: {ip}"),
            Err(e) => println!("Auto bind unavailable: {e:#}"),
        }
        return Ok(());
    }

    // Double-click / no-args: launch GUI (console app would otherwise flash and exit).
    // Explicit --gui also launches GUI. CLI requires a destination.
    #[cfg(feature = "gui")]
    {
        let want_gui = cli.gui || cli.destination.is_none();
        if want_gui {
            let (prefill_user, prefill_host) = if let Some(ref dest) = cli.destination {
                let (u, h) = parse_destination(dest, cli.user.clone())?;
                (Some(u), Some(h))
            } else {
                // Prefer root for typical VPS password login; env USERNAME on Windows is local.
                (cli.user.clone().or_else(|| Some("root".into())), None)
            };
            match gui::run_gui(prefill_host, prefill_user, cli.port) {
                Ok(()) => return Ok(()),
                Err(e) => {
                    let msg = format!("GUI failed to start: {e}");
                    let path = write_crash_log("gui_init", &msg);
                    #[cfg(windows)]
                    {
                        show_windows_error(
                            "Tern GUI failed",
                            &format!(
                                "Could not open the GUI window (OpenGL / winit / egui).\n\n\
                                 Log file:\n{}\n\n{msg}\n\n\
                                 Try running tern-cli.exe",
                                path.display()
                            ),
                        );
                    }
                    bail!("{msg} (log: {})", path.display());
                }
            }
        }
    }

    #[cfg(not(feature = "gui"))]
    if cli.gui {
        bail!("this binary was built without GUI (--features gui); use a GUI build or CLI mode");
    }

    let dest = match cli.destination.clone() {
        Some(d) => d,
        None => {
            // CLI-only binary double-clicked: print usage clearly.
            eprintln!(
                "Tern (CLI)\n\n\
                 Usage: tern-cli.exe [OPTIONS] <user@host>\n\n\
                 Examples:\n  \
                   tern-cli.exe root@203.0.113.10\n  \
                   tern-cli.exe --bind-addr 192.168.1.10 -p 22 root@vps\n\n\
                 For the graphical UI, run tern.exe\n"
            );
            bail!("missing destination; CLI mode needs [user@]host");
        }
    };

    // CLI path needs its own tokio runtime.
    let rt = tokio::runtime::Runtime::new()?;
    let result = rt.block_on(cli_main(cli, &dest));
    // Don't wait for the blocking stdin reader thread (it would only return
    // after one more key press).
    rt.shutdown_background();
    result
}

async fn cli_main(cli: Cli, dest: &str) -> Result<()> {
    let password = env_password();
    if cli.bind_addr.is_some() && cli.bind_iface.is_some() {
        bail!("use only one of --bind-addr / --bind-iface");
    }

    let (user, host) = parse_destination(dest, cli.user)?;

    let remote = bypass::resolve_host(
        &host,
        cli.port,
        None,
        cli.bind_addr,
        cli.bind_iface.as_deref(),
    )
    .await?;

    eprintln!("[connect] {user}@{host}:{} → {remote}", cli.port);

    let stream =
        bypass::connect_bypassing_tun(remote, cli.bind_addr, cli.bind_iface.as_deref()).await?;

    if let Ok(local) = stream.local_addr() {
        eprintln!("[connect] local socket bound as {local}");
    }

    let key_host = host.clone();
    let key_port = cli.port;
    let accept_new = cli.accept_new_hostkey;
    let verifier: session::HostKeyVerifier = std::sync::Arc::new(move |key| {
        let host = key_host.clone();
        Box::pin(async move {
            let k = key.clone();
            let h = host.clone();
            let verdict =
                tokio::task::spawn_blocking(move || hostkeys::check(&h, key_port, &k)).await;
            let fp = hostkeys::fingerprint(&key);
            match verdict {
                Ok(Ok(hostkeys::Verdict::Match)) => true,
                Ok(Ok(v @ (hostkeys::Verdict::New | hostkeys::Verdict::OtherType(_)))) => {
                    // A new key type for a known host is always asked about
                    // (--accept-new-hostkey only covers hosts never seen).
                    let other = match v {
                        hostkeys::Verdict::OtherType(old) => Some(old),
                        _ => None,
                    };
                    let accepted = if accept_new && other.is_none() {
                        eprintln!("[hostkey] trusting new key for {host}:{key_port} ({fp})");
                        true
                    } else {
                        let question = match &other {
                            None => format!(
                                "[hostkey] first connection to {host}:{key_port}\n  \
                                 {} {fp}\nTrust this host? [y/N] ",
                                key.algorithm()
                            ),
                            Some(old) => format!(
                                "[hostkey] {host}:{key_port} now shows a {} key; saved so far: {} {}\n  \
                                 {} {fp}\nTrust this key too? [y/N] ",
                                key.algorithm(),
                                old.algorithm,
                                old.fingerprint,
                                key.algorithm()
                            ),
                        };
                        tokio::task::spawn_blocking(move || ask_yes_no(&question))
                            .await
                            .unwrap_or(false)
                    };
                    if !accepted {
                        eprintln!("[hostkey] not trusted; disconnecting");
                        return false;
                    }
                    let h = host.clone();
                    let k = key.clone();
                    match tokio::task::spawn_blocking(move || hostkeys::trust(&h, key_port, &k))
                        .await
                    {
                        Ok(Ok(())) => true,
                        Ok(Err(e)) => {
                            eprintln!("[hostkey] could not save key: {e:#}");
                            false
                        }
                        Err(_) => false,
                    }
                }
                Ok(Ok(hostkeys::Verdict::Changed(previous))) => {
                    eprintln!(
                        "[hostkey] REJECTED changed key for {host}:{key_port} (saved {}, received {fp})",
                        previous.fingerprint
                    );
                    false
                }
                Ok(Err(e)) => {
                    eprintln!("[hostkey] could not verify key for {host}: {e:#}");
                    false
                }
                Err(_) => false,
            }
        })
    });

    let (handle, _server_key) = session::connect_and_auth(
        stream,
        session::SessionOptions {
            user: &user,
            host: &host,
            identity: cli.identity.as_deref(),
            key_passphrase: None,
            password: password.as_deref(),
            ask_password: !cli.no_password,
            host_key_verifier: verifier,
        },
    )
    .await?;

    let code = session::interactive_shell(&handle).await?;
    if code != 0 {
        std::process::exit(code as i32);
    }
    Ok(())
}
