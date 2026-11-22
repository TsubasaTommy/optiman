# optimize-image-folder

写真フォルダを監視して、追加された写真を **画質の劣化なしで JPEG XL に変換する** macOS / Windows 用のツールです。
Rust から OS のネイティブ API を直接呼び出しており、**何もしていない間はリソースを使いません** (macOS ではプロセス自体が存在せず、Windows では 1 スレッドがカーネルの待機状態で眠るだけです)。

- ダウンロードとインストール手順: <https://tsubasatommy.github.io/optimize-image-folder/>
- リリース: <https://github.com/TsubasaTommy/optimize-image-folder/releases>

## 変換の仕様

| 元の形式 | 変換方法 | 元ファイルを消す前の検証 |
|---|---|---|
| JPEG | libjxl による JPEG のロスレス再圧縮。EXIF・XMP・ICC などのマーカーもすべて保持 | JPEG XL から JPEG を復元し、**元ファイルとバイト単位で一致**すること。さらに jxl-rs で全体をデコードできること |
| PNG / TIFF / BMP / ロスレス WebP | OS でデコードした値をそのまま、完全ロスレス (modular lossless) でエンコード。ICC (sRGB / Display P3 は CICP)、8 / 16 bit、透過、グレースケール、向き (Orientation)、EXIF / XMP を保持 | jxl-rs でデコードした画素が、**元の画素と完全に一致**すること |
| 変換しない (元のまま残す) | HEIC / HEIF、非可逆 WebP、RAW、CMYK、浮動小数点 (HDR)、ゲインマップ付き JPEG、HDR の PNG、アニメーション | — |

- 「圧縮の強さ」(1〜9、既定 7) を変えて変わるのは、ファイルサイズと変換時間だけです。画質は常に元と同じです。
- 変換後に小さくならないファイルは、元のまま残します。
- 4K の写真での実測値 (Apple Silicon、低優先度で実行):

  | 元のファイル | JPEG XL | 時間 |
  |---|---|---|
  | JPEG 2.29 MB | 1.85 MB (-19%) | 1.9 秒 |
  | PNG 10.3 MB | 6.87 MB (-33%) / 強さ 3 | 6.6 秒 |
  | PNG 10.3 MB | 5.76 MB (-44%) / 強さ 7 | 28 秒 |

> [!WARNING]
> 既定では、検証に成功した**元ファイルを完全に削除**します。インストール時に「ゴミ箱へ移動」か「残す」を選べます。
> 削除したあとでも `optimize-image-folder restore <ファイルまたはフォルダ>` で元に戻せます (JPEG はバイト単位で元どおり、それ以外は同じ画素の PNG になります)。

## JPEG XL を表示できる環境

- **macOS 14 以降**: Finder、プレビュー、写真で標準で表示できます。
- **Windows 11 24H2 以降**: Microsoft 公式の無料拡張「[JPEG XL Image Extension](https://apps.microsoft.com/detail/9MZPRTH5C0TB)」を入れると、エクスプローラーやフォトで表示できます。**インストーラーがこの拡張も一緒にインストールします** (チェックを外せば入れません)。あとから入れるには、設定画面の「拡張機能をインストール」を押すか、次を実行します。

  ```powershell
  winget install --id 9MZPRTH5C0TB --source msstore
  ```

- それ以前の OS でも変換は行われます。表示できない場合は `restore` で元に戻せます。

## インストール

### macOS (Homebrew)

```sh
brew install tsubasatommy/tap/optimize-image-folder
optimize-image-folder install
```

`install` を実行すると、監視するフォルダ (既定は `~/Pictures`。写真ライブラリの中身は対象外)、元ファイルの扱い、既存の写真も変換するかを聞かれます。
設定画面は `optimize-image-folder settings` か、`~/Applications/Optimize Image Folder.app` で開けます。

Homebrew を使わない場合は `curl -fsSL https://tsubasatommy.github.io/optimize-image-folder/install.sh | sh` でも入れられます (SHA256 を確認してから `~/.local/bin` に配置します)。

### Windows (winget)

```powershell
winget install TsubasaTommy.OptimizeImageFolder
```

winget への登録は Microsoft の審査中です ([winget-pkgs #486181](https://github.com/microsoft/winget-pkgs/pull/486181))。見つからない場合は、下の Setup.exe か PowerShell のコマンドで入れてください。

[リリース](https://github.com/TsubasaTommy/optimize-image-folder/releases/latest)の `OptimizeImageFolder-Setup.exe` を直接実行しても同じです。x64 と ARM64 の両方を含むユーザー単位のインストーラーで、管理者権限は不要です。インストール中に、元ファイルの扱いと既存の写真も変換するかを選べます。
設定画面は、スタートメニューの「Optimize Image Folder」から開けます。コマンド (`status`、`restore` など) は PATH に追加されます。

PowerShell から入れる場合は `irm https://tsubasatommy.github.io/optimize-image-folder/install.ps1 | iex` を実行します (最新の Setup.exe を SHA256 を確認してから起動します)。

> [!NOTE]
> インストーラーにはコード署名をしていません (署名証明書は有料のため)。winget で入れると SmartScreen の警告は出ませんが、ブラウザでダウンロードした Setup.exe を実行すると、警告が出ることがあります。その場合は、[SHA256SUMS.txt](https://github.com/TsubasaTommy/optimize-image-folder/releases/latest/download/SHA256SUMS.txt) でハッシュを確認してから「詳細情報」→「実行」を選んでください。

## 使い方

| コマンド | 内容 |
|---|---|
| `install` | 設定を作り、自動起動に登録する。`--folder` `--exclude` `--new-only` / `--scan-existing` `--original delete\|trash\|keep` `--effort 1-9` `--yes` |
| `settings` | 設定画面を開く (圧縮の強さ、変換するフォルダ、除外するフォルダ) |
| `status` | 状態、設定、監視フォルダにアクセスできるか、最近のログを表示する |
| `convert <パス>...` | ファイルやフォルダを今すぐ変換する (`--dry-run` で確認のみ、`--original` `--effort` `-j`) |
| `restore <パス>...` | JPEG XL から元のファイルを復元する。`--delete-jxl` で復元に成功した .jxl を削除 |
| `start` / `stop` / `restart` | 監視の開始・停止・再起動 (設定ファイルを手で編集したとき) |
| `config` | 設定ファイルの場所と内容を表示する |
| `uninstall [--purge]` | 自動起動を解除する。`--purge` で設定・状態・ログも削除 |

### 設定ファイル

設定画面で変更できますが、手で編集することもできます (編集後は `restart`)。

- macOS: `~/Library/Application Support/optimize-image-folder/config.toml`
- Windows: `%APPDATA%\optimize-image-folder\config.toml`

```toml
folders = ["~/Pictures"]   # 変換するフォルダ (サブフォルダも対象)
exclude_folders = []       # 変換しないフォルダ
effort = 7                 # 圧縮の強さ 1 (速い) - 9 (小さい)。画質は常に劣化なし
original = "delete"        # delete | trash | keep
scan_existing = false      # 起動時に既存の写真も変換するか
jobs = 0                   # 同時変換数 (0 = 自動)
settle_seconds = 2         # 書き込みが止まってから変換するまでの秒数
skip_if_larger = true      # JPEG XL の方が大きくなる場合は元ファイルを残す
extensions = ["jpg", "jpeg", "png", "bmp", "tif", "tiff", "webp"]
exclude = []               # 除外するフォルダ/ファイル名のパターン (* と ?)
```

ログは macOS が `~/Library/Logs/optimize-image-folder/`、Windows が `%LOCALAPPDATA%\optimize-image-folder\logs\` にあります。

## 仕組み

### 待機中にリソースを使わない理由

| | macOS | Windows |
|---|---|---|
| 起動 | launchd の `LaunchEvents` (`com.apple.fsevents.matching`)。監視フォルダの中で何かが変わったときだけ launchd がプロセスを起動し、処理が終わって 5 秒たつとプロセスは終了する。**待機中はプロセスが存在しない** | ログイン時に起動 (HKCU の Run)。メインスレッドが 1 つの I/O 完了ポートで待つ |
| 変更の検出 | FSEvents。前回の event id から履歴を再生するので、止まっていた間の変更も取りこぼさない。除外フォルダと写真ライブラリは `FSEventStreamSetExclusionPaths` で通知自体を止める | `ReadDirectoryChangesW` (サブツリー、overlapped)。停止要求 (名前付きパイプ) と変換結果の通知も、同じ完了ポートで受ける。**待機中のスレッドは 1 本**で、`GetQueuedCompletionStatus(INFINITE)` で眠っている |
| 変換 | 必要なときだけ子プロセス (`worker`) を起動。`PRIO_DARWIN_BG` と Background QoS で効率コア・I/O 抑制 | 必要なときだけ子プロセスを起動。`PROCESS_MODE_BACKGROUND_BEGIN` と EcoQoS |
| 待機中のメモリ | 0 (プロセスがない) | 常駐プロセスは EcoQoS と最低のメモリ優先度で動き、待機に入るとワーキングセットを OS に返す |
| 設定画面 | 開いている間だけ存在する別プロセス (AppKit)。閉じれば終了 | 開いている間だけ存在する別プロセス (Win32)。閉じれば終了 |

CI では、macOS と Windows の両方で待機中に CPU 時間が増えないことを確認しています (`scripts/idle-check.sh` / `scripts/idle-check.ps1`)。

### 1 枚を変換する手順

1. 隠しファイル、写真ライブラリ (`*.photoslibrary` など) の中身、クラウド上にしかないファイル (iCloud の未ダウンロード / OneDrive のオンデマンド)、除外フォルダは対象外にする。
2. 書き込みが終わったファイルだけを扱う。サイズと更新日時が `settle_seconds` の間変わらないこと、Windows では書き込み中のプロセスがないことを確認する。
3. 失われる情報があるファイル (上の「変換しない」) は、元のまま残してスキップ一覧に記録する。
4. メモリ上で JPEG XL にエンコードし、上の表の方法で検証する。
5. 隠し一時ファイルに書き出して `fsync` し、`rename` で配置する。更新日時・作成日時・Finder タグを元ファイルから引き継ぐ。
6. 元ファイルがエンコード中に変わっていないことを確認してから、設定に従って削除 / ゴミ箱へ移動 / 残すのいずれかを行う。

スキップしたファイルは、内容が変わるまで再挑戦しません。

### 使っているもの

- エンコード: [libjxl](https://github.com/libjxl/libjxl) 0.13 (JPEG XL の公式リファレンス実装、BSD-3-Clause)。git submodule から静的にリンクしています。
- 検証用デコード: [jxl-rs](https://github.com/libjxl/jxl-rs) (BSD-3-Clause)
- 画像の読み込み: ImageIO (macOS) / WIC (Windows)。色変換も回転もせず、保存されている値をそのまま取り出します。
- 同梱ソフトウェアのライセンスは `THIRD_PARTY_LICENSES.txt` にまとめています。

## 既知の制約

- 「新規のみ」モードでは、ほかの場所からフォルダごと**移動**してきた写真は対象になりません (コピーなら対象)。必要なら `convert <フォルダ>` を実行してください。
- 監視中のフォルダで `restore` すると、復元したファイルはスキップ一覧に記録されるので、再び変換されることはありません。
- 署名なし (ad-hoc 署名) のビルドでは、更新のたびに macOS がフォルダへのアクセス許可を求め直すことがあります。`status` で監視フォルダにアクセスできるか確認できます。

## ソースからビルド

Rust 1.100 (`rust-toolchain.toml` で固定) と CMake が必要です。

```sh
scripts/fetch-libjxl.sh     # libjxl と必要なサブモジュールだけを浅く取得
cargo build --release
cargo test
scripts/idle-check.sh target/release/optimize-image-folder 60
```

リリースは `v*` タグを push すると GitHub Actions が作ります (macOS Universal の tar.gz、Windows の Setup.exe と x64 / ARM64 の zip)。
Homebrew の Formula と winget のマニフェストは `scripts/render_packaging.py` で生成します。

## ライセンス

MIT。同梱しているライブラリのライセンスは `THIRD_PARTY_LICENSES.txt` を参照してください。
