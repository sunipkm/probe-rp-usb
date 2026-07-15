<#
.SYNOPSIS
    Installs probe-rp-usb on Windows.
.DESCRIPTION
    Downloads the latest (or a specified) prebuilt probe-rp-usb binary from
    GitHub Releases and installs it to a directory on your PATH.

    Supports x86-64 and ARM64 Windows.  TLS 1.2+ is enforced and, when a
    SHA-256 checksum file is published alongside the archive, the download is
    verified before extraction.
.PARAMETER Version
    Release tag to install (e.g. "v0.2.0").  Omit or pass "" to install the
    latest release.
.PARAMETER InstallDir
    Directory to install the binary into.
    Default: %LOCALAPPDATA%\probe-rp-usb\bin
.PARAMETER NoModifyPath
    Skip adding InstallDir to the current-user PATH environment variable.
.EXAMPLE
    # One-liner — install latest release
    powershell -ExecutionPolicy Bypass -c "irm https://github.com/sunipkm/probe-rp-usb/releases/latest/download/probe-rp-usb-installer.ps1 | iex"
.EXAMPLE
    # Specific version, custom directory
    .\probe-rp-usb-installer.ps1 -Version "v0.2.0" -InstallDir "$env:ProgramFiles\probe-rp-usb"
.NOTES
    After installation, the `reset` and `flash` subcommands require a WinUSB
    driver installed via Zadig (https://zadig.akeo.ie/) for your device.
    Serial-port commands (attach / watch / run) work with the built-in
    CDC-ACM driver — no extra setup needed.
#>
[CmdletBinding()]
param (
    [Parameter(HelpMessage = 'Release tag to install, e.g. "v0.2.0". Defaults to latest.')]
    [String] $Version = "",

    [Parameter(HelpMessage = 'Directory to install the binary. Defaults to %LOCALAPPDATA%\probe-rp-usb\bin.')]
    [String] $InstallDir = "",

    [Parameter(HelpMessage = 'Skip modifying the PATH environment variable.')]
    [Switch] $NoModifyPath
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

# ── Constants ──────────────────────────────────────────────────────────────────
$REPO        = "sunipkm/probe-rp-usb"
$BINARY_NAME = "probe-rp-usb"
$API_BASE    = "https://api.github.com/repos/$REPO"
$RELEASES    = "https://github.com/$REPO/releases"

# ── Helpers ────────────────────────────────────────────────────────────────────

function Write-Step {
    param([String] $Message)
    Write-Host "  " -NoNewline
    Write-Host $Message -ForegroundColor Cyan
}

function Write-Success {
    param([String] $Message)
    Write-Host $Message -ForegroundColor Green
}

function Write-Warn {
    param([String] $Message)
    Write-Host "warning: $Message" -ForegroundColor Yellow
}

# Enforce TLS 1.2+ for all web requests.
function Set-TlsPolicy {
    [Net.ServicePointManager]::SecurityProtocol =
        [Net.SecurityProtocolType]::Tls12 -bor
        [Net.SecurityProtocolType]::Tls13
}

# Map the current processor architecture to a Rust target triple.
function Get-TargetTriple {
    $arch = $env:PROCESSOR_ARCHITEW6432
    if (-not $arch) { $arch = $env:PROCESSOR_ARCHITECTURE }

    switch ($arch.ToUpperInvariant()) {
        'AMD64' { return 'x86_64-pc-windows-msvc' }
        'ARM64' { return 'aarch64-pc-windows-msvc' }
        default {
            throw "Unsupported architecture '$arch'. " +
                  "Only x86-64 (AMD64) and ARM64 are supported."
        }
    }
}

# Resolve the tag to install.  Empty string → fetch the latest release tag.
function Resolve-Version {
    param([String] $RequestedVersion)

    if ($RequestedVersion -ne "") {
        # Normalise: accept both "v0.1.0" and "0.1.0"
        if (-not $RequestedVersion.StartsWith('v')) {
            $RequestedVersion = "v$RequestedVersion"
        }
        return $RequestedVersion
    }

    Write-Step "Fetching latest release from GitHub…"
    $url  = "$API_BASE/releases/latest"
    $json = Invoke-RestMethod -Uri $url -UseBasicParsing `
                -Headers @{ 'User-Agent' = 'probe-rp-usb-installer' }
    return $json.tag_name
}

# Build the download URL for the archive (and its optional checksum file).
function Get-DownloadUrls {
    param([String] $Tag, [String] $Target)

    $archive  = "$BINARY_NAME-$Target.zip"
    $base     = "$RELEASES/download/$Tag"
    return @{
        Archive  = "$base/$archive"
        Checksum = "$base/$archive.sha256"
        FileName = $archive
    }
}

# Download a file; returns $true on success, $false on 404.
function Invoke-Download {
    param([String] $Url, [String] $Destination)

    try {
        Invoke-WebRequest -Uri $Url -OutFile $Destination `
            -UseBasicParsing `
            -Headers @{ 'User-Agent' = 'probe-rp-usb-installer' }
        return $true
    } catch [System.Net.WebException] {
        $code = [int]$_.Exception.Response.StatusCode
        if ($code -eq 404) { return $false }
        throw
    }
}

# Verify the SHA-256 hash of $File against the expected value in $ChecksumFile.
# The checksum file should contain "<hex>  <filename>" (shasum -a 256 format).
function Test-Checksum {
    param([String] $File, [String] $ChecksumFile)

    $line     = (Get-Content $ChecksumFile -Raw).Trim()
    $expected = ($line -split '\s+')[0].ToLower()
    $actual   = (Get-FileHash $File -Algorithm SHA256).Hash.ToLower()

    if ($actual -ne $expected) {
        throw "SHA-256 mismatch for '$File'.`n  expected: $expected`n  actual:   $actual"
    }
    Write-Step "SHA-256 verified."
}

# Add $Dir to the current-user PATH if it is not already present.
function Add-ToPath {
    param([String] $Dir)

    $scope   = [System.EnvironmentVariableTarget]::User
    $current = [System.Environment]::GetEnvironmentVariable('PATH', $scope)

    $entries = $current -split ';' | Where-Object { $_ -ne '' }
    if ($entries -contains $Dir) {
        Write-Step "'$Dir' is already in your PATH."
        return
    }

    $newPath = ($entries + $Dir) -join ';'
    [System.Environment]::SetEnvironmentVariable('PATH', $newPath, $scope)

    # Also update the current session's PATH so the binary is immediately usable.
    $env:PATH = "$env:PATH;$Dir"
    Write-Step "Added '$Dir' to your user PATH."
    Write-Warn "Restart your shell (or open a new terminal) for the PATH change to take effect."
}

# ── Main ───────────────────────────────────────────────────────────────────────

function Install-ProbeRpUsb {
    param(
        [String] $Version,
        [String] $InstallDir,
        [Bool]   $NoModifyPath
    )

    Set-TlsPolicy

    Write-Host ""
    Write-Host "probe-rp-usb installer" -ForegroundColor White
    Write-Host "══════════════════════" -ForegroundColor DarkGray
    Write-Host ""

    # 1. Resolve version tag.
    $tag = Resolve-Version -RequestedVersion $Version
    Write-Step "Installing $BINARY_NAME $tag"

    # 2. Detect architecture.
    $target = Get-TargetTriple
    Write-Step "Detected target: $target"

    # 3. Resolve install directory.
    if ($InstallDir -eq "") {
        $InstallDir = Join-Path $env:LOCALAPPDATA "probe-rp-usb\bin"
    }
    $exeDest = Join-Path $InstallDir "$BINARY_NAME.exe"

    # 4. Build download URLs.
    $urls = Get-DownloadUrls -Tag $tag -Target $target

    # 5. Create a temporary working directory.
    $tmp = Join-Path $env:TEMP ([System.IO.Path]::GetRandomFileName())
    New-Item -ItemType Directory -Path $tmp | Out-Null

    try {
        $archivePath  = Join-Path $tmp $urls.FileName
        $checksumPath = Join-Path $tmp "$($urls.FileName).sha256"

        # 6. Download archive.
        Write-Step "Downloading $($urls.Archive)…"
        if (-not (Invoke-Download -Url $urls.Archive -Destination $archivePath)) {
            throw "Release asset not found: $($urls.Archive)`n" +
                  "Check that the release '$tag' has a Windows build for '$target'."
        }

        # 7. Optionally verify checksum.
        if (Invoke-Download -Url $urls.Checksum -Destination $checksumPath) {
            Test-Checksum -File $archivePath -ChecksumFile $checksumPath
        } else {
            Write-Warn "No checksum file found for this release; skipping verification."
        }

        # 8. Extract.
        Write-Step "Extracting archive…"
        $extractDir = Join-Path $tmp "extracted"
        Expand-Archive -Path $archivePath -DestinationPath $extractDir -Force

        # The exe may be at the root or inside a subdirectory — find it.
        $exeSource = Get-ChildItem -Path $extractDir -Filter "$BINARY_NAME.exe" `
                         -Recurse | Select-Object -First 1 -ExpandProperty FullName
        if (-not $exeSource) {
            throw "$BINARY_NAME.exe not found inside the archive."
        }

        # 9. Install.
        if (-not (Test-Path $InstallDir)) {
            New-Item -ItemType Directory -Path $InstallDir | Out-Null
        }
        Copy-Item -Path $exeSource -Destination $exeDest -Force
        Write-Step "Installed to: $exeDest"

    } finally {
        # 10. Clean up temp files regardless of success or failure.
        Remove-Item -Recurse -Force -Path $tmp -ErrorAction SilentlyContinue
    }

    # 11. Modify PATH.
    if (-not $NoModifyPath) {
        Add-ToPath -Dir $InstallDir
    } else {
        Write-Step "Skipping PATH modification (--NoModifyPath)."
        Write-Step "Add '$InstallDir' to your PATH manually."
    }

    Write-Host ""
    Write-Success "probe-rp-usb $tag installed successfully!"
    Write-Host ""
    Write-Host "Quick-start:" -ForegroundColor White
    Write-Host "  probe-rp-usb flash  firmware.elf    # flash via BOOTSEL"
    Write-Host "  probe-rp-usb watch  firmware.elf    # stream defmt logs"
    Write-Host "  probe-rp-usb run    firmware.elf    # flash + watch"
    Write-Host ""
    Write-Host "Windows USB driver note:" -ForegroundColor DarkGray
    Write-Host "  The 'reset' and 'flash' commands need a WinUSB driver for your device."
    Write-Host "  Install it once with Zadig: https://zadig.akeo.ie/"
    Write-Host "  Serial-port commands (attach/watch/run) work out of the box."
    Write-Host ""
}

# Entry point — catches all errors and exits with a non-zero code.
try {
    Install-ProbeRpUsb `
        -Version      $Version `
        -InstallDir   $InstallDir `
        -NoModifyPath $NoModifyPath.IsPresent
} catch {
    Write-Host ""
    Write-Host "error: $_" -ForegroundColor Red
    Write-Host ""
    Write-Host "If the problem persists, please open an issue at:"
    Write-Host "  https://github.com/$REPO/issues"
    Write-Host ""
    exit 1
}
