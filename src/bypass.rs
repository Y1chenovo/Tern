//! TUN/VPN bypass: bind outbound TCP to a physical (non-tunnel) interface.
//!
//! Why this is needed
//! ------------------
//! When a local VPN (WireGuard / OpenVPN / …) is up, the kernel default route
//! usually points into a `tun*` / `wg*` / `tap*` device. A normal `connect()`
//! therefore sends the SSH TCP handshake into the tunnel — which may be
//! undesirable (split-tunnel exceptions, double-encryption, broken routing to
//! the VPS itself, etc.).
//!
//! Bypass strategies
//! -----------------
//! * **Option A — bind to a local IPv4** (`--bind-addr` or auto-detect):
//!   create a `TcpSocket`, `bind(local_ip:0)`, then `connect(remote)`.
//!   Works on Linux and Windows (best-effort on Windows — pick the physical
//!   adapter IP explicitly if auto-detect is wrong).
//! * **Option B — `SO_BINDTODEVICE`** (`--bind-iface`, **Linux only**): force
//!   the socket onto a named interface. Requires `CAP_NET_RAW` (or root).

use anyhow::{bail, Context, Result};
use std::collections::HashSet;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::time::Duration;
use tokio::net::{TcpSocket, TcpStream};

/// Short interface-name prefixes matched only on token boundaries (`tun`, `tun0`,
/// `wg-mullvad`, `TAP-Windows`, etc.), avoiding false positives on words such as
/// `"Tuning"`, `"Opportunistic"`, or `"AWG"`.
const TUNNEL_TOKEN_PREFIXES: &[&str] = &["tun", "tap", "wg", "utun", "ppp"];

/// Specific VPN / tunnel driver keywords matched anywhere in the lowercase name/description.
const TUNNEL_KEYWORDS: &[&str] = &[
    "wintun",
    "wireguard",
    "nordlynx",
    "tailscale",
    "zerotier",
    "openvpn",
    "cloudflarewarp",
    "isatap",
    "teredo",
    "mihomo",
    "singbox",
    "sing-box",
];

/// How long to wait for the TCP handshake before giving up.
pub const CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);

/// Decide which local bind strategy to use, then open a TCP connection to
/// `remote` (giving up after [`CONNECT_TIMEOUT`]).
pub async fn connect_bypassing_tun(
    remote: SocketAddr,
    bind_addr: Option<IpAddr>,
    bind_iface: Option<&str>,
) -> Result<TcpStream> {
    tokio::time::timeout(
        CONNECT_TIMEOUT,
        connect_inner(remote, bind_addr, bind_iface),
    )
    .await
    .map_err(|_| {
        anyhow::anyhow!(
            "no answer from {remote} within {} s",
            CONNECT_TIMEOUT.as_secs()
        )
    })?
}

// Each platform block ends in `return` so the cfg'd-out blocks don't matter.
#[allow(clippy::needless_return)]
async fn connect_inner(
    remote: SocketAddr,
    bind_addr: Option<IpAddr>,
    bind_iface: Option<&str>,
) -> Result<TcpStream> {
    // Loopback never goes through a VPN, and pinning it to a physical NIC
    // (by device or by source IP) makes the connect hang or fail.
    if bind_addr.is_none() && bind_iface.is_none() && remote.ip().is_loopback() {
        eprintln!("[bypass] {remote} is loopback; connecting directly");
        return TcpStream::connect(remote)
            .await
            .with_context(|| format!("connect({remote}) failed"));
    }

    #[cfg(target_os = "linux")]
    {
        if let Some(iface) = bind_iface {
            eprintln!("[bypass] SO_BINDTODEVICE → iface={iface} (needs CAP_NET_RAW/root)");
            return connect_bind_to_device(remote, iface).await;
        }

        if let Some(ip) = bind_addr {
            // The source IP alone doesn't decide the route on Linux: pin the
            // interface that owns it too, when allowed.
            // Not for loopback / dummy (virtual source addresses); and if
            // pinning fails for any reason (e.g. policy routing sends this
            // source out of another interface), fall back to plain
            // source-IP binding, which is what was asked for.
            let iface = iface_of_ip(ip).filter(|n| n != "lo" && !n.starts_with("dummy"));
            if let Some(iface) = iface {
                match connect_bound_device(remote, ip, &iface).await {
                    Ok(s) => return Ok(s),
                    Err(e) if is_permission_error(&e) => eprintln!(
                        "[bypass] WARNING: SO_BINDTODEVICE({iface}) not permitted; using \
                         source-IP binding only, which a full-tunnel VPN may still capture. \
                         Grant once: sudo setcap cap_net_raw+ep <path to tern>"
                    ),
                    Err(e) => eprintln!(
                        "[bypass] pinning {iface} failed ({e:#}); using source-IP binding only"
                    ),
                }
            }
            eprintln!("[bypass] binding outbound socket to {ip}:0 before connect");
            return connect_bound(remote, ip).await;
        }

        // Auto: binding only the source IP does not change Linux's routing
        // decision (a VPN's catch-all routes would still win), so also pin the
        // socket to the physical interface when we are allowed to.
        if remote.is_ipv6() {
            if let Ok((iface, _)) = auto_detect_non_tun_iface() {
                eprintln!("[bypass] auto-detected non-TUN interface {iface} for IPv6");
                match connect_bind_to_device(remote, &iface).await {
                    Ok(s) => return Ok(s),
                    Err(e) if is_permission_error(&e) => eprintln!(
                        "[bypass] WARNING: SO_BINDTODEVICE({iface}) not permitted for IPv6; \
                         connecting directly. Grant once: sudo setcap cap_net_raw+ep <path to tern>"
                    ),
                    Err(e) => return Err(e),
                }
            } else {
                eprintln!(
                    "[bypass] no non-TUN IPv4 interface found for IPv6 {remote}; connecting directly"
                );
            }
            return TcpStream::connect(remote)
                .await
                .with_context(|| format!("connect({remote}) failed"));
        }
        let (iface, ip) =
            auto_detect_non_tun_iface().context("auto-detect non-TUN interface IPv4 failed")?;
        eprintln!("[bypass] auto-detected non-TUN interface {iface} ({ip})");
        match connect_bind_to_device(remote, &iface).await {
            Ok(s) => return Ok(s),
            Err(e) if is_permission_error(&e) => {
                eprintln!(
                    "[bypass] WARNING: SO_BINDTODEVICE({iface}) not permitted; falling back to \
                     source-IP binding, which a full-tunnel VPN may still capture. For a complete \
                     bypass grant the capability once: sudo setcap cap_net_raw+ep <path to tern>"
                );
            }
            Err(e) => return Err(e),
        }
        eprintln!("[bypass] binding outbound socket to {ip}:0 before connect");
        return connect_bound(remote, IpAddr::V4(ip)).await;
    }

    #[cfg(windows)]
    {
        if bind_iface.is_some() {
            eprintln!(
                "[bypass] WARNING: --bind-iface / SO_BINDTODEVICE is Linux-only; \
                 ignoring and using bind-to-IP (Option A)"
            );
        }
        if bind_addr.is_none() && remote.is_ipv6() {
            eprintln!(
                "[bypass] {remote} is IPv6 and auto-detect only finds IPv4; connecting directly \
                 (pass an IPv6 bind address explicitly to pin an interface)"
            );
            return TcpStream::connect(remote)
                .await
                .with_context(|| format!("connect({remote}) failed"));
        }
        let local_ip = if let Some(ip) = bind_addr {
            ip
        } else {
            let ip =
                auto_detect_non_tun_ipv4().context("auto-detect non-TUN interface IPv4 failed")?;
            eprintln!("[bypass] auto-detected non-TUN bind address: {ip}");
            IpAddr::V4(ip)
        };
        eprintln!("[bypass] binding outbound socket to {local_ip}:0 before connect");
        return connect_bound(remote, local_ip).await;
    }

    #[cfg(not(any(target_os = "linux", windows)))]
    {
        if bind_iface.is_some() {
            eprintln!("[bypass] WARNING: interface binding is Linux-only; using local IP binding");
        }
        if bind_addr.is_none() && remote.is_ipv6() {
            eprintln!(
                "[bypass] {remote} is IPv6 and auto-detect only finds IPv4; connecting directly"
            );
            return TcpStream::connect(remote)
                .await
                .with_context(|| format!("connect({remote}) failed"));
        }
        let local_ip = match bind_addr {
            Some(ip) => ip,
            None => IpAddr::V4(auto_detect_non_tun_ipv4()?),
        };
        connect_bound(remote, local_ip).await
    }
}

/// Option A: bind to a specific local IP, then connect.
async fn connect_bound(remote: SocketAddr, local: IpAddr) -> Result<TcpStream> {
    if remote.is_ipv4() != local.is_ipv4() {
        bail!("bind address {local} and remote {remote} use different IP families");
    }
    let socket = match local {
        IpAddr::V4(_) => TcpSocket::new_v4()?,
        IpAddr::V6(_) => TcpSocket::new_v6()?,
    };
    socket.bind(SocketAddr::new(local, 0)).with_context(|| {
        format!("bind({local}:0) failed — is that address present on a local interface?")
    })?;
    let stream = socket
        .connect(remote)
        .await
        .with_context(|| format!("connect({remote}) after bind({local}) failed"))?;
    Ok(stream)
}

/// Resolve `host` without using a VPN-hijacked system resolver when `dns` is set.
/// An IP literal always skips DNS. `dns` accepts `1.1.1.1` or `1.1.1.1:53`.
pub async fn resolve_host(
    host: &str,
    port: u16,
    dns: Option<&str>,
    bind_addr: Option<IpAddr>,
    bind_iface: Option<&str>,
) -> Result<SocketAddr> {
    let host = host.trim();
    let host = host
        .strip_prefix('[')
        .and_then(|s| s.strip_suffix(']'))
        .unwrap_or(host);
    if let Ok(ip) = host.parse::<IpAddr>() {
        if bind_addr.is_some_and(|local| local.is_ipv4() != ip.is_ipv4()) {
            bail!("host {host} and bind address use different IP families");
        }
        return Ok(SocketAddr::new(ip, port));
    }
    let Some(dns) = dns.map(str::trim).filter(|s| !s.is_empty()) else {
        let addresses: Vec<_> = tokio::net::lookup_host((host, port))
            .await
            .with_context(|| format!("DNS lookup {host}:{port}"))?
            .collect();
        if let Some(local) = bind_addr {
            let want_ipv4 = local.is_ipv4();
            return addresses
                .into_iter()
                .find(|addr| addr.is_ipv4() == want_ipv4)
                .with_context(|| {
                    format!(
                        "no {} address for {host}",
                        if want_ipv4 { "IPv4" } else { "IPv6" }
                    )
                });
        }
        return addresses
            .iter()
            .copied()
            .find(|addr| addr.is_ipv4())
            .or_else(|| addresses.into_iter().next())
            .with_context(|| format!("no address found for {host}"));
    };
    if bind_addr.is_some_and(|ip| ip.is_ipv6()) {
        bail!("custom DNS currently supports A records only; use an IPv4 bind address");
    }
    let server = if dns.contains(':') {
        dns.parse()
            .with_context(|| format!("invalid DNS server {dns}"))?
    } else {
        SocketAddr::new(
            dns.parse()
                .with_context(|| format!("invalid DNS server {dns}"))?,
            53,
        )
    };
    if !server.is_ipv4() {
        bail!("custom DNS server must be IPv4 when using an IPv4 bind address");
    }
    // Send the query out of the same physical interface as the connection,
    // or a full-tunnel VPN could still capture (and answer) it.
    let (local_ip, iface): (Ipv4Addr, Option<String>) = if let Some(IpAddr::V4(ip)) = bind_addr {
        #[cfg(target_os = "linux")]
        {
            (ip, iface_of_ipv4(ip))
        }
        #[cfg(not(target_os = "linux"))]
        {
            (ip, None)
        }
    } else if let Some(iface) = bind_iface {
        #[cfg(target_os = "linux")]
        {
            (
                ipv4_of_iface(iface)
                    .with_context(|| format!("no IPv4 address on bind interface {iface}"))?,
                Some(iface.to_string()),
            )
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = iface;
            (auto_detect_non_tun_ipv4()?, None)
        }
    } else {
        #[cfg(target_os = "linux")]
        {
            let (name, ip) = auto_detect_non_tun_iface()?;
            (ip, Some(name))
        }
        #[cfg(not(target_os = "linux"))]
        {
            (auto_detect_non_tun_ipv4()?, None)
        }
    };
    let ip = query_a(host, server, local_ip, iface.as_deref()).await?;
    Ok(SocketAddr::new(IpAddr::V4(ip), port))
}

async fn query_a(
    host: &str,
    server: SocketAddr,
    local_ip: Ipv4Addr,
    iface: Option<&str>,
) -> Result<Ipv4Addr> {
    let labels: Vec<_> = host.trim_end_matches('.').split('.').collect();
    if host.len() > 253
        || labels
            .iter()
            .any(|label| label.is_empty() || label.len() > 63)
    {
        bail!("invalid DNS name: {host}");
    }
    // Unpredictable query id (plus a random source port from the kernel), so
    // an off-path attacker can't easily forge the answer.
    let mut idb = [0u8; 2];
    getrandom::getrandom(&mut idb).context("random DNS query id")?;
    let id = u16::from_be_bytes(idb);
    let packet = encode_a_query(id, host);
    let sock = tokio::net::UdpSocket::bind(SocketAddr::new(IpAddr::V4(local_ip), 0)).await?;
    #[cfg(target_os = "linux")]
    if let Some(iface) = iface {
        use nix::sys::socket::{setsockopt, sockopt::BindToDevice};
        use std::os::fd::AsFd;
        if let Err(e) = setsockopt(
            &sock.as_fd(),
            BindToDevice,
            &std::ffi::OsString::from(iface),
        ) {
            eprintln!(
                "[bypass] DNS: SO_BINDTODEVICE({iface}) not applied ({e}); using source-IP binding"
            );
        }
    }
    #[cfg(not(target_os = "linux"))]
    let _ = iface;
    sock.connect(server).await?;
    sock.send(&packet).await?;
    // Keep listening until the deadline: a stray or forged datagram with the
    // wrong id must not make the lookup fail (or succeed).
    let start = tokio::time::Instant::now();
    let deadline = start + Duration::from_secs(4);
    // One retransmission 1.5 s after the first send (UDP may be dropped);
    // stray datagrams in between don't push it back.
    let resend_at = start + Duration::from_millis(1500);
    let mut buf = [0u8; 4096];
    let mut resent = false;
    loop {
        let wait = if resent { deadline } else { resend_at };
        match tokio::time::timeout_at(wait, sock.recv(&mut buf)).await {
            Ok(Ok(n)) => {
                if let Some(ip) = decode_first_a(&buf[..n], id) {
                    return Ok(ip);
                }
                if is_reply_to(&buf[..n], id) {
                    let rcode = buf[3] & 0x0f;
                    if rcode == 3 {
                        bail!("DNS {server}: no such host {host} (NXDOMAIN)");
                    } else if rcode != 0 {
                        bail!("DNS {server} error (rcode {rcode}) for {host}");
                    }
                    bail!("DNS {server} returned no A record for {host}");
                }
            }
            Ok(Err(e)) => return Err(e).context("DNS receive"),
            Err(_) if !resent && tokio::time::Instant::now() < deadline => {
                resent = true;
                sock.send(&packet).await?;
            }
            Err(_) => bail!("DNS query to {server} timed out"),
        }
    }
}

/// A well-formed response carrying our id (even if it has no A record).
fn is_reply_to(buf: &[u8], id: u16) -> bool {
    buf.len() >= 12 && u16::from_be_bytes([buf[0], buf[1]]) == id && buf[2] & 0x80 != 0
}

fn encode_a_query(id: u16, host: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(64);
    out.extend_from_slice(&id.to_be_bytes());
    out.extend_from_slice(&[0x01, 0x00, 0x00, 0x01, 0, 0, 0, 0, 0, 0]);
    for label in host.trim_end_matches('.').split('.') {
        let bytes = label.as_bytes();
        out.push(bytes.len().min(63) as u8);
        out.extend_from_slice(&bytes[..bytes.len().min(63)]);
    }
    out.extend_from_slice(&[0, 0, 1, 0, 1]);
    out
}

fn decode_first_a(buf: &[u8], expected_id: u16) -> Option<Ipv4Addr> {
    if buf.len() < 12
        || u16::from_be_bytes([buf[0], buf[1]]) != expected_id
        || buf[2] & 0x80 == 0
        || buf[3] & 0x0f != 0
    {
        return None;
    }
    let qd = u16::from_be_bytes([buf[4], buf[5]]) as usize;
    let an = u16::from_be_bytes([buf[6], buf[7]]) as usize;
    let mut i = 12;
    for _ in 0..qd {
        i = skip_name(buf, i)?;
        i = i.checked_add(4)?;
    }
    for _ in 0..an {
        i = skip_name(buf, i)?;
        if i + 10 > buf.len() {
            return None;
        }
        let kind = u16::from_be_bytes([buf[i], buf[i + 1]]);
        let len = u16::from_be_bytes([buf[i + 8], buf[i + 9]]) as usize;
        i += 10;
        if i + len > buf.len() {
            return None;
        }
        if kind == 1 && len == 4 {
            return Some(Ipv4Addr::new(buf[i], buf[i + 1], buf[i + 2], buf[i + 3]));
        }
        i += len;
    }
    None
}

/// Index just past the name at `i`. Labels always move forward and a
/// compression pointer ends the name in place, so the loop is bounded by the
/// buffer; there is no label-count limit (long names have many labels).
fn skip_name(buf: &[u8], mut i: usize) -> Option<usize> {
    loop {
        let len = *buf.get(i)?;
        if len == 0 {
            return Some(i + 1);
        }
        match len & 0xc0 {
            0xc0 => return (i + 2 <= buf.len()).then_some(i + 2),
            0x00 => i += 1 + len as usize,
            _ => return None,
        }
    }
}

#[cfg(target_os = "linux")]
fn is_permission_error(e: &anyhow::Error) -> bool {
    e.chain().any(|c| {
        c.downcast_ref::<nix::errno::Errno>()
            .is_some_and(|n| matches!(n, nix::errno::Errno::EPERM | nix::errno::Errno::EACCES))
    })
}

/// Like [`auto_detect_non_tun_ipv4`], but also returns the interface name.
#[cfg(target_os = "linux")]
fn auto_detect_non_tun_iface() -> Result<(String, Ipv4Addr)> {
    let mut routes: Vec<_> = parse_default_routes()
        .unwrap_or_default()
        .into_iter()
        .filter(|r| !is_linux_tunnel_iface(&r.iface))
        .collect();
    routes.sort_by_key(|r| r.metric);
    for route in &routes {
        if let Some(ip) = ipv4_of_iface(&route.iface) {
            return Ok((route.iface.clone(), ip));
        }
    }
    let mut ifaces = list_ipv4_ifaces()?;
    ifaces.sort_by_key(|(name, _)| iface_preference_score(name));
    ifaces
        .into_iter()
        .find(|(name, _)| !is_linux_tunnel_iface(name) && name != "lo")
        .context("no suitable non-TUN IPv4 found; pass --bind-addr or --bind-iface explicitly")
}

/// Option B: `SO_BINDTODEVICE` on Linux.
#[cfg(target_os = "linux")]
async fn connect_bind_to_device(remote: SocketAddr, iface: &str) -> Result<TcpStream> {
    use nix::sys::socket::{setsockopt, sockopt::BindToDevice};
    use std::ffi::OsString;
    use std::os::fd::AsFd;

    let socket = if remote.is_ipv4() {
        TcpSocket::new_v4()?
    } else {
        TcpSocket::new_v6()?
    };

    setsockopt(&socket.as_fd(), BindToDevice, &OsString::from(iface)).with_context(|| {
        format!(
            "SO_BINDTODEVICE({iface}) failed — need CAP_NET_RAW/root, \
             and the interface must exist"
        )
    })?;

    let stream = socket
        .connect(remote)
        .await
        .with_context(|| format!("connect({remote}) after BINDTODEVICE({iface}) failed"))?;
    Ok(stream)
}

/// Source IP and interface both pinned (explicit `--bind-addr` on Linux).
#[cfg(target_os = "linux")]
async fn connect_bound_device(remote: SocketAddr, local: IpAddr, iface: &str) -> Result<TcpStream> {
    use nix::sys::socket::{setsockopt, sockopt::BindToDevice};
    use std::os::fd::AsFd;
    if remote.is_ipv4() != local.is_ipv4() {
        bail!("bind address {local} and remote {remote} use different IP families");
    }
    let socket = match local {
        IpAddr::V4(_) => TcpSocket::new_v4()?,
        IpAddr::V6(_) => TcpSocket::new_v6()?,
    };
    setsockopt(
        &socket.as_fd(),
        BindToDevice,
        &std::ffi::OsString::from(iface),
    )
    .with_context(|| format!("SO_BINDTODEVICE({iface}) failed"))?;
    socket
        .bind(SocketAddr::new(local, 0))
        .with_context(|| format!("bind({local}:0) failed"))?;
    eprintln!("[bypass] bound to {local} on {iface} (SO_BINDTODEVICE)");
    socket
        .connect(remote)
        .await
        .with_context(|| format!("connect({remote}) via {iface} failed"))
}

/// Returns true if `name` (or an adapter description) looks like a tunnel / VPN virtual NIC.
pub fn is_tunnel_iface(name: &str) -> bool {
    let lower = name.trim().to_ascii_lowercase();
    if lower.is_empty() {
        return false;
    }
    if TUNNEL_KEYWORDS.iter().any(|kw| lower.contains(kw)) {
        return true;
    }
    // ZeroTier interfaces on Linux/macOS/BSD are `zt` + base32 network/node ID (e.g. `ztcf23aa10`).
    if lower.starts_with("zt")
        && lower.len() >= 8
        && lower[2..].bytes().all(|b| b.is_ascii_alphanumeric())
    {
        return true;
    }
    for tok in lower.split(|c: char| !c.is_ascii_alphanumeric()) {
        if tok.is_empty() {
            continue;
        }
        for &p in TUNNEL_TOKEN_PREFIXES {
            if tok == p
                || tok
                    .strip_prefix(p)
                    .is_some_and(|rest| rest.starts_with(|c: char| c.is_ascii_digit()))
            {
                return true;
            }
        }
    }
    false
}

/// Inspect Linux `/sys/class/net/<name>` to catch custom-named TUN/TAP, WireGuard,
/// PPP, or IP-in-IP tunnel devices (e.g. `Meta`, `clash`, `corp-vpn`).
#[cfg(target_os = "linux")]
fn is_linux_sysfs_tunnel(name: &str) -> bool {
    use std::path::Path;
    if name.is_empty() || name.contains(['/', '\0']) {
        return false;
    }
    let sys_dir = Path::new("/sys/class/net").join(name);
    if sys_dir.join("tun_flags").exists() {
        return true;
    }
    if let Ok(raw_type) = std::fs::read_to_string(sys_dir.join("type")) {
        if let Ok(arp_type) = raw_type.trim().parse::<u32>() {
            // 512=PPP, 768=IPIP, 769=IP6IP6, 776=SIT, 778=GRE, 65534=NONE (TUN/WireGuard/Tailscale)
            if matches!(arp_type, 512 | 768 | 769 | 776 | 778 | 65534) {
                return true;
            }
        }
    }
    if let Ok(uevent) = std::fs::read_to_string(sys_dir.join("uevent")) {
        if uevent.lines().any(|l| {
            matches!(
                l.trim(),
                "DEVTYPE=wireguard" | "DEVTYPE=ppp" | "DEVTYPE=ipip" | "DEVTYPE=gre"
            )
        }) {
            return true;
        }
    }
    false
}

#[cfg(target_os = "linux")]
fn is_linux_tunnel_iface(name: &str) -> bool {
    is_tunnel_iface(name) || is_linux_sysfs_tunnel(name)
}

/// A candidate local interface + IPv4 for the GUI picker / CLI auto-detect.
#[derive(Debug, Clone)]
pub struct IfaceCandidate {
    pub name: String,
    pub ip: Ipv4Addr,
    pub is_tunnel: bool,
    /// Effective metric of this interface's IPv4 default route; `u32::MAX`
    /// when it has none. Lower wins, as in the OS's own route choice.
    pub metric: u32,
}

/// List IPv4 interfaces (including tunnels) for the GUI picker.
pub fn list_bind_candidates() -> Result<Vec<IfaceCandidate>> {
    #[cfg(windows)]
    let mut out = list_windows_bind_candidates()?;
    #[cfg(not(windows))]
    let mut out: Vec<IfaceCandidate> = {
        #[cfg(target_os = "linux")]
        let routes = parse_default_routes().unwrap_or_default();
        list_ipv4_ifaces()?
            .into_iter()
            .map(|(name, ip)| {
                #[cfg(target_os = "linux")]
                let tunnel_match = is_linux_tunnel_iface(&name);
                #[cfg(not(target_os = "linux"))]
                let tunnel_match = is_tunnel_iface(&name);
                let is_tunnel = tunnel_match
                    || name == "lo"
                    || name.eq_ignore_ascii_case("Loopback Pseudo-Interface 1");
                #[cfg(target_os = "linux")]
                let metric = routes
                    .iter()
                    .filter(|r| r.iface == name)
                    .map(|r| r.metric)
                    .min()
                    .unwrap_or(u32::MAX);
                #[cfg(not(target_os = "linux"))]
                let metric = u32::MAX;
                IfaceCandidate {
                    name,
                    ip,
                    is_tunnel,
                    metric,
                }
            })
            .collect()
    };
    // Physical first, then the interface the OS itself would route through,
    // then the name heuristic for adapters without a default route.
    out.sort_by_key(|c| {
        (
            c.is_tunnel,
            c.metric,
            iface_preference_score(&c.name),
            c.name.clone(),
        )
    });
    Ok(out)
}

/// Windows adapter names can be arbitrary (for example, a VPN called "Meta").
/// Check the OS hardware flag and interface type instead of trusting the name.
#[cfg(windows)]
fn list_windows_bind_candidates() -> Result<Vec<IfaceCandidate>> {
    use windows_sys::Win32::NetworkManagement::IpHelper::{
        GetIfEntry2, IF_TYPE_ETHERNET_CSMACD, IF_TYPE_IEEE80211, MIB_IF_ROW2,
    };
    use windows_sys::Win32::NetworkManagement::Ndis::IfOperStatusUp;

    let mut out = Vec::new();
    let mut seen = HashSet::new();
    let metrics = windows_default_route_metrics();
    for iface in if_addrs::get_if_addrs().context("enumerate network interfaces")? {
        let ip = match iface.addr {
            if_addrs::IfAddr::V4(v4) => v4.ip,
            _ => continue,
        };
        if ip.is_loopback() || ip.is_unspecified() || !seen.insert((iface.name.clone(), ip)) {
            continue;
        }
        let metric = iface
            .index
            .and_then(|i| metrics.get(&i).copied())
            .unwrap_or(u32::MAX);
        let mut is_tunnel = true;
        if let Some(index) = iface.index {
            let mut row = MIB_IF_ROW2 {
                InterfaceIndex: index,
                ..Default::default()
            };
            if unsafe { GetIfEntry2(&mut row) } == 0 {
                // MIB_IF_ROW2 declares HardwareInterface as the first bit.
                let hardware = row.InterfaceAndOperStatusFlags._bitfield & 1 != 0;
                let desc_end = row
                    .Description
                    .iter()
                    .position(|&c| c == 0)
                    .unwrap_or(row.Description.len());
                let description = String::from_utf16_lossy(&row.Description[..desc_end]);
                is_tunnel = !hardware
                    || row.OperStatus != IfOperStatusUp
                    || !matches!(row.Type, IF_TYPE_ETHERNET_CSMACD | IF_TYPE_IEEE80211)
                    || is_tunnel_iface(&description)
                    || description.to_ascii_lowercase().contains("virtual")
                    || is_tunnel_iface(&iface.name)
                    || is_benchmark_ip(ip);
            }
        }
        out.push(IfaceCandidate {
            name: iface.name,
            ip,
            is_tunnel,
            metric,
        });
    }
    Ok(out)
}

/// Interface index → lowest effective metric (route + interface metric) of
/// its IPv4 default routes, the same sum Windows uses to pick a route.
#[cfg(windows)]
fn windows_default_route_metrics() -> std::collections::HashMap<u32, u32> {
    use windows_sys::Win32::NetworkManagement::IpHelper::{
        FreeMibTable, GetIpForwardTable2, GetIpInterfaceEntry, InitializeIpInterfaceEntry,
        MIB_IPFORWARD_ROW2, MIB_IPFORWARD_TABLE2, MIB_IPINTERFACE_ROW,
    };
    use windows_sys::Win32::Networking::WinSock::AF_INET;

    let mut out = std::collections::HashMap::new();
    // SAFETY: the table is allocated by GetIpForwardTable2, read only within
    // its NumEntries, and released with FreeMibTable.
    unsafe {
        let mut table: *mut MIB_IPFORWARD_TABLE2 = std::ptr::null_mut();
        if GetIpForwardTable2(AF_INET, &mut table) != 0 || table.is_null() {
            return out;
        }
        let first = std::ptr::addr_of!((*table).Table).cast::<MIB_IPFORWARD_ROW2>();
        let rows = std::slice::from_raw_parts(first, (*table).NumEntries as usize);
        for row in rows {
            if row.DestinationPrefix.PrefixLength != 0 {
                continue;
            }
            let mut ifrow: MIB_IPINTERFACE_ROW = std::mem::zeroed();
            InitializeIpInterfaceEntry(&mut ifrow);
            ifrow.Family = AF_INET;
            ifrow.InterfaceIndex = row.InterfaceIndex;
            let if_metric = if GetIpInterfaceEntry(&mut ifrow) == 0 {
                ifrow.Metric
            } else {
                0
            };
            let m = row.Metric.saturating_add(if_metric);
            out.entry(row.InterfaceIndex)
                .and_modify(|v: &mut u32| *v = (*v).min(m))
                .or_insert(m);
        }
        FreeMibTable(table as *const core::ffi::c_void);
    }
    out
}

#[cfg(windows)]
fn is_benchmark_ip(ip: Ipv4Addr) -> bool {
    let [a, b, ..] = ip.octets();
    a == 198 && (b == 18 || b == 19)
}

/// Auto-detect: pick an IPv4 on the best non-TUN default-route interface.
pub fn auto_detect_non_tun_ipv4() -> Result<Ipv4Addr> {
    #[cfg(windows)]
    {
        let chosen = list_bind_candidates()?
            .into_iter()
            .find(|c| !c.is_tunnel)
            .context("no active physical Ethernet/Wi-Fi IPv4 found; select a bind IP manually")?;
        eprintln!(
            "[bypass] using physical Windows adapter={} ip={} metric={}",
            chosen.name,
            chosen.ip,
            if chosen.metric == u32::MAX {
                "-".to_string()
            } else {
                chosen.metric.to_string()
            }
        );
        Ok(chosen.ip)
    }

    #[cfg(target_os = "linux")]
    {
        let candidates = parse_default_routes().unwrap_or_default();

        let mut non_tun_defaults: Vec<_> = candidates
            .into_iter()
            .filter(|r| !is_linux_tunnel_iface(&r.iface))
            .collect();
        non_tun_defaults.sort_by_key(|r| r.metric);

        for route in &non_tun_defaults {
            if let Some(ip) = ipv4_of_iface(&route.iface) {
                eprintln!(
                    "[bypass] using non-TUN default-route iface={} metric={} ip={}",
                    route.iface, route.metric, ip
                );
                return Ok(ip);
            }
        }
    }

    #[cfg(not(windows))]
    {
        // Fallback when no usable non-tunnel default route exists.
        let mut ifaces = list_ipv4_ifaces()?;
        ifaces.sort_by_key(|(name, _)| iface_preference_score(name));

        for (name, ip) in &ifaces {
            #[cfg(target_os = "linux")]
            let tunnel = is_linux_tunnel_iface(name);
            #[cfg(not(target_os = "linux"))]
            let tunnel = is_tunnel_iface(name);
            if tunnel || name == "lo" || name.eq_ignore_ascii_case("Loopback Pseudo-Interface 1") {
                continue;
            }
            eprintln!("[bypass] fallback iface={name} ip={ip}");
            return Ok(*ip);
        }

        bail!("no suitable non-TUN IPv4 found; pass --bind-addr or --bind-iface explicitly");
    }
}

#[cfg(target_os = "linux")]
#[derive(Debug, Clone)]
struct RouteEntry {
    iface: String,
    metric: u32,
}

/// Parse default routes from `/proc/net/route` (Linux only).
#[cfg(target_os = "linux")]
fn parse_default_routes() -> Result<Vec<RouteEntry>> {
    use std::fs;
    use std::path::Path;

    let path = Path::new("/proc/net/route");
    let text = fs::read_to_string(path).context("read /proc/net/route")?;
    let mut out = Vec::new();

    for (i, line) in text.lines().enumerate() {
        if i == 0 {
            continue;
        }
        let cols: Vec<&str> = line.split_whitespace().collect();
        if cols.len() < 8 {
            continue;
        }
        let iface = cols[0].to_string();
        let dest = cols[1];
        let metric = cols[6].parse::<u32>().unwrap_or(u32::MAX);

        if dest == "00000000" {
            out.push(RouteEntry { iface, metric });
        }
    }
    Ok(out)
}

/// Collect (iface_name, ipv4) via `if-addrs` (cross-platform).
#[cfg(not(windows))]
fn list_ipv4_ifaces() -> Result<Vec<(String, Ipv4Addr)>> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();

    for iface in if_addrs::get_if_addrs().context("enumerate network interfaces")? {
        let name = iface.name;
        let ip = match iface.addr {
            if_addrs::IfAddr::V4(v4) => v4.ip,
            _ => continue,
        };
        if ip.is_loopback() || ip.is_unspecified() {
            continue;
        }
        if seen.insert((name.clone(), ip)) {
            out.push((name, ip));
        }
    }
    Ok(out)
}

/// The interface that owns a local address (IPv4 or IPv6).
#[cfg(target_os = "linux")]
fn iface_of_ip(ip: IpAddr) -> Option<String> {
    if_addrs::get_if_addrs()
        .ok()?
        .into_iter()
        .find(|i| i.ip() == ip)
        .map(|i| i.name)
}

/// The interface that owns a local IPv4 address.
#[cfg(target_os = "linux")]
fn iface_of_ipv4(ip: Ipv4Addr) -> Option<String> {
    list_ipv4_ifaces()
        .ok()?
        .into_iter()
        .find_map(|(n, a)| (a == ip).then_some(n))
}

#[cfg(target_os = "linux")]
fn ipv4_of_iface(iface: &str) -> Option<Ipv4Addr> {
    list_ipv4_ifaces().ok()?.into_iter().find_map(
        |(n, ip)| {
            if n == iface {
                Some(ip)
            } else {
                None
            }
        },
    )
}

/// Lower score = higher preference when falling back.
fn iface_preference_score(name: &str) -> u32 {
    let n = name.to_ascii_lowercase();
    if n.starts_with("eth")
        || n.starts_with("enp")
        || n.starts_with("ens")
        || n.starts_with("eno")
        || n.starts_with("ethernet")
        || n.starts_with("以太网")
    {
        0
    } else if n.starts_with("en") {
        1
    } else if n.starts_with("wlan")
        || n.starts_with("wlp")
        || n.starts_with("wl")
        || n.starts_with("wi-fi")
        || n.starts_with("wifi")
        || n.contains("wi-fi")
        || n.contains("wireless")
        || n.contains("无线")
    {
        2
    } else {
        10
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tunnel_names() {
        for tun in [
            "tun0",
            "wg0",
            "wg-mullvad",
            "tap1",
            "TAP-Windows Adapter V9",
            "utun3",
            "Wintun",
            "tailscale0",
            "ztcf23aa10",
            "CloudflareWARP",
            "nordlynx",
            "Mihomo",
            "sing-box",
        ] {
            assert!(is_tunnel_iface(tun), "expected tunnel: {tun}");
        }
        for phys in [
            "eth0",
            "enp0s3",
            "wlan0",
            "Intel(R) Ethernet Performance Tuning",
            "Killer E3100G Opportunistic Adapter",
            "AWG-25G PCIe NIC",
            "Realtek Gaming 2.5GbE Family Controller",
        ] {
            assert!(
                !is_tunnel_iface(phys),
                "false positive on physical NIC: {phys}"
            );
        }
    }

    #[test]
    fn dns_reply_must_match_query_id() {
        let id = 0x1234;
        let mut reply = encode_a_query(id, "example.com");
        reply[2] = 0x81;
        reply[3] = 0x80;
        reply[7] = 1;
        reply.extend_from_slice(&[
            0xc0, 0x0c, // name pointer
            0, 1, 0, 1, // A, IN
            0, 0, 0, 60, // TTL
            0, 4, 203, 0, 113, 7,
        ]);
        assert_eq!(
            decode_first_a(&reply, id),
            Some(Ipv4Addr::new(203, 0, 113, 7))
        );
        assert_eq!(decode_first_a(&reply, id + 1), None);
        reply[3] = 0x83; // NXDOMAIN
        assert_eq!(decode_first_a(&reply, id), None);
    }

    #[test]
    fn dns_decodes_names_with_many_labels() {
        let id = 7;
        let host = "a.b.c.d.e.f.g.h.i.j.k.l.example.com";
        let mut reply = encode_a_query(id, host);
        reply[2] = 0x81;
        reply[3] = 0x80;
        reply[7] = 1;
        reply.extend_from_slice(&[0xc0, 0x0c, 0, 1, 0, 1, 0, 0, 0, 60, 0, 4, 10, 0, 0, 9]);
        assert_eq!(decode_first_a(&reply, id), Some(Ipv4Addr::new(10, 0, 0, 9)));
        // Truncated pointer must not read past the end.
        let n = reply.len();
        assert_eq!(skip_name(&reply[..n - 15], n - 16), None);
    }

    #[cfg(windows)]
    #[test]
    fn windows_picker_never_recommends_benchmark_tunnel_addresses() {
        let candidates = list_bind_candidates().unwrap();
        assert!(candidates
            .iter()
            .filter(|c| is_benchmark_ip(c.ip))
            .all(|c| c.is_tunnel));
    }

    #[tokio::test]
    async fn resolve_host_accepts_bracketed_ipv6() {
        let addr = resolve_host("[::1]", 2222, None, None, None).await.unwrap();
        assert_eq!(
            addr,
            SocketAddr::new(IpAddr::V6("::1".parse().unwrap()), 2222)
        );
        assert!(resolve_host(
            "[::1]",
            22,
            None,
            Some(IpAddr::V4(Ipv4Addr::LOCALHOST)),
            None
        )
        .await
        .is_err());
    }

    #[tokio::test]
    async fn custom_dns_reports_nxdomain() {
        let server = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let addr = server.local_addr().unwrap();
        tokio::spawn(async move {
            let mut req = [0u8; 512];
            if let Ok((n, peer)) = server.recv_from(&mut req).await {
                if n >= 12 {
                    let mut resp = req[..n].to_vec();
                    resp[2] = 0x81;
                    resp[3] = 0x83; // NXDOMAIN
                    let _ = server.send_to(&resp, peer).await;
                }
            }
        });
        let err = query_a("missing.example.invalid", addr, Ipv4Addr::LOCALHOST, None)
            .await
            .unwrap_err();
        assert!(format!("{err:#}").contains("NXDOMAIN"), "{err:#}");
    }
}
