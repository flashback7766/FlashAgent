# FlashAgent Windows one-line installer
# Usage: irm https://raw.githubusercontent.com/flashback7766/FlashAgent/main/install.ps1 | iex
#
# $env:FLASHAGENT_CHANNEL = "beta"   the latest beta build instead of the latest stable
# $env:FLASHAGENT_VERSION = "<tag>"  one stable release by its tag, vX.Y.Z+bN
# $env:INSTALL_DIR = "<folder>"      where to put flashagent.exe

param(
    [string]$Version = $env:FLASHAGENT_VERSION,
    [string]$Channel = $env:FLASHAGENT_CHANNEL
)

# Everything happens inside this function: under `irm | iex` a failure then
# returns to the prompt instead of closing the window, and the preferences set
# here stay out of your session. Failures are thrown and printed once, below.
function Install-FlashAgent {
    param([string]$Version, [string]$Channel)

    $ErrorActionPreference = "Stop"
    # Windows PowerShell 5.1 downloads many times slower while it draws a progress bar.
    $ProgressPreference = "SilentlyContinue"

    if (-not $Version) { $Version = $env:VERSION }
    if (-not $Channel) { $Channel = $env:CHANNEL }
    if (-not $Channel) { $Channel = "stable" }

    Write-Host "Installing FlashAgent for Windows..." -ForegroundColor Cyan

    # 1. TLS 1.2 / 1.3 for PowerShell 5.1
    try {
        [Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12 -bor [Net.SecurityProtocolType]::Tls13
    } catch {
        # Older .NET Framework has no Tls13
        [Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12
    }

    # 2. Verify platform architecture
    if (-not [System.Environment]::Is64BitOperatingSystem) {
        throw "FlashAgent requires a 64-bit Windows operating system (x86_64)."
    }

    # 3. Ensure extraction dependencies are available
    $hasExpandArchive = $null -ne (Get-Command Expand-Archive -ErrorAction SilentlyContinue)
    if (-not $hasExpandArchive) {
        try {
            Add-Type -AssemblyName System.IO.Compression.FileSystem
        } catch {
            throw "Neither Expand-Archive nor System.IO.Compression.FileSystem is available."
        }
    }

    $Repo = "flashback7766/FlashAgent"
    $InstallDir = if ($env:INSTALL_DIR) { $env:INSTALL_DIR } elseif ($env:BIN_DIR) { $env:BIN_DIR } else { Join-Path $env:LOCALAPPDATA "Programs\FlashAgent" }
    $UserLocalBin = Join-Path $env:USERPROFILE ".local\bin"
    $Headers = @{ "User-Agent" = "FlashAgent-Installer" }

    # The Windows zip in a release from the API, or in its expanded_assets page.
    function Find-ZipAsset($Release) {
        foreach ($asset in @($Release.assets)) {
            if ($asset.name -match "windows.*x86_64.*\.zip$") { return $asset.browser_download_url }
        }
        return $null
    }
    function Find-ZipInHtml([string]$Html) {
        if ($Html -match 'href="([^"]*releases/download/[^"]*windows[^"]*\.zip)"') { return "https://github.com" + $Matches[1] }
        return $null
    }

    # 4. Fetch release metadata from GitHub
    Write-Host "Querying release data from GitHub..." -ForegroundColor Gray

    $DownloadUrl = $null

    if ($Version) {
        # Strategy 1: a specific version was asked for. Nothing else will do.
        if ($Version -match '^[0-9]') { $Version = "v$Version" }
        Write-Host "Target version requested: $Version" -ForegroundColor Gray
        # The + in a stable tag (vX.Y.Z+bN) must not be read as a space.
        $TagInUrl = [uri]::EscapeDataString($Version)
        try {
            $Release = Invoke-RestMethod -Uri "https://api.github.com/repos/$Repo/releases/tags/$TagInUrl" -UseBasicParsing -Headers $Headers
            $DownloadUrl = Find-ZipAsset $Release
        } catch {
            # Not found, or the API is rate-limited: the release page answers too.
            try {
                $DownloadUrl = Find-ZipInHtml (Invoke-WebRequest -Uri "https://github.com/$Repo/releases/expanded_assets/$TagInUrl" -UseBasicParsing -Headers $Headers).Content
            } catch {}
        }
        if (-not $DownloadUrl) {
            throw ("No FlashAgent release $Version with a Windows build was found.`n" +
                "Stable releases are tagged vX.Y.Z+bN; the list is at https://github.com/$Repo/releases`n" +
                "Beta builds are not kept one by one: for the latest beta, clear FLASHAGENT_VERSION and set FLASHAGENT_CHANNEL=beta.")
        }
    } elseif ($Channel -ne "beta" -and $Channel -ne "prerelease") {
        # Strategy 2: the latest stable release
        Write-Host "Checking for latest stable release..." -ForegroundColor Gray
        try {
            $StableRelease = Invoke-RestMethod -Uri "https://api.github.com/repos/$Repo/releases/latest" -UseBasicParsing -Headers $Headers
            $DownloadUrl = Find-ZipAsset $StableRelease
            if ($DownloadUrl) {
                Write-Host "Found latest stable release: $($StableRelease.tag_name)" -ForegroundColor Green
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
                    Write-Host "Found latest stable release: $stableTag" -ForegroundColor Green
                    $DownloadUrl = Find-ZipInHtml (Invoke-WebRequest -Uri "https://github.com/$Repo/releases/expanded_assets/$stableTag" -UseBasicParsing -Headers $Headers).Content
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
            $BetaRelease = Invoke-RestMethod -Uri "https://api.github.com/repos/$Repo/releases/tags/beta" -UseBasicParsing -Headers $Headers
            $DownloadUrl = Find-ZipAsset $BetaRelease
        } catch {}
    }

    # Strategy 4: newest pre-release in the release list
    if (-not $DownloadUrl) {
        try {
            $ReleaseData = Invoke-RestMethod -Uri "https://api.github.com/repos/$Repo/releases" -UseBasicParsing -Headers $Headers
            foreach ($release in @($ReleaseData)) {
                $DownloadUrl = Find-ZipAsset $release
                if ($DownloadUrl) { break }
            }
        } catch {
            # Rate-limited: the scrape below takes over
        }

        # Strategy 4b: scrape the release page when the API is rate-limited
        if (-not $DownloadUrl) {
            try {
                $releasesHtml = (Invoke-WebRequest -Uri "https://github.com/$Repo/releases" -UseBasicParsing -Headers $Headers).Content
                if ($releasesHtml -match 'data-item-id="release-([^"]+)"') {
                    $latestTag = $Matches[1]
                    $DownloadUrl = Find-ZipInHtml (Invoke-WebRequest -Uri "https://github.com/$Repo/releases/expanded_assets/$latestTag" -UseBasicParsing -Headers $Headers).Content
                }
            } catch {}
        }
    }

    if (-not $DownloadUrl) {
        throw "Could not find the Windows release asset on https://github.com/$Repo/releases"
    }

    # 5. Download asset to temporary directory
    $TempRoot = [System.IO.Path]::GetTempPath()
    $TempId = [System.Guid]::NewGuid().ToString()
    $TempZip = Join-Path $TempRoot "flashagent-win-$TempId.zip"
    $TempSums = Join-Path $TempRoot "flashagent-win-$TempId.SHA256SUMS"
    $ExtractDir = Join-Path $TempRoot "flashagent-win-ext-$TempId"

    try {
        Write-Host "Downloading from $DownloadUrl..." -ForegroundColor Gray
        Invoke-WebRequest -Uri $DownloadUrl -OutFile $TempZip -UseBasicParsing -Headers $Headers

        # 6. Check the download against the release's SHA256SUMS, as the in-app
        # updater does. Releases from before the manifest existed have none:
        # that is a warning. A manifest that lacks the file or disagrees with it
        # stops the install.
        $AssetName = [uri]::UnescapeDataString(($DownloadUrl -split '/')[-1])
        $SumsUrl = $DownloadUrl.Substring(0, $DownloadUrl.LastIndexOf('/')) + "/SHA256SUMS"
        $haveSums = $true
        try {
            Invoke-WebRequest -Uri $SumsUrl -OutFile $TempSums -UseBasicParsing -Headers $Headers
        } catch {
            $haveSums = $false
        }
        if (-not $haveSums) {
            Write-Host "Warning: this release has no SHA256SUMS (older releases lack it); the download is not verified." -ForegroundColor Yellow
        } else {
            # GitHub may rewrite '+' in an asset name; compare with it normalised too.
            $wanted = $AssetName -replace '\+', '.'
            $expected = $null
            foreach ($line in Get-Content -LiteralPath $TempSums) {
                if ($line -match '^([0-9A-Fa-f]{64})\s+\*?(.+?)\s*$') {
                    $hash = $Matches[1]
                    $name = $Matches[2]
                    if ($name -eq $AssetName -or ($name -replace '\+', '.') -eq $wanted) {
                        $expected = $hash.ToLowerInvariant()
                        break
                    }
                }
            }
            if (-not $expected) {
                throw "$AssetName is not listed in the release's SHA256SUMS. Nothing was installed."
            }
            if (Get-Command Get-FileHash -ErrorAction SilentlyContinue) {
                $actual = (Get-FileHash -Algorithm SHA256 -LiteralPath $TempZip).Hash.ToLowerInvariant()
            } else {
                $sha = [System.Security.Cryptography.SHA256]::Create()
                $stream = [System.IO.File]::OpenRead($TempZip)
                try {
                    $actual = ([BitConverter]::ToString($sha.ComputeHash($stream)) -replace '-', '').ToLowerInvariant()
                } finally {
                    $stream.Dispose()
                    $sha.Dispose()
                }
            }
            if ($actual -ne $expected) {
                throw ("Checksum mismatch for $AssetName.`n  expected $expected`n  got      $actual`n" +
                    "The download is damaged or was altered. Nothing was installed.")
            }
            Write-Host "Checksum verified (SHA256SUMS)" -ForegroundColor Green
        }

        # 7. Find the executable before touching the existing install.
        New-Item -ItemType Directory -Path $ExtractDir -Force | Out-Null
        Write-Host "Extracting archive..." -ForegroundColor Gray
        if ($hasExpandArchive) {
            Expand-Archive -Path $TempZip -DestinationPath $ExtractDir -Force
        } else {
            [System.IO.Compression.ZipFile]::ExtractToDirectory($TempZip, $ExtractDir)
        }

        $TargetExe = $null
        foreach ($exe in @(Get-ChildItem -Path $ExtractDir -Recurse -Filter "*.exe")) {
            if ($exe.Name -eq "flashagent.exe" -or $exe.Name -eq "flashagent-tui.exe") {
                $TargetExe = $exe.FullName
                break
            }
        }
        if (-not $TargetExe) {
            throw "No flashagent.exe in the downloaded archive. Nothing was installed."
        }

        # 8. Install. A running .exe can be renamed but not overwritten, so the
        # old one is moved aside first, and put back if anything after fails.
        if (-not (Test-Path -LiteralPath $InstallDir)) {
            New-Item -ItemType Directory -Path $InstallDir -Force | Out-Null
        }
        $DestExe = Join-Path $InstallDir "flashagent.exe"
        $Backup = "$DestExe.old"
        if (Test-Path -LiteralPath $Backup) { Remove-Item -LiteralPath $Backup -Force -ErrorAction SilentlyContinue }
        # Still there: a flashagent started before the last update is running from it.
        if (Test-Path -LiteralPath $Backup) { $Backup = "$DestExe.$([System.Guid]::NewGuid().ToString('N')).old" }

        $hadOld = Test-Path -LiteralPath $DestExe
        if ($hadOld) {
            try {
                Move-Item -LiteralPath $DestExe -Destination $Backup -Force
            } catch {
                throw "Could not move the existing $DestExe aside ($($_.Exception.Message)). Nothing was changed."
            }
        }

        $installed = $false
        $versionOut = $null
        try {
            Copy-Item -LiteralPath $TargetExe -Destination $DestExe -Force
            $versionOut = & $DestExe --version
            $installed = ($LASTEXITCODE -eq 0) -and [bool]$versionOut
        } catch {
            $installed = $false
        }
        if (-not $installed) {
            Remove-Item -LiteralPath $DestExe -Force -ErrorAction SilentlyContinue
            $restored = ""
            if ($hadOld -and (Test-Path -LiteralPath $Backup)) {
                Move-Item -LiteralPath $Backup -Destination $DestExe -Force
                $restored = " The previous flashagent.exe was put back."
            }
            throw "The downloaded flashagent.exe could not be installed or does not run ($DestExe --version failed).$restored"
        }
        Remove-Item -LiteralPath $Backup -Force -ErrorAction SilentlyContinue

        # Older installs also placed a flashagent-tui.exe copy; the command is just `flashagent` now.
        $OldTuiExe = Join-Path $InstallDir "flashagent-tui.exe"
        if (Test-Path -LiteralPath $OldTuiExe) { Remove-Item -LiteralPath $OldTuiExe -Force -ErrorAction SilentlyContinue }

        # Older installers also put a second copy in ~\.local\bin. The updater
        # replaces only the copy that is running, so the other one went stale
        # and could shadow the new install on PATH.
        $StaleCopy = Join-Path $UserLocalBin "flashagent.exe"
        if ((Test-Path -LiteralPath $StaleCopy) -and ($UserLocalBin.TrimEnd('\') -ne $InstallDir.TrimEnd('\'))) {
            try {
                Remove-Item -LiteralPath $StaleCopy -Force
                Write-Host "Removed the older copy at $StaleCopy; FlashAgent now lives in $InstallDir only." -ForegroundColor Gray
            } catch {
                Write-Host "Note: an older copy at $StaleCopy could not be removed (is it running?). Delete it once it has exited." -ForegroundColor Yellow
            }
        }

        Write-Host "Successfully installed $versionOut to $DestExe" -ForegroundColor Green

        # 9. PATH: nothing to do when the folder is already on it.
        $normalize = { param($p) $p.Trim().TrimEnd('\').ToLowerInvariant() }
        $wantedDir = & $normalize $InstallDir
        $UserPath = [System.Environment]::GetEnvironmentVariable("Path", "User")
        $MachinePath = [System.Environment]::GetEnvironmentVariable("Path", "Machine")
        $known = @("$MachinePath;$UserPath" -split ';' | Where-Object { $_.Trim() -ne "" } | ForEach-Object { & $normalize $_ })
        $onPath = $known -contains $wantedDir

        if ($onPath) {
            Write-Host "$InstallDir is already on your PATH." -ForegroundColor Green
        } else {
            $NewUserPath = if ($UserPath -and $UserPath.Trim() -ne "") { "$UserPath;$InstallDir" } else { $InstallDir }
            [System.Environment]::SetEnvironmentVariable("Path", $NewUserPath, "User")
            Write-Host "Added $InstallDir to your user PATH." -ForegroundColor Green
        }

        # This session too, so flashagent runs right away
        $sessionDirs = @($env:Path -split ';' | Where-Object { $_.Trim() -ne "" } | ForEach-Object { & $normalize $_ })
        if ($sessionDirs -notcontains $wantedDir) {
            $env:Path = "$InstallDir;$env:Path"
        }

        Write-Host ""
        Write-Host "Run 'flashagent' to launch!" -ForegroundColor Cyan
        if (-not $onPath) {
            Write-Host "Other terminals that were already open need to be reopened to find it." -ForegroundColor Gray
        }
    }
    finally {
        if (Test-Path -LiteralPath $TempZip) { Remove-Item -LiteralPath $TempZip -Force -ErrorAction SilentlyContinue }
        if (Test-Path -LiteralPath $TempSums) { Remove-Item -LiteralPath $TempSums -Force -ErrorAction SilentlyContinue }
        if (Test-Path -LiteralPath $ExtractDir) { Remove-Item -LiteralPath $ExtractDir -Recurse -Force -ErrorAction SilentlyContinue }
    }
}

try {
    Install-FlashAgent -Version $Version -Channel $Channel
} catch {
    Write-Host "Error: $($_.Exception.Message)" -ForegroundColor Red
    # Run as a file, the exit code says so. Under `irm | iex` there is no
    # script to leave, and `exit` would close the window.
    if ($MyInvocation.MyCommand.Path) { exit 1 }
}
