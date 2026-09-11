# FlashAgent Windows one-line installer
# Usage: irm https://raw.githubusercontent.com/flashback7766/FlashAgent/main/install.ps1 | iex

param(
    [string]$Version = $env:FLASHAGENT_VERSION,
    [string]$Channel = $env:FLASHAGENT_CHANNEL
)
if (-not $Version) { $Version = $env:VERSION }
if (-not $Channel) { $Channel = $env:CHANNEL }
if (-not $Channel) { $Channel = "stable" }

$ErrorActionPreference = "Stop"

Write-Host "⚡ Installing FlashAgent for Windows..." -ForegroundColor Cyan

# 1. Enable modern TLS protocols (TLS 1.2 / TLS 1.3) for older PowerShell 5.1 hosts
try {
    [Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12 -bor [Net.SecurityProtocolType]::Tls13
} catch {
    # Fallback if Tls13 is not defined on older .NET Framework versions
    [Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12
}

# 2. Verify platform architecture
$is64 = [System.Environment]::Is64BitOperatingSystem
if (-not $is64) {
    Write-Error "Error: FlashAgent requires a 64-bit Windows operating system (x86_64)."
    exit 1
}

# 3. Ensure extraction dependencies are available
$hasExpandArchive = (Get-Command Expand-Archive -ErrorAction SilentlyContinue) -ne $null
if (-not $hasExpandArchive) {
    try {
        Add-Type -AssemblyName System.IO.Compression.FileSystem
    } catch {
        Write-Error "Error: Neither Expand-Archive nor System.IO.Compression.FileSystem is available."
        exit 1
    }
}

$Repo = "flashback7766/FlashAgent"
$InstallDir = if ($env:INSTALL_DIR) { $env:INSTALL_DIR } elseif ($env:BIN_DIR) { $env:BIN_DIR } else { Join-Path $env:LOCALAPPDATA "Programs\FlashAgent" }
$UserLocalBin = Join-Path $env:USERPROFILE ".local\bin"

# 4. Fetch release metadata from GitHub
Write-Host "Querying release data from GitHub..." -ForegroundColor Gray

$DownloadUrl = $null

# Strategy 1: Specific version requested
if ($Version) {
    Write-Host "Target version requested: $Version" -ForegroundColor Gray
    try {
        $Release = Invoke-RestMethod -Uri "https://api.github.com/repos/$Repo/releases/tags/$Version" -UseBasicParsing -Headers @{ "User-Agent" = "FlashAgent-Installer" }
        if ($Release -and $Release.assets) {
            foreach ($asset in $Release.assets) {
                if ($asset.name -match "windows.*x86_64.*\.zip$" -or $asset.name -match "windows.*\.zip$") {
                    $DownloadUrl = $asset.browser_download_url
                    break
                }
            }
        }
    } catch {
        # Fallback to web scraping if API rate-limited
        try {
            $expandedHtml = (Invoke-WebRequest -Uri "https://github.com/$Repo/releases/expanded_assets/$Version" -UseBasicParsing).Content
            if ($expandedHtml -match 'href="([^"]*releases/download/[^"]*windows[^\"]*\.zip)"') {
                $DownloadUrl = "https://github.com" + $Matches[1]
            }
        } catch {}
    }
} elseif ($Channel -ne "beta" -and $Channel -ne "prerelease") {
    # Strategy 2: Official Stable Release Priority
    Write-Host "Checking for latest stable release..." -ForegroundColor Gray
    try {
        $StableRelease = Invoke-RestMethod -Uri "https://api.github.com/repos/$Repo/releases/latest" -UseBasicParsing -Headers @{ "User-Agent" = "FlashAgent-Installer" }
        if ($StableRelease -and $StableRelease.assets) {
            foreach ($asset in $StableRelease.assets) {
                if ($asset.name -match "windows.*x86_64.*\.zip$" -or $asset.name -match "windows.*\.zip$") {
                    $DownloadUrl = $asset.browser_download_url
                    Write-Host "✔ Found latest stable release: $($StableRelease.tag_name)" -ForegroundColor Green
                    break
                }
            }
        }
    } catch {
        # Fallback to web redirect for stable release
        try {
            $req = [System.Net.WebRequest]::Create("https://github.com/$Repo/releases/latest")
            $req.AllowAutoRedirect = $false
            $req.UserAgent = "FlashAgent-Installer"
            $resp = $req.GetResponse()
            $location = $resp.GetResponseHeader("Location")
            $resp.Close()
            if ($location -match "/releases/tag/([^/]+)$") {
                $stableTag = $Matches[1]
                Write-Host "✔ Found latest stable release: $stableTag" -ForegroundColor Green
                $expandedHtml = (Invoke-WebRequest -Uri "https://github.com/$Repo/releases/expanded_assets/$stableTag" -UseBasicParsing).Content
                if ($expandedHtml -match 'href="([^"]*releases/download/[^"]*windows[^\"]*\.zip)"') {
                    $DownloadUrl = "https://github.com" + $Matches[1]
                }
            }
        } catch {}
    }

    if (-not $DownloadUrl) {
        Write-Host "Notice: No official stable release published yet. Falling back to latest pre-release..." -ForegroundColor Yellow
    }
}

# Strategy 3: the rolling `beta` pre-release (edited in place per build, so
# it keeps an old creation date and is not first in the release list)
if (-not $DownloadUrl) {
    try {
        $BetaRelease = Invoke-RestMethod -Uri "https://api.github.com/repos/$Repo/releases/tags/beta" -UseBasicParsing -Headers @{ "User-Agent" = "FlashAgent-Installer" }
        foreach ($asset in $BetaRelease.assets) {
            if ($asset.name -match "windows.*x86_64.*\.zip$") {
                $DownloadUrl = $asset.browser_download_url
                break
            }
        }
    } catch {}
}

# Strategy 4: newest pre-release in the release list
if (-not $DownloadUrl) {
    try {
        $ReleaseData = Invoke-RestMethod -Uri "https://api.github.com/repos/$Repo/releases" -UseBasicParsing -Headers @{ "User-Agent" = "FlashAgent-Installer" }
        if ($ReleaseData) {
            foreach ($release in $ReleaseData) {
                if ($release.assets) {
                    foreach ($asset in $release.assets) {
                        if ($asset.name -match "windows.*x86_64.*\.zip$" -or $asset.name -match "windows.*\.zip$") {
                            $DownloadUrl = $asset.browser_download_url
                            break
                        }
                    }
                }
                if ($DownloadUrl) { break }
            }
        }
    } catch {
        # API failed / rate-limited -> Fallback to web scraping
    }

    # Strategy 4b: GitHub web scraping fallback (immune to GitHub API rate limits)
    if (-not $DownloadUrl) {
        try {
            $releasesHtml = (Invoke-WebRequest -Uri "https://github.com/$Repo/releases" -UseBasicParsing).Content
            if ($releasesHtml -match 'data-item-id="release-([^"]+)"') {
                $latestTag = $Matches[1]
                $expandedHtml = (Invoke-WebRequest -Uri "https://github.com/$Repo/releases/expanded_assets/$latestTag" -UseBasicParsing).Content
                if ($expandedHtml -match 'href="([^"]*releases/download/[^"]*windows[^\"]*\.zip)"') {
                    $DownloadUrl = "https://github.com" + $Matches[1]
                }
            }
        } catch {}
    }
}

if (-not $DownloadUrl) {
    Write-Error "Error: Could not find Windows release asset on https://github.com/$Repo/releases"
    exit 1
}

# 5. Download asset to temporary directory
$TempZip = [System.IO.Path]::Combine([System.IO.Path]::GetTempPath(), "flashagent-win-" + [System.Guid]::NewGuid().ToString() + ".zip")
$ExtractDir = [System.IO.Path]::Combine([System.IO.Path]::GetTempPath(), "flashagent-win-ext-" + [System.Guid]::NewGuid().ToString())

try {
    Write-Host "Downloading from $DownloadUrl..." -ForegroundColor Gray
    Invoke-WebRequest -Uri $DownloadUrl -OutFile $TempZip -UseBasicParsing -Headers @{ "User-Agent" = "FlashAgent-Installer" }

    if (-not (Test-Path $ExtractDir)) {
        New-Item -ItemType Directory -Path $ExtractDir -Force | Out-Null
    }

    Write-Host "Extracting archive..." -ForegroundColor Gray
    if ($hasExpandArchive) {
        Expand-Archive -Path $TempZip -DestinationPath $ExtractDir -Force
    } else {
        [System.IO.Compression.ZipFile]::ExtractToDirectory($TempZip, $ExtractDir)
    }

    # Find flashagent executable in extracted archive
    $ExeFiles = Get-ChildItem -Path $ExtractDir -Recurse -Filter "*.exe"
    $TargetExe = $null
    foreach ($exe in $ExeFiles) {
        if ($exe.Name -eq "flashagent.exe" -or $exe.Name -eq "flashagent-tui.exe") {
            $TargetExe = $exe.FullName
            break
        }
    }

    if (-not $TargetExe -and $ExeFiles.Count -gt 0) {
        $TargetExe = $ExeFiles[0].FullName
    }

    if (-not $TargetExe) {
        Write-Error "Error: Could not locate flashagent executable inside the downloaded archive."
        exit 1
    }

    # Create destination install directory
    if (-not (Test-Path $InstallDir)) {
        New-Item -ItemType Directory -Path $InstallDir -Force | Out-Null
    }

    function Safe-InstallExe($src, $dst) {
        try {
            Copy-Item -Path $src -Destination $dst -Force -ErrorAction Stop
        } catch {
            $old = "$dst.old"
            if (Test-Path $old) { Remove-Item -Path $old -Force -ErrorAction SilentlyContinue }
            Move-Item -Path $dst -Destination $old -Force -ErrorAction SilentlyContinue
            Copy-Item -Path $src -Destination $dst -Force
            Remove-Item -Path $old -Force -ErrorAction SilentlyContinue
        }
    }

    # Copy binary to destination (with running process replacement support)
    $DestExe = Join-Path $InstallDir "flashagent.exe"
    Safe-InstallExe $TargetExe $DestExe
    # Older installs also placed a flashagent-tui.exe copy; the command is just `flashagent` now.
    $OldTuiExe = Join-Path $InstallDir "flashagent-tui.exe"
    if (Test-Path $OldTuiExe) { Remove-Item -Path $OldTuiExe -Force -ErrorAction SilentlyContinue }

    # Also mirror into ~/.local/bin if directory exists or for cross-environment convenience
    try {
        if (-not (Test-Path $UserLocalBin)) {
            New-Item -ItemType Directory -Path $UserLocalBin -Force | Out-Null
        }
        Safe-InstallExe $TargetExe (Join-Path $UserLocalBin "flashagent.exe")
    } catch {
        # Optional fallback, ignore error
    }

    Write-Host "✔ Successfully installed FlashAgent to $DestExe" -ForegroundColor Green

    # 6. Automatically add to User PATH environment variable
    $UserPath = [System.Environment]::GetEnvironmentVariable("Path", "User")
    $PathArray = if ($UserPath) { ($UserPath -split ';') | Where-Object { $_.Trim() -ne "" } } else { @() }

    if ($PathArray -notcontains $InstallDir) {
        $NewUserPath = if ($UserPath -and $UserPath.Trim() -ne "") { "$UserPath;$InstallDir" } else { $InstallDir }
        [System.Environment]::SetEnvironmentVariable("Path", $NewUserPath, "User")
        Write-Host "✔ Automatically added $InstallDir to User PATH." -ForegroundColor Green
    }

    # Update current session environment PATH so flashagent can run immediately
    if (($env:Path -split ';') -notcontains $InstallDir) {
        $env:Path = "$InstallDir;$env:Path"
    }

    Write-Host ""
    Write-Host "Run 'flashagent' to launch!" -ForegroundColor Cyan
}
finally {
    # Clean up temporary files
    if (Test-Path $TempZip) { Remove-Item -Path $TempZip -Force -ErrorAction SilentlyContinue }
    if (Test-Path $ExtractDir) { Remove-Item -Path $ExtractDir -Recurse -Force -ErrorAction SilentlyContinue }
}
