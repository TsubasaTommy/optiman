//! The resident loop.
//!
//! One thread blocks in `Receiver::recv()` (no timeout) whenever nothing is settling or encoding,
//! so the idle process uses no CPU. A timeout is used only while files are waiting to settle.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Result, bail};

use crate::config::Config;
use crate::convert::{ConvertCtx, Fingerprint, Outcome};
use crate::encode;
use crate::event::Msg;
use crate::filter::Filter;
use crate::platform::{self, fsutil};
use crate::scheduler::{Obs, Pool, Probe, Scheduler, WorkerSpec};
use crate::state::{State, now_unix};
use crate::walk::walk;
use crate::{error, info, warn};

/// Slack applied to the catch-up timestamp threshold (clock granularity, slow copies).
const CATCHUP_MARGIN: Duration = Duration::from_secs(300);

pub fn state_path_for(config_path: &Path) -> PathBuf {
    config_path.with_file_name("state.toml")
}

/// Canonical, existing watch roots (FSEvents reports canonical paths, e.g. /private/var/...).
pub fn resolve_roots(cfg: &Config) -> Vec<PathBuf> {
    let mut roots: Vec<PathBuf> = Vec::new();
    for p in cfg.folder_paths() {
        match platform::paths::canonical(&p) {
            Ok(c) if c.is_dir() => {
                if !roots.iter().any(|r| c.starts_with(r)) {
                    roots.retain(|r| !r.starts_with(&c));
                    roots.push(c);
                }
            }
            _ => warn!("フォルダが見つからないため監視しません: {}", p.display()),
        }
    }
    roots
}

fn observe(path: &Path) -> Option<Obs> {
    let md = fs::symlink_metadata(path).ok()?;
    md.is_file().then(|| Obs { size: md.len(), mtime: md.modified().unwrap_or(UNIX_EPOCH) })
}

fn probe(path: &Path) -> Probe {
    match observe(path) {
        None => Probe::Gone,
        Some(obs) if fsutil::writer_active(path) => Probe::Busy(obs),
        Some(obs) => Probe::Stable(obs),
    }
}

struct Daemon {
    roots: Vec<PathBuf>,
    filter: Filter,
    sched: Scheduler,
    pool: Pool,
    state: Arc<Mutex<State>>,
    state_path: PathBuf,
    scan_existing: bool,
    /// Files that arrived before this are "existing" (ignored by rescans unless scan_existing).
    catchup_since: SystemTime,
    latest_event_id: Option<u64>,
    busy_since_idle: bool,
    converted_since_idle: usize,
}

pub fn run(cfg: Config, config_path: &Path) -> Result<()> {
    let state_path = state_path_for(config_path);
    let state = State::load(&state_path);
    let roots = resolve_roots(&cfg);
    if roots.is_empty() {
        bail!("監視できるフォルダがありません。設定ファイルの folders を確認してください: {}", config_path.display());
    }
    let encoder = encode::resolve(cfg.encoder);
    info!(
        "起動 v{} / 監視: {} / エンコーダ: {encoder} / 品質: {} / 元ファイル: {} / 既存ファイル: {}",
        crate::VERSION,
        roots.iter().map(|r| r.display().to_string()).collect::<Vec<_>>().join(", "),
        cfg.quality,
        cfg.original.describe(),
        if cfg.scan_existing { "変換する" } else { "新規のみ" }
    );

    let (tx, rx) = mpsc::channel();
    let _stop = platform::instance::listen_stop(tx.clone())?;

    // Replay FSEvents history (macOS) instead of rescanning when only new files matter.
    let since = if cfg.scan_existing { None } else { state.last_event_id };
    let watcher = platform::watch::Watcher::start(&roots, since, tx.clone())?;
    let latest_event_id = since.or_else(platform::watch::current_event_id);

    let catchup_since = state
        .last_idle_unix
        .map(|s| UNIX_EPOCH + Duration::from_secs(s))
        .unwrap_or_else(SystemTime::now)
        .checked_sub(CATCHUP_MARGIN)
        .unwrap_or(UNIX_EPOCH);
    let had_previous_run = state.last_idle_unix.is_some();

    let state = Arc::new(Mutex::new(state));
    let ctx = ConvertCtx::new(&cfg, encoder);
    let spec = WorkerSpec::for_ctx(&ctx);
    let mut d = Daemon {
        filter: Filter::new(&cfg),
        sched: Scheduler::new(Duration::from_secs(cfg.settle_seconds)),
        pool: Pool::new(cfg.effective_jobs(), Arc::new(ctx), spec, tx.clone()),
        roots,
        state,
        state_path,
        scan_existing: cfg.scan_existing,
        catchup_since,
        latest_event_id,
        busy_since_idle: true,
        converted_since_idle: 0,
    };
    drop(tx);

    // Startup catch-up. With FSEvents history available nothing needs scanning.
    if d.scan_existing || (since.is_none() && had_previous_run) {
        for root in d.roots.clone() {
            d.rescan(&root);
        }
    }

    d.event_loop(&rx);
    drop(watcher);
    d.shutdown(&rx);
    Ok(())
}

impl Daemon {
    fn root_of(&self, path: &Path) -> Option<&PathBuf> {
        self.roots.iter().find(|r| path.starts_with(r))
    }

    fn rescan(&mut self, dir: &Path) {
        let Some(root) = self.root_of(dir).cloned() else { return };
        let threshold = (!self.scan_existing).then_some(self.catchup_since);
        let now = Instant::now();
        let mut found = Vec::new();
        walk(&root, dir, &self.filter, &mut |p| found.push(p));
        let mut queued = 0;
        for path in found {
            let Ok(md) = fs::symlink_metadata(&path) else { continue };
            if threshold.is_some_and(|t| fsutil::arrival_time(&md) < t) {
                continue;
            }
            let obs = Obs { size: md.len(), mtime: md.modified().unwrap_or(UNIX_EPOCH) };
            self.sched.on_changed(path, now, Some(obs));
            queued += 1;
        }
        if queued > 1 {
            info!("スキャン: {} で {queued} 件の候補", dir.display());
        }
    }

    fn handle(&mut self, msg: Msg) -> bool {
        match msg {
            Msg::Changed(path) => {
                self.busy_since_idle = true;
                if self.root_of(&path).is_some_and(|root| self.filter.accepts(root, &path)) {
                    let obs = observe(&path);
                    self.sched.on_changed(path, Instant::now(), obs);
                }
            }
            Msg::Rescan(dir) => {
                self.busy_since_idle = true;
                warn!("イベントの取りこぼしの可能性があるため再スキャン: {}", dir.display());
                self.rescan(&dir);
            }
            Msg::NewDir(dir) => {
                self.busy_since_idle = true;
                self.rescan(&dir);
            }
            Msg::EventId(id) => {
                self.busy_since_idle = true;
                self.latest_event_id = Some(self.latest_event_id.map_or(id, |cur| cur.max(id)));
            }
            Msg::HistoryDone => {}
            Msg::JobFinished(path, outcome) => {
                self.sched.finished(&path);
                if let Some(line) = outcome.describe(&path) {
                    match outcome {
                        Outcome::Converted { note: None, .. } | Outcome::AlreadyDone(_) => info!("{line}"),
                        Outcome::Failed { .. } => error!("{line}"),
                        _ => warn!("{line}"),
                    }
                }
                if let Some((fp, reason)) = outcome.remember() {
                    self.state.lock().unwrap_or_else(|e| e.into_inner()).record_skip(&path, fp, reason);
                }
                match outcome {
                    Outcome::Converted { .. } => self.converted_since_idle += 1,
                    Outcome::Requeue(_) => {
                        let obs = observe(&path);
                        self.sched.on_changed(path, Instant::now(), obs);
                    }
                    _ => {}
                }
            }
            Msg::WorkerExited => {}
            Msg::Stop => return false,
        }
        true
    }

    fn event_loop(&mut self, rx: &Receiver<Msg>) {
        loop {
            if self.busy_since_idle && self.sched.is_idle() && self.pool.is_idle() {
                self.on_idle();
            }
            let msg = match self.sched.next_deadline() {
                // Nothing settling: block until the OS reports something. This is the idle state.
                None => match rx.recv() {
                    Ok(m) => Some(m),
                    Err(_) => return,
                },
                Some(deadline) => match rx.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
                    Ok(m) => Some(m),
                    Err(RecvTimeoutError::Timeout) => None,
                    Err(RecvTimeoutError::Disconnected) => return,
                },
            };
            if let Some(msg) = msg {
                if !self.handle(msg) {
                    info!("停止要求を受け取りました");
                    return;
                }
                // Drain whatever else is queued before doing filesystem work.
                while let Ok(m) = rx.try_recv() {
                    if !self.handle(m) {
                        info!("停止要求を受け取りました");
                        return;
                    }
                }
            }
            for path in self.sched.take_ready(Instant::now(), &probe) {
                // Files skipped earlier are retried only after they change.
                let skipped = Fingerprint::of_path(&path)
                    .is_some_and(|fp| self.state.lock().unwrap_or_else(|e| e.into_inner()).is_skipped(&path, fp));
                if skipped {
                    self.sched.finished(&path);
                } else {
                    self.pool.submit(path);
                }
            }
        }
    }

    fn on_idle(&mut self) {
        self.busy_since_idle = false;
        let now = now_unix();
        self.catchup_since = SystemTime::now().checked_sub(CATCHUP_MARGIN).unwrap_or(UNIX_EPOCH);
        {
            let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
            st.last_idle_unix = Some(now);
            if self.latest_event_id.is_some() {
                st.last_event_id = self.latest_event_id;
            }
            st.prune_missing();
            if let Err(e) = st.save(&self.state_path) {
                warn!("状態を保存できません: {e:#}");
            }
        }
        if self.converted_since_idle > 0 {
            info!("{} 件を変換しました。待機します", self.converted_since_idle);
            self.converted_since_idle = 0;
        }
        platform::power::trim_memory();
    }

    fn shutdown(&mut self, rx: &Receiver<Msg>) {
        let dropped = self.pool.stop();
        if !dropped.is_empty() {
            info!("未処理の {} 件は次回起動時に処理します", dropped.len());
        }
        while self.pool.active() > 0 {
            match rx.recv_timeout(Duration::from_secs(600)) {
                Ok(Msg::JobFinished(path, outcome)) => {
                    if let Some(line) = outcome.describe(&path) {
                        info!("{line}");
                    }
                }
                Ok(_) => {}
                Err(_) => break,
            }
        }
        let st = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if let Err(e) = st.save(&self.state_path) {
            warn!("状態を保存できません: {e:#}");
        }
        info!("終了しました");
    }
}
