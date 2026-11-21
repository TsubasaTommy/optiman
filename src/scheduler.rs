//! Debounce + on-demand worker pool.
//!
//! The scheduler is pure logic driven by the daemon loop: it only asks for a wake-up
//! (`next_deadline`) while files are settling, so an idle daemon blocks forever in `recv()`.

use std::collections::{HashMap, HashSet, VecDeque};
use std::ffi::OsString;
use std::io::{self, BufRead, BufReader, Write};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use crate::config::OriginalPolicy;
use crate::convert::{ConvertCtx, Fingerprint, Outcome, convert_file};
use crate::encode::EncoderKind;
use crate::event::Msg;
use crate::{ipc, platform};

/// What we observed about a file the last time we looked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Obs {
    pub size: u64,
    pub mtime: SystemTime,
}

#[derive(Debug)]
struct Pending {
    deadline: Instant,
    obs: Obs,
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

    /// A change was reported. `obs` is None if the file no longer exists.
    pub fn on_changed(&mut self, path: PathBuf, now: Instant, obs: Option<Obs>) {
        match obs {
            None => {
                self.pending.remove(&path);
            }
            Some(obs) => {
                self.pending.insert(path, Pending { deadline: now + self.settle, obs });
            }
        }
    }

    pub fn next_deadline(&self) -> Option<Instant> {
        self.pending.values().map(|p| p.deadline).min()
    }

    /// Files whose settle time elapsed and that did not change since; ready to convert.
    /// `probe` re-reads size/mtime and reports whether a writer still holds the file.
    pub fn take_ready(&mut self, now: Instant, probe: &dyn Fn(&Path) -> Probe) -> Vec<PathBuf> {
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
                Probe::Busy(obs) => {
                    self.pending.insert(path, Pending { deadline: now + self.settle, obs });
                }
                Probe::Stable(obs) => {
                    let prev = self.pending.get(&path).map(|p| p.obs);
                    if prev == Some(obs) {
                        self.pending.remove(&path);
                        self.in_flight.insert(path.clone());
                        ready.push(path);
                    } else {
                        self.pending.insert(path, Pending { deadline: now + self.settle, obs });
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

    pub fn pending_len(&self) -> usize {
        self.pending.len()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Probe {
    Gone,
    /// Exists but another process still has it open for writing.
    Busy(Obs),
    Stable(Obs),
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
        let policy = match ctx.original {
            OriginalPolicy::Delete => "delete",
            OriginalPolicy::Trash => "trash",
            OriginalPolicy::Keep => "keep",
        };
        let encoder = match ctx.encoder {
            EncoderKind::Native => "native",
            EncoderKind::Rav1e => "rav1e",
        };
        let args = [
            "worker".to_string(),
            format!("--quality={}", ctx.quality),
            format!("--speed={}", ctx.speed),
            format!("--original={policy}"),
            format!("--encoder={encoder}"),
            format!("--skip-if-larger={}", ctx.skip_if_larger),
        ];
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
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            cmd.creation_flags(CREATE_NO_WINDOW);
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
    tx: Sender<Msg>,
}

impl Pool {
    pub fn new(max: usize, ctx: Arc<ConvertCtx>, spec: Option<WorkerSpec>, tx: Sender<Msg>) -> Pool {
        Pool {
            shared: Arc::new(Mutex::new(PoolShared { queue: VecDeque::new(), active: 0, stopping: false })),
            max: max.max(1),
            ctx,
            spec,
            tx,
        }
    }

    pub fn submit(&self, path: PathBuf) {
        let mut s = self.shared.lock().unwrap_or_else(|e| e.into_inner());
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
            .name("oif-encode".into())
            .stack_size(WORKER_STACK)
            .spawn(move || pool_thread(&shared, &ctx, spec.as_ref(), &tx));
        if spawned.is_err() {
            self.shared.lock().unwrap_or_else(|e| e.into_inner()).active -= 1;
            crate::error!("変換スレッドを作成できませんでした");
        }
    }

    pub fn is_idle(&self) -> bool {
        let s = self.shared.lock().unwrap_or_else(|e| e.into_inner());
        s.active == 0 && s.queue.is_empty()
    }

    pub fn active(&self) -> usize {
        self.shared.lock().unwrap_or_else(|e| e.into_inner()).active
    }

    /// Drop queued jobs; running workers finish their current file and exit.
    pub fn stop(&self) -> Vec<PathBuf> {
        let mut s = self.shared.lock().unwrap_or_else(|e| e.into_inner());
        s.stopping = true;
        s.queue.drain(..).collect()
    }
}

fn convert_in_process(ctx: &ConvertCtx, path: &Path) -> Outcome {
    platform::power::enter_background_thread();
    let _com = platform::ThreadInit::new();
    catch_unwind(AssertUnwindSafe(|| convert_file(ctx, path))).unwrap_or_else(|_| Outcome::Failed {
        reason: "内部エラー (panic)".into(),
        remember: Fingerprint::of_path(path),
    })
}

fn pool_thread(shared: &Mutex<PoolShared>, ctx: &ConvertCtx, spec: Option<&WorkerSpec>, tx: &Sender<Msg>) {
    let mut worker: Option<WorkerProc> = None;
    loop {
        let job = {
            let mut s = shared.lock().unwrap_or_else(|e| e.into_inner());
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
    let _com = platform::ThreadInit::new();
    let stdin = io::stdin();
    let mut out = io::stdout().lock();
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        let path = PathBuf::from(ipc::unescape(line.trim_end_matches('\r')));
        let outcome = catch_unwind(AssertUnwindSafe(|| convert_file(ctx, &path))).unwrap_or_else(|_| Outcome::Failed {
            reason: "内部エラー (panic)".into(),
            remember: Fingerprint::of_path(&path),
        });
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

    fn obs(size: u64) -> Obs {
        Obs { size, mtime: SystemTime::UNIX_EPOCH + Duration::from_secs(size) }
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
        assert_eq!(s.take_ready(t0 + Duration::from_secs(2), &probe), vec![p.clone()]);
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
        assert_eq!(s.take_ready(t0 + Duration::from_secs(4), &probe), vec![p.clone()]);
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
