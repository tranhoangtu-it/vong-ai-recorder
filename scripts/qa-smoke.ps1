#Requires -Version 5.1
<#
.SYNOPSIS
    Post-install smoke verification for Vong AI Recorder v0.1.0-beta.1.

.DESCRIPTION
    Verifies the expected on-disk layout, running process, AppData structure,
    and log file health after a successful MSI install + first launch.
    Does NOT install the MSI — run this after manual install and first launch.

.USAGE
    powershell.exe -ExecutionPolicy Bypass -File qa-smoke.ps1

.EXIT CODES
    0  All checks passed
    1  One or more checks failed
#>

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

# ---------------------------------------------------------------------------
# Colour helpers
# ---------------------------------------------------------------------------
function Write-Pass  { param([string]$msg) Write-Host "  [PASS] $msg" -ForegroundColor Green  }
function Write-Fail  { param([string]$msg) Write-Host "  [FAIL] $msg" -ForegroundColor Red    }
function Write-Info  { param([string]$msg) Write-Host "  [INFO] $msg" -ForegroundColor Cyan   }
function Write-Head  { param([string]$msg) Write-Host "`n== $msg ==" -ForegroundColor Yellow  }

$script:failures = 0

function Assert-True {
    param(
        [bool]   $Condition,
        [string] $PassMsg,
        [string] $FailMsg
    )
    if ($Condition) {
        Write-Pass $PassMsg
    } else {
        Write-Fail $FailMsg
        $script:failures++
    }
}

# ---------------------------------------------------------------------------
# Paths — adjust if install path differs
# ---------------------------------------------------------------------------
$InstallDir  = Join-Path $env:LOCALAPPDATA 'Programs\Vong AI Recorder'
$ExePath     = Join-Path $InstallDir 'vong.exe'
$AppDataRoot = Join-Path $env:APPDATA 'Vong\Vong'
$ConfigDir   = Join-Path $AppDataRoot 'config'
$DataDir     = Join-Path $AppDataRoot 'data'
$LogsDir     = Join-Path $DataDir 'logs'
$DbPath      = Join-Path $DataDir 'db.sqlite3'
$ModelsDir   = Join-Path $env:LOCALAPPDATA 'Vong\Vong AI Recorder\data\models'

Write-Host "`nVong AI Recorder — Post-Install Smoke Check" -ForegroundColor Magenta
Write-Host "============================================`n"

# ---------------------------------------------------------------------------
# CHECK 1: Executable present at expected install path
# ---------------------------------------------------------------------------
Write-Head 'CHECK 1 — Executable'

Assert-True `
    (Test-Path $ExePath) `
    "vong.exe found at: $ExePath" `
    "vong.exe NOT found at: $ExePath  (is the MSI installed?)"

if (Test-Path $ExePath) {
    $size = (Get-Item $ExePath).Length
    $sizeMb = [math]::Round($size / 1MB, 1)
    Write-Info "Binary size: $sizeMb MB"

    # CPU build ~8-14 MB, Vulkan build ~55-65 MB
    Assert-True `
        ($size -gt 5MB) `
        "Binary size is above 5 MB (sanity check)" `
        "Binary size $sizeMb MB is suspiciously small — possible corrupt install"
}

# ---------------------------------------------------------------------------
# CHECK 2: AppData layout — config, data, logs directories exist
# ---------------------------------------------------------------------------
Write-Head 'CHECK 2 — AppData Layout'

foreach ($dir in @($AppDataRoot, $ConfigDir, $DataDir, $LogsDir)) {
    $label = $dir.Replace($env:APPDATA, '%APPDATA%')
    Assert-True `
        (Test-Path $dir) `
        "Directory exists: $label" `
        "Directory MISSING: $label  (app may not have been launched yet)"
}

# ---------------------------------------------------------------------------
# CHECK 3: SQLite database exists and is non-empty
# ---------------------------------------------------------------------------
Write-Head 'CHECK 3 — SQLite Database'

Assert-True `
    (Test-Path $DbPath) `
    "db.sqlite3 exists at: $($DbPath.Replace($env:APPDATA, '%APPDATA%'))" `
    "db.sqlite3 NOT found — database never initialised (was the app launched?)"

if (Test-Path $DbPath) {
    $dbSize = (Get-Item $DbPath).Length
    Assert-True `
        ($dbSize -gt 0) `
        "db.sqlite3 is non-empty ($dbSize bytes)" `
        "db.sqlite3 is zero bytes — storage init may have failed"
    Write-Info "DB size: $([math]::Round($dbSize / 1KB, 1)) KB"
}

# ---------------------------------------------------------------------------
# CHECK 4: Log file exists and tail shows no startup panic
# ---------------------------------------------------------------------------
Write-Head 'CHECK 4 — Log File Health'

$logFiles = @()
if (Test-Path $LogsDir) {
    $logFiles = Get-ChildItem -Path $LogsDir -Filter 'vong.log.*' -File |
                Sort-Object LastWriteTime -Descending
}

Assert-True `
    ($logFiles.Count -gt 0) `
    "At least one log file found in: $($LogsDir.Replace($env:APPDATA, '%APPDATA%'))" `
    "No log files found — logging may not have initialised"

if ($logFiles.Count -gt 0) {
    $latest = $logFiles[0]
    Write-Info "Latest log: $($latest.Name) ($([math]::Round($latest.Length / 1KB, 1)) KB)"

    # Tail last 50 lines — look for panic or ERROR at startup
    $lines = Get-Content -Path $latest.FullName -Tail 50 -ErrorAction SilentlyContinue
    if ($null -ne $lines) {
        $panicLines = $lines | Where-Object { $_ -match '\bpanic\b|\bPANIC\b|thread.*panicked' }
        Assert-True `
            ($panicLines.Count -eq 0) `
            "No panic lines in last 50 log lines" `
            "PANIC detected in log! ($($panicLines.Count) line(s))"

        if ($panicLines.Count -gt 0) {
            foreach ($pl in $panicLines | Select-Object -First 3) {
                Write-Fail "  > $pl"
            }
        }

        $errorLines = $lines | Where-Object { $_ -match ' ERROR ' }
        Write-Info "ERROR lines in last 50: $($errorLines.Count)  (some are expected during startup probe)"
    } else {
        Write-Info "Log file is empty or unreadable — skipping content check"
    }
}

# ---------------------------------------------------------------------------
# CHECK 5: Process running
# ---------------------------------------------------------------------------
Write-Head 'CHECK 5 — Process Running'

$proc = Get-Process -Name 'vong' -ErrorAction SilentlyContinue
Assert-True `
    ($null -ne $proc) `
    "vong process is running (PID $($proc.Id))" `
    "vong process NOT running — launch the app first, then re-run this script"

if ($null -ne $proc) {
    $memMb = [math]::Round($proc.WorkingSet64 / 1MB, 1)
    Write-Info "Working set: $memMb MB | CPU time: $($proc.CPU)s"
}

# ---------------------------------------------------------------------------
# CHECK 6: Config files written by wizard / first launch
# ---------------------------------------------------------------------------
Write-Head 'CHECK 6 — Config Files'

$sourceFile   = Join-Path $ConfigDir 'source.txt'
$providerFile = Join-Path $ConfigDir 'provider.txt'
$onboardFile  = Join-Path $ConfigDir 'onboarded.txt'

foreach ($pair in @(
    @{ Path = $sourceFile;   Label = 'source.txt   (audio source)' },
    @{ Path = $providerFile; Label = 'provider.txt (STT provider)' },
    @{ Path = $onboardFile;  Label = 'onboarded.txt (wizard state)' }
)) {
    $exists = Test-Path $pair.Path
    Assert-True `
        $exists `
        "$($pair.Label) exists" `
        "$($pair.Label) MISSING — wizard may not have completed"

    if ($exists) {
        $content = (Get-Content -Path $pair.Path -Raw -ErrorAction SilentlyContinue) -replace "`r`n","`n"
        Write-Info "$($pair.Label): $(($content -split "`n")[0].Trim())"
    }
}

# Verify onboarded.txt is v2 format with step.complete
if (Test-Path $onboardFile) {
    $ob = Get-Content -Path $onboardFile -Raw -ErrorAction SilentlyContinue
    Assert-True `
        ($ob -match 'v=2') `
        "onboarded.txt has v=2 header (wizard v2 format)" `
        "onboarded.txt missing v=2 header — may be legacy alpha format or corrupt"

    Assert-True `
        ($ob -match 'step\.complete=') `
        "onboarded.txt has step.complete= line (wizard fully done)" `
        "onboarded.txt missing step.complete= — wizard was not completed"
}

# ---------------------------------------------------------------------------
# CHECK 7: Windows Credential Manager — keychain entries (count only, no values)
# ---------------------------------------------------------------------------
Write-Head 'CHECK 7 — Credential Manager (keychain)'

Write-Info "Listing Windows Credential Manager entries for service 'com.vong.stt' (values hidden)"

try {
    # cmdkey /list returns all stored credentials; filter by target containing vong
    $cmdkeyOutput = & cmdkey.exe /list 2>&1 | Out-String
    $vongEntries  = ($cmdkeyOutput -split "`n") | Where-Object { $_ -match 'vong' }

    if ($vongEntries.Count -gt 0) {
        Write-Pass "Found $($vongEntries.Count) vong-related Credential Manager entry(ies)"
        foreach ($entry in $vongEntries) {
            Write-Info "  Entry: $($entry.Trim())"
        }
    } else {
        Write-Info "No vong Credential Manager entries found (expected if no API key saved yet)"
    }
    # Not a failure — keychain entries only exist if user entered Soniox/OpenAI key
} catch {
    Write-Info "Could not enumerate Credential Manager: $_"
}

# ---------------------------------------------------------------------------
# CHECK 8: Models directory (optional — only after model download)
# ---------------------------------------------------------------------------
Write-Head 'CHECK 8 — Whisper Model File'

if (Test-Path $ModelsDir) {
    $modelFiles = Get-ChildItem -Path $ModelsDir -Filter 'ggml-*.bin' -File -ErrorAction SilentlyContinue
    if ($modelFiles.Count -gt 0) {
        foreach ($m in $modelFiles) {
            $mMb = [math]::Round($m.Length / 1MB, 1)
            Write-Pass "Model found: $($m.Name) ($mMb MB)"
        }
    } else {
        Write-Info "No ggml-*.bin files found in models dir — model not yet downloaded (expected before wizard step 4)"
    }
} else {
    Write-Info "Models directory does not exist yet: $ModelsDir  (created after model download)"
}

# ---------------------------------------------------------------------------
# Summary
# ---------------------------------------------------------------------------
Write-Host "`n============================================" -ForegroundColor Magenta

if ($script:failures -eq 0) {
    Write-Host "RESULT: ALL CHECKS PASSED" -ForegroundColor Green
    Write-Host "============================================`n" -ForegroundColor Magenta
    exit 0
} else {
    Write-Host "RESULT: $($script:failures) CHECK(S) FAILED" -ForegroundColor Red
    Write-Host "Review FAIL lines above and address before announcing beta." -ForegroundColor Red
    Write-Host "============================================`n" -ForegroundColor Magenta
    exit 1
}
