//! Command-line interface.

use std::fs;
use std::io::{self, BufRead, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result, bail};
use clap::{Args, Parser, Subcommand};

use crate::config::{self, Config, EncoderChoice, OriginalPolicy};
use crate::convert::{ConvertCtx, Outcome, convert_file, human};
use crate::daemon::{self, state_path_for};
use crate::encode::EncoderKind;
use crate::filter::Filter;
use crate::platform::{self, autostart, paths};
use crate::state::State;
use crate::walk::walk;
use crate::{APP_NAME, VERSION, logger};

#[derive(Parser, Debug)]
#[command(name = APP_NAME, version, about = "写真フォルダを監視して AV1 (AVIF) に変換する常駐ツール")]
pub struct Cli {
    /// 設定ファイルのパス (既定: OS 標準の場所)
    #[arg(long, global = true, value_name = "PATH")]
    pub config: Option<PathBuf>,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// 設定を作成して自動起動に登録し、常駐を開始する
    Install(InstallArgs),
    /// 自動起動を解除して常駐を停止する
    Uninstall {
        /// 設定・状態・ログも削除する
        #[arg(long)]
        purge: bool,
    },
    /// 常駐を開始する
    Start,
    /// 常駐を停止する
    Stop,
    /// 常駐を再起動する (設定の反映)
    Restart,
    /// 状態を表示する
    Status,
    /// フォアグラウンドで常駐する (自動起動から呼ばれます)
    Run {
        /// ログをファイルではなく標準エラーに出す
        #[arg(long)]
        log_stderr: bool,
    },
    /// ファイル/フォルダを今すぐ変換する
    Convert(ConvertArgs),
    /// 設定ファイルの場所と内容を表示する
    Config,
    /// (内部用) 標準入力から受け取ったファイルを変換する使い捨てプロセス
    #[command(hide = true)]
    Worker(WorkerArgs),
}

#[derive(Args, Debug)]
pub struct WorkerArgs {
    #[arg(long)]
    pub quality: u8,
    #[arg(long)]
    pub speed: u8,
    #[arg(long, value_enum)]
    pub original: OriginalPolicy,
    #[arg(long)]
    pub encoder: String,
    #[arg(long, action = clap::ArgAction::Set)]
    pub skip_if_larger: bool,
}

#[derive(Args, Debug, Default)]
pub struct InstallArgs {
    /// 監視するフォルダ (複数指定可。既定: ピクチャフォルダ)
    #[arg(long = "folder", value_name = "DIR")]
    pub folders: Vec<PathBuf>,
    /// 既存の写真も変換する
    #[arg(long, conflicts_with = "new_only")]
    pub scan_existing: bool,
    /// 新しく追加された写真だけ変換する
    #[arg(long)]
    pub new_only: bool,
    /// 変換後の元ファイルの扱い
    #[arg(long, value_enum)]
    pub original: Option<OriginalPolicy>,
    /// 画質 1-100
    #[arg(long)]
    pub quality: Option<u8>,
    /// 確認なしで進める
    #[arg(long, short = 'y')]
    pub yes: bool,
    /// 安全チェック (ホームフォルダ全体などの拒否) を無効にする
    #[arg(long)]
    pub force: bool,
    /// 登録だけして常駐は開始しない
    #[arg(long)]
    pub no_start: bool,
}

#[derive(Args, Debug)]
pub struct ConvertArgs {
    /// 変換するファイルまたはフォルダ
    #[arg(required = true)]
    pub paths: Vec<PathBuf>,
    /// 変換後の元ファイルの扱い (既定: 設定ファイルの値)
    #[arg(long, value_enum)]
    pub original: Option<OriginalPolicy>,
    #[arg(long)]
    pub quality: Option<u8>,
    #[arg(long, value_enum)]
    pub encoder: Option<EncoderChoice>,
    /// 何が変換されるか表示するだけ
    #[arg(long)]
    pub dry_run: bool,
    /// 同時変換数
    #[arg(long, short = 'j')]
    pub jobs: Option<usize>,
}

pub fn default_config_path() -> PathBuf {
    match std::env::var_os("OIF_HOME") {
        Some(home) => PathBuf::from(home).join("config.toml"),
        None => paths::config_dir().join("config.toml"),
    }
}

/// The standard log folder for the standard config; otherwise next to the config file. Derived
/// from the config path only, so the CLI and the launchd/Run-key agent always agree.
fn default_log_path(config_path: &Path) -> PathBuf {
    if config_path == paths::config_dir().join("config.toml") {
        paths::log_dir().join(format!("{APP_NAME}.log"))
    } else {
        config_path.with_file_name(format!("{APP_NAME}.log"))
    }
}

fn lock_path(config_path: &Path) -> PathBuf {
    config_path.with_file_name("agent.lock")
}

/// Exclusive lock held by the running agent; also used to detect it.
pub fn agent_running(config_path: &Path) -> bool {
    let Ok(f) = fs::OpenOptions::new().read(true).write(true).create(true).truncate(false).open(lock_path(config_path))
    else {
        return false;
    };
    match f.try_lock() {
        Ok(()) => {
            let _ = f.unlock();
            false
        }
        Err(_) => true,
    }
}

pub fn main() -> i32 {
    let cli = Cli::parse();
    let config_path = cli.config.clone().unwrap_or_else(default_config_path);
    let result = match cli.command {
        Command::Install(args) => install(&config_path, args),
        Command::Uninstall { purge } => uninstall(&config_path, purge),
        Command::Start => autostart::start().map(|_| println!("常駐を開始しました")),
        Command::Stop => stop(&config_path).map(|_| println!("常駐を停止しました")),
        Command::Restart => {
            stop(&config_path).and_then(|_| autostart::start()).map(|_| println!("常駐を再起動しました"))
        }
        Command::Status => status(&config_path),
        Command::Run { log_stderr } => return run_agent(&config_path, log_stderr),
        Command::Convert(args) => convert(&config_path, args),
        Command::Config => show_config(&config_path),
        Command::Worker(args) => {
            let ctx = ConvertCtx {
                quality: args.quality,
                speed: args.speed,
                original: args.original,
                skip_if_larger: args.skip_if_larger,
                encoder: if args.encoder == "native" { EncoderKind::Native } else { EncoderKind::Rav1e },
                dry_run: false,
            };
            return crate::scheduler::run_worker(&ctx);
        }
    };
    match result {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("エラー: {e:#}");
            1
        }
    }
}

/// Entry point of the resident agent. Configuration problems exit with status 0 so launchd's
/// KeepAlive does not restart us in a loop (which would wake the machine periodically).
pub fn run_agent(config_path: &Path, log_stderr: bool) -> i32 {
    if log_stderr {
        logger::init_stderr();
    } else if let Err(e) = logger::init_file(default_log_path(config_path), false) {
        eprintln!("ログファイルを開けません: {e}");
    }
    let cfg = match Config::load(config_path) {
        Ok(c) => c,
        Err(e) => {
            crate::error!("{e:#}");
            return 0;
        }
    };
    let lock =
        match fs::OpenOptions::new().read(true).write(true).create(true).truncate(false).open(lock_path(config_path)) {
            Ok(f) => f,
            Err(e) => {
                crate::error!("ロックファイルを開けません: {e}");
                return 0;
            }
        };
    if lock.try_lock().is_err() {
        crate::info!("すでに起動しています");
        return 0;
    }
    match daemon::run(cfg, config_path) {
        Ok(()) => 0,
        Err(e) => {
            crate::error!("{e:#}");
            0
        }
    }
}

// ---------- prompts ----------

struct Prompt {
    input: Box<dyn BufRead>,
    interactive: bool,
}

impl Prompt {
    fn open(yes: bool) -> Prompt {
        if yes {
            return Prompt { input: Box::new(io::empty()), interactive: false };
        }
        // `curl ... | sh` leaves stdin as the pipe; read answers from the terminal instead.
        #[cfg(unix)]
        if !io::stdin().is_terminal()
            && let Ok(tty) = fs::File::open("/dev/tty")
        {
            return Prompt { input: Box::new(io::BufReader::new(tty)), interactive: true };
        }
        let interactive = io::stdin().is_terminal();
        Prompt { input: Box::new(io::BufReader::new(io::stdin())), interactive }
    }

    fn ask(&mut self, question: &str, default: &str) -> String {
        if !self.interactive {
            return default.to_string();
        }
        print!("{question}");
        if !default.is_empty() {
            print!(" [{default}]");
        }
        print!(": ");
        let _ = io::stdout().flush();
        let mut line = String::new();
        if self.input.read_line(&mut line).unwrap_or(0) == 0 {
            return default.to_string();
        }
        let t = line.trim();
        if t.is_empty() { default.to_string() } else { t.to_string() }
    }

    fn confirm(&mut self, question: &str, default: bool) -> bool {
        let d = if default { "Y/n" } else { "y/N" };
        let a = self.ask(question, d);
        if a == d {
            return default;
        }
        matches!(a.to_lowercase().as_str(), "y" | "yes" | "はい")
    }
}

fn install(config_path: &Path, args: InstallArgs) -> Result<()> {
    let existing = Config::load(config_path).ok();
    let mut cfg = existing.clone().unwrap_or_default();
    let mut prompt = Prompt::open(args.yes);
    println!("{APP_NAME} v{VERSION} のセットアップ");

    // Folders
    let folders: Vec<PathBuf> = if !args.folders.is_empty() {
        args.folders.clone()
    } else {
        let default = existing
            .as_ref()
            .and_then(|c| c.folders.first().cloned())
            .map(|f| config::expand_tilde(&f))
            .unwrap_or_else(paths::pictures_dir);
        let answer = prompt.ask("監視するフォルダ", &default.to_string_lossy());
        vec![config::expand_tilde(&answer)]
    };
    for f in &folders {
        if !args.force {
            config::check_folder_safe(f)?;
        } else if !f.is_dir() {
            bail!("フォルダが見つかりません: {}", f.display());
        }
    }
    cfg.folders = folders.iter().map(|f| config::contract_tilde(&paths::canonical(f).unwrap_or(f.clone()))).collect();

    // Original policy
    cfg.original = match args.original {
        Some(p) => p,
        None => {
            let current = match cfg.original {
                OriginalPolicy::Delete => "1",
                OriginalPolicy::Trash => "2",
                OriginalPolicy::Keep => "3",
            };
            match prompt.ask("変換後の元ファイル 1) 完全に削除  2) ゴミ箱へ移動  3) 残す", current).as_str()
            {
                "2" => OriginalPolicy::Trash,
                "3" => OriginalPolicy::Keep,
                _ => OriginalPolicy::Delete,
            }
        }
    };

    // Existing photos
    cfg.scan_existing = if args.scan_existing {
        true
    } else if args.new_only {
        false
    } else {
        prompt.confirm("すでにフォルダにある写真も変換しますか? (いいえ = 新しく追加された写真のみ)", cfg.scan_existing)
    };
    if let Some(q) = args.quality {
        cfg.quality = q;
    }
    cfg.validate()?;

    // Show what will happen to existing files before anything is deleted.
    if cfg.scan_existing {
        let filter = Filter::new(&cfg);
        let mut count = 0u64;
        let mut bytes = 0u64;
        for f in &folders {
            let root = paths::canonical(f)?;
            walk(&root, &root, &filter, &mut |p| {
                count += 1;
                bytes += fs::metadata(&p).map(|m| m.len()).unwrap_or(0);
            });
        }
        println!("既存の写真: {count} 件 ({})", human(bytes));
        if count > 0 && cfg.original != OriginalPolicy::Keep {
            let q =
                format!("これらを AVIF に変換し、元ファイルを「{}」します。よろしいですか?", cfg.original.describe());
            if !args.yes && !prompt.confirm(&q, false) {
                bail!("中止しました");
            }
        }
    } else if cfg.original == OriginalPolicy::Delete
        && !args.yes
        && !prompt.confirm("変換に成功した元ファイルは完全に削除されます (元に戻せません)。よろしいですか?", false)
    {
        bail!("中止しました");
    }

    cfg.save(config_path)?;
    println!("設定を保存しました: {}", config_path.display());

    let exe = autostart::agent_executable()?;
    let log_dir = default_log_path(config_path).parent().map(Path::to_path_buf).unwrap_or_else(paths::log_dir);
    if agent_running(config_path) {
        let _ = stop(config_path);
    }
    autostart::install(&exe, config_path, &log_dir, !args.no_start)?;
    println!("自動起動に登録しました ({})", autostart::describe());
    if !args.no_start {
        println!("常駐を開始しました。ログ: {}", default_log_path(config_path).display());
    }
    println!(
        "エンコーダ: {}  / 監視: {}  / 元ファイル: {}  / 既存の写真: {}",
        crate::encode::resolve(cfg.encoder),
        cfg.folders.join(", "),
        cfg.original.describe(),
        if cfg.scan_existing { "変換する" } else { "新規のみ" }
    );
    Ok(())
}

fn stop(config_path: &Path) -> Result<()> {
    autostart::stop()?;
    if agent_running(config_path) {
        platform::instance::request_stop();
        for _ in 0..100 {
            if !agent_running(config_path) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
    }
    Ok(())
}

fn uninstall(config_path: &Path, purge: bool) -> Result<()> {
    stop(config_path)?;
    autostart::uninstall()?;
    println!("自動起動を解除しました");
    if purge {
        purge_files(config_path);
        println!("設定とログを削除しました");
    }
    if let Ok(exe) = std::env::current_exe() {
        println!("実行ファイルは手動で削除してください: {}", exe.parent().unwrap_or(&exe).display());
    }
    Ok(())
}

/// Removes only files this tool created. Folders are removed only when left empty, so a
/// config placed in a shared folder (e.g. `--config ~/Documents/x.toml`) cannot take it along.
fn purge_files(config_path: &Path) -> Vec<PathBuf> {
    let state = state_path_for(config_path);
    let log = default_log_path(config_path);
    let mut files = vec![
        config_path.to_path_buf(),
        config_path.with_extension("tmp"),
        state.clone(),
        state.with_extension("tmp"),
        lock_path(config_path),
        log.clone(),
        log.with_extension("log.1"),
    ];
    if let Some(log_dir) = log.parent() {
        files.push(log_dir.join("launchd.log"));
    }
    let mut removed = Vec::new();
    for f in files {
        if fs::remove_file(&f).is_ok() {
            removed.push(f);
        }
    }
    for dir in [config_path.parent(), log.parent()].into_iter().flatten() {
        let _ = fs::remove_dir(dir); // fails unless empty
    }
    removed
}

fn status(config_path: &Path) -> Result<()> {
    println!("{APP_NAME} v{VERSION}");
    println!("設定ファイル: {}", config_path.display());
    println!("常駐: {}", if agent_running(config_path) { "動作中" } else { "停止中" });
    println!("自動起動: {}", if autostart::is_registered() { autostart::describe() } else { "未登録".into() });
    match Config::load(config_path) {
        Ok(cfg) => {
            println!(
                "エンコーダ: {} / 品質: {} / 元ファイル: {} / 既存の写真: {}",
                crate::encode::resolve(cfg.encoder),
                cfg.quality,
                cfg.original.describe(),
                if cfg.scan_existing { "変換する" } else { "新規のみ" }
            );
            for f in cfg.folder_paths() {
                let access = match fs::read_dir(&f) {
                    Ok(_) => "OK".to_string(),
                    Err(e) => format!("アクセスできません ({e})"),
                };
                println!("監視フォルダ: {}  [{access}]", f.display());
            }
            let state = State::load(&state_path_for(config_path));
            if !state.skipped.is_empty() {
                println!("スキップ中のファイル: {} 件", state.skipped.len());
            }
        }
        Err(e) => println!("設定: {e:#}"),
    }
    let log = default_log_path(config_path);
    if let Ok(text) = fs::read_to_string(&log) {
        println!("--- ログ ({}) ---", log.display());
        let lines: Vec<&str> = text.lines().collect();
        for l in &lines[lines.len().saturating_sub(10)..] {
            println!("{l}");
        }
    }
    Ok(())
}

fn show_config(config_path: &Path) -> Result<()> {
    println!("# {}", config_path.display());
    match fs::read_to_string(config_path) {
        Ok(t) => print!("{t}"),
        Err(_) => println!("(未作成。`{APP_NAME} install` で作成されます)"),
    }
    Ok(())
}

fn convert(config_path: &Path, args: ConvertArgs) -> Result<()> {
    let mut cfg = Config::load(config_path).unwrap_or_default();
    if let Some(o) = args.original {
        cfg.original = o;
    }
    if let Some(q) = args.quality {
        cfg.quality = q;
    }
    if let Some(e) = args.encoder {
        cfg.encoder = e;
    }
    cfg.validate()?;
    let filter = Filter::new(&cfg);
    let mut files = Vec::new();
    for p in &args.paths {
        let canon = paths::canonical(p).with_context(|| format!("見つかりません: {}", p.display()))?;
        if canon.is_dir() {
            walk(&canon, &canon, &filter, &mut |f| files.push(f));
        } else if canon.parent().is_some_and(|parent| filter.accepts(parent, &canon)) {
            files.push(canon);
        } else {
            eprintln!("対象外の形式: {}", p.display());
        }
    }
    if files.is_empty() {
        println!("変換対象のファイルがありません");
        return Ok(());
    }
    let state_path = state_path_for(config_path);
    let state = Arc::new(Mutex::new(State::load(&state_path)));
    // Manual runs retry files the agent skipped earlier.
    state.lock().unwrap_or_else(|e| e.into_inner()).skipped.clear();
    let encoder = crate::encode::resolve(cfg.encoder);
    let mut ctx = ConvertCtx::new(&cfg, encoder);
    ctx.dry_run = args.dry_run;
    println!(
        "{} 件 / エンコーダ: {encoder} / 品質: {} / 元ファイル: {}{}",
        files.len(),
        cfg.quality,
        cfg.original.describe(),
        if args.dry_run { " (dry run)" } else { "" }
    );

    let jobs = args.jobs.unwrap_or_else(|| cfg.effective_jobs()).max(1);
    let queue = Mutex::new(files);
    let totals = Mutex::new((0usize, 0u64, 0u64, 0usize));
    std::thread::scope(|s| {
        for _ in 0..jobs {
            s.spawn(|| {
                let _com = platform::ThreadInit::new();
                loop {
                    let Some(path) = queue.lock().unwrap_or_else(|e| e.into_inner()).pop() else { break };
                    let outcome = convert_file(&ctx, &path);
                    if let Some(line) = outcome.describe(&path) {
                        println!("{line}");
                    }
                    if let Some((fp, reason)) = outcome.remember() {
                        state.lock().unwrap_or_else(|e| e.into_inner()).record_skip(&path, fp, reason);
                    }
                    let mut t = totals.lock().unwrap_or_else(|e| e.into_inner());
                    match outcome {
                        Outcome::Converted { before, after, .. } => {
                            t.0 += 1;
                            t.1 += before;
                            t.2 += after;
                        }
                        Outcome::Failed { .. } => t.3 += 1,
                        _ => {}
                    }
                }
            });
        }
    });
    let (n, before, after, failed) = *totals.lock().unwrap_or_else(|e| e.into_inner());
    if !args.dry_run {
        println!("完了: {n} 件変換 ({} → {})、失敗 {failed} 件", human(before), human(after));
        let st = state.lock().unwrap_or_else(|e| e.into_inner());
        let _ = st.save(&state_path);
    }
    if failed > 0 {
        bail!("{failed} 件の変換に失敗しました");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn purge_removes_only_our_files() {
        let dir = tempfile::tempdir().unwrap();
        let shared = dir.path().join("Documents");
        fs::create_dir_all(shared.join("sub")).unwrap();
        let config = shared.join("my-config.toml");
        for f in
            ["my-config.toml", "state.toml", "agent.lock", "optimize-image-folder.log", "notes.txt", "sub/photo.jpg"]
        {
            fs::write(shared.join(f), b"x").unwrap();
        }
        let removed = purge_files(&config);
        assert_eq!(removed.len(), 4, "{removed:?}");
        assert!(shared.join("notes.txt").exists());
        assert!(shared.join("sub/photo.jpg").exists());
        assert!(!config.exists());
    }

    #[test]
    fn purge_removes_empty_dedicated_folder() {
        let dir = tempfile::tempdir().unwrap();
        let own = dir.path().join("optimize-image-folder");
        fs::create_dir_all(&own).unwrap();
        let config = own.join("config.toml");
        fs::write(&config, b"x").unwrap();
        fs::write(own.join("state.toml"), b"x").unwrap();
        purge_files(&config);
        assert!(!own.exists());
    }
}
