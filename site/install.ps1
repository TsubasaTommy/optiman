# optimize-image-folder installer for Windows.
#   irm https://tsubasatommy.github.io/optimize-image-folder/install.ps1 | iex
$ErrorActionPreference = "Stop"
$ProgressPreference = "SilentlyContinue"
[Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12

$repo = "TsubasaTommy/optimize-image-folder"
$isArm = ($env:PROCESSOR_ARCHITECTURE -eq "ARM64") -or ($env:PROCESSOR_ARCHITEW6432 -eq "ARM64")
$arch = if ($isArm) { "arm64" } else { "x64" }
$asset = "optimize-image-folder-windows-$arch.zip"
$base = "https://github.com/$repo/releases/latest/download"
$dest = if ($env:OIF_INSTALL_DIR) { $env:OIF_INSTALL_DIR } else { Join-Path $env:LOCALAPPDATA "Programs\optimize-image-folder" }
$tmp = Join-Path ([IO.Path]::GetTempPath()) ("oif-" + [guid]::NewGuid())
New-Item -ItemType Directory -Path $tmp | Out-Null

try {
    Write-Host "ダウンロード中: $asset"
    Invoke-WebRequest -UseBasicParsing -Uri "$base/$asset" -OutFile (Join-Path $tmp $asset)
    Invoke-WebRequest -UseBasicParsing -Uri "$base/SHA256SUMS.txt" -OutFile (Join-Path $tmp "SHA256SUMS.txt")

    $expected = Get-Content (Join-Path $tmp "SHA256SUMS.txt") |
        ForEach-Object { $parts = $_ -split '\s+'; if ($parts[1].TrimStart('*') -eq $asset) { $parts[0] } } |
        Select-Object -First 1
    $actual = (Get-FileHash (Join-Path $tmp $asset) -Algorithm SHA256).Hash.ToLower()
    if (-not $expected -or $expected -ne $actual) { throw "チェックサムが一致しません (期待値: $expected, 実際: $actual)" }

    $cli = Join-Path $dest "optimize-image-folder.exe"
    if (Test-Path $cli) {
        & $cli stop *> $null
        Start-Sleep -Seconds 1
    }
    New-Item -ItemType Directory -Force -Path $dest | Out-Null
    Expand-Archive -Path (Join-Path $tmp $asset) -DestinationPath $dest -Force
    Get-ChildItem $dest -File | Unblock-File
    Write-Host "インストールしました: $dest"

    $userPath = [Environment]::GetEnvironmentVariable("Path", "User")
    if ($null -eq $userPath) { $userPath = "" }
    if (-not (($userPath -split ';') -contains $dest)) {
        [Environment]::SetEnvironmentVariable("Path", (($userPath.TrimEnd(';') + ";$dest").TrimStart(';')), "User")
        $env:Path += ";$dest"
        Write-Host "PATH に追加しました (新しいターミナルで optimize-image-folder コマンドが使えます)"
    }

    Write-Host ""
    & $cli install @args
}
finally {
    Remove-Item -Recurse -Force $tmp -ErrorAction SilentlyContinue
}
