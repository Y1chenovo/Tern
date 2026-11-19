//! In-memory SSH/SFTP regression tests. No sockets, OS credentials or real files.
use super::*;
use russh::server::{Auth, ChannelOpenHandle, Session};
use russh_sftp::protocol::{
    Attrs, Data, File, FileAttributes, Handle, Name, OpenFlags, Status, StatusCode, Version,
};
use std::collections::HashMap;
use std::sync::Mutex;

#[derive(Default)]
struct State {
    files: HashMap<String, Vec<u8>>,
    dirs: std::collections::HashSet<String>,
    symlinks: HashMap<String, String>,
    perms: HashMap<String, u32>,
    enforce_parents: bool,
    deny_temp: bool,
    change_on_close: bool,
    opens: Vec<(String, OpenFlags)>,
    inits: usize,
    fail_write: bool,
    exec_eof_only: bool,
    exec_closed: usize,
    sftp_kills: Vec<tokio::sync::oneshot::Sender<()>>,
    cancel_on_read: Option<Arc<std::sync::atomic::AtomicBool>>,
}

impl State {
    fn resolve(&self, path: String) -> String {
        let mut cur = path;
        for _ in 0..8 {
            if let Some(next) = self.symlinks.get(&cur) {
                cur = next.clone();
            } else {
                break;
            }
        }
        cur
    }
}

struct Server {
    channels: HashMap<russh::ChannelId, Channel<russh::server::Msg>>,
    state: Arc<Mutex<State>>,
}

impl russh::server::Handler for Server {
    type Error = anyhow::Error;
    async fn auth_none(&mut self, _: &str) -> Result<Auth> {
        Ok(Auth::Accept)
    }
    async fn channel_open_session(
        &mut self,
        channel: Channel<russh::server::Msg>,
        reply: ChannelOpenHandle,
        _: &mut Session,
    ) -> Result<()> {
        self.channels.insert(channel.id(), channel);
        reply.accept().await;
        Ok(())
    }
    async fn subsystem_request(
        &mut self,
        id: russh::ChannelId,
        _: &str,
        session: &mut Session,
    ) -> Result<()> {
        let channel = self.channels.remove(&id).unwrap();
        session.channel_success(id)?;
        let (ktx, krx) = tokio::sync::oneshot::channel::<()>();
        self.state.lock().unwrap().sftp_kills.push(ktx);
        let handler = Files {
            state: Arc::clone(&self.state),
            listed_handles: std::collections::HashSet::new(),
        };
        tokio::spawn(async move {
            tokio::select! {
                _ = russh_sftp::server::run(channel.into_stream(), handler) => {}
                _ = krx => {}
            }
        });
        Ok(())
    }
    async fn exec_request(
        &mut self,
        id: russh::ChannelId,
        _: &[u8],
        session: &mut Session,
    ) -> Result<()> {
        if self.state.lock().unwrap().exec_eof_only {
            session.channel_success(id)?;
            session.data(id, b"ok".as_slice())?;
            session.eof(id)?;
            return Ok(());
        }
        session.channel_failure(id)?;
        Ok(())
    }

    async fn channel_close(&mut self, id: russh::ChannelId, _: &mut Session) -> Result<()> {
        if self.channels.remove(&id).is_some() {
            let mut state = self.state.lock().unwrap();
            if state.exec_eof_only {
                state.exec_closed += 1;
            }
        }
        Ok(())
    }
}

struct Files {
    state: Arc<Mutex<State>>,
    listed_handles: std::collections::HashSet<String>,
}
fn ok(id: u32) -> Status {
    Status {
        id,
        status_code: StatusCode::Ok,
        error_message: String::new(),
        language_tag: String::new(),
    }
}
fn attrs(data: &[u8]) -> FileAttributes {
    FileAttributes {
        size: Some(data.len() as u64),
        uid: Some(100),
        gid: Some(100),
        permissions: Some(0o100600),
        mtime: Some(1),
        ..Default::default()
    }
}

impl russh_sftp::server::Handler for Files {
    type Error = StatusCode;
    fn unimplemented(&self) -> StatusCode {
        StatusCode::OpUnsupported
    }
    async fn init(&mut self, _: u32, _: HashMap<String, String>) -> Result<Version, StatusCode> {
        // Widen the cache-miss overlap to make duplicate initialization observable.
        tokio::time::sleep(Duration::from_millis(10)).await;
        self.state.lock().unwrap().inits += 1;
        Ok(Version::new())
    }
    async fn open(
        &mut self,
        id: u32,
        path: String,
        flags: OpenFlags,
        _: FileAttributes,
    ) -> Result<Handle, StatusCode> {
        let mut state = self.state.lock().unwrap();
        state.opens.push((path.clone(), flags));
        if state.deny_temp && path.contains(".tern-") {
            return Err(StatusCode::PermissionDenied);
        }
        let path = state.resolve(path);
        if flags.contains(OpenFlags::CREATE) {
            if state.enforce_parents {
                if let Some((parent, _)) = path.rsplit_once('/') {
                    if !parent.is_empty() && parent != "." && !state.dirs.contains(parent) {
                        return Err(StatusCode::NoSuchFile);
                    }
                }
            }
            state.files.entry(path.clone()).or_default();
        }
        let file = state.files.get_mut(&path).ok_or(StatusCode::NoSuchFile)?;
        if flags.contains(OpenFlags::TRUNCATE) {
            file.clear();
        }
        Ok(Handle { id, handle: path })
    }
    async fn close(&mut self, id: u32, handle: String) -> Result<Status, StatusCode> {
        let mut state = self.state.lock().unwrap();
        if state.change_on_close && handle.contains(".tern-") {
            state.change_on_close = false;
            state
                .files
                .insert("/target".into(), b"another client's newer content".to_vec());
        }
        Ok(ok(id))
    }
    async fn read(
        &mut self,
        id: u32,
        handle: String,
        offset: u64,
        len: u32,
    ) -> Result<Data, StatusCode> {
        let delay = {
            let state = self.state.lock().unwrap();
            state.cancel_on_read.as_ref().map(|c| {
                c.store(true, std::sync::atomic::Ordering::Relaxed);
                Duration::from_millis(250)
            })
        };
        if let Some(d) = delay {
            tokio::time::sleep(d).await;
        }
        let state = self.state.lock().unwrap();
        let data = state.files.get(&handle).ok_or(StatusCode::NoSuchFile)?;
        let off = offset as usize;
        if off >= data.len() {
            return Err(StatusCode::Eof);
        }
        Ok(Data {
            id,
            data: data[off..data.len().min(off + len as usize)].to_vec(),
        })
    }
    async fn write(
        &mut self,
        id: u32,
        handle: String,
        offset: u64,
        data: Vec<u8>,
    ) -> Result<Status, StatusCode> {
        let mut state = self.state.lock().unwrap();
        if state.fail_write {
            return Err(StatusCode::Failure);
        }
        let file = state.files.get_mut(&handle).ok_or(StatusCode::NoSuchFile)?;
        let off = offset as usize;
        file.resize(file.len().max(off + data.len()), 0);
        file[off..off + data.len()].copy_from_slice(&data);
        Ok(ok(id))
    }
    async fn stat(&mut self, id: u32, path: String) -> Result<Attrs, StatusCode> {
        let state = self.state.lock().unwrap();
        let cur = state.resolve(path);
        let perm = state.perms.get(&cur).copied();
        if state.dirs.contains(&cur) {
            return Ok(Attrs {
                id,
                attrs: FileAttributes {
                    permissions: Some(perm.map(|p| 0o040000 | (p & 0o7777)).unwrap_or(0o040755)),
                    ..Default::default()
                },
            });
        }
        let mut a = attrs(state.files.get(&cur).ok_or(StatusCode::NoSuchFile)?);
        if let Some(p) = perm {
            a.permissions = Some(0o100000 | (p & 0o7777));
        }
        Ok(Attrs { id, attrs: a })
    }
    async fn lstat(&mut self, id: u32, path: String) -> Result<Attrs, StatusCode> {
        if self.state.lock().unwrap().symlinks.contains_key(&path) {
            return Ok(Attrs {
                id,
                attrs: FileAttributes {
                    permissions: Some(0o120777),
                    ..Default::default()
                },
            });
        }
        self.stat(id, path).await
    }
    async fn fstat(&mut self, id: u32, handle: String) -> Result<Attrs, StatusCode> {
        self.stat(id, handle).await
    }
    async fn realpath(&mut self, id: u32, path: String) -> Result<Name, StatusCode> {
        Ok(Name {
            id,
            files: vec![File::dummy(path)],
        })
    }
    async fn opendir(&mut self, id: u32, path: String) -> Result<Handle, StatusCode> {
        Ok(Handle {
            id,
            handle: format!("{id}:{path}"),
        })
    }
    async fn readdir(&mut self, id: u32, handle: String) -> Result<Name, StatusCode> {
        if !self.listed_handles.insert(handle.clone()) {
            return Err(StatusCode::Eof);
        }
        let dir_path = handle.split_once(':').map(|(_, p)| p).unwrap_or(&handle);
        let state = self.state.lock().unwrap();
        if dir_path == "/" && state.dirs.is_empty() && state.symlinks.is_empty() {
            return Ok(Name {
                id,
                files: state
                    .files
                    .iter()
                    .map(|(p, data)| {
                        let mut file = File::new(p.trim_start_matches('/'), attrs(data));
                        file.longname = "-rw------- 1 user group 3 Jan 1 00:00 target".into();
                        file
                    })
                    .collect(),
            });
        }
        let prefix = if dir_path == "/" {
            "/".to_string()
        } else if dir_path == "." {
            String::new()
        } else {
            format!("{}/", dir_path.trim_end_matches('/'))
        };
        fn direct<'a>(p: &'a str, prefix: &str) -> Option<&'a str> {
            if prefix.is_empty() {
                (!p.starts_with('/') && !p.contains('/')).then_some(p)
            } else {
                p.strip_prefix(prefix)
                    .filter(|rest| !rest.is_empty() && !rest.contains('/'))
            }
        }
        let mut out = Vec::new();
        for d in &state.dirs {
            if let Some(name) = direct(d, &prefix) {
                let perm = state
                    .perms
                    .get(d)
                    .copied()
                    .map(|p| 0o040000 | (p & 0o7777))
                    .unwrap_or(0o040755);
                out.push(File::new(
                    name,
                    FileAttributes {
                        permissions: Some(perm),
                        ..Default::default()
                    },
                ));
            }
        }
        for (p, data) in &state.files {
            if let Some(name) = direct(p, &prefix) {
                let mut a = attrs(data);
                if let Some(perm) = state.perms.get(p).copied() {
                    a.permissions = Some(0o100000 | (perm & 0o7777));
                }
                let mut f = File::new(name, a);
                f.longname = format!("-rw------- 1 user group {} Jan 1 00:00 {name}", data.len());
                out.push(f);
            }
        }
        for p in state.symlinks.keys() {
            if let Some(name) = direct(p, &prefix) {
                out.push(File::new(
                    name,
                    FileAttributes {
                        permissions: Some(0o120777),
                        ..Default::default()
                    },
                ));
            }
        }
        Ok(Name { id, files: out })
    }
    async fn readlink(&mut self, id: u32, path: String) -> Result<Name, StatusCode> {
        let target = self
            .state
            .lock()
            .unwrap()
            .symlinks
            .get(&path)
            .cloned()
            .ok_or(StatusCode::NoSuchFile)?;
        Ok(Name {
            id,
            files: vec![File::dummy(target)],
        })
    }
    async fn setstat(
        &mut self,
        id: u32,
        path: String,
        attrs: FileAttributes,
    ) -> Result<Status, StatusCode> {
        if let Some(perm) = attrs.permissions {
            let mut state = self.state.lock().unwrap();
            let resolved = state.resolve(path);
            state.perms.insert(resolved, perm);
        }
        Ok(ok(id))
    }
    async fn remove(&mut self, id: u32, path: String) -> Result<Status, StatusCode> {
        self.state
            .lock()
            .unwrap()
            .files
            .remove(&path)
            .ok_or(StatusCode::NoSuchFile)?;
        Ok(ok(id))
    }
    async fn mkdir(
        &mut self,
        id: u32,
        path: String,
        _: FileAttributes,
    ) -> Result<Status, StatusCode> {
        let mut state = self.state.lock().unwrap();
        if state.enforce_parents {
            if let Some((parent, _)) = path.rsplit_once('/') {
                if !parent.is_empty() && parent != "." && !state.dirs.contains(parent) {
                    return Err(StatusCode::NoSuchFile);
                }
            }
        }
        if state.dirs.contains(&path) || state.files.contains_key(&path) {
            return Err(StatusCode::Failure);
        }
        state.dirs.insert(path);
        Ok(ok(id))
    }
    async fn rename(&mut self, id: u32, from: String, to: String) -> Result<Status, StatusCode> {
        let mut state = self.state.lock().unwrap();
        let data = state.files.remove(&from).ok_or(StatusCode::NoSuchFile)?;
        state.files.insert(to, data);
        Ok(ok(id))
    }
}

async fn connection(state: Arc<Mutex<State>>) -> Arc<SshHandle> {
    let (client_io, server_io) = tokio::io::duplex(256 * 1024);
    let key = russh::keys::ssh_key::private::Ed25519Keypair::from_seed(&[7; 32]);
    let config = russh::server::Config {
        keys: vec![key.into()],
        ..Default::default()
    };
    tokio::spawn(async move {
        let session = russh::server::run_stream(
            Arc::new(config),
            server_io,
            Server {
                channels: HashMap::new(),
                state,
            },
        )
        .await
        .unwrap();
        let _ = session.await;
    });
    let handler = ClientHandler {
        server_key: Arc::new(Mutex::new(None)),
        host_key_verifier: Arc::new(|_| Box::pin(async { true })),
    };
    let mut h = client::connect_stream(Arc::new(client::Config::default()), client_io, handler)
        .await
        .unwrap();
    assert!(h.authenticate_none("test").await.unwrap().success());
    Arc::new(h)
}

#[test]
fn remote_paths_preserve_filename_whitespace() {
    for path in ["/tmp/report ", " report", " ", "\tname\n"] {
        assert_eq!(remote_home_relative(path), path);
    }
    assert_eq!(remote_home_relative("~/report "), "report ");
    assert_eq!(remote_home_relative("~"), ".");
    assert_eq!(remote_home_relative(""), ".");
}

#[tokio::test]
async fn exec_closes_channel_after_remote_eof() {
    tokio::time::timeout(Duration::from_secs(10), async {
        let state = Arc::new(Mutex::new(State {
            exec_eof_only: true,
            ..Default::default()
        }));
        let h = connection(Arc::clone(&state)).await;
        for _ in 0..12 {
            assert_eq!(exec_capture(&h, "echo ok").await.unwrap(), "ok");
        }
        tokio::time::timeout(Duration::from_secs(2), async {
            while state.lock().unwrap().exec_closed < 12 {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("exec channels were not closed");
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn concurrent_sftp_requests_share_one_initialization() {
    tokio::time::timeout(Duration::from_secs(10), async {
        let state = Arc::new(Mutex::new(State::default()));
        let h = connection(Arc::clone(&state)).await;
        let sessions = futures_util::future::join_all((0..16).map(|_| sftp_session(&h))).await;
        let first = sessions[0].as_ref().unwrap();
        assert!(sessions
            .iter()
            .all(|s| Arc::ptr_eq(first, s.as_ref().unwrap())));
        assert_eq!(state.lock().unwrap().inits, 1);
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn denied_temp_file_never_opens_original_for_writing() {
    tokio::time::timeout(Duration::from_secs(10), async {
        let state = Arc::new(Mutex::new(State {
            deny_temp: true,
            ..Default::default()
        }));
        state
            .lock()
            .unwrap()
            .files
            .insert("/target".into(), b"original".to_vec());
        let h = connection(Arc::clone(&state)).await;
        let mut w = RemoteWriter::new(&h).await.unwrap();
        let t = w.prepare("/target", None).await.unwrap();
        let result = w
            .put(&t, Source::Bytes(b"new"), &Default::default(), &mut |_| {})
            .await;
        assert!(format!("{:#}", result.unwrap_err()).contains("original unchanged"));
        let state = state.lock().unwrap();
        assert_eq!(state.files["/target"], b"original");
        assert!(state
            .opens
            .iter()
            .all(|(p, f)| p != "/target" || !f.contains(OpenFlags::WRITE)));
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn change_during_upload_is_detected_before_commit() {
    tokio::time::timeout(Duration::from_secs(10), async {
        let state = Arc::new(Mutex::new(State {
            change_on_close: true,
            ..Default::default()
        }));
        state
            .lock()
            .unwrap()
            .files
            .insert("/target".into(), b"original".to_vec());
        let h = connection(Arc::clone(&state)).await;
        let mut w = RemoteWriter::new(&h).await.unwrap();
        w.exec_stats = None;
        let stamp = FileStamp { size: 8, mtime: 1 };
        let t = w.prepare("/target", Some(stamp)).await.unwrap();
        let e = w
            .put(
                &t,
                Source::Bytes(b"my edit"),
                &Default::default(),
                &mut |_| {},
            )
            .await
            .unwrap_err();
        assert!(e.downcast_ref::<RemoteChanged>().is_some(), "{e:#}");
        let state = state.lock().unwrap();
        assert_eq!(state.files["/target"], b"another client's newer content");
        assert_eq!(
            state.files.len(),
            1,
            "conflicted temporary upload should be removed"
        );
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn in_place_copy_uses_completed_remote_snapshot() {
    tokio::time::timeout(Duration::from_secs(10), async {
        let state = Arc::new(Mutex::new(State::default()));
        state.lock().unwrap().files.extend([
            ("/target".into(), b"old".to_vec()),
            ("/snapshot".into(), b"complete uploaded content".to_vec()),
        ]);
        let h = connection(Arc::clone(&state)).await;
        let mut w = RemoteWriter::new(&h).await.unwrap();
        let t = w.prepare("/target", None).await.unwrap();
        w.copy_in_place("/snapshot", &t).await.unwrap();
        let state = state.lock().unwrap();
        assert_eq!(state.files["/target"], state.files["/snapshot"]);
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn saving_space_suffixed_file_does_not_touch_its_neighbor() {
    tokio::time::timeout(Duration::from_secs(10), async {
        let state = Arc::new(Mutex::new(State::default()));
        state.lock().unwrap().files.extend([
            ("/target".into(), b"neighbor".to_vec()),
            ("/target ".into(), b"original".to_vec()),
        ]);
        let h = connection(Arc::clone(&state)).await;
        let (text, stamp) = sftp_read_text(&h, "/target ", 1024).await.unwrap();
        assert_eq!(text, "original");
        sftp_write_file(&h, "/target ", b"edited", stamp)
            .await
            .unwrap();
        let state = state.lock().unwrap();
        assert_eq!(state.files["/target"], b"neighbor");
        assert_eq!(state.files["/target "], b"edited");
        assert_eq!(state.files.len(), 2);
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn failed_temporary_upload_keeps_original_and_cleans_up() {
    tokio::time::timeout(Duration::from_secs(10), async {
        let state = Arc::new(Mutex::new(State {
            fail_write: true,
            ..Default::default()
        }));
        state
            .lock()
            .unwrap()
            .files
            .insert("/target".into(), b"original".to_vec());
        let h = connection(Arc::clone(&state)).await;
        let mut w = RemoteWriter::new(&h).await.unwrap();
        let t = w.prepare("/target", None).await.unwrap();
        assert!(w
            .put(
                &t,
                Source::Bytes(b"edited"),
                &Default::default(),
                &mut |_| {}
            )
            .await
            .is_err());
        let state = state.lock().unwrap();
        assert_eq!(state.files["/target"], b"original");
        assert_eq!(state.files.len(), 1);
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn sftp_ops_normalize_home_relative_paths() {
    tokio::time::timeout(Duration::from_secs(10), async {
        let state = Arc::new(Mutex::new(State {
            enforce_parents: true,
            ..Default::default()
        }));
        let h = connection(Arc::clone(&state)).await;
        sftp_mkdir(&h, "~/sub").await.unwrap();
        sftp_create_file(&h, "~/sub/a.txt").await.unwrap();
        sftp_rename(&h, "~/sub/a.txt", "~/sub/b.txt").await.unwrap();
        {
            let s = state.lock().unwrap();
            assert!(s.dirs.contains("sub"));
            assert!(s.files.contains_key("sub/b.txt"));
        }
        let res = sftp_remove_many(&h, vec!["~/sub/b.txt".into()])
            .await
            .unwrap();
        assert_eq!(res[0].0, "~/sub/b.txt");
        res[0].1.as_ref().unwrap();
        assert!(!state.lock().unwrap().files.contains_key("sub/b.txt"));
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn upload_tree_creates_missing_parent_dirs() {
    tokio::time::timeout(Duration::from_secs(10), async {
        let state = Arc::new(Mutex::new(State {
            enforce_parents: true,
            ..Default::default()
        }));
        let h = connection(Arc::clone(&state)).await;
        let tmp = std::env::temp_dir().join(format!("tern-ut-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        let src = tmp.join("hello.txt");
        std::fs::write(&src, b"hello tree").unwrap();

        let (tx, _rx) = mpsc::unbounded_channel();
        let dirs = vec!["~/deploy/app/v1".to_string()];
        let items = vec![UploadItem {
            local: src,
            remote: "~/deploy/app/v1/hello.txt".to_string(),
            size: 10,
        }];
        sftp_upload_tree(&h, &dirs, &items, tx, Default::default())
            .await
            .unwrap();

        let s = state.lock().unwrap();
        assert!(s.dirs.contains("deploy"));
        assert!(s.dirs.contains("deploy/app"));
        assert!(s.dirs.contains("deploy/app/v1"));
        assert_eq!(s.files["deploy/app/v1/hello.txt"], b"hello tree");
        let _ = std::fs::remove_dir_all(&tmp);
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn download_entries_recursively_downloads_folders_sanitizes_names_and_skips_dir_symlinks() {
    tokio::time::timeout(Duration::from_secs(10), async {
        let state = Arc::new(Mutex::new(State::default()));
        {
            let mut s = state.lock().unwrap();
            s.dirs.extend([
                "deploy".into(),
                "deploy/sub".into(),
                "deploy/empty".into(),
                "other".into(),
            ]);
            s.files.extend([
                ("deploy/top.txt".into(), b"top".to_vec()),
                ("deploy/sub/bad*name?.txt".into(), b"nested2".to_vec()),
                ("deploy/sub/bad:name?.txt".into(), b"nested".to_vec()),
                ("other/target.bin".into(), b"linked".to_vec()),
            ]);
            s.symlinks.extend([
                ("deploy/link_to_file".into(), "other/target.bin".into()),
                ("deploy/link_to_dir".into(), "other".into()),
            ]);
        }
        let h = connection(Arc::clone(&state)).await;
        let dest = std::env::temp_dir().join(format!("tern-dl-tree-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dest);

        let (tx, mut rx) = mpsc::unbounded_channel();
        let (out1, res1) = sftp_download_entries(
            &h,
            &["~/deploy".into()],
            &dest,
            tx.clone(),
            Default::default(),
        )
        .await;
        res1.unwrap();
        let mut events = Vec::new();
        while let Ok(ev) = rx.try_recv() {
            events.push(ev);
        }
        assert!(
            events.iter().any(|p| p.scanning_files.is_some()),
            "expected scanning progress update, got {events:?}"
        );
        let last = events.last().expect("expected final progress event");
        assert_eq!(last.scanning_files, None);
        assert!(last.total > 0);
        assert_eq!(last.done, last.total);

        assert_eq!(out1.files_total, 4);
        assert_eq!(out1.files_done, 4);
        assert_eq!(
            out1.saved_path.as_deref(),
            Some(dest.join("deploy").as_path())
        );
        assert_eq!(out1.skipped.len(), 1);
        assert!(out1.skipped[0].1.contains("link to a folder"));

        assert_eq!(std::fs::read(dest.join("deploy/top.txt")).unwrap(), b"top");
        assert_eq!(
            std::fs::read(dest.join("deploy/link_to_file")).unwrap(),
            b"linked"
        );
        let safe_first = local_safe_name("bad*name?.txt");
        assert_eq!(
            std::fs::read(dest.join("deploy/sub").join(&safe_first)).unwrap(),
            b"nested2"
        );
        if cfg!(windows) {
            assert_eq!(
                std::fs::read(dest.join("deploy/sub/bad_name_ (1).txt")).unwrap(),
                b"nested"
            );
        } else {
            assert_eq!(
                std::fs::read(
                    dest.join("deploy/sub")
                        .join(local_safe_name("bad:name?.txt"))
                )
                .unwrap(),
                b"nested"
            );
        }
        assert!(
            std::fs::read_dir(dest.join("deploy/sub"))
                .unwrap()
                .filter_map(|e| e.ok())
                .all(|e| !e.file_name().to_string_lossy().ends_with(".part")),
            "no .part files should remain"
        );
        assert!(dest.join("deploy/empty").is_dir());

        // Downloading the same folder again allocates `deploy (1)` instead of overwriting.
        let (out2, res2) =
            sftp_download_entries(&h, &["~/deploy".into()], &dest, tx, Default::default()).await;
        res2.unwrap();
        assert_eq!(
            out2.saved_path.as_deref(),
            Some(dest.join("deploy (1)").as_path())
        );
        assert_eq!(
            std::fs::read(dest.join("deploy (1)/top.txt")).unwrap(),
            b"top"
        );

        let _ = std::fs::remove_dir_all(&dest);
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn download_entries_cancel_cleans_up_empty_dirs_and_placeholders() {
    tokio::time::timeout(Duration::from_secs(10), async {
        let cancel = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let state = Arc::new(Mutex::new(State {
            cancel_on_read: Some(Arc::clone(&cancel)),
            ..Default::default()
        }));
        {
            let mut s = state.lock().unwrap();
            s.dirs.extend(["deploy".into(), "deploy/sub".into()]);
            s.files.extend([
                ("top.txt".into(), b"top payload".to_vec()),
                ("deploy/sub/nested.txt".into(), b"nested payload".to_vec()),
            ]);
        }
        let h = connection(Arc::clone(&state)).await;
        let dest = std::env::temp_dir().join(format!("tern-dl-cancel-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dest);

        let (tx, _rx) = mpsc::unbounded_channel();
        let (out, res) = sftp_download_entries(
            &h,
            &["~/top.txt".into(), "~/deploy".into()],
            &dest,
            tx,
            cancel,
        )
        .await;
        let err = res.unwrap_err();
        assert!(err.downcast_ref::<Cancelled>().is_some(), "{err:#}");
        assert_eq!(out.files_done, 0);
        assert_eq!(out.saved_path, None);
        assert_eq!(
            std::fs::read_dir(&dest).unwrap().count(),
            0,
            "cancelled download with zero completed files should leave dest clean"
        );

        let _ = std::fs::remove_dir_all(&dest);
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn upload_recovers_from_stale_cached_sftp_session() {
    tokio::time::timeout(Duration::from_secs(10), async {
        let state = Arc::new(Mutex::new(State::default()));
        let h = connection(Arc::clone(&state)).await;
        let _ = sftp_session(&h).await.unwrap();
        assert_eq!(state.lock().unwrap().inits, 1);

        // Simulate the idle cached SFTP subsystem dropping on the server side.
        let kill = state.lock().unwrap().sftp_kills.remove(0);
        let _ = kill.send(());
        tokio::time::sleep(Duration::from_millis(20)).await;

        sftp_write_file(&h, "/recovered.txt", b"ok after drop", None)
            .await
            .unwrap();
        let state = state.lock().unwrap();
        assert_eq!(state.inits, 2);
        assert_eq!(state.files["/recovered.txt"], b"ok after drop");
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn sftp_list_reports_symlink_targets_and_chmod_updates_permissions() {
    tokio::time::timeout(Duration::from_secs(10), async {
        let state = Arc::new(Mutex::new(State::default()));
        {
            let mut s = state.lock().unwrap();
            s.dirs.insert("app".into());
            s.files.insert("app/run.sh".into(), b"#!/bin/sh\n".to_vec());
            s.symlinks
                .insert("app/current".into(), "/srv/app/release-42".into());
            s.files.insert("/srv/app/release-42".into(), b"ok".to_vec());
        }
        let h = connection(Arc::clone(&state)).await;
        sftp_chmod(&h, "~/app/run.sh", 0o755).await.unwrap();
        assert_eq!(state.lock().unwrap().perms["app/run.sh"], 0o755);

        let (_cwd, entries) = sftp_list(&h, "~/app").await.unwrap();
        let script = entries.iter().find(|e| e.name == "run.sh").unwrap();
        assert_eq!(script.permissions.map(|m| m & 0o7777), Some(0o755));
        assert_eq!(script.mode, "-rwxr-xr-x");

        let link = entries.iter().find(|e| e.name == "current").unwrap();
        assert!(link.is_link);
        assert_eq!(link.link_target.as_deref(), Some("/srv/app/release-42"));
    })
    .await
    .unwrap();
}
