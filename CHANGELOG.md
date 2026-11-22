# Changelog

All notable user-facing changes to **Tern** are documented in this file.

## v1.0.1 (2026-11-23)

### Bug Fixes & Hardening
- **Safe SFTP → Terminal `cd`**: Blocks sending `cd` keystrokes while a full-screen terminal app (`vim`, `htop`, `less`) owns the alternate screen, and adds `--` before the quoted path so dash-prefixed directory names never fail as flags.
- **Deterministic Port-Forward Teardown**: Explicitly stops and awaits local `-L` port-forward listeners before session teardown finishes (both on clean exit and network drops), preventing lingering bound ports when minimized or `Address already in use` errors on fast reconnects.
- **Atomic Session Close & Credential Persistence**: Unifies closing notices and authentication generation tracking inside `SharedUi` so closing banners never precede trailing PTY output and ultra-short login sessions reliably persist remembered passwords.
- **Accurate Tunnel Interface Detection**: Replaces greedy short-prefix substring matching (`tun`/`tap`/`wg`) with token-boundary checks to prevent false positives on physical NIC descriptions (e.g. `"Tuning"`, `"AWG"`), adds `tailscale`/`zerotier`/`CloudflareWARP`/`Mihomo`/`sing-box` signatures, and inspects Linux `/sys/class/net/<iface>` (`tun_flags`, ARP hardware type, `uevent`) to detect custom-named tunnels.
- **Complete `~/.ssh/config` Importer**: Adds support for `Include` globs, wildcard & `!negated` `Host` patterns, `%h` hostname expansion, `LocalForward`, `BindAddress`, `BindInterface`, and local OS username fallback.

### Performance & UX Polish
- **Zero-Allocation Idle Editor Rendering**: Caches syntax-highlighted `Galley` layouts and search match ranges across frames in the built-in file editor.
- **Shared Tokio Runtime for SFTP UI Tasks**: Replaces per-operation OS thread spawning (`thread::spawn` + `block_on`) in `files.rs` with tasks on the shared runtime.
- **Bracketed Paste for Multi-Line Commands & `chmod` Special Bits**: Wraps multi-line snippets/commands in bracketed paste when supported by the shell, and preserves `setuid`/`setgid`/`sticky` bits (`0o7000`) when clicking `chmod` octal presets.

## v1.0.0 (2026-11-19)

Initial public open-source release.

### Productivity & Workflow
- **Command Snippets Library**: Categorized, searchable snippet manager embedded in both the terminal command bar and multi-host broadcast bar.
- **`~/.ssh/config` Import**: One-click OpenSSH config importer supporting `Include` globs, aliases, `IdentityFile`, `LocalForward`, `BindAddress`, and `BindInterface` with automatic duplicate skipping.
- **Terminal ↔ SFTP Directory Sync**: One-click `cd` from the file panel to the terminal (with POSIX shell escaping), plus `OSC 7` working-directory tracking back to the SFTP panel.
- **Clickable Terminal URLs**: `Ctrl + Click` (or context menu) safely launches detected `http://` / `https://` links in the system browser.
- **Auto-Reconnect**: Configurable exponential backoff (`2s / 4s / 8s / 16s / 30s`, up to 5 attempts) with a live cancellable countdown banner that stays non-intrusive for background tabs.

### SFTP & Built-in Editor
- **Recursive Folder Downloads & Non-Blocking Scans**: Full recursive folder transfer for both single-tab SFTP and Group Batch File transfer with 4-way BFS pre-scanning, live progress counters, animated indeterminate bar, system sleep prevention, 4-host batch concurrency limits, and automatic cleanup on cancellation.
- **Remote Permissions & Symlinks**: Interactive `chmod` dialog (`Owner / Group / Others` matrix + live octal input) and concurrent symlink target resolution on hover.
- **Enhanced Remote Editor**: Syntax highlighting (`JSON`, `YAML`, `TOML/INI`, `Shell`, `Nginx`) and UTF-8-safe `Ctrl+F` / `Ctrl+H` find and replace; atomic temporary-file saves with `(size, mtime)` conflict checking and filename whitespace preservation.

### Security & Stability
- **Keychain & Known Hosts Hardening**: Asynchronous ordered OS keychain queue with zero plaintext fallback on write failures; case-insensitive hostname canonicalization and fail-closed `known_hosts.json` validation.
- **UI & Verification**: Capsule tab styling, optically aligned settings controls, cross-platform GitHub Actions CI & release packaging workflows, and 57 GUI + 14 CLI automated tests (including in-memory SSH/SFTP regression tests).

---

## Pre-1.0 Milestones

- **v0.9.x**: OS native credential storage (`keyring`), strict per-algorithm host-key verification, 16 MiB per-tunnel port-forward isolation, 4 MiB pipelined SFTP transfers, atomic remote file replacement, IME cursor tracking, and visible-row SFTP virtualization.
- **v0.8.x**: Rebranded to **Tern**, origami tern icon & custom Windows DWM titlebar chrome, automatic config directory migration.
- **v0.7.x**: Recursive folder uploads and themed built-in cross-platform file/key picker.
- **v0.6.x**: Multi-host **Group Control** grid (`Ctrl+Shift+G`), broadcast bar, synchronized keyboard mirroring, batch file dispatch, cluster monitor, and `egui` visual design system overhaul.
- **v0.5.0**: Full `vt100` canvas terminal emulator, quick-connect bar, remote resource monitor, and interactive SFTP browser.
