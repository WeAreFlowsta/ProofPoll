# Windows code-signing hook, invoked by Tauri's bundle.windows.signCommand
# (see src-tauri/tauri.conf.json) once per produced artifact.
#
# REQUIRES a Windows code-signing certificate — OV or EV — from a CA such as
# SSL.com, DigiCert, Sectigo, or Azure Trusted Signing. This script drives
# SSL.com's eSigner (cloud HSM) via CodeSignTool; the credentials come from
# the ESIGNER_* GitHub Actions secrets, never from the repo. NO certificate
# or key is stored here.
#
# FORKING: if your CA isn't SSL.com, replace the CodeSignTool invocation below
# with your provider's signing tool (e.g. Azure Trusted Signing's
# `azuresigntool`, or signtool.exe with a local cert) and read its credentials
# from your own secrets. If you have no cert at all, leave the ESIGNER_*
# secrets unset — the build skips signing and ships an unsigned (SmartScreen-
# warned) installer. See "Code Signing (Releases)" in README.md.

param(
    [Parameter(Mandatory = $true, Position = 0)]
    [string]$FilePath
)

$ErrorActionPreference = "Stop"

# Pre-release builds skip Authenticode signing (cloud signing services bill
# per signing operation; beta testers just click through SmartScreen). The
# release workflow sets this for -beta tags; rc and stable tags never do.
if ($env:PROOFPOLL_SKIP_WINDOWS_SIGNING -eq "1") {
    Write-Host "Skipping Authenticode signing (pre-release build): $FilePath"
    exit 0
}

# Mirror all output to a log file so CI can dump it on failure
# (tauri-bundler captures stdout/stderr but drops them on non-zero exit).
$logDir = if ($env:RUNNER_TEMP) { $env:RUNNER_TEMP } else { [System.IO.Path]::GetTempPath() }
$logFile = Join-Path $logDir "sign-windows.log"
Start-Transcript -Path $logFile -Append -Force | Out-Null

# Fail fast if Tauri didn't substitute the signCommand placeholder.
if ($FilePath -match '^%\d+$' -or $FilePath -eq '%1') {
    throw "signCommand placeholder was passed literally: '$FilePath'. Tauri did not substitute the file path."
}

foreach ($name in 'ESIGNER_USERNAME','ESIGNER_PASSWORD','ESIGNER_CREDENTIAL_ID','ESIGNER_TOTP_SECRET','CODE_SIGN_TOOL_DIR') {
    if ([string]::IsNullOrWhiteSpace([Environment]::GetEnvironmentVariable($name))) {
        throw "Environment variable $name is not set"
    }
}

$jar = Get-ChildItem -Path (Join-Path $env:CODE_SIGN_TOOL_DIR 'jar') -Filter 'code_sign_tool-*.jar' | Select-Object -First 1
if (-not $jar) { throw "CodeSignTool jar not found under $env:CODE_SIGN_TOOL_DIR\jar" }

if (-not (Test-Path -LiteralPath $FilePath)) {
    throw "File to sign does not exist: $FilePath"
}

# Skip extensions Authenticode / CodeSignTool can't sign. NSIS occasionally
# asks us to sign its own .tmp scratch files during plugin packing — without
# this guard, CodeSignTool errors and the build log gets noisy (NSIS itself
# tolerates the failure, but it masks real errors).
$signableExt = @('.exe', '.dll', '.msi', '.msix', '.appx', '.cab', '.ocx', '.sys', '.cat')
$ext = [System.IO.Path]::GetExtension($FilePath).ToLowerInvariant()
if (-not $signableExt.Contains($ext)) {
    Write-Host "Skipping unsignable file ($ext): $FilePath"
    exit 0
}

# Tauri passes the file as a path relative to its own CWD. We change CWD
# below to load CodeSignTool's conf/, so resolve to absolute first or
# CodeSignTool will look in its own directory and fail with
# "Invalid input file path".
$FilePath = (Resolve-Path -LiteralPath $FilePath).Path

Write-Host "Signing $FilePath"

# CodeSignTool reads conf/code_sign_tool.properties relative to CWD, so run from its root dir.
Push-Location $env:CODE_SIGN_TOOL_DIR
try {
    $output = & java -jar $jar.FullName sign `
        "-username=$env:ESIGNER_USERNAME" `
        "-password=$env:ESIGNER_PASSWORD" `
        "-credential_id=$env:ESIGNER_CREDENTIAL_ID" `
        "-totp_secret=$env:ESIGNER_TOTP_SECRET" `
        "-input_file_path=$FilePath" `
        -override 2>&1 | Out-String
    $code = $LASTEXITCODE
} finally {
    Pop-Location
}

Write-Host $output

# CodeSignTool sometimes prints "Error: ..." on stdout while exiting zero, so check both.
if ($code -ne 0 -or $output -match '(?m)^Error:') {
    throw "CodeSignTool failed (exit=$code) for $FilePath"
}
Write-Host "Signed $FilePath"
