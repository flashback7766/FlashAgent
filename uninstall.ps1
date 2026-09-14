# FlashAgent uninstaller for Windows.
#
#   irm https://raw.githubusercontent.com/flashback7766/FlashAgent/main/uninstall.ps1 | iex
#
# When a working flashagent.exe is on this machine it does the job itself
# (flashagent --uninstall) and asks about your data part by part. This script
# only removes things by hand when that binary is missing or broken.
$ErrorActionPreference = "Stop"

$InstallDir = Join-Path $env:LOCALAPPDATA "Programs\FlashAgent"
$UserLocalBin = Join-Path $env:USERPROFILE ".local\bin"

$candidates = @()
$onPath = Get-Command flashagent -ErrorAction SilentlyContinue
if ($onPath) { $candidates += $onPath.Source }
$candidates += (Join-Path $InstallDir "flashagent.exe")
$candidates += (Join-Path $UserLocalBin "flashagent.exe")

foreach ($exe in $candidates) {
    if ($exe -and (Test-Path $exe)) {
        & $exe --version *> $null
        if ($LASTEXITCODE -eq 0) {
            & $exe --uninstall
            exit $LASTEXITCODE
        }
    }
}

Write-Host "No working flashagent.exe found; removing what the installer put down."
Write-Host ""

$removed = @()
foreach ($dir in @($InstallDir, $UserLocalBin)) {
    foreach ($name in @("flashagent.exe", "flashagent-tui.exe", "flashagent.exe.old")) {
        $target = Join-Path $dir $name
        if (Test-Path $target) {
            try {
                Remove-Item -Path $target -Force
                $removed += "  removed  $target"
            } catch {
                Write-Host "  could not remove $target : $_" -ForegroundColor Yellow
            }
        }
    }
}
if ((Test-Path $InstallDir) -and -not (Get-ChildItem -Path $InstallDir -Force)) {
    Remove-Item -Path $InstallDir -Force
    $removed += "  removed  $InstallDir"
}

# The installer added its folder to the user PATH; take it back out once the
# folder is gone.
if (-not (Test-Path $InstallDir)) {
    $userPath = [Environment]::GetEnvironmentVariable("Path", "User")
    if ($userPath) {
        $normalized = $InstallDir.TrimEnd('\').ToLowerInvariant()
        $entries = $userPath -split ';' | Where-Object { $_.Trim() -ne "" }
        $kept = $entries | Where-Object { $_.Trim().TrimEnd('\').ToLowerInvariant() -ne $normalized }
        if (@($kept).Count -ne @($entries).Count) {
            [Environment]::SetEnvironmentVariable("Path", ($kept -join ';'), "User")
            $removed += "  removed  $InstallDir from the user PATH"
        }
    }
}

$data = Join-Path $env:USERPROFILE ".flashagent"
if (Test-Path $data) {
    Write-Host "Your data is in ${data}: settings, saved sessions, memory, /rewind copies."
    $answer = Read-Host "Delete it too? [y/N]"
    if ($answer -match '^(y|yes)$') {
        Remove-Item -Path $data -Recurse -Force
        $removed += "  removed  $data"
    } else {
        Write-Host "  kept     $data"
    }
}

Write-Host ""
Write-Host "FlashAgent is uninstalled." -ForegroundColor Green
$removed | ForEach-Object { Write-Host $_ }
