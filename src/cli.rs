//! Command-line interface.

use std::fs;
use std::io::{self, BufRead, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use clap::{Args, Parser, Subcommand};

use crate::LockUnpoisoned;
use crate::app::{self, agent_running, default_config_path, lock_path_for, log_path_for};
use crate::config::{self, Config, EFFORT_RANGE, OriginalPolicy, Quality};
use crate::convert::{ConvertCtx, Outcome, convert_guarded, default_threads, human};
use crate::daemon::{self, state_path_for};
use crate::filter::Filter;
use crate::platform::{self, autostart, paths};
use crate::state::State;
use crate::walk::walk;
use crate::{APP_NAME, DISPLAY_NAME, VERSION, logger, restore, update};

/// How long an on-demand (launchd) run stays alive after the last activity.
const ON_DEMAND_IDLE_EXIT: Duration = Duration::from_secs(5);

#[derive(Parser, Debug)]
#[command(name = APP_NAME, version, about = "写真フォルダの写真を劣化なしで JPEG XL に変換します")]
pub struct Cli {
    /// 設定ファイルのパス (既定: OS 標準の場所)
    #[arg(long, global = true, value_name = "PATH")]
    pub config: Option<PathBuf>,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// 設定を作成して自動起動に登録する
    Install(InstallArgs),
    /// 自動起動を解除する
    Uninstall {
        /// 設定・状態・ログも削除する
        #[arg(long)]
        purge: bool,
    },
    /// 設定画面を開く
    Settings,
    /// 監視を開始する
    Start,
    /// 監視を停止する
    Stop,
    /// 監視を再起動する (設定ファイルを手で編集したとき)
    Restart,
    /// 状態を表示する
    Status,
    /// 変換を実行する (自動起動から呼ばれます)
    Run {
        /// ログをファイルではなく標準エラーに出す
        #[arg(long)]
        log_stderr: bool,
        /// 待機状態になったら終了する (macOS の launchd からの起動用)
        #[arg(long)]
        on_demand: bool,
    },
    /// ファイル/フォルダを今すぐ変換する
    Convert(ConvertArgs),
    /// JPEG XL から元のファイルを復元する (JPEG はバイト単位で元どおり、それ以外は PNG)
    Restore(RestoreArgs),
    /// 設定ファイルの場所と内容を表示する
    Config,
    /// 新しいバージョンがあれば更新する
    Update {
        /// (自動実行用) 自動アップデートがオンで、前回の確認から時間がたっているときだけ確認する
        #[arg(long, hide = true)]
        auto: bool,
    },
    /// (内部用) 標準入力から受け取ったファイルを変換する使い捨てプロセス
    #[command(hide = true)]
    Worker(WorkerArgs),
    /// (内部用) メニューバーのアイコン。ログイン時に launchd が起動する
    #[cfg(target_os = "macos")]
    #[command(hide = true)]
    Tray,
    /// (インストーラー用) JPEG XL を表示できるか確認し、必要なら表示用の拡張機能を入れる。
    /// 終了コード: 0 = 表示できる / 1 = できない / 2 = インストール失敗 / 3 = この OS では非対応
    #[command(hide = true)]
    Viewer {
        /// 表示できなければ拡張機能 (Microsoft 公式・無料) をインストールする
        #[arg(long)]
        install: bool,
        /// インストールに失敗したら Microsoft Store のページを開く
        #[arg(long)]
        open_store: bool,
    },
}

/// Encoding options shared by `install` and `convert`; unset ones keep the config file's value.
#[derive(Args, Debug, Default)]
pub struct EncodingArgs {
    /// 変換後の元ファイルの扱い
    #[arg(long, value_enum)]
    pub original: Option<OriginalPolicy>,
    /// 圧縮率 1(弱)-9(強)。変わるのは速度と容量だけで、画質は同じ
    #[arg(long, value_parser = clap::value_parser!(u8).range(i64::from(*EFFORT_RANGE.start())..=i64::from(*EFFORT_RANGE.end())))]
    pub effort: Option<u8>,
    /// PNG、TIFF、BMP、ロスレス WebP の画質 (JPEG はどれでも劣化なし)
    #[arg(long, value_enum)]
    pub quality: Option<Quality>,
}

impl EncodingArgs {
    fn apply(&self, cfg: &mut Config) {
        if let Some(o) = self.original {
            cfg.original = o;
        }
        if let Some(e) = self.effort {
            cfg.effort = e;
        }
        if let Some(q) = self.quality {
            cfg.quality = q;
        }
    }
}

#[derive(Args, Debug, Default)]
pub struct InstallArgs {
    /// 変換するフォルダ (複数指定可。既定: ピクチャフォルダ)
    #[arg(long = "folder", value_name = "DIR")]
    pub folders: Vec<PathBuf>,
    /// 除外するフォルダ (複数指定可)
    #[arg(long = "exclude", value_name = "DIR")]
    pub exclude: Vec<PathBuf>,
    /// 既存の写真も変換する
    #[arg(long, conflicts_with = "new_only")]
    pub scan_existing: bool,
    /// 新しく追加された写真だけ変換する
    #[arg(long)]
    pub new_only: bool,
    #[command(flatten)]
    pub encoding: EncodingArgs,
    /// 確認なしで進める
    #[arg(long, short = 'y')]
    pub yes: bool,
    /// 安全チェック (ホームフォルダ全体などの拒否) を無効にする
    #[arg(long)]
    pub force: bool,
    /// 登録だけして開始しない
    #[arg(long)]
    pub no_start: bool,
}

#[derive(Args, Debug)]
pub struct ConvertArgs {
    /// 変換するファイルまたはフォルダ
    #[arg(required = true)]
    pub paths: Vec<PathBuf>,
    #[command(flatten)]
    pub encoding: EncodingArgs,
    /// 何が変換されるか表示するだけ
    #[arg(long)]
    pub dry_run: bool,
    /// 同時変換数
    #[arg(long, short = 'j')]
    pub jobs: Option<usize>,
}

#[derive(Args, Debug)]
pub struct RestoreArgs {
    /// 復元する .jxl ファイルまたはフォルダ
    #[arg(required = true)]
    pub paths: Vec<PathBuf>,
    /// 復元に成功した .jxl を削除する
    #[arg(long)]
    pub delete_jxl: bool,
}

/// Arguments of the hidden worker. Keep them compatible across versions: during an upgrade, a
/// running agent of the previous version starts workers from the new binary.
#[derive(Args, Debug)]
pub struct WorkerArgs {
    #[arg(long)]
    pub effort: u8,
    #[arg(long)]
    pub threads: usize,
    #[arg(long, value_enum)]
    pub original: OriginalPolicy,
    /// libjxl quality of a lossy level (absent = lossless).
    #[arg(long, value_parser = parse_worker_quality)]
    pub quality: Option<Quality>,
    #[arg(long, action = clap::ArgAction::Set)]
    pub skip_if_larger: bool,
}

fn parse_worker_quality(s: &str) -> Result<Quality, String> {
    let n = s.parse::<u8>().map_err(|e| e.to_string())?;
    Quality::ALL.into_iter().find(|q| q.jxl_quality() == Some(n)).ok_or_else(|| format!("未対応の画質: {n}"))
}

pub fn main() -> i32 {
    // Opened from the settings app bundle (macOS) without arguments.
    #[cfg(target_os = "macos")]
    if std::env::args_os().len() == 1 && autostart::launched_from_app_bundle() {
        return report(platform::ui::run_settings(&default_config_path()));
    }
    let cli = Cli::parse();
    let config_path = cli.config.clone().unwrap_or_else(default_config_path);
    let result = match cli.command {
        Command::Install(args) => install(&config_path, args),
        Command::Uninstall { purge } => uninstall(&config_path, purge),
        Command::Settings => platform::ui::run_settings(&config_path),
        Command::Start => autostart::start().map(|_| println!("監視を開始しました")),
        Command::Stop => app::stop_agent(&config_path).map(|_| println!("監視を停止しました")),
        Command::Restart => {
            app::stop_agent(&config_path).and_then(|_| autostart::start()).map(|_| println!("監視を再起動しました"))
        }
        Command::Status => status(&config_path),
        Command::Run { log_stderr, on_demand } => return run_agent(&config_path, log_stderr, on_demand),
        Command::Convert(args) => convert(&config_path, args),
        Command::Restore(args) => restore_cmd(&config_path, args),
        Command::Config => show_config(&config_path),
        Command::Update { auto: true } => {
            if let Err(e) = logger::init_file(log_path_for(&config_path)) {
                eprintln!("ログファイルを開けません: {e}");
            }
            update::log_result(update::check(&config_path, true));
            return 0;
        }
        Command::Update { auto: false } => {
            update::check(&config_path, false).map(|message| println!("{}", message.unwrap_or_default()))
        }
        Command::Viewer { install, open_store } => return viewer_cmd(install, open_store),
        #[cfg(target_os = "macos")]
        Command::Tray => platform::tray::run(&config_path),
        Command::Worker(args) => {
            let ctx = ConvertCtx {
                effort: args.effort,
                quality: args.quality.unwrap_or_default(),
                threads: args.threads.max(1),
                original: args.original,
                skip_if_larger: args.skip_if_larger,
                dry_run: false,
            };
            return crate::scheduler::run_worker(&ctx);
        }
    };
    report(result)
}

fn report(result: Result<()>) -> i32 {
    match result {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("エラー: {e:#}");
            1
        }
    }
}

/// Entry point of the agent. Configuration problems exit with status 0 so launchd does not
/// restart us in a loop (which would wake the machine periodically).
pub fn run_agent(config_path: &Path, log_stderr: bool, on_demand: bool) -> i32 {
    if log_stderr {
        logger::init_stderr();
    } else if let Err(e) = logger::init_file(log_path_for(config_path)) {
        eprintln!("ログファイルを開けません: {e}");
    }
    let cfg = match Config::load_or_create(config_path) {
        Ok(c) => c,
        Err(e) => {
            crate::error!("{e:#}");
            return 0;
        }
    };
    let lock = match app::open_lock(config_path) {
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
    if let Ok(spec) = app::agent_spec(config_path, &cfg) {
        platform::on_agent_start(&spec);
    }
    let opts = daemon::Options { exit_when_idle: on_demand.then_some(ON_DEMAND_IDLE_EXIT) };
    let result = daemon::run(cfg, config_path, opts);
    platform::on_agent_exit();
    if let Err(e) = result {
        crate::error!("{e:#}");
    }
    0
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
        match line.trim() {
            "" => default.to_string(),
            t => t.to_string(),
        }
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
    let mut cfg = Config::load(config_path).unwrap_or_default();
    let mut prompt = Prompt::open(args.yes);
    println!("{DISPLAY_NAME} v{VERSION} のセットアップ");

    let folders: Vec<PathBuf> = if !args.folders.is_empty() {
        args.folders.clone()
    } else if args.yes {
        // Non-interactive (installer, upgrades): keep every configured folder.
        cfg.folder_paths()
    } else {
        let default = cfg.folder_paths().into_iter().next().unwrap_or_else(paths::pictures_dir);
        let answer = prompt.ask("変換するフォルダ", &default.to_string_lossy());
        vec![config::expand_tilde(&answer)]
    };
    let pictures = paths::pictures_dir();
    for f in &folders {
        // The OS's standard Pictures folder can be missing on a fresh profile; create it.
        if *f == pictures && !f.exists() {
            fs::create_dir_all(f).with_context(|| format!("フォルダを作成できません: {}", f.display()))?;
        }
        if !args.force {
            config::check_folder_safe(f)?;
        } else if !f.is_dir() {
            bail!("フォルダが見つかりません: {}", f.display());
        }
    }
    cfg.folders = folders.iter().map(|f| config::contract_tilde(&paths::canonical(f).unwrap_or(f.clone()))).collect();
    if !args.exclude.is_empty() {
        cfg.exclude_folders = args.exclude.iter().map(|f| config::contract_tilde(f)).collect();
    }

    cfg.original = match args.encoding.original {
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
    cfg.scan_existing = if args.scan_existing {
        true
    } else if args.new_only {
        false
    } else {
        prompt.confirm("すでにフォルダにある写真も変換しますか? (いいえ = 新しく追加された写真のみ)", cfg.scan_existing)
    };
    args.encoding.apply(&mut cfg);
    cfg.validate()?;

    if cfg.scan_existing {
        let filter = Filter::new(&cfg);
        let (mut count, mut bytes) = (0u64, 0u64);
        for f in &folders {
            let root = paths::canonical(f)?;
            walk(&root, &root, &filter, &mut |_, md| {
                count += 1;
                bytes += md.len();
            });
        }
        println!("既存の写真: {count} 件 ({})", human(bytes));
        if count > 0
            && cfg.original != OriginalPolicy::Keep
            && !args.yes
            && !prompt.confirm(
                &format!(
                    "JPEG XL に変換し (画質: {})、検証に成功した元ファイルを「{}」します。よろしいですか?",
                    app::mode_label(&cfg),
                    cfg.original.describe()
                ),
                false,
            )
        {
            bail!("中止しました");
        }
    }

    cfg.save(config_path)?;
    println!("設定を保存しました: {}", config_path.display());
    if agent_running(config_path) {
        app::stop_agent(config_path)?;
    }
    app::register_agent(config_path, &cfg, !args.no_start)?;
    println!("自動起動に登録しました ({})", autostart::describe());
    println!("監視: {} / {}", cfg.folders.join(", "), app::summary(&cfg));
    if let Some(hint) = platform::image::viewer_hint() {
        println!("注意: {hint}");
    }
    println!("設定はいつでも `{APP_NAME} settings` で変更できます。");
    Ok(())
}

fn uninstall(config_path: &Path, purge: bool) -> Result<()> {
    app::stop_agent(config_path)?;
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
    let log = log_path_for(config_path);
    let mut files = vec![
        config_path.to_path_buf(),
        config::temp_path(config_path),
        config::temp_path(&state),
        state,
        lock_path_for(config_path),
        logger::backup_path(&log),
        log.clone(),
        update::stamp_path(config_path),
    ];
    if let Some(log_dir) = log.parent() {
        files.extend(autostart::LOG_FILES.iter().map(|f| log_dir.join(f)));
    }
    let removed: Vec<PathBuf> = files.into_iter().filter(|f| fs::remove_file(f).is_ok()).collect();
    for dir in [config_path.parent(), log.parent()].into_iter().flatten() {
        let _ = fs::remove_dir(dir); // fails unless empty
    }
    removed
}

fn status(config_path: &Path) -> Result<()> {
    println!("{DISPLAY_NAME} v{VERSION} (libjxl {})", crate::jxl::libjxl_version());
    println!("設定ファイル: {}", config_path.display());
    let running = agent_running(config_path);
    let registered = autostart::is_registered();
    let state = match (running, registered, cfg!(target_os = "macos")) {
        (true, _, true) => "変換処理中",
        (true, _, false) => "監視中",
        (false, true, true) => "待機中 (launchd がフォルダを監視。プロセスは起動していません)",
        (false, true, false) => "停止中",
        (false, false, _) => "未登録",
    };
    println!("状態: {state}");
    if registered {
        println!("自動起動: {}", autostart::describe());
    }
    match Config::load(config_path) {
        Ok(cfg) => {
            println!("{}", app::summary(&cfg));
            for f in cfg.folder_paths() {
                let access = match fs::read_dir(&f) {
                    Ok(_) => "OK".to_string(),
                    Err(e) => format!("アクセスできません ({e})"),
                };
                println!("変換するフォルダ: {}  [{access}]", f.display());
            }
            for f in cfg.exclude_folder_paths() {
                println!("除外するフォルダ: {}", f.display());
            }
            let state = State::load(&state_path_for(config_path));
            if !state.skipped.is_empty() {
                println!("変換しなかったファイル: {} 件 (劣化なしに変換できない等)", state.skipped.len());
            }
        }
        Err(e) => println!("設定: {e:#}"),
    }
    if let Some(hint) = platform::image::viewer_hint() {
        println!("注意: {hint}");
    }
    let log = log_path_for(config_path);
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
        Err(_) => println!("(未作成。`{APP_NAME} install` または設定画面で作成されます)"),
    }
    Ok(())
}

fn convert(config_path: &Path, args: ConvertArgs) -> Result<()> {
    let mut cfg = Config::load(config_path).unwrap_or_default();
    args.encoding.apply(&mut cfg);
    cfg.validate()?;
    let filter = Filter::new(&cfg);
    let mut files = Vec::new();
    for p in &args.paths {
        let canon = paths::canonical(p).with_context(|| format!("見つかりません: {}", p.display()))?;
        if canon.is_dir() {
            walk(&canon, &canon, &filter, &mut |f, _| files.push(f));
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
    state.lock_unpoisoned().skipped.clear();
    let jobs = args.jobs.unwrap_or(1).max(1);
    let mut ctx = ConvertCtx::new(&cfg);
    ctx.threads = (default_threads() * 2 / jobs).max(1); // user-initiated: use the whole machine
    ctx.dry_run = args.dry_run;
    println!("{} 件 / {}{}", files.len(), app::encoding_summary(&cfg), if args.dry_run { " (dry run)" } else { "" });

    let queue = Mutex::new(files);
    let totals = Mutex::new((0usize, 0u64, 0u64, 0usize));
    std::thread::scope(|s| {
        for _ in 0..jobs {
            s.spawn(|| {
                loop {
                    let Some(path) = queue.lock_unpoisoned().pop() else { break };
                    let outcome = convert_guarded(&ctx, &path);
                    if let Some(line) = outcome.describe(&path) {
                        println!("{line}");
                    }
                    state.lock_unpoisoned().record(&path, &outcome);
                    let mut t = totals.lock_unpoisoned();
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
    let (n, before, after, failed) = *totals.lock_unpoisoned();
    if !args.dry_run {
        println!("完了: {n} 件変換 ({} → {})、失敗 {failed} 件", human(before), human(after));
        let st = state.lock_unpoisoned();
        let _ = st.save(&state_path);
    }
    if failed > 0 {
        bail!("{failed} 件の変換に失敗しました");
    }
    Ok(())
}

fn viewer_cmd(install: bool, open_store: bool) -> i32 {
    if platform::image::viewer_available() {
        println!("JPEG XL を表示できます");
        return 0;
    }
    if let Some(hint) = platform::image::viewer_hint() {
        println!("{hint}");
    }
    if !install {
        return 1;
    }
    #[cfg(windows)]
    {
        if !platform::image::viewer_extension_supported() {
            return 3;
        }
        if platform::image::install_viewer_extension() {
            println!("JPEG XL Image Extension をインストールしました");
            return 0;
        }
        if open_store {
            platform::image::open_store_page();
        }
        2
    }
    #[cfg(not(windows))]
    {
        let _ = open_store;
        3
    }
}

fn restore_cmd(config_path: &Path, args: RestoreArgs) -> Result<()> {
    let mut files = Vec::new();
    for p in &args.paths {
        let canon = paths::canonical(p).with_context(|| format!("見つかりません: {}", p.display()))?;
        if canon.is_dir() {
            collect_jxl(&canon, &mut files);
        } else {
            files.push(canon);
        }
    }
    // Restored files must not be converted again: pause the agent, restore, and record them as
    // skipped in its state before it resumes (it would otherwise see them as new photos).
    let registered = config_path.exists() && autostart::is_registered();
    if registered {
        app::stop_agent(config_path)?;
    }
    let mut failed = 0;
    let mut restored = Vec::new();
    for f in &files {
        match restore::restore_file(f, args.delete_jxl) {
            Ok(out) => {
                println!("復元: {} → {}", f.display(), out.display());
                restored.push(out);
            }
            Err(e) => {
                failed += 1;
                eprintln!("失敗: {} ({e:#})", f.display());
            }
        }
    }
    if config_path.exists() && !restored.is_empty() {
        let state_path = state_path_for(config_path);
        let mut state = State::load(&state_path);
        for out in &restored {
            if let Some(fp) = crate::convert::Fingerprint::of_path(out) {
                state.record_skip(out, fp, "restore で復元したファイル");
            }
        }
        state.save(&state_path)?;
    }
    if registered {
        autostart::start()?;
    }
    if failed > 0 {
        bail!("{failed} 件の復元に失敗しました");
    }
    Ok(())
}

fn collect_jxl(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else { return };
    for e in entries.flatten() {
        let p = e.path();
        if e.file_type().is_ok_and(|t| t.is_dir()) {
            collect_jxl(&p, out);
        } else if p.extension().is_some_and(|x| x.eq_ignore_ascii_case(crate::convert::OUTPUT_EXTENSION)) {
            out.push(p);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn worker_quality(args: &[&str]) -> Option<Quality> {
        let base = ["optiman", "worker", "--effort=7", "--threads=2", "--original=delete", "--skip-if-larger=true"];
        match Cli::try_parse_from(base.iter().chain(args)).unwrap().command {
            Command::Worker(w) => w.quality,
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn worker_reads_the_arguments_of_older_agents() {
        // v0.4.0 passes nothing for lossless and libjxl's number for the lossy levels.
        assert_eq!(worker_quality(&[]), None);
        assert_eq!(worker_quality(&["--quality=80"]), Some(Quality::Medium));
        assert!(
            Cli::try_parse_from([
                "optiman",
                "worker",
                "--effort=7",
                "--threads=2",
                "--original=keep",
                "--skip-if-larger=false",
                "--quality=85"
            ])
            .is_err()
        );
        for q in Quality::ALL {
            let ctx = ConvertCtx { quality: q, ..ConvertCtx::new(&Config::default()) };
            let spec = crate::scheduler::WorkerSpec::for_ctx(&ctx).unwrap();
            let args: Vec<&str> = spec.args.iter().map(|a| a.to_str().unwrap()).collect();
            let parsed = Cli::try_parse_from(std::iter::once("optiman").chain(args)).unwrap();
            assert!(matches!(parsed.command, Command::Worker(w) if w.quality.unwrap_or_default() == q), "{q:?}");
        }
    }

    #[test]
    fn purge_removes_only_our_files() {
        let dir = tempfile::tempdir().unwrap();
        let shared = dir.path().join("Documents");
        fs::create_dir_all(shared.join("sub")).unwrap();
        let config = shared.join("my-config.toml");
        for f in ["my-config.toml", "state.toml", "agent.lock", "optiman.log", "notes.txt", "sub/photo.jpg"] {
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
        let own = dir.path().join("optiman");
        fs::create_dir_all(&own).unwrap();
        let config = own.join("config.toml");
        fs::write(&config, b"x").unwrap();
        fs::write(own.join("state.toml"), b"x").unwrap();
        purge_files(&config);
        assert!(!own.exists());
    }
}
