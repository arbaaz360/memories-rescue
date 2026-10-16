# Windows Authenticode signing dispatcher, called by Tauri for every binary and
# installer it produces (bundle.windows.signCommand in tauri.release.conf.json).
#
# Picks the signing service from which environment variables are present:
#   SSL.com eSigner        ESIGNER_USERNAME, ESIGNER_PASSWORD, ESIGNER_CREDENTIAL_ID, ESIGNER_TOTP_SECRET,
#                          CODESIGNTOOL_DIR (folder holding CodeSignTool.bat; Java 11+ on PATH)
#   Azure Trusted Signing  AZURE_TENANT_ID, AZURE_CLIENT_ID, AZURE_CLIENT_SECRET (read by the dlib itself),
#                          TRUSTED_SIGNING_ENDPOINT, TRUSTED_SIGNING_ACCOUNT, TRUSTED_SIGNING_PROFILE,
#                          TRUSTED_SIGNING_DLIB (path to Azure.CodeSigning.Dlib.dll)
#   neither                the file is left unsigned and MR_RELEASE_SIGNED=false is noted; the build goes on.
#
# Values are only ever passed to the signing tool's own command line; nothing is
# written to disk except the Trusted Signing metadata file (endpoint and account
# names, which are not secret). The release workflow sets these from repository
# secrets; see docs/release/README.md for where each one comes from.
param(
    [Parameter(Mandatory = $true)][string]$Path
)
$ErrorActionPreference = "Stop"

function Find-SignTool {
    $cmd = Get-Command signtool.exe -ErrorAction SilentlyContinue
    if ($cmd) { return $cmd.Source }
    $kits = Join-Path ${env:ProgramFiles(x86)} "Windows Kits\10\bin"
    $found = Get-ChildItem -Path $kits -Filter signtool.exe -Recurse -ErrorAction SilentlyContinue |
        Where-Object { $_.FullName -match '\\x64\\' } | Sort-Object FullName | Select-Object -Last 1
    if ($found) { return $found.FullName }
    throw "signtool.exe not found (install the Windows SDK)"
}

$file = (Resolve-Path $Path).Path
$name = Split-Path $file -Leaf

if ($env:ESIGNER_USERNAME -and $env:ESIGNER_CREDENTIAL_ID) {
    if (-not $env:CODESIGNTOOL_DIR) { throw "CODESIGNTOOL_DIR is not set" }
    $tool = Join-Path $env:CODESIGNTOOL_DIR "CodeSignTool.bat"
    Write-Host "sign-windows: eSigner -> $name"
    & $tool sign `
        "-username=$env:ESIGNER_USERNAME" `
        "-password=$env:ESIGNER_PASSWORD" `
        "-credential_id=$env:ESIGNER_CREDENTIAL_ID" `
        "-totp_secret=$env:ESIGNER_TOTP_SECRET" `
        "-input_file_path=$file" `
        "-override=true"
    if ($LASTEXITCODE -ne 0) { throw "CodeSignTool failed for $name" }
}
elseif ($env:TRUSTED_SIGNING_ENDPOINT -and $env:TRUSTED_SIGNING_ACCOUNT -and $env:TRUSTED_SIGNING_PROFILE) {
    if (-not $env:TRUSTED_SIGNING_DLIB) { throw "TRUSTED_SIGNING_DLIB is not set" }
    $metadata = Join-Path $env:RUNNER_TEMP "trusted-signing.json"
    if (-not $env:RUNNER_TEMP) { $metadata = Join-Path $env:TEMP "trusted-signing.json" }
    @{
        Endpoint               = $env:TRUSTED_SIGNING_ENDPOINT
        CodeSigningAccountName = $env:TRUSTED_SIGNING_ACCOUNT
        CertificateProfileName = $env:TRUSTED_SIGNING_PROFILE
    } | ConvertTo-Json | Set-Content -Path $metadata -Encoding UTF8
    $signtool = Find-SignTool
    Write-Host "sign-windows: Azure Trusted Signing -> $name"
    & $signtool sign /v /fd SHA256 /tr "http://timestamp.acs.microsoft.com" /td SHA256 `
        /dlib $env:TRUSTED_SIGNING_DLIB /dmdf $metadata $file
    if ($LASTEXITCODE -ne 0) { throw "signtool (Trusted Signing) failed for $name" }
}
else {
    Write-Host "sign-windows: no signing secrets present, leaving $name unsigned"
    if ($env:GITHUB_ENV) { Add-Content -Path $env:GITHUB_ENV -Value "MR_RELEASE_SIGNED=false" }
    exit 0
}

$signtool = Find-SignTool
& $signtool verify /pa /v $file | Out-Null
if ($LASTEXITCODE -ne 0) { throw "signature verification failed for $name" }
Write-Host "sign-windows: verified $name"
