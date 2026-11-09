//! Поток синхронизации: сеть не тормозит окно. Главный цикл шлёт задания
//! (`Job`), поток отвечает (`Done`). Журналы своего устройства пишет только
//! главный поток; поток синхронизации их читает и пишет чужие файлы —
//! поэтому откат здесь только скачивает точку, а применяет его окно.

use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender, channel};

use browser_sync::DirStore;
use browser_sync::MemStore;
use browser_sync::remote::folder::FolderRemote;
use browser_sync::remote::{self, Account, Error, Provider, SyncPoint, oauth};
use browser_sync::sync::{self, Report, SyncState};

use crate::net::Client;
use crate::profile::now_ms;

pub enum Job {
    /// Синхронизировать; `Some(подпись)` — с точкой синхронизации.
    Sync { account: Account, device: String, dir: PathBuf, point: Option<String> },
    Points { account: Account },
    FetchPoint { account: Account, point: String },
    LoginStart { account: Account },
    LoginPoll { account: Account, device_code: String },
    /// Проверить вход и создать репозиторий/папку.
    Check { account: Account },
}

pub enum Done {
    Synced(Result<Report, Error>, Option<Account>),
    Points(Result<Vec<SyncPoint>, Error>),
    Point(String, Result<MemStore, Error>),
    LoginStarted(Result<oauth::DeviceCode, Error>),
    LoginPolled(Result<oauth::Poll, Error>),
    Checked(Result<(), Error>),
}

pub struct Syncer {
    jobs: Sender<Job>,
    pub done: Receiver<Done>,
    pub busy: usize,
}

/// Обновить токен, если истекает. Вернуть обновлённую запись, если менялась.
fn fresh(account: &Account) -> Result<Option<Account>, Error> {
    if !oauth::expiring(account, now_ms()) {
        return Ok(None);
    }
    match oauth::refresh(account, &Client)? {
        Some(tokens) => {
            let mut updated = account.clone();
            oauth::apply(&mut updated, &tokens, now_ms());
            Ok(Some(updated))
        }
        None => Ok(None),
    }
}

fn with_remote<T>(account: &Account, f: impl FnOnce(&mut dyn remote::Remote) -> Result<T, Error>) -> Result<T, Error> {
    account.check().map_err(Error::Auth)?;
    if account.provider == Provider::Folder {
        let mut folder = FolderRemote::new(account.repo.trim());
        return f(&mut folder);
    }
    let client = Client;
    let mut boxed = remote::open(account, &client).ok_or_else(|| Error::Protocol("площадка".into()))?;
    f(&mut *boxed)
}

fn run(job: Job) -> Done {
    match job {
        Job::Sync { account, device, dir, point } => {
            let refreshed = match fresh(&account) {
                Ok(r) => r,
                Err(e) => return Done::Synced(Err(e), None),
            };
            let acc = refreshed.clone().unwrap_or(account);
            let state_path = dir.join("sync-state.json");
            let mut state = SyncState::parse(&std::fs::read_to_string(&state_path).unwrap_or_default());
            let mut store = DirStore::new(dir.join("sync"));
            let result = with_remote(&acc, |r| sync::sync(&mut store, r, &device, &mut state, point.as_deref(), now_ms()));
            if let Err(e) = &result {
                state.last_error = e.to_string();
            }
            let _ = std::fs::write(&state_path, state.to_text());
            Done::Synced(result, refreshed)
        }
        Job::Points { account } => Done::Points(with_remote(&account, sync::points)),
        Job::FetchPoint { account, point } => {
            let result = with_remote(&account, |r| sync::fetch_point(r, &point));
            Done::Point(point, result)
        }
        Job::LoginStart { account } => Done::LoginStarted(oauth::start(&account, &Client)),
        Job::LoginPoll { account, device_code } => Done::LoginPolled(oauth::poll(&account, &Client, &device_code)),
        Job::Check { account } => Done::Checked(with_remote(&account, |r| r.prepare())),
    }
}

impl Syncer {
    pub fn start() -> Syncer {
        let (jobs, rx) = channel::<Job>();
        let (tx, done) = channel::<Done>();
        std::thread::Builder::new()
            .name("obsos-sync".into())
            .spawn(move || {
                while let Ok(job) = rx.recv() {
                    if tx.send(run(job)).is_err() {
                        break;
                    }
                }
            })
            .expect("sync thread");
        Syncer { jobs, done, busy: 0 }
    }

    pub fn send(&mut self, job: Job) {
        if self.jobs.send(job).is_ok() {
            self.busy += 1;
        }
    }

    pub fn poll(&mut self) -> Option<Done> {
        let done = self.done.try_recv().ok()?;
        self.busy = self.busy.saturating_sub(1);
        Some(done)
    }
}
