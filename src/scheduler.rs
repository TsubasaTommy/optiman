//! Debounce + on-demand worker pool.
//!
//! The scheduler is pure logic driven by the daemon loop: it only asks for a wake-up
//! (`next_deadline`) while files are settling, so an idle daemon waits without a timeout.

use std::collections::{HashMap, HashSet, VecDeque};
use std::ffi::OsString;
use std::io::{self, BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use crate::LockUnpoisoned;
use crate::convert::{ConvertCtx, Fingerprint, Outcome, convert_guarded};
use crate::event::Msg;
use crate::platform::events::Notifier;
use crate::{ipc, platform};

#[derive(Debug)]
struct Pending {
    deadline: Instant,
    /// The file's size and mtime when it was last looked at.
    fp: Fingerprint,
}

#[derive(Debug)]
pub struct Scheduler {
    settle: Duration,
    pending: HashMap<PathBuf, Pending>,
    in_flight: HashSet<PathBuf>,
}

impl Scheduler {
    pub fn new(settle: Duration) -> Scheduler {
        Scheduler { settle, pending: HashMap::new(), in_flight: HashSet::new() }
    }

    /// A change was reported. `fp` is None if the file no longer exists.
    pub fn on_changed(&mut self, path: PathBuf, now: Instant, fp: Option<Fingerprint>) {
        match fp {
            None => {
                self.pending.remove(&path);
            }
            Some(fp) => {
                self.pending.insert(path, Pending { deadline: now + self.settle, fp });
            }
        }
    }

    pub fn next_deadline(&self) -> Option<Instant> {
        self.pending.values().map(|p| p.deadline).min()
    }

    /// Files whose settle time elapsed and that did not change since; ready to convert, with
    /// their current fingerprint. `probe` re-reads size/mtime and reports whether a writer still
    /// holds the file.
    pub fn take_ready(&mut self, now: Instant, probe: &dyn Fn(&Path) -> Probe) -> Vec<(PathBuf, Fingerprint)> {
        let due: Vec<PathBuf> =
            self.pending.iter().filter(|(_, p)| p.deadline <= now).map(|(k, _)| k.clone()).collect();
        let mut ready = Vec::new();
        for path in due {
            if self.in_flight.contains(&path) {
                // Still being converted; look again after another settle period.
                if let Some(p) = self.pending.get_mut(&path) {
                    p.deadline = now + self.settle;
                }
                continue;
            }
            match probe(&path) {
                Probe::Gone => {
                    self.pending.remove(&path);
                }
                Probe::Busy(fp) => {
                    self.pending.insert(path, Pending { deadline: now + self.settle, fp });
                }
                Probe::Stable(fp) => {
                    if self.pending.get(&path).map(|p| p.fp) == Some(fp) {
                        self.pending.remove(&path);
                        self.in_flight.insert(path.clone());
                        ready.push((path, fp));
                    } else {
                        self.pending.insert(path, Pending { deadline: now + self.settle, fp });
                    }
                }
            }
        }
        ready
    }

    pub fn finished(&mut self, path: &Path) {
        self.in_flight.remove(path);
    }

    pub fn is_idle(&self) -> bool {
        self.pending.is_empty() && self.in_flight.is_empty()
    }

    #[cfg(test)]
    fn pending_len(&self) -> usize {
        self.pending.len()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Probe {
    Gone,
    /// Exists but another process still has it open for writing.
    Busy(Fingerprint),
    Stable(Fingerprint),
}

const WORKER_STACK: usize = 16 * 1024 * 1024;

struct PoolShared {
    queue: VecDeque<PathBuf>,
    active: usize,
    stopping: bool,
}

/// How to launch the encoder process (`<exe> worker <args>`).
#[derive(Debug, Clone)]
pub struct WorkerSpec {
    pub exe: PathBuf,
    pub args: Vec<OsString>,
}

impl WorkerSpec {
    pub fn for_ctx(ctx: &ConvertCtx) -> Option<WorkerSpec> {
        let exe = std::env::current_exe().ok()?;
        // The format stays readable by workers of later versions (see `cli::WorkerArgs`).
        let mut args = vec![
            "worker".to_string(),
            format!("--effort={}", ctx.effort),
            format!("--threads={}", ctx.threads),
            format!("--original={}", ctx.original.as_str()),
            format!("--skip-if-larger={}", ctx.skip_if_larger),
        ];
        if let Some(q) = ctx.quality.jxl_quality() {
            args.push(format!("--quality={q}"));
        }
        Some(WorkerSpec { exe, args: args.into_iter().map(OsString::from).collect() })
    }
}

/// A running encoder process. Dropping/finishing it closes stdin, so it exits and the OS
/// reclaims every byte the decoders and encoder allocated.
struct WorkerProc {
    child: Child,
    stdin: Option<ChildStdin>,
    stdout: BufReader<ChildStdout>,
}

impl WorkerProc {
    fn spawn(spec: &WorkerSpec) -> io::Result<WorkerProc> {
        let mut cmd = Command::new(&spec.exe);
        cmd.args(&spec.args).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::inherit());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            cmd.creation_flags(windows::Win32::System::Threading::CREATE_NO_WINDOW.0);
        }
        let mut child = cmd.spawn()?;
        let stdin = child.stdin.take();
        let stdout = child.stdout.take().ok_or_else(|| io::Error::other("no stdout"))?;
        Ok(WorkerProc { child, stdin, stdout: BufReader::new(stdout) })
    }

    fn convert(&mut self, path: &Path) -> io::Result<Outcome> {
        let stdin = self.stdin.as_mut().ok_or_else(|| io::Error::other("stdin closed"))?;
        writeln!(stdin, "{}", ipc::escape(&path.to_string_lossy()))?;
        stdin.flush()?;
        let mut line = String::new();
        if self.stdout.read_line(&mut line)? == 0 {
            return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "worker exited"));
        }
        ipc::outcome_from_line(line.trim_end_matches(['\r', '\n'])).ok_or_else(|| io::Error::other("bad worker reply"))
    }

    fn finish(mut self) {
        drop(self.stdin.take());
        let _ = self.child.wait();
    }

    fn kill(mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Encoder workers are created on demand and exit as soon as the queue is empty, so nothing
/// is left running (or holding memory) while idle. Each pool thread drives one encoder
/// process; if the process cannot be started the file is converted in-process instead.
pub struct Pool {
    shared: Arc<Mutex<PoolShared>>,
    max: usize,
    ctx: Arc<ConvertCtx>,
    spec: Option<WorkerSpec>,
    tx: Notifier,
}

impl Pool {
    pub fn new(max: usize, ctx: Arc<ConvertCtx>, spec: Option<WorkerSpec>, tx: Notifier) -> Pool {
        Pool {
            shared: Arc::new(Mutex::new(PoolShared { queue: VecDeque::new(), active: 0, stopping: false })),
            max: max.max(1),
            ctx,
            spec,
            tx,
        }
    }

    pub fn submit(&self, path: PathBuf) {
        let mut s = self.shared.lock_unpoisoned();
        if s.stopping {
            return;
        }
        s.queue.push_back(path);
        if s.active < self.max {
            s.active += 1;
            drop(s);
            self.spawn_thread();
        }
    }

    fn spawn_thread(&self) {
        let shared = Arc::clone(&self.shared);
        let ctx = Arc::clone(&self.ctx);
        let spec = self.spec.clone();
        let tx = self.tx.clone();
        let spawned = thread::Builder::new()
            .name("optiman-encode".into())
            .stack_size(WORKER_STACK)
            .spawn(move || pool_thread(&shared, &ctx, spec.as_ref(), &tx));
        if spawned.is_err() {
            const REASON: &str = "変換スレッドを作成できませんでした";
            crate::error!("{REASON}");
            let mut s = self.shared.lock_unpoisoned();
            s.active -= 1;
            // No thread is left to take the queued files: report them as failed (not remembered,
            // so the next change or scan retries them) instead of waiting forever.
            if s.active == 0 {
                for job in s.queue.drain(..) {
                    let _ =
                        self.tx.send(Msg::JobFinished(job, Outcome::Failed { reason: REASON.into(), remember: None }));
                }
            }
        }
    }

    pub fn is_idle(&self) -> bool {
        let s = self.shared.lock_unpoisoned();
        s.active == 0 && s.queue.is_empty()
    }

    pub fn active(&self) -> usize {
        self.shared.lock_unpoisoned().active
    }

    /// Drop queued jobs; running workers finish their current file and exit.
    pub fn stop(&self) -> Vec<PathBuf> {
        let mut s = self.shared.lock_unpoisoned();
        s.stopping = true;
        s.queue.drain(..).collect()
    }
}

fn convert_in_process(ctx: &ConvertCtx, path: &Path) -> Outcome {
    platform::power::enter_background_thread();
    convert_guarded(ctx, path)
}

fn pool_thread(shared: &Mutex<PoolShared>, ctx: &ConvertCtx, spec: Option<&WorkerSpec>, tx: &Notifier) {
    let mut worker: Option<WorkerProc> = None;
    loop {
        let job = {
            let mut s = shared.lock_unpoisoned();
            match s.queue.pop_front() {
                Some(job) if !s.stopping => job,
                _ => {
                    s.active -= 1;
                    break;
                }
            }
        };
        if worker.is_none()
            && let Some(spec) = spec
        {
            match WorkerProc::spawn(spec) {
                Ok(w) => worker = Some(w),
                Err(e) => crate::warn!("変換プロセスを起動できないためプロセス内で変換します: {e}"),
            }
        }
        let outcome = match worker.as_mut() {
            None => convert_in_process(ctx, &job),
            Some(w) => match w.convert(&job) {
                Ok(o) => o,
                Err(e) => {
                    // The encoder process died on this file (e.g. a decoder crash). Remember the
                    // file so it is not retried in a loop, and start a fresh process next time.
                    if let Some(w) = worker.take() {
                        w.kill();
                    }
                    Outcome::Failed {
                        reason: format!("変換プロセスが異常終了しました ({e})"),
                        remember: Fingerprint::of_path(&job),
                    }
                }
            },
        };
        let _ = tx.send(Msg::JobFinished(job, outcome));
    }
    if let Some(w) = worker {
        w.finish();
    }
    let _ = tx.send(Msg::WorkerExited);
}

/// Body of the hidden `worker` subcommand: convert paths read from stdin until it closes.
pub fn run_worker(ctx: &ConvertCtx) -> i32 {
    platform::power::enter_background_process();
    let stdin = io::stdin();
    let mut out = io::stdout().lock();
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        let path = PathBuf::from(ipc::unescape(line.trim_end_matches('\r')));
        let outcome = convert_guarded(ctx, &path);
        if writeln!(out, "{}", ipc::outcome_to_line(&outcome)).and_then(|_| out.flush()).is_err() {
            break;
        }
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    fn obs(size: u64) -> Fingerprint {
        Fingerprint { size, mtime_ns: i128::from(size) * 1_000_000_000 }
    }

    #[test]
    fn idle_scheduler_asks_for_no_wakeup() {
        let s = Scheduler::new(Duration::from_secs(2));
        assert_eq!(s.next_deadline(), None);
        assert!(s.is_idle());
    }

    #[test]
    fn file_becomes_ready_after_settling() {
        let mut s = Scheduler::new(Duration::from_secs(2));
        let t0 = Instant::now();
        let p = PathBuf::from("/r/a.jpg");
        s.on_changed(p.clone(), t0, Some(obs(10)));
        assert_eq!(s.next_deadline(), Some(t0 + Duration::from_secs(2)));
        let probe = |_: &Path| Probe::Stable(obs(10));
        assert!(s.take_ready(t0 + Duration::from_secs(1), &probe).is_empty());
        assert_eq!(s.take_ready(t0 + Duration::from_secs(2), &probe), vec![(p.clone(), obs(10))]);
        assert!(!s.is_idle(), "in flight until finished");
        assert_eq!(s.next_deadline(), None);
        s.finished(&p);
        assert!(s.is_idle());
    }

    #[test]
    fn growing_file_is_rearmed() {
        let mut s = Scheduler::new(Duration::from_secs(2));
        let t0 = Instant::now();
        let p = PathBuf::from("/r/a.jpg");
        s.on_changed(p.clone(), t0, Some(obs(10)));
        let size = RefCell::new(20);
        let probe = |_: &Path| Probe::Stable(obs(*size.borrow()));
        assert!(s.take_ready(t0 + Duration::from_secs(2), &probe).is_empty());
        assert_eq!(s.next_deadline(), Some(t0 + Duration::from_secs(4)));
        assert_eq!(s.take_ready(t0 + Duration::from_secs(4), &probe), vec![(p.clone(), obs(20))]);
    }

    #[test]
    fn busy_and_gone_files() {
        let mut s = Scheduler::new(Duration::from_secs(1));
        let t0 = Instant::now();
        let a = PathBuf::from("/r/a.jpg");
        let b = PathBuf::from("/r/b.jpg");
        s.on_changed(a.clone(), t0, Some(obs(1)));
        s.on_changed(b.clone(), t0, Some(obs(1)));
        let probe = |p: &Path| if p == a { Probe::Busy(obs(1)) } else { Probe::Gone };
        assert!(s.take_ready(t0 + Duration::from_secs(1), &probe).is_empty());
        assert_eq!(s.pending_len(), 1);
        s.on_changed(a.clone(), t0, None);
        assert!(s.is_idle());
    }

    #[test]
    fn in_flight_file_is_not_submitted_twice() {
        let mut s = Scheduler::new(Duration::from_secs(1));
        let t0 = Instant::now();
        let p = PathBuf::from("/r/a.jpg");
        let probe = |_: &Path| Probe::Stable(obs(1));
        s.on_changed(p.clone(), t0, Some(obs(1)));
        assert_eq!(s.take_ready(t0 + Duration::from_secs(1), &probe).len(), 1);
        s.on_changed(p.clone(), t0 + Duration::from_secs(1), Some(obs(1)));
        assert!(s.take_ready(t0 + Duration::from_secs(2), &probe).is_empty());
        s.finished(&p);
        assert_eq!(s.take_ready(t0 + Duration::from_secs(3), &probe).len(), 1);
    }
}
