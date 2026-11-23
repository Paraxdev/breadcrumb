# Installs or updates breadcrumb on Windows and sets it up for Claude Code.
#   irm https://raw.githubusercontent.com/Paraxdev/breadcrumb/main/install.ps1 | iex
# Run it again any time to update. $env:BREADCRUMB_REPO picks a fork (owner/name).
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
[Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12

$repo = if ($env:BREADCRUMB_REPO) { $env:BREADCRUMB_REPO } else { 'Paraxdev/breadcrumb' }
$asset = 'breadcrumb-windows-x64.zip'

function Find-Dir {
    $have = Get-Command breadcrumb -ErrorAction SilentlyContinue
    if ($have) { return Split-Path -Parent $have.Source }
    $cargoBin = Join-Path $HOME '.cargo\bin'
    if (Test-Path $cargoBin) { return $cargoBin }
    return Join-Path $env:LOCALAPPDATA 'Programs\breadcrumb\bin'
}

# A running MCP server keeps breadcrumb.exe open, Windows lets it be renamed but not replaced.
function Move-Aside([string]$dir) {
    $stamp = [DateTimeOffset]::UtcNow.ToUnixTimeSeconds()
    foreach ($b in 'breadcrumb', 'crumb') {
        $p = Join-Path $dir "$b.exe"
        if (Test-Path $p) { Rename-Item $p "breadcrumb.old-$b-$stamp.exe" }
    }
}

$dir = Find-Dir
New-Item -ItemType Directory -Force $dir | Out-Null
$installed = $false

try {
    $rel = Invoke-RestMethod "https://api.github.com/repos/$repo/releases/latest" -Headers @{ 'User-Agent' = 'breadcrumb-install' }
    $url = ($rel.assets | Where-Object { $_.name -eq $asset } | Select-Object -First 1).browser_download_url
    if (-not $url) { throw "release $($rel.tag_name) has no $asset" }
    $tmp = Join-Path ([IO.Path]::GetTempPath()) ("breadcrumb-" + [guid]::NewGuid())
    New-Item -ItemType Directory $tmp | Out-Null
    Invoke-WebRequest $url -OutFile (Join-Path $tmp $asset) -UseBasicParsing
    Expand-Archive (Join-Path $tmp $asset) -DestinationPath $tmp -Force
    Move-Aside $dir
    Copy-Item (Join-Path $tmp 'breadcrumb.exe'), (Join-Path $tmp 'crumb.exe') $dir -Force
    Remove-Item $tmp -Recurse -Force
    Write-Host "installed breadcrumb $($rel.tag_name) into $dir"
    $installed = $true
} catch {
    Write-Host "no release download ($($_.Exception.Message)), building from source"
}

if (-not $installed) {
    if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
        throw 'no release to download and no cargo to build with, install Rust from https://rustup.rs and run this again'
    }
    Move-Aside $dir
    $root = Split-Path -Parent $dir
    cargo install --git "https://github.com/$repo" breadcrumb --force --root $root
    if ($LASTEXITCODE -ne 0) { throw 'cargo install failed' }
}

$userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
if (($userPath -split ';') -notcontains $dir -and ($env:Path -split ';') -notcontains $dir) {
    [Environment]::SetEnvironmentVariable('Path', "$userPath;$dir", 'User')
    Write-Host "added $dir to your PATH, new terminals see it"
}
$env:Path = "$dir;$env:Path"
$exe = Join-Path $dir 'breadcrumb.exe'

& $exe init | Out-Host
if (Get-Command claude -ErrorAction SilentlyContinue) {
    # Note: Windows PowerShell 5.1 turns redirected native stderr into a terminating error under Stop.
    $ErrorActionPreference = 'Continue'
    claude mcp get breadcrumb *> $null
    if ($LASTEXITCODE -ne 0) {
        claude mcp add --scope user breadcrumb -- $exe mcp | Out-Host
    }
    $ErrorActionPreference = 'Stop'
} else {
    Write-Host "Claude Code not found, register later with: $(& $exe mcp-config)"
}
& $exe hooks install | Out-Host
if (Test-Path '.breadcrumb\project.toml') { & $exe share pull | Out-Host }
Write-Host "$(& $exe --version) ready. Restart Claude Code so its MCP server and hooks use the new version."
