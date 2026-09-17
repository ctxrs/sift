# Offline, dependency-free checks. Run: powershell -NoProfile -File tests/install.Tests.ps1
# Platform variables are synthetic, so these checks also run with pwsh on Unix.
$ErrorActionPreference = 'Stop'
$installer = Join-Path (Split-Path $PSScriptRoot -Parent) 'install.ps1'
$root = Join-Path ([IO.Path]::GetTempPath()) ('retok-installer-test-' + [guid]::NewGuid())
$variables = @('OS', 'PROCESSOR_ARCHITECTURE', 'PROCESSOR_ARCHITEW6432',
    'LOCALAPPDATA', 'RETOK_VERSION', 'RETOK_INSTALL_DIR', 'TEMP', 'TMP', 'TMPDIR')
$saved = @{}
foreach ($name in $variables) { $saved[$name] = [Environment]::GetEnvironmentVariable($name) }

function Assert($condition, $message) {
    if (-not $condition) { throw $message }
}

# These mocks never invoke a network command. They serve a synthetic release and
# calculate the checksum locally, with explicit download/checksum failure cases.
function Invoke-WebRequest {
    param([switch]$UseBasicParsing, [string]$Uri, [string]$OutFile)
    Assert $UseBasicParsing 'Expected Windows PowerShell compatible download'
    $script:requests += $Uri
    $asset = ($Uri -split '/')[-1]
    Assert ($Uri -ceq "$script:base/$asset") "Unexpected release URL: $Uri"
    Assert ($asset -in @('retok-windows-x64.exe', 'retok-windows-x64.exe.third-party-notices.txt', 'SHA256SUMS')) 'Unexpected asset'
    Assert ((Split-Path (Split-Path $OutFile -Parent) -Parent) -ceq (Split-Path $script:destination -Parent)) 'Download was not staged beside the destination'
    if ($asset -eq $script:failDownload) {
        [IO.File]::WriteAllText($OutFile, 'partial download')
        throw 'Synthetic download failure'
    }
    if ($asset -eq 'SHA256SUMS') {
        [IO.File]::WriteAllText($OutFile, $script:manifest)
    } elseif ($asset -eq 'retok-windows-x64.exe') {
        [IO.File]::WriteAllBytes($OutFile, $script:payload)
    } else {
        [IO.File]::WriteAllBytes($OutFile, $script:notices)
    }
}

function Get-FileHash {
    param([string]$LiteralPath, [string]$Algorithm)
    Assert ($Algorithm -eq 'SHA256') 'Expected SHA256'
    if ($script:failChecksum) { throw 'Synthetic checksum failure' }
    $result = Microsoft.PowerShell.Utility\Get-FileHash -LiteralPath $LiteralPath -Algorithm SHA256
    if ($script:denyBinarySource -and $LiteralPath.EndsWith('.third-party-notices.txt')) {
        # Portable fault injection: deny the final file operation by turning only
        # the staged executable into a directory after hashing. Never touch the
        # installed executable. Native Windows tests below instead use a lock.
        $binary = Join-Path (Split-Path $LiteralPath -Parent) 'retok-windows-x64.exe'
        [IO.File]::Delete($binary)
        [IO.Directory]::CreateDirectory($binary) | Out-Null
    }
    $result
}

function Test-DeniedBinaryReplacement([bool]$hadBinary, [bool]$hadNotices, [bool]$nativeLock = $false) {
    $env:RETOK_INSTALL_DIR = Join-Path $script:root ("denied-$hadBinary-$hadNotices-$nativeLock")
    [IO.Directory]::CreateDirectory($env:RETOK_INSTALL_DIR) | Out-Null
    $script:destination = Join-Path $env:RETOK_INSTALL_DIR 'retok.exe'
    $noticePath = $script:destination + '.third-party-notices.txt'
    $oldNotices = [byte[]]@(111, 108, 100, 13, 10, 0, 255, 10)
    if ($hadBinary) { [IO.File]::WriteAllText($script:destination, 'previous working retok') }
    if ($hadNotices) { [IO.File]::WriteAllBytes($noticePath, $oldNotices) }
    $script:requests = @()
    $script:denyBinarySource = -not $nativeLock
    $handle = $null
    if ($nativeLock) {
        # No FileShare.Delete: Windows must reject replacement of this file.
        $handle = [IO.File]::Open($script:destination, [IO.FileMode]::Open, [IO.FileAccess]::Read, [IO.FileShare]::Read)
    }
    try {
        $failure = $null
        try { Invoke-Expression ([IO.File]::ReadAllText($installer)) } catch { $failure = $_.Exception }
        Assert ($null -ne $failure) 'Expected executable replacement to be denied'
        Assert ($failure.InnerException -is [IO.IOException] -or $failure.InnerException -is [UnauthorizedAccessException]) "Unexpected replacement failure: $failure"
        Assert ($script:requests.Count -eq 3) 'Failed before downloading all release files'
        if ($hadBinary) {
            Assert ([IO.File]::ReadAllText($script:destination) -ceq 'previous working retok') 'Changed denied executable'
        } else {
            Assert (-not (Test-Path -LiteralPath $script:destination)) 'Left a binary after failed fresh install'
        }
        if ($hadNotices) {
            Assert ([Convert]::ToBase64String([IO.File]::ReadAllBytes($noticePath)) -ceq [Convert]::ToBase64String($oldNotices)) 'Did not restore exact previous notices bytes'
        } else {
            Assert (-not (Test-Path -LiteralPath $noticePath)) 'Left new notices after executable replacement failed'
        }
        Assert (@(Get-ChildItem -LiteralPath $env:RETOK_INSTALL_DIR -Filter '.retok-*' -Force).Count -eq 0) 'Leaked staged files after rollback'
        $script:checks++
    } finally {
        if ($null -ne $handle) { $handle.Dispose() }
        $script:denyBinarySource = $false
    }
}

function Run-Installer($expectedFailure = '') {
    $beforePreference = $ErrorActionPreference
    $beforePath = $env:PATH
    $failure = $null
    try { Invoke-Expression ([IO.File]::ReadAllText($installer)) } catch { $failure = $_.Exception.Message }
    Assert ($ErrorActionPreference -eq $beforePreference) 'Changed caller error preference'
    Assert ($env:PATH -ceq $beforePath) 'Changed caller PATH'
    Assert (@(Get-ChildItem -LiteralPath $script:temp).Count -eq 0) 'Leaked temporary download'
    $parent = Split-Path $script:destination -Parent
    Assert (@(Get-ChildItem -LiteralPath $parent -Filter '.retok-*' -Force).Count -eq 0) 'Leaked staged files'
    $installedNotices = $script:destination + '.third-party-notices.txt'
    if ($expectedFailure) {
        Assert ($null -ne $failure -and $failure.Contains($expectedFailure)) "Expected '$expectedFailure'; got '$failure'"
        Assert ([IO.File]::ReadAllText($script:destination) -ceq 'previous working retok') 'Replaced existing binary on failure'
        Assert ([IO.File]::ReadAllText($installedNotices) -ceq 'previous notices') 'Replaced existing notices on failure'
    } else {
        Assert ($null -eq $failure) "Install failed: $failure"
        Assert ([Convert]::ToBase64String([IO.File]::ReadAllBytes($script:destination)) -ceq [Convert]::ToBase64String($script:payload)) 'Wrong installed bytes'
        Assert ([Convert]::ToBase64String([IO.File]::ReadAllBytes($installedNotices)) -ceq [Convert]::ToBase64String($script:notices)) 'Wrong installed notices'
        Assert ($script:requests.Count -eq 3) 'Expected exactly three downloads'
        Assert ($script:requests[0] -ceq "$script:base/SHA256SUMS") 'Wrong checksum URL'
        Assert ($script:requests[1] -ceq "$script:base/retok-windows-x64.exe") 'Wrong binary URL'
        Assert ($script:requests[2] -ceq "$script:base/retok-windows-x64.exe.third-party-notices.txt") 'Wrong notices URL'
    }
    $script:requests = @()
    $script:checks++
}

try {
    $temp = Join-Path $root 'temporary downloads'
    [IO.Directory]::CreateDirectory($temp) | Out-Null
    $env:TEMP = $temp
    $env:TMP = $temp
    $env:TMPDIR = $temp
    $env:OS = 'Windows_NT'
    $env:PROCESSOR_ARCHITECTURE = 'AMD64'
    $env:PROCESSOR_ARCHITEW6432 = ''
    $env:LOCALAPPDATA = Join-Path $root 'local app data'
    $env:RETOK_VERSION = ''
    $env:RETOK_INSTALL_DIR = ''
    $payload = [Text.Encoding]::UTF8.GetBytes("synthetic Windows release`n")
    $notices = [Text.Encoding]::UTF8.GetBytes("synthetic third-party notices`n")
    $sha = [Security.Cryptography.SHA256]::Create()
    try {
        $digest = [BitConverter]::ToString($sha.ComputeHash($payload)).Replace('-', '').ToLowerInvariant()
        $noticesDigest = [BitConverter]::ToString($sha.ComputeHash($notices)).Replace('-', '').ToLowerInvariant()
    }
    finally { $sha.Dispose() }
    $binaryEntry = "$digest  retok-windows-x64.exe`r`n"
    $noticesEntry = "$noticesDigest  retok-windows-x64.exe.third-party-notices.txt`r`n"
    $validManifest = $binaryEntry + $noticesEntry
    $manifest = $validManifest
    $base = 'https://github.com/ctxrs/retok/releases/latest/download'
    $requests = @()
    $checks = 0
    $failChecksum = $false
    $failDownload = ''
    $destination = Join-Path $env:LOCALAPPDATA 'Programs/Retok/retok.exe'
    Run-Installer

    # Pinned release, custom path with spaces, and 32-bit PowerShell on x64 Windows.
    $env:RETOK_VERSION = 'v0.1.0'
    $base = 'https://github.com/ctxrs/retok/releases/download/v0.1.0'
    $env:RETOK_INSTALL_DIR = Join-Path $root 'custom bin'
    $destination = Join-Path $env:RETOK_INSTALL_DIR 'retok.exe'
    $env:PROCESSOR_ARCHITECTURE = 'x86'
    $env:PROCESSOR_ARCHITEW6432 = 'AMD64'
    $manifest = $digest.ToUpperInvariant() + " *retok-windows-x64.exe`n" + $noticesDigest.ToUpperInvariant() + " *retok-windows-x64.exe.third-party-notices.txt`n"
    Run-Installer
    Run-Installer # Upgrade an existing installation.

    # Hold the old files open: a replacement must not truncate their contents.
    [IO.File]::WriteAllText($destination, 'previous working retok')
    [IO.File]::WriteAllText(($destination + '.third-party-notices.txt'), 'previous notices')
    $sharing = [IO.FileShare]::ReadWrite -bor [IO.FileShare]::Delete
    $oldBinary = [IO.File]::Open($destination, [IO.FileMode]::Open, [IO.FileAccess]::Read, $sharing)
    $oldNotices = [IO.File]::Open(($destination + '.third-party-notices.txt'), [IO.FileMode]::Open, [IO.FileAccess]::Read, $sharing)
    try {
        Run-Installer
        $binaryReader = [IO.StreamReader]::new($oldBinary)
        $noticesReader = [IO.StreamReader]::new($oldNotices)
        try {
            Assert ($binaryReader.ReadToEnd() -ceq 'previous working retok') 'Truncated old executable'
            Assert ($noticesReader.ReadToEnd() -ceq 'previous notices') 'Truncated old notices'
        } finally {
            $binaryReader.Dispose()
            $noticesReader.Dispose()
        }
    } finally {
        $oldBinary.Dispose()
        $oldNotices.Dispose()
    }

    [IO.File]::WriteAllText($destination, 'previous working retok')
    [IO.File]::WriteAllText(($destination + '.third-party-notices.txt'), 'previous notices')
    foreach ($entry in @($binaryEntry, $noticesEntry)) {
        $name = ($entry.Trim() -split '  ')[1]
        foreach ($bad in @('', ($entry * 2), ('z' * 64 + "  $name`r`n"),
                "abc  $name`r`n", ($entry + "abc  $name`r`n"), ($entry.Replace($name, 'other-asset')))) {
            $manifest = $validManifest.Replace($entry, $bad)
            Run-Installer 'checksum'
        }
        $manifest = $validManifest.Replace($entry, ('0' * 64 + "  $name`r`n"))
        Run-Installer 'SHA-256 mismatch'
    }
    $manifest = $validManifest
    $failChecksum = $true
    Run-Installer 'Synthetic checksum failure'
    $failChecksum = $false
    foreach ($asset in @('retok-windows-x64.exe', 'retok-windows-x64.exe.third-party-notices.txt', 'SHA256SUMS')) {
        $failDownload = $asset
        Run-Installer 'Synthetic download failure'
    }
    $failDownload = ''

    $env:RETOK_VERSION = '../../other'
    Run-Installer 'Invalid RETOK_VERSION'
    $env:RETOK_VERSION = 'v0.1.0'
    $env:PROCESSOR_ARCHITEW6432 = ''
    foreach ($arch in @('ARM64', 'x86')) {
        $env:PROCESSOR_ARCHITECTURE = $arch
        Run-Installer 'require x64 Windows'
    }
    $env:PROCESSOR_ARCHITECTURE = 'AMD64'
    $env:OS = 'Other'
    Run-Installer 'requires Windows'
    $env:OS = 'Windows_NT'

    # Neither destination may be a directory.
    foreach ($target in @($destination, ($destination + '.third-party-notices.txt'))) {
        Remove-Item -LiteralPath $target
        [IO.Directory]::CreateDirectory($target) | Out-Null
        $failure = ''
        try { Invoke-Expression ([IO.File]::ReadAllText($installer)) } catch { $failure = $_.Exception.Message }
        Assert ($failure.Contains('is a directory')) 'Accepted a directory as the file destination'
        Assert (@(Get-ChildItem -LiteralPath $target).Count -eq 0) 'Wrote inside destination directory'
        Remove-Item -LiteralPath $target
        $checks++
    }
    # A failed fresh install must leave neither binary nor notices installed.
    $manifest = $binaryEntry
    $failure = ''
    try { Invoke-Expression ([IO.File]::ReadAllText($installer)) } catch { $failure = $_.Exception.Message }
    Assert ($failure.Contains('checksum')) 'Accepted missing notices checksum'
    Assert (-not (Test-Path -LiteralPath $destination)) 'Installed binary without notices'
    Assert (-not (Test-Path -LiteralPath ($destination + '.third-party-notices.txt'))) 'Installed unverified notices'
    Assert (@(Get-ChildItem -LiteralPath (Split-Path $destination -Parent) -Filter '.retok-*' -Force).Count -eq 0) 'Leaked staged files'
    $checks++
    $manifest = $validManifest
    foreach ($hadBinary in @($false, $true)) {
        foreach ($hadNotices in @($false, $true)) {
            Test-DeniedBinaryReplacement $hadBinary $hadNotices
        }
    }
    if ([Environment]::OSVersion.Platform -eq [PlatformID]::Win32NT) {
        Test-DeniedBinaryReplacement $true $false $true
        Test-DeniedBinaryReplacement $true $true $true
    } else {
        Write-Host 'Skipped 2 native Windows no-delete-sharing lock checks: requires Windows.'
    }
    Write-Host "Passed $checks offline PowerShell installer checks."
} finally {
    foreach ($name in $variables) { [Environment]::SetEnvironmentVariable($name, $saved[$name]) }
    if (Test-Path -LiteralPath $root) { Remove-Item -LiteralPath $root -Recurse -Force }
}
