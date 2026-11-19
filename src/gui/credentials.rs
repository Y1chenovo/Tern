//! Run whole credential operations off the UI thread, in submission order.
use super::*;
use std::thread;

type Update = Box<dyn FnOnce(&mut GuiApp) + Send>;
type Job = Box<dyn FnOnce() -> Update + Send>;

pub(super) struct CredentialQueue {
    tx: std::sync::mpsc::Sender<(String, Job)>,
    rx: std::sync::mpsc::Receiver<(String, Update)>,
    pending: std::collections::HashMap<String, usize>,
}

impl CredentialQueue {
    pub fn new(ctx: egui::Context) -> Self {
        let (tx, jobs) = std::sync::mpsc::channel::<(String, Job)>();
        let (done, rx) = std::sync::mpsc::channel();
        thread::spawn(move || {
            while let Ok((key, job)) = jobs.recv() {
                if done.send((key, job())).is_err() {
                    break;
                }
                ctx.request_repaint();
            }
        });
        Self {
            tx,
            rx,
            pending: Default::default(),
        }
    }

    pub fn busy(&self, key: &str) -> bool {
        self.pending.contains_key(key) || (key != "startup" && self.pending.contains_key("startup"))
    }

    pub fn is_pending(&self) -> bool {
        !self.pending.is_empty()
    }

    pub fn submit<T: Send + 'static>(
        &mut self,
        key: String,
        work: impl FnOnce() -> T + Send + 'static,
        apply: impl FnOnce(&mut GuiApp, T) + Send + 'static,
    ) {
        *self.pending.entry(key.clone()).or_default() += 1;
        let job: Job = Box::new(move || {
            let result = work();
            Box::new(move |app| apply(app, result))
        });
        if self.tx.send((key.clone(), job)).is_err() {
            self.finish(&key);
        }
    }

    pub fn drain(&mut self) -> Vec<Update> {
        let mut updates = Vec::new();
        while let Ok((key, update)) = self.rx.try_recv() {
            self.finish(&key);
            updates.push(update);
        }
        updates
    }

    fn finish(&mut self, key: &str) {
        if let Some(count) = self.pending.get_mut(key) {
            *count -= 1;
            if *count == 0 {
                self.pending.remove(key);
            }
        }
    }
}

impl GuiApp {
    pub(super) fn credentials_busy(&mut self, id: &str) -> bool {
        let busy = self.credentials.busy(id);
        if busy {
            self.toast_msg(
                self.t(
                    "正在处理此主机的凭据，请稍候",
                    "Credentials are still being processed",
                ),
                StatusKind::Busy,
            );
        }
        busy
    }

    pub(super) fn credential_warning(&mut self) {
        self.toast_msg(
            self.t(
                "部分凭据未能保存到系统凭据库；失败项保留原值",
                "Some credentials could not be saved; failed entries keep their previous values",
            ),
            StatusKind::Error,
        );
    }

    pub(super) fn init_credentials(&mut self) {
        let original = self.config.profiles.clone();
        self.credentials.submit(
            "startup".into(),
            move || {
                profiles::retry_pending_deletes();
                let mut migrated = original.clone();
                for p in &mut migrated {
                    p.migrate_secrets();
                }
                profiles::preload_secrets(&migrated);
                (original, migrated)
            },
            |app, (original, migrated)| {
                let mut changed = false;
                for (old, new) in original.iter().zip(migrated) {
                    if let Some(p) = app.config.profiles.iter_mut().find(|p| p.id == old.id) {
                        if p.password_obfuscated == old.password_obfuscated
                            && p.password_obfuscated != new.password_obfuscated
                        {
                            p.password_obfuscated = new.password_obfuscated;
                            changed = true;
                        }
                        if p.key_passphrase_obfuscated == old.key_passphrase_obfuscated
                            && p.key_passphrase_obfuscated != new.key_passphrase_obfuscated
                        {
                            p.key_passphrase_obfuscated = new.key_passphrase_obfuscated;
                            changed = true;
                        }
                    }
                }
                if changed {
                    app.persist();
                }
            },
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slow_credentials_leave_caller_free_and_keep_writes_in_order() {
        use std::time::Duration;
        let mut queue = CredentialQueue::new(egui::Context::default());
        let (release, wait) = std::sync::mpsc::channel();
        let (observed, events) = std::sync::mpsc::channel();
        let first = observed.clone();
        queue.submit(
            "host".into(),
            move || {
                first.send("started").unwrap();
                wait.recv().unwrap();
                first.send("saved").unwrap();
            },
            |_, _| {},
        );
        // The caller can continue while the credential backend is blocked.
        assert_eq!(
            events.recv_timeout(Duration::from_secs(2)).unwrap(),
            "started"
        );
        queue.submit(
            "host".into(),
            move || {
                observed.send("deleted").unwrap();
            },
            |_, _| {},
        );
        assert!(queue.busy("host"));
        assert_eq!(queue.pending["host"], 2);
        assert!(queue.drain().is_empty());
        assert!(events.try_recv().is_err());
        release.send(()).unwrap();
        assert_eq!(
            events.recv_timeout(Duration::from_secs(2)).unwrap(),
            "saved"
        );
        assert_eq!(
            events.recv_timeout(Duration::from_secs(2)).unwrap(),
            "deleted"
        );
        let deadline = Instant::now() + Duration::from_secs(2);
        let mut completed = 0;
        while queue.is_pending() && Instant::now() < deadline {
            completed += queue.drain().len();
            thread::sleep(Duration::from_millis(1));
        }
        assert_eq!(completed, 2);
        assert!(!queue.busy("host"));
    }
}
