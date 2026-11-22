//! The agent loop, shared by both platforms.
//!
//! The only waiting happens in `EventLoop::wait`, a kernel wait (channel/IOCP) without a timeout
//! whenever nothing is settling or encoding. A timeout is used only while files settle.
//!
//! - macOS: launchd starts the agent on demand (`Options::exit_when_idle`), so between bursts of
//!   activity no process exists at all.
//! - Windows: the agent stays resident with a single thread blocked in the IOCP.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Result, bail};

use crate::config::Config;
use crate::convert::{ConvertCtx, Fingerprint, Outcome};
use crate::event::Msg;
use crate::filter::Filter;
use crate::platform::events::{self, EventLoop, Wait};
use crate::platform::{self, fsutil};
use crate::scheduler::{Obs, Pool, Probe, Scheduler, WorkerSpec};
use crate::state::{State, now_unix};
use crate::walk::walk;
use crate::{error, info, warn};

/// Slack applied to the catch-up timestamp threshold (clock granularity, slow copies).
const CATCHUP_MARGIN: Duration = Duration::from_secs(300);

#[derive(Debug, Clone, Copy, Default)]
pub struct Options {
    /// Exit after being idle this long (macOS on-demand runs started by launchd).
    pub exit_when_idle: Option<Duration>,
}

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

/// Subtrees the OS watcher should not report: excluded folders and library bundles directly
/// inside a root (e.g. `~/Pictures/Photos Library.photoslibrary`, which Photos writes constantly).
fn watcher_exclusions(roots: &[PathBuf], filter: &Filter) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> =
        filter.excluded_dirs().iter().filter(|d| roots.iter().any(|r| d.starts_with(r))).cloned().collect();
    for root in roots {
        let Ok(entries) = fs::read_dir(root) else { continue };
        for e in entries.flatten() {
            let name = e.file_name();
            if e.file_type().is_ok_and(|t| t.is_dir()) && filter.is_excluded_name(&name.to_string_lossy()) {
                out.push(e.path());
            }
        }
    }
    out
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
    history_done: bool,
    busy_since_idle: bool,
    converted_since_idle: usize,
    opts: Options,
}

pub fn run(cfg: Config, config_path: &Path, opts: Options) -> Result<()> {
    let state_path = state_path_for(config_path);
    let state = State::load(&state_path);
    let roots = resolve_roots(&cfg);
    if roots.is_empty() {
        bail!(
            "監視できるフォルダがありません。設定の「変換するフォルダ」を確認してください: {}",
            config_path.display()
        );
    }
    let filter = Filter::new(&cfg);
    info!(
        "起動 v{} (libjxl {}) / 監視: {} / 除外: {} / 圧縮の強さ: {} / 元ファイル: {} / 既存ファイル: {}{}",
        crate::VERSION,
        crate::jxl::libjxl_version(),
        roots.iter().map(|r| r.display().to_string()).collect::<Vec<_>>().join(", "),
        filter.excluded_dirs().len(),
        cfg.effort,
        cfg.original.describe(),
        if cfg.scan_existing { "変換する" } else { "新規のみ" },
        if opts.exit_when_idle.is_some() { " / オンデマンド起動" } else { "" },
    );

    platform::power::enter_resident_mode();
    let (mut ev, notifier) = EventLoop::new()?;
    ev.listen_control()?;
    if opts.exit_when_idle.is_some() {
        ev.listen_launch_events();
    }
    // Replay FSEvents history (macOS) instead of rescanning when only new files matter.
    let since = if cfg.scan_existing { None } else { state.last_event_id };
    ev.watch(&roots, since, &watcher_exclusions(&roots, &filter))?;
    let latest_event_id = since.or_else(events::current_event_id);

    let catchup_since = state
        .last_idle_unix
        .map(|s| UNIX_EPOCH + Duration::from_secs(s))
        .unwrap_or_else(SystemTime::now)
        .checked_sub(CATCHUP_MARGIN)
        .unwrap_or(UNIX_EPOCH);
    let had_previous_run = state.last_idle_unix.is_some();

    let state = Arc::new(Mutex::new(state));
    let ctx = ConvertCtx::new(&cfg);
    let spec = WorkerSpec::for_ctx(&ctx);
    let mut d = Daemon {
        filter,
        sched: Scheduler::new(Duration::from_secs(cfg.settle_seconds)),
        pool: Pool::new(cfg.effective_jobs(), Arc::new(ctx), spec, notifier),
        roots,
        state,
        state_path,
        scan_existing: cfg.scan_existing,
        catchup_since,
        latest_event_id,
        history_done: since.is_none(),
        busy_since_idle: true,
        converted_since_idle: 0,
        opts,
    };

    // Startup catch-up. With FSEvents history available nothing needs scanning.
    if d.scan_existing || (since.is_none() && (had_previous_run || opts.exit_when_idle.is_some())) {
        for root in d.roots.clone() {
            d.rescan(&root);
        }
    }

    d.event_loop(&mut ev);
    ev.stop_watching();
    d.shutdown(&mut ev);
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

    /// Returns false when the loop should stop.
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
            Msg::HistoryDone => self.history_done = true,
            Msg::Wake => self.busy_since_idle = true,
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
            Msg::Stop => {
                info!("停止要求を受け取りました");
                return false;
            }
        }
        true
    }

    fn is_idle(&self) -> bool {
        self.sched.is_idle() && self.pool.is_idle()
    }

    fn event_loop(&mut self, ev: &mut EventLoop) {
        let mut idle_since: Option<Instant> = None;
        loop {
            if self.is_idle() {
                if self.busy_since_idle {
                    self.on_idle();
                }
                idle_since.get_or_insert_with(Instant::now);
            } else {
                idle_since = None;
            }
            // On-demand runs end once idle long enough (and FSEvents history has been replayed).
            let exit_at = match (self.opts.exit_when_idle, idle_since) {
                (Some(grace), Some(since)) if self.history_done => Some(since + grace),
                _ => None,
            };
            if exit_at.is_some_and(|t| t <= Instant::now()) {
                info!("待機状態になったため終了します (次の変更で launchd が起動します)");
                return;
            }
            let deadline = [self.sched.next_deadline(), exit_at].into_iter().flatten().min();
            // No deadline: block until the OS reports something. This is the idle state.
            let wait = ev.wait(deadline.map(|d| d.saturating_duration_since(Instant::now())));
            match wait {
                Wait::Closed => return,
                Wait::Timeout => {}
                Wait::Msg(msg) => {
                    if !matches!(msg, Msg::HistoryDone | Msg::WorkerExited) {
                        idle_since = None;
                    }
                    if !self.handle(msg) {
                        return;
                    }
                    // Drain whatever else is queued before doing filesystem work.
                    while let Some(m) = ev.try_next() {
                        if !self.handle(m) {
                            return;
                        }
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
        self.catchup_since = SystemTime::now().checked_sub(CATCHUP_MARGIN).unwrap_or(UNIX_EPOCH);
        {
            let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
            st.last_idle_unix = Some(now_unix());
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

    fn shutdown(&mut self, ev: &mut EventLoop) {
        let dropped = self.pool.stop();
        if !dropped.is_empty() {
            info!("未処理の {} 件は次回起動時に処理します", dropped.len());
        }
        while self.pool.active() > 0 {
            match ev.wait(Some(Duration::from_secs(600))) {
                Wait::Msg(Msg::JobFinished(path, outcome)) => {
                    if let Some(line) = outcome.describe(&path) {
                        info!("{line}");
                    }
                }
                Wait::Msg(_) => {}
                Wait::Timeout | Wait::Closed => break,
            }
        }
        let st = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if let Err(e) = st.save(&self.state_path) {
            warn!("状態を保存できません: {e:#}");
        }
        info!("終了しました");
    }
}
