//! Persisted SSH host keys (`known_hosts`-style, app-local JSON).

use anyhow::{Context, Result};
use russh::keys::{HashAlg, PublicKey, PublicKeyBase64};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KnownHost {
    pub host: String,
    pub port: u16,
    pub algorithm: String,
    pub fingerprint: String,
    pub key_base64: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct Store {
    hosts: Vec<KnownHost>,
}

fn path() -> PathBuf {
    crate::profiles::config_dir().join("known_hosts.json")
}

/// Serializes read-modify-write cycles (several tabs may connect at once).
static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// A missing file is an empty store. A file that exists but can't be read or
/// parsed is an error: treating it as empty would silently trust every host
/// and then overwrite the saved fingerprints.
fn load() -> Result<Store> {
    let p = path();
    match fs::read_to_string(&p) {
        Ok(t) => serde_json::from_str(&t).with_context(|| {
            format!(
                "{} is damaged — fix or remove it to continue (it protects against spoofed servers)",
                p.display()
            )
        }),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Store::default()),
        Err(e) => Err(e).with_context(|| format!("read {}", p.display())),
    }
}

fn save(store: &Store) -> Result<()> {
    let text = serde_json::to_string_pretty(store)?;
    crate::profiles::write_atomic(&path(), text.as_bytes())
}

pub fn fingerprint(key: &PublicKey) -> String {
    key.fingerprint(HashAlg::Sha256).to_string()
}

/// What we know about a server key.
pub enum Verdict {
    /// Same key as saved before.
    Match,
    /// Never seen this host:port — ask the user before trusting it.
    New,
    /// A different key of the same type was saved for this host:port.
    Changed(KnownHost),
    /// The server shows a key of a type not saved for it yet, while keys of
    /// other types are (it added a key, or now prefers another kind): ask,
    /// but it is not the "key changed" alarm.
    OtherType(KnownHost),
}

fn normalize_host(host: &str) -> String {
    host.trim().to_ascii_lowercase()
}

fn check_in_store(store: &Store, host: &str, port: u16, key: &PublicKey) -> Verdict {
    let host = normalize_host(host);
    let fp = fingerprint(key);
    let algo = key.algorithm().to_string();
    let mut saved = store
        .hosts
        .iter()
        .filter(|h| h.host.eq_ignore_ascii_case(&host) && h.port == port);
    let Some(first) = saved.clone().next() else {
        return Verdict::New;
    };
    match saved.find(|h| h.algorithm == algo) {
        Some(h) if h.fingerprint == fp => Verdict::Match,
        Some(h) => Verdict::Changed(h.clone()),
        None => Verdict::OtherType(first.clone()),
    }
}

fn trust_in_store(store: &mut Store, host: &str, port: u16, key: &PublicKey) {
    let host = normalize_host(host);
    let algo = key.algorithm().to_string();
    store
        .hosts
        .retain(|h| !(h.host.eq_ignore_ascii_case(&host) && h.port == port && h.algorithm == algo));
    store.hosts.push(KnownHost {
        host,
        port,
        algorithm: algo,
        fingerprint: fingerprint(key),
        key_base64: key.public_key_base64(),
    });
}

/// Look the key up without storing anything; call [`trust`] once the user
/// has accepted a new or changed key.
///
/// Keys are kept per host, port *and* key type (like OpenSSH's
/// known_hosts), so a server with several host keys never looks "changed"
/// only because another type got negotiated.
pub fn check(host: &str, port: u16, key: &PublicKey) -> Result<Verdict> {
    let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let store = load()?;
    Ok(check_in_store(&store, host, port, key))
}

/// Save `key` for host:port, replacing a saved key of the same type (keys
/// of other types stay).
pub fn trust(host: &str, port: u16, key: &PublicKey) -> Result<()> {
    let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut store = load()?;
    trust_in_store(&mut store, host, port, key);
    save(&store)
}

#[cfg(test)]
mod tests {
    use super::*;
    use russh::keys::PrivateKey;

    fn sample_ed25519(seed: u8) -> PublicKey {
        let mut raw = [0u8; 32];
        raw.fill(seed);
        let kp = russh::keys::ssh_key::private::Ed25519Keypair::from_seed(&raw);
        let pk = PrivateKey::from(kp);
        pk.public_key().clone()
    }

    #[test]
    fn host_matching_is_case_insensitive_and_canonicalizes_storage() {
        let mut store = Store::default();
        let k1 = sample_ed25519(1);
        let k2 = sample_ed25519(2);

        assert!(matches!(
            check_in_store(&store, "VPS.Example.COM", 22, &k1),
            Verdict::New
        ));
        trust_in_store(&mut store, "  VPS.Example.COM ", 22, &k1);
        assert_eq!(store.hosts.len(), 1);
        assert_eq!(store.hosts[0].host, "vps.example.com");

        assert!(matches!(
            check_in_store(&store, "vps.example.com", 22, &k1),
            Verdict::Match
        ));
        assert!(matches!(
            check_in_store(&store, "Vps.Example.Com", 22, &k2),
            Verdict::Changed(_)
        ));

        // Replacing with uppercase still updates the existing entry in place.
        trust_in_store(&mut store, "VPS.EXAMPLE.COM", 22, &k2);
        assert_eq!(store.hosts.len(), 1);
        assert!(matches!(
            check_in_store(&store, "vps.example.com", 22, &k2),
            Verdict::Match
        ));
    }
}
