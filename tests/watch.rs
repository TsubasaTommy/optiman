//! Runs the real agent binary against a temporary folder.

use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

struct Agent {
    child: Child,
    lines: mpsc::Receiver<String>,
}

impl Agent {
    fn start(home: &Path) -> Agent {
        let mut child = Command::new(env!("CARGO_BIN_EXE_optimize-image-folder"))
            .env("OIF_HOME", home)
            .args(["--config"])
            .arg(home.join("config.toml"))
            .args(["run", "--log-stderr"])
            .stderr(Stdio::piped())
            .stdout(Stdio::null())
            .spawn()
            .expect("spawn agent");
        let stderr = child.stderr.take().unwrap();
        let (tx, lines) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                eprintln!("[agent] {line}");
                if tx.send(line).is_err() {
                    break;
                }
            }
        });
        let agent = Agent { child, lines };
        agent.wait_for_line("起動", Duration::from_secs(20));
        // Give the OS watcher a moment to arm before files are dropped in.
        std::thread::sleep(Duration::from_millis(1500));
        agent
    }

    fn wait_for_line(&self, needle: &str, timeout: Duration) -> String {
        let deadline = Instant::now() + timeout;
        while let Some(left) = deadline.checked_duration_since(Instant::now()) {
            match self.lines.recv_timeout(left) {
                Ok(l) if l.contains(needle) => return l,
                Ok(_) => {}
                Err(_) => break,
            }
        }
        panic!("agent did not log {needle:?} within {timeout:?}");
    }
}

impl Drop for Agent {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures").join(name)
}

fn write_config(home: &Path, photos: &Path, scan_existing: bool) {
    let folder = photos.to_string_lossy().replace('\\', "\\\\");
    fs::write(
        home.join("config.toml"),
        format!("folders = [\"{folder}\"]\noriginal = \"delete\"\nscan_existing = {scan_existing}\nsettle_seconds = 1\nspeed = 10\n"),
    )
    .unwrap();
}

fn wait_until(timeout: Duration, mut cond: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if cond() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    cond()
}

#[test]
fn converts_files_dropped_into_watched_folder() {
    let home = tempfile::tempdir().unwrap();
    let photos = tempfile::tempdir().unwrap();
    let existing = photos.path().join("existing.bmp");
    fs::copy(fixture("photo.bmp"), &existing).unwrap();
    write_config(home.path(), photos.path(), false);

    let agent = Agent::start(home.path());

    let sub = photos.path().join("2026").join("11");
    fs::create_dir_all(&sub).unwrap();
    // Simulate a slow copy: write half, pause, write the rest.
    let data = fs::read(fixture("photo_orient6.jpg")).unwrap();
    let target = sub.join("IMG_0001.jpg");
    fs::write(&target, &data[..data.len() / 2]).unwrap();
    std::thread::sleep(Duration::from_millis(400));
    fs::write(&target, &data).unwrap();

    let avif = sub.join("IMG_0001.avif");
    assert!(
        wait_until(Duration::from_secs(60), || avif.exists() && !target.exists()),
        "new file was not converted (avif exists: {}, original exists: {})",
        avif.exists(),
        target.exists()
    );
    agent.wait_for_line("待機します", Duration::from_secs(30));
    assert!(existing.exists(), "existing photos must be left alone when scan_existing = false");
    assert!(!photos.path().join("existing.avif").exists());
}

#[test]
fn converts_existing_files_when_requested() {
    let home = tempfile::tempdir().unwrap();
    let photos = tempfile::tempdir().unwrap();
    let existing = photos.path().join("existing.bmp");
    fs::copy(fixture("photo.bmp"), &existing).unwrap();
    let hidden = photos.path().join(".hidden.bmp");
    fs::copy(fixture("photo.bmp"), &hidden).unwrap();
    write_config(home.path(), photos.path(), true);

    let _agent = Agent::start(home.path());
    let avif = photos.path().join("existing.avif");
    assert!(wait_until(Duration::from_secs(60), || avif.exists() && !existing.exists()), "existing file not converted");
    assert!(hidden.exists(), "hidden files must be ignored");
}
