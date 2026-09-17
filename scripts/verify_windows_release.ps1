param(
    [Parameter(Mandatory = $true)][string]$Artifact,
    [string]$ExpectedVersion = ''
)

$ErrorActionPreference = 'Stop'
$item = Get-Item -LiteralPath $Artifact -Force
if (-not $item.PSIsContainer -and ($item.Attributes -band [IO.FileAttributes]::ReparsePoint) -eq 0) {
    # Expected regular file.
} else {
    throw 'Windows signing verification: artifact must be a regular non-symlink file'
}
$resolved = $item.FullName

$root = Split-Path $PSScriptRoot -Parent
$contractPath = Join-Path $root 'contracts/release-signing-v1.json'
$contract = Get-Content -LiteralPath $contractPath -Raw | ConvertFrom-Json
$policy = $contract.windows
$before = (Get-FileHash -LiteralPath $resolved -Algorithm SHA256).Hash.ToLowerInvariant()
$signature = Get-AuthenticodeSignature -LiteralPath $resolved
if ($signature.Status -ne [System.Management.Automation.SignatureStatus]::Valid) {
    throw "Windows signing verification: Authenticode status is $($signature.Status)"
}
if ($null -eq $signature.SignerCertificate) {
    throw 'Windows signing verification: missing signer certificate'
}
$commonName = $signature.SignerCertificate.GetNameInfo(
    [Security.Cryptography.X509Certificates.X509NameType]::SimpleName, $false)
if ($commonName -cne [string]$policy.expected_common_name) {
    throw 'Windows signing verification: unexpected publisher common name'
}
$organizationPattern = '(^|,\s*)O="?' + [regex]::Escape([string]$policy.expected_organization) + '"?(,|$)'
if ($signature.SignerCertificate.Subject -notmatch $organizationPattern) {
    throw 'Windows signing verification: unexpected publisher organization'
}
if ($null -eq $signature.TimeStamperCertificate) {
    throw 'Windows signing verification: missing trusted timestamp'
}

if ($ExpectedVersion) {
    $versionOutput = & $resolved --version 2>&1
    if ($LASTEXITCODE -ne 0 -or (($versionOutput -join "`n") -cne "Retok $ExpectedVersion")) {
        throw 'Windows signing verification: unexpected version output'
    }
}
$after = (Get-FileHash -LiteralPath $resolved -Algorithm SHA256).Hash.ToLowerInvariant()
if ($after -cne $before) {
    throw 'Windows signing verification: artifact changed during verification'
}

[ordered]@{
    schema_version = 1
    artifact = [IO.Path]::GetFileName($resolved)
    artifact_sha256 = $after
    platform = 'windows'
    status = 'passed'
    publisher_common_name = [string]$policy.expected_common_name
    publisher_organization = [string]$policy.expected_organization
    timestamped = $true
} | ConvertTo-Json -Compress
