# optimize-image-folder installer for Windows.
#   irm https://tsubasatommy.github.io/optimize-image-folder/install.ps1 | iex
# Downloads the latest OptimizeImageFolder-Setup.exe, checks its SHA256 against the release's
# SHA256SUMS.txt and runs it. Set $env:OIF_SILENT = "1" for a silent install with the defaults.
$ErrorActionPreference = "Stop"
$ProgressPreference = "SilentlyContinue"
[Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12

$repo = "TsubasaTommy/optimize-image-folder"
$asset = "OptimizeImageFolder-Setup.exe"
$base = "https://github.com/$repo/releases/latest/download"
$tmp = Join-Path ([IO.Path]::GetTempPath()) ("oif-" + [guid]::NewGuid())
New-Item -ItemType Directory -Path $tmp | Out-Null

try {
    Write-Host "ダウンロード中: $asset"
    $exe = Join-Path $tmp $asset
    Invoke-WebRequest -UseBasicParsing -Uri "$base/$asset" -OutFile $exe
    Invoke-WebRequest -UseBasicParsing -Uri "$base/SHA256SUMS.txt" -OutFile (Join-Path $tmp "SHA256SUMS.txt")

    $expected = Get-Content (Join-Path $tmp "SHA256SUMS.txt") |
        ForEach-Object { $parts = $_ -split '\s+'; if ($parts[1].TrimStart('*') -eq $asset) { $parts[0] } } |
        Select-Object -First 1
    $actual = (Get-FileHash $exe -Algorithm SHA256).Hash.ToLower()
    if (-not $expected -or $expected -ne $actual) { throw "チェックサムが一致しません (期待値: $expected, 実際: $actual)" }

    $start = @{ FilePath = $exe; PassThru = $true }
    if ($env:OIF_SILENT) { $start.ArgumentList = @("/VERYSILENT", "/SUPPRESSMSGBOXES", "/NORESTART", "/SP-") }
    Write-Host "インストーラーを起動します"
    # Wait for the installer only (Start-Process -Wait would also wait for the agent it starts).
    $p = Start-Process @start
    $null = $p.Handle
    $p.WaitForExit()
    if ($p.ExitCode -ne 0) { throw "インストーラーが終了コード $($p.ExitCode) で終了しました" }
    Write-Host "インストールしました"
}
finally {
    Remove-Item -Recurse -Force $tmp -ErrorAction SilentlyContinue
}
