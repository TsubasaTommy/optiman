# optimize-image-folder

写真フォルダを監視して、新しく入った写真を AV1 (AVIF) に自動で変換する macOS / Windows 用の常駐ツールです。
Rust から OS のネイティブ API を直接呼び出しており、**何もしていない間は CPU を一切使いません**。

- ダウンロードとインストール手順: <https://tsubasatommy.github.io/optimize-image-folder/>
- リリース: <https://github.com/TsubasaTommy/optimize-image-folder/releases>

> [!WARNING]
> 既定では、変換に成功した**元の写真を完全に削除**します。インストール時に「ゴミ箱へ移動」(`trash`) か「残す」(`keep`) を選ぶこともできます。

## インストール

macOS (ターミナル):

```sh
curl -fsSL https://tsubasatommy.github.io/optimize-image-folder/install.sh | sh
```

Windows (PowerShell):

```powershell
irm https://tsubasatommy.github.io/optimize-image-folder/install.ps1 | iex
```

インストーラは最新のリリースをダウンロードし、SHA256 を確認してから配置します (macOS: `~/.local/bin`、Windows: `%LOCALAPPDATA%\Programs\optimize-image-folder`)。
続いて `optimize-image-folder install` が起動し、次の 3 点を聞かれます。

1. 監視するフォルダ (既定: ピクチャフォルダ)
2. 変換後の元ファイル: 完全に削除 / ゴミ箱へ移動 / 残す
3. すでにあるフォルダ内の写真も変換するか (いいえ = 新しく追加された写真だけ)

対話なしで設定する場合は、`curl ... | sh -s -- --folder ~/Pictures --new-only --original trash --yes` のようにオプションを渡します。

## 使い方

| コマンド | 内容 |
|---|---|
| `install` | 設定を作り、自動起動に登録して常駐を始める。`--folder` `--new-only` / `--scan-existing` `--original delete\|trash\|keep` `--quality` `--yes` |
| `status` | 常駐の状態、設定、監視フォルダにアクセスできるか、最近のログを表示 |
| `convert <パス>...` | ファイルやフォルダを今すぐ変換 (`--dry-run` で確認のみ、`--original` `--quality` `--encoder` `-j`) |
| `start` / `stop` / `restart` | 常駐の開始・停止・再起動 (設定の変更を反映) |
| `config` | 設定ファイルの場所と内容を表示 |
| `uninstall [--purge]` | 自動起動を解除。`--purge` で設定・状態・ログも削除 |
| `run` | フォアグラウンドで常駐 (自動起動から呼ばれる) |

### 設定ファイル

- macOS: `~/Library/Application Support/optimize-image-folder/config.toml`
- Windows: `%APPDATA%\optimize-image-folder\config.toml`

```toml
folders = ["~/Pictures"]   # 監視するフォルダ (サブフォルダも対象)
original = "delete"        # delete | trash | keep
scan_existing = false      # 起動時に既存の写真も変換するか
quality = 85               # 1-100
speed = 6                  # rav1e の速度 1 (高画質・遅い) - 10 (速い)
encoder = "auto"           # auto (=rav1e) | native (OS のエンコーダ) | rav1e
jobs = 0                   # 同時変換数 (0 = 自動、最大 2)
settle_seconds = 2         # 書き込みが止まってから変換するまでの秒数
skip_if_larger = true      # AVIF の方が大きくなる場合は元ファイルを残す
extensions = ["jpg", "jpeg", "png", "heic", "heif", "tif", "tiff", "bmp", "webp"]
exclude = []               # 除外するフォルダ/ファイル名のパターン (* と ?)
```

変更したら `optimize-image-folder restart` で反映されます。ログは macOS が `~/Library/Logs/optimize-image-folder/`、Windows が `%LOCALAPPDATA%\optimize-image-folder\logs\` にあります。

## 仕組み

### 待機中に CPU 0% である理由

常駐プロセスには、タイマーもポーリングもありません。どのスレッドも OS のブロッキング待ちで眠っています。

| | macOS | Windows |
|---|---|---|
| フォルダ監視 | FSEvents。専用の GCD キューに配送されるので、待機中は監視用のスレッドすら存在しない。`IgnoreSelf` フラグで、自分の書き込みや削除では起こされない | `ReadDirectoryChangesW` (サブツリー、overlapped) + I/O 完了ポート。監視スレッドは `GetQueuedCompletionStatus(INFINITE)` で待つ |
| メインループ | `mpsc::Receiver::recv()` (タイムアウトなし)。タイマー付きで待つのは、書き込み完了を待っているファイルがあるときだけ | 同左 |
| 停止要求 | launchd (SIGTERM) | 名前付きイベントを `RegisterWaitForSingleObject` で待つ (専用スレッドなし) |
| 変換 | 必要なときだけ子プロセス (`worker`) を起動。`PRIO_DARWIN_BG` と Background QoS で効率コア・I/O 抑制 | 必要なときだけ子プロセスを起動。`PROCESS_MODE_BACKGROUND_BEGIN` と EcoQoS |
| 変換後 | 子プロセスが終了し、デコーダとエンコーダのメモリはすべて OS に戻る | 同左 (常駐プロセス側でも `SetProcessWorkingSetSizeEx` で縮小) |

macOS (Apple Silicon) で `scripts/idle-check.sh` を実行し、1 枚変換したあとの待機状態を計測した結果です。

| 項目 | 結果 |
|---|---|
| 60 秒間に消費した CPU 時間 | 0 ms |
| アイドル中の起床 (top の idle wakeups) | 0 |
| スレッド数 | 2 |
| 常駐プロセスのメモリ (phys_footprint) | 変換前 1.9 MB / 変換後 2.0 MB |

CI でも、macOS と Windows の両方で待機中の CPU 時間が増えないことを確認しています (`scripts/idle-check.sh` / `scripts/idle-check.ps1`)。

### 1 枚を変換する手順

元ファイルを削除する前提なので、次の順に確認しながら進めます。

1. 隠しファイル、写真ライブラリ (`*.photoslibrary` など) の中身、クラウド上にしかないファイル (iCloud の未ダウンロード / OneDrive のオンデマンド)、RAW、複数フレームの画像は対象外にする。
2. 書き込みが終わったファイルだけを扱う。サイズと更新日時が `settle_seconds` の間変わらないこと、Windows では書き込み中のプロセスがないことを確認する。
3. メモリ上で AVIF にエンコードする。回転は画素に焼き込み、EXIF の Orientation は 1 にする。EXIF、Display P3 の色域、透過も保つ。
4. 検証する。AVIF の構造と AV1 ヘッダ、画像サイズを確認し、OS に AVIF デコーダがあれば (macOS 13 以降、Windows では AV1 拡張機能があれば) 実際にデコードする。
5. 隠し一時ファイルに書き出して `fsync` し、`rename` で配置する。更新日時・作成日時・Finder タグを元ファイルから引き継ぐ。
6. 元ファイルがエンコード中に変わっていないことを確認してから、設定に従って削除 / ゴミ箱へ移動 / 残すのいずれかを行う。

AVIF の方が大きくなる場合や、デコードできない場合は、元ファイルを残してスキップ一覧に記録します。スキップしたファイルは、内容が変わるまで再挑戦しません。

### エンコーダ

| | rav1e (既定) | native (macOS ImageIO) |
|---|---|---|
| 4K 写真 (2.29 MB の JPEG) | 0.70 MB / SSIM 0.970 / 約 7 秒 | 1.14 MB / SSIM 0.970 / 1 秒未満 |
| 形式 | 10bit 4:4:4、BT.601 フルレンジ、CICP で sRGB / Display P3 | ImageIO の設定に従う (大きい画像はグリッド形式) |

同じ画質なら、rav1e の方がファイルは約 40% 小さくなります。容量を減らすことが目的なので既定は rav1e にしています。変換は低優先度の子プロセスで行うので、時間がかかっても作業の邪魔にはなりません。急ぐ場合は `encoder = "native"` にしてください。

## 既知の制約

- Windows で HEIC を読むには、Microsoft Store の「HEIF 画像拡張機能」と「HEVC ビデオ拡張機能」が必要です。入っていなければ、そのファイルは元のまま残してスキップします。
- rav1e の経路では、HDR のゲインマップと 16bit の階調は 8bit 入力として扱います。XMP は引き継がず、EXIF のみ保持します。
- 「新規のみ」モードでは、ほかの場所からフォルダごと**移動**してきた写真は対象になりません (コピーなら対象)。必要なら `convert <フォルダ>` を実行してください。
- 署名なし (ad-hoc 署名) のビルドでは、更新のたびに macOS がフォルダへのアクセス許可を求め直すことがあります。`status` で監視フォルダにアクセスできるか確認できます。
- 1 枚の rav1e エンコードは、12MP の写真で最大 0.7 GB ほどメモリを使います (変換中だけ)。

## ソースからビルド

```sh
cargo build --release
# x86_64 で rav1e の SIMD を使うには nasm が必要です。nasm がない場合:
cargo build --release --no-default-features
cargo test
scripts/idle-check.sh target/release/optimize-image-folder 60
```

リリースは `v*` タグを push すると GitHub Actions が作ります (macOS Universal、Windows x64 / ARM64)。

## ライセンス

MIT
