param(
    [string]$Output = (Join-Path ([Environment]::GetFolderPath("Desktop")) "MotsDits-Windows-release")
)

$ErrorActionPreference = "Stop"
$RepoRoot = Split-Path -Parent $PSScriptRoot
$TargetDir = "C:\t"
$Target = "x86_64-pc-windows-msvc"
$expectedSigner = "CN=9570-0720"
$ProgramFilesX86 = ${env:ProgramFiles(x86)}
$VcvarsCandidates = @(
    (Join-Path $ProgramFilesX86 "Microsoft Visual Studio\2022\BuildTools\VC\Auxiliary\Build\vcvars64.bat"),
    (Join-Path $ProgramFilesX86 "Microsoft Visual Studio\2022\Community\VC\Auxiliary\Build\vcvars64.bat"),
    (Join-Path $ProgramFilesX86 "Microsoft Visual Studio\2022\Professional\VC\Auxiliary\Build\vcvars64.bat"),
    (Join-Path $ProgramFilesX86 "Microsoft Visual Studio\2022\Enterprise\VC\Auxiliary\Build\vcvars64.bat")
)
$Vcvars = $VcvarsCandidates | Where-Object { Test-Path -LiteralPath $_ } | Select-Object -First 1
if (-not $Vcvars) { throw "Visual Studio vcvars64.bat is missing" }

function Get-AppVersion {
    $packageVersion = (Get-Content -Raw -LiteralPath (Join-Path $RepoRoot "package.json") | ConvertFrom-Json).version
    $tauriVersion = (Get-Content -Raw -LiteralPath (Join-Path $RepoRoot "src-tauri\tauri.conf.json") | ConvertFrom-Json).version
    $cargoToml = Get-Content -Raw -LiteralPath (Join-Path $RepoRoot "src-tauri\Cargo.toml")
    $cargoMatch = [regex]::Match($cargoToml, '(?m)^version\s*=\s*"([^"]+)"')
    if (-not $cargoMatch.Success) { throw "Unable to read the Cargo package version" }
    if ($packageVersion -ne $tauriVersion -or $packageVersion -ne $cargoMatch.Groups[1].Value) {
        throw "MotsDits package, Tauri, and Cargo versions must match"
    }
    return $packageVersion
}

function Assert-TrustedSignature {
    param([Parameter(Mandatory = $true)][string]$Path)

    $signature = Get-AuthenticodeSignature -FilePath $Path
    if ($signature.Status -ne "Valid") {
        throw "Authenticode status is $($signature.Status): $Path"
    }
    if (-not $signature.SignerCertificate.Subject.StartsWith($expectedSigner, [System.StringComparison]::Ordinal)) {
        throw "Unexpected Authenticode signer: $($signature.SignerCertificate.Subject)"
    }
    if (-not $signature.TimeStamperCertificate) {
        throw "Authenticode timestamp is missing: $Path"
    }
}

if ($env:OS -ne "Windows_NT") { throw "Windows release builds require Windows" }
$existingOutput = Get-Item -LiteralPath $Output -Force -ErrorAction SilentlyContinue
if ($null -ne $existingOutput) { throw "Release output already exists: $Output" }

Set-Location $RepoRoot
$version = Get-AppVersion
$sourceCommit = (& git rev-parse HEAD).Trim()
if ($LASTEXITCODE -ne 0 -or $sourceCommit -notmatch '^[0-9a-f]{40}$') { throw "Unable to resolve source commit" }
$allowedReleaseFiles = @(
    "?? scripts/build-macos-release.sh",
    "?? scripts/build-windows-release.ps1",
    "?? scripts/windows-trusted-sign.ps1"
)
$unexpectedChanges = @(& git status --porcelain) | Where-Object { $_ -notin $allowedReleaseFiles }
if ($unexpectedChanges) {
    throw "The MotsDits source tree contains changes outside the release tooling: $($unexpectedChanges -join ', ')"
}

& powershell.exe -NoProfile -ExecutionPolicy Bypass -File (Join-Path $PSScriptRoot "windows-trusted-sign.ps1") -Preflight
if ($LASTEXITCODE -ne 0) { throw "Windows Trusted Signing preflight failed" }

$signerScript = (Join-Path $PSScriptRoot "windows-trusted-sign.ps1").Replace('\', '/')
$powerShellExe = (Join-Path $env:SystemRoot "System32\WindowsPowerShell\v1.0\powershell.exe").Replace('\', '/')
$releaseConfig = Join-Path $env:TEMP ("motsdits-windows-release-" + [guid]::NewGuid().ToString("N") + ".json")
$cmdFile = $null
$config = [ordered]@{
    bundle = [ordered]@{
        targets = @("nsis", "msi")
        windows = [ordered]@{
            signCommand = [ordered]@{
                cmd = $powerShellExe
                args = @("-NoProfile", "-ExecutionPolicy", "Bypass", "-File", $signerScript, "%1")
            }
        }
    }
}
[System.IO.File]::WriteAllText($releaseConfig, ($config | ConvertTo-Json -Depth 16), (New-Object System.Text.UTF8Encoding($false)))

$staging = Join-Path ([System.IO.Path]::GetDirectoryName($Output)) (".motsdits-windows-release-" + [guid]::NewGuid().ToString("N"))
try {
    & powershell.exe -NoProfile -ExecutionPolicy Bypass -File (Join-Path $PSScriptRoot "setup-onnxruntime-windows.ps1") -Arch x64
    if ($LASTEXITCODE -ne 0) { throw "ONNX Runtime setup failed" }

    $releaseRoot = Join-Path $TargetDir "release"
    $existingNsis = Get-ChildItem -LiteralPath (Join-Path $releaseRoot "bundle\nsis") -Filter "MotsDits_${version}_x64-setup.exe" -File -ErrorAction SilentlyContinue
    $existingMsi = Get-ChildItem -LiteralPath (Join-Path $releaseRoot "bundle\msi") -Filter "MotsDits_${version}_x64_en-US.msi" -File -ErrorAction SilentlyContinue

    if ($existingNsis.Count -eq 1 -and $existingMsi.Count -eq 1) {
        Write-Host "Reusing completed signed Windows bundles from $releaseRoot"
    }
    else {
        Remove-Item -LiteralPath (Join-Path $releaseRoot "motsdits.exe") -Force -ErrorAction SilentlyContinue
        Remove-Item -LiteralPath (Join-Path $releaseRoot "bundle") -Recurse -Force -ErrorAction SilentlyContinue
        Remove-Item -LiteralPath (Join-Path $releaseRoot "build") -Recurse -Force -ErrorAction SilentlyContinue

        $cmdFile = Join-Path $env:TEMP ("motsdits-windows-release-" + [guid]::NewGuid().ToString("N") + ".cmd")
        $escapedConfig = $releaseConfig.Replace("%", "%%")
        $cmdBody = @"
@echo off
call "$Vcvars"
if errorlevel 1 exit /b %errorlevel%
set "TMP=$env:TEMP"
set "TEMP=$env:TEMP"
set "CMAKE_POLICY_VERSION_MINIMUM=3.5"
set "CARGO_TARGET_DIR=$TargetDir"
cd /d "$RepoRoot"
bunx tauri build --bundles nsis,msi --config "$escapedConfig"
"@
        Set-Content -LiteralPath $cmdFile -Value $cmdBody -Encoding ASCII
        & cmd.exe /d /c $cmdFile
        if ($LASTEXITCODE -ne 0) { throw "Windows Tauri release build failed" }
    }

    $bundleRoot = Join-Path $releaseRoot "bundle"
    $nsis = Get-ChildItem -LiteralPath (Join-Path $bundleRoot "nsis") -Filter "MotsDits_${version}_x64-setup.exe" -File
    $msi = Get-ChildItem -LiteralPath (Join-Path $bundleRoot "msi") -Filter "MotsDits_${version}_x64_en-US.msi" -File
    if ($nsis.Count -ne 1 -or $msi.Count -ne 1) {
        throw "Expected Windows release artifacts are incomplete"
    }

    Assert-TrustedSignature -Path $nsis[0].FullName
    Assert-TrustedSignature -Path $msi[0].FullName

    $sevenZip = Get-Command 7z.exe -ErrorAction SilentlyContinue
    if (-not $sevenZip) {
        $sevenZipPath = Join-Path $env:ProgramFiles "7-Zip\7z.exe"
        if (Test-Path -LiteralPath $sevenZipPath) { $sevenZip = Get-Item -LiteralPath $sevenZipPath }
    }
    if (-not $sevenZip) { throw "7-Zip is required to verify the application embedded in the NSIS installer" }
    $inspectionDirectory = Join-Path $env:TEMP ("motsdits-installer-inspection-" + [guid]::NewGuid().ToString("N"))
    New-Item -ItemType Directory -Path $inspectionDirectory | Out-Null
    try {
        & $sevenZip.FullName e -y "-o$inspectionDirectory" $nsis[0].FullName "motsdits.exe" | Out-Null
        if ($LASTEXITCODE -ne 0) { throw "Unable to extract motsdits.exe from the NSIS installer" }
        $embeddedApplication = Join-Path $inspectionDirectory "motsdits.exe"
        if (-not (Test-Path -LiteralPath $embeddedApplication -PathType Leaf)) {
            throw "motsdits.exe is missing from the NSIS installer"
        }
        Assert-TrustedSignature -Path $embeddedApplication

        New-Item -ItemType Directory -Path $staging | Out-Null
        Copy-Item -LiteralPath $nsis[0].FullName -Destination (Join-Path $staging "MotsDits-Windows-Setup.exe")
        Copy-Item -LiteralPath $msi[0].FullName -Destination (Join-Path $staging "MotsDits-Windows-x64.msi")
        Copy-Item -LiteralPath $embeddedApplication -Destination (Join-Path $staging "motsdits.exe")
    }
    finally {
        Remove-Item -LiteralPath $inspectionDirectory -Recurse -Force -ErrorAction SilentlyContinue
    }

    $report = [ordered]@{
        productName = "MotsDits"
        version = $version
        platform = "windows"
        architecture = "x64"
        sourceCommit = $sourceCommit
        signer = (Get-AuthenticodeSignature -FilePath $nsis[0].FullName).SignerCertificate.Subject
        artifacts = @(
            Get-ChildItem -LiteralPath $staging -File | Sort-Object Name | ForEach-Object {
                [ordered]@{
                    name = $_.Name
                    bytes = $_.Length
                    sha256 = (Get-FileHash -Algorithm SHA256 -LiteralPath $_.FullName).Hash.ToLowerInvariant()
                }
            }
        )
    }
    [System.IO.File]::WriteAllText(
        (Join-Path $staging "MotsDits-Windows-verification.json"),
        ($report | ConvertTo-Json -Depth 8) + [Environment]::NewLine,
        (New-Object System.Text.UTF8Encoding($false))
    )
    Move-Item -LiteralPath $staging -Destination $Output
    $staging = $null
    Write-Host "Signed MotsDits $version Windows release staged at $Output"
}
finally {
    Remove-Item Env:CARGO_TARGET_DIR -ErrorAction SilentlyContinue
    Remove-Item -LiteralPath $releaseConfig -Force -ErrorAction SilentlyContinue
    if ($cmdFile) { Remove-Item -LiteralPath $cmdFile -Force -ErrorAction SilentlyContinue }
    if ($staging) { Remove-Item -LiteralPath $staging -Recurse -Force -ErrorAction SilentlyContinue }
}
