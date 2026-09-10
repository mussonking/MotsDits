param(
    [Parameter(Position = 0)]
    [string]$FilePath,

    [switch]$Preflight
)

$ErrorActionPreference = "Stop"

$Endpoint = "https://ncus.codesigning.azure.net/"
$CodeSigningAccountName = "leclasseur"
$CertificateProfileName = "leClasseur"
$TenantId = "9f32d14e-3afc-478c-8bb2-26da9b9ced36"
$SubscriptionId = "fa6c53d7-05aa-4434-a4aa-882f0f41b2be"
$TimestampUrl = "http://timestamp.acs.microsoft.com"

function Refresh-ProcessPath {
    $machinePath = [System.Environment]::GetEnvironmentVariable("Path", "Machine")
    $userPath = [System.Environment]::GetEnvironmentVariable("Path", "User")
    $env:Path = @($machinePath, $userPath, $env:Path) -join ";"
}

function Get-LatestSignTool {
    if ($env:MOTSDITS_SIGNTOOL -and (Test-Path -LiteralPath $env:MOTSDITS_SIGNTOOL)) {
        return (Resolve-Path -LiteralPath $env:MOTSDITS_SIGNTOOL).Path
    }

    $windowsKits = Join-Path ${env:ProgramFiles(x86)} "Windows Kits\10\bin"
    if (Test-Path -LiteralPath $windowsKits) {
        $candidate = Get-ChildItem -LiteralPath $windowsKits -Directory -ErrorAction SilentlyContinue |
            Sort-Object Name -Descending |
            ForEach-Object { Join-Path $_.FullName "x64\signtool.exe" } |
            Where-Object { Test-Path -LiteralPath $_ } |
            Select-Object -First 1
        if ($candidate) { return $candidate }
    }

    $fromPath = Get-Command signtool.exe -ErrorAction SilentlyContinue
    if ($fromPath) { return $fromPath.Source }
    return $null
}

function Get-ArtifactSigningDlib {
    if ($env:MOTSDITS_TRUSTED_SIGNING_DLIB -and (Test-Path -LiteralPath $env:MOTSDITS_TRUSTED_SIGNING_DLIB)) {
        return (Resolve-Path -LiteralPath $env:MOTSDITS_TRUSTED_SIGNING_DLIB).Path
    }

    $roots = @()
    if ($env:LOCALAPPDATA) {
        $roots += (Join-Path $env:LOCALAPPDATA "Microsoft\MicrosoftArtifactSigningClientTools")
    }
    if ($env:ProgramFiles) { $roots += $env:ProgramFiles }
    if (${env:ProgramFiles(x86)}) { $roots += ${env:ProgramFiles(x86)} }
    $nuget = Join-Path $HOME ".nuget\packages"
    if (Test-Path -LiteralPath $nuget) { $roots += $nuget }

    $matches = foreach ($root in $roots) {
        if (Test-Path -LiteralPath $root) {
            Get-ChildItem -LiteralPath $root -Recurse -Filter Azure.CodeSigning.Dlib.dll -ErrorAction SilentlyContinue
        }
    }
    $preferred = $matches |
        Where-Object { $_.FullName -match "\\x64\\" -or $_.FullName -match "x64" } |
        Sort-Object FullName -Descending |
        Select-Object -First 1
    if ($preferred) { return $preferred.FullName }

    $fallback = $matches | Sort-Object FullName -Descending | Select-Object -First 1
    if ($fallback) { return $fallback.FullName }
    return $null
}

Refresh-ProcessPath

$signtool = Get-LatestSignTool
if (-not $signtool) {
    Write-Host "SIGNTOOL_MISSING: install Windows SDK SignTool 10.0.22621.755 or newer."
    exit 20
}

$dlib = Get-ArtifactSigningDlib
if (-not $dlib) {
    Write-Host "ARTIFACT_SIGNING_TOOLS_MISSING: install Microsoft.Azure.ArtifactSigningClientTools."
    exit 21
}

$az = Get-Command az -ErrorAction SilentlyContinue
if (-not $az) {
    Write-Host "AZURE_CLI_MISSING: install Azure CLI and sign in to the release tenant."
    exit 24
}

$previousErrorActionPreference = $ErrorActionPreference
$ErrorActionPreference = "Continue"
$accountJson = & $az.Source account show --output json 2>$null
$accountExitCode = $LASTEXITCODE
$ErrorActionPreference = $previousErrorActionPreference
if ($accountExitCode -ne 0 -or -not $accountJson) {
    Write-Host "AZURE_LOGIN_MISSING: sign in to tenant $TenantId."
    exit 25
}

$account = $accountJson | ConvertFrom-Json
if ($account.tenantId -ne $TenantId) {
    Write-Host "AZURE_TENANT_MISMATCH current=$($account.tenantId) expected=$TenantId"
    exit 26
}
if ($account.id -ne $SubscriptionId) {
    & $az.Source account set --subscription $SubscriptionId
    if ($LASTEXITCODE -ne 0) {
        Write-Host "AZURE_SUBSCRIPTION_MISMATCH current=$($account.id) expected=$SubscriptionId"
        exit 27
    }
}

Write-Host "TRUSTED_SIGNING_PREFLIGHT_OK"
Write-Host "signtool=$signtool"
Write-Host "dlib=$dlib"
Write-Host "endpoint=$Endpoint"
Write-Host "account=$CodeSigningAccountName"
Write-Host "profile=$CertificateProfileName"
Write-Host "azureAccount=$($account.user.name)"

if ($Preflight) { exit 0 }
if (-not $FilePath) {
    Write-Host "SIGN_TARGET_MISSING"
    exit 22
}

$target = Resolve-Path -LiteralPath $FilePath -ErrorAction Stop
$metadataDirectory = Join-Path $env:TEMP ("motsdits-trusted-signing-" + [guid]::NewGuid().ToString("N"))
$metadataPath = Join-Path $metadataDirectory "metadata.json"
New-Item -ItemType Directory -Path $metadataDirectory | Out-Null

try {
    $metadata = [ordered]@{
        Endpoint = $Endpoint
        CodeSigningAccountName = $CodeSigningAccountName
        CertificateProfileName = $CertificateProfileName
        ExcludeCredentials = @(
            "ManagedIdentityCredential",
            "WorkloadIdentityCredential",
            "SharedTokenCacheCredential",
            "VisualStudioCredential",
            "VisualStudioCodeCredential",
            "InteractiveBrowserCredential"
        )
    }
    $metadataJson = $metadata | ConvertTo-Json -Compress
    [System.IO.File]::WriteAllText($metadataPath, $metadataJson, (New-Object System.Text.UTF8Encoding($false)))

    Write-Host "TRUSTED_SIGNING_START target=$($target.Path)"
    & $signtool sign /v /fd SHA256 /tr $TimestampUrl /td SHA256 /dlib $dlib /dmdf $metadataPath $target.Path
    if ($LASTEXITCODE -ne 0) {
        Write-Host "TRUSTED_SIGNING_FAILED rc=$LASTEXITCODE"
        exit $LASTEXITCODE
    }

    $signature = Get-AuthenticodeSignature -FilePath $target.Path
    if ($signature.Status -ne "Valid") {
        Write-Host "AUTHENTICODE_INVALID status=$($signature.Status)"
        exit 23
    }
    Write-Host "AUTHENTICODE_VALID signer=$($signature.SignerCertificate.Subject)"
}
finally {
    Remove-Item -LiteralPath $metadataDirectory -Recurse -Force -ErrorAction SilentlyContinue
}
