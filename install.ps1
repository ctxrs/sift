# Run in a child scope so piping this script to iex does not change preferences.
& {
    $ErrorActionPreference = 'Stop'
    if ($env:OS -ne 'Windows_NT') {
        throw 'This installer requires Windows. Use install.sh on Linux or macOS.'
    }
    $arch = $env:PROCESSOR_ARCHITEW6432
    if (-not $arch) { $arch = $env:PROCESSOR_ARCHITECTURE }
    if ($arch -ne 'AMD64') { throw 'Retok Windows releases require x64 Windows.' }

    $asset = 'retok-windows-x64.exe'
    $notices = "$asset.third-party-notices.txt"
    $base = 'https://github.com/ctxrs/retok/releases'
    if ($env:RETOK_VERSION) {
        if ($env:RETOK_VERSION -notmatch '^[A-Za-z0-9._-]+$') {
            throw 'Invalid RETOK_VERSION release tag.'
        }
        $base += '/download/' + $env:RETOK_VERSION
    } else {
        $base += '/latest/download'
    }
    $installDir = $env:RETOK_INSTALL_DIR
    if (-not $installDir) {
        $installDir = Join-Path $env:LOCALAPPDATA 'Programs/Retok'
    }
    $installDir = $ExecutionContext.SessionState.Path.GetUnresolvedProviderPathFromPSPath($installDir)
    [IO.Directory]::CreateDirectory($installDir) | Out-Null
    foreach ($name in @('retok.exe', 'retok.exe.third-party-notices.txt')) {
        $target = Join-Path $installDir $name
        if (Test-Path -LiteralPath $target -PathType Container) { throw "$target is a directory." }
    }
    $tempDir = Join-Path $installDir ('.retok-' + [guid]::NewGuid())
    [IO.Directory]::CreateDirectory($tempDir) | Out-Null
    $keepTempDir = $false
    try {
        $sums = Join-Path $tempDir 'SHA256SUMS'
        Invoke-WebRequest -UseBasicParsing -Uri "$base/SHA256SUMS" -OutFile $sums
        foreach ($file in @($asset, $notices)) {
            $download = Join-Path $tempDir $file
            Invoke-WebRequest -UseBasicParsing -Uri "$base/$file" -OutFile $download
            $pattern = '^\S+[ \t]+\*?' + [regex]::Escape($file) + '$'
            $entries = @(Get-Content -LiteralPath $sums | Where-Object { $_ -cmatch $pattern })
            if ($entries.Count -ne 1) { throw "Missing or ambiguous SHA-256 checksum for $file." }
            $match = [regex]::Match($entries[0], '^([0-9a-fA-F]{64}) [ *]' + [regex]::Escape($file) + '$')
            if (-not $match.Success) { throw "Invalid SHA-256 checksum for $file." }
            $actual = (Get-FileHash -LiteralPath $download -Algorithm SHA256).Hash
            if ($actual -ne $match.Groups[1].Value) { throw "SHA-256 mismatch for $file." }
        }

        # Both files are verified and staged on the destination filesystem.
        # Replace existing files without opening them for truncation; binary last.
        $noticeSource = Join-Path $tempDir $notices
        $noticeDestination = Join-Path $installDir 'retok.exe.third-party-notices.txt'
        $noticeBackup = Join-Path $tempDir 'previous-notices'
        if ([IO.File]::Exists($noticeDestination)) {
            [IO.File]::Replace($noticeSource, $noticeDestination, $noticeBackup)
        } else {
            [IO.File]::Move($noticeSource, $noticeDestination)
        }
        $source = Join-Path $tempDir $asset
        $destination = Join-Path $installDir 'retok.exe'
        try {
            if ([IO.File]::Exists($destination)) {
                [IO.File]::Replace($source, $destination, [NullString]::Value)
            } else {
                [IO.File]::Move($source, $destination)
            }
        } catch {
            $installFailure = $_
            try {
                if ([IO.File]::Exists($noticeBackup)) {
                    [IO.File]::Replace($noticeBackup, $noticeDestination, [NullString]::Value)
                } else {
                    [IO.File]::Delete($noticeDestination)
                }
            } catch {
                $keepTempDir = $true
                throw "Executable replacement and notices rollback failed; staged files retained at ${tempDir}: $_"
            }
            throw $installFailure
        }
        Write-Host "Installed retok to $destination"
        Write-Host "Add $installDir to your user PATH if needed, then run retok --help."
    } finally {
        if (-not $keepTempDir) { Remove-Item -LiteralPath $tempDir -Recurse -Force }
    }
}
