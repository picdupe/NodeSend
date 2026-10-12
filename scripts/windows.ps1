param(
    [switch]$Check,
    [switch]$Dev,
    [switch]$Fast,
    [ValidateSet('all', 'nsis', 'msi')]
    [string]$Bundle = 'all',
    [string]$WindowsSdk = '10.0.22621.0',
    [string]$UpdateDownloadBaseUrl = ''
)

$ErrorActionPreference = 'Stop'
$projectRoot = Split-Path $PSScriptRoot -Parent
$versionScript = Join-Path $PSScriptRoot 'version.py'
$python = Get-Command python -ErrorAction SilentlyContinue
if (-not $python) { throw 'Python 3 is required to synchronize the release version. Install Python and ensure python.exe is on PATH.' }
$versionAction = & $python.Source $versionScript sync
if ($LASTEXITCODE -ne 0) { throw 'Could not synchronize NodeSend version metadata.' }
Write-Output $versionAction
$msvcScript = Join-Path $PSScriptRoot 'msvc.cmd'

$vswhereCandidates = @(
    (Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio/Installer/vswhere.exe'),
    (Join-Path $env:ProgramFiles 'Microsoft Visual Studio/Installer/vswhere.exe'),
    'D:\Program Files (x86)\Microsoft Visual Studio\Installer\vswhere.exe'
) | Where-Object { $_ -and (Test-Path -LiteralPath $_) }
$vswhere = $vswhereCandidates | Select-Object -First 1
if (-not $vswhere) { throw 'vswhere.exe was not found. Install Visual Studio Installer first.' }

$vsInstall = (& $vswhere -latest -products * -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath | Select-Object -First 1).Trim()
if (-not $vsInstall) { throw 'Visual Studio C++ x64 build tools were not found.' }
$vcvars = Join-Path $vsInstall 'VC/Auxiliary/Build/vcvarsall.bat'
if (-not (Test-Path -LiteralPath $vcvars)) { throw "vcvarsall.bat was not found: $vcvars" }

$sdkRoot = if ($env:WindowsSdkDir) { $env:WindowsSdkDir.TrimEnd('\') } else { 'D:\Windows Kits\10' }
$rc = Join-Path $sdkRoot "bin/$WindowsSdk/x64/rc.exe"
if (-not (Test-Path -LiteralPath $rc)) {
    $rc = Get-ChildItem -LiteralPath (Join-Path $sdkRoot 'bin') -Filter rc.exe -Recurse -File -ErrorAction SilentlyContinue |
        Where-Object { $_.FullName -match '\\x64\\rc\.exe$' } |
        Sort-Object FullName -Descending |
        Select-Object -First 1 -ExpandProperty FullName
}
$missing = @()
if (-not (Test-Path -LiteralPath $rc)) { $missing += "Windows SDK rc.exe ($WindowsSdk)" }
if (-not (Test-Path -LiteralPath $msvcScript)) { $missing += "MSVC wrapper: $msvcScript" }
if ($missing.Count) { throw ("Missing Windows build prerequisites:`n - " + ($missing -join "`n - ")) }

Write-Output "Visual Studio: $vsInstall"
Write-Output "Windows SDK rc.exe: $rc"
Write-Output "Bundle: $Bundle"
if ($Check) {
    Write-Output 'Windows build prerequisites found.'
    exit 0
}

if (-not $Dev -and -not $Fast) {
    if (-not $env:TAURI_SIGNING_PRIVATE_KEY) { throw 'Run scripts/signing.ps1 first, or set TAURI_SIGNING_PRIVATE_KEY to the updater signing key outside the repository.' }
    $tauriConfig = Get-Content -LiteralPath (Join-Path $projectRoot 'src-tauri/tauri.conf.json') -Raw | ConvertFrom-Json
    $env:NODESEND_UPDATER_PUBLIC_KEY = [string]$tauriConfig.plugins.updater.pubkey
    if (-not $env:NODESEND_UPDATER_PUBLIC_KEY.Trim()) { throw 'The updater public key is missing from src-tauri/tauri.conf.json. Run scripts/signing.ps1 first.' }
}

# msvc.cmd imports vcvarsall's INCLUDE/LIB/PATH and explicitly sets RC for
# tauri-winres. Run the complete build in that same cmd.exe environment.
$env:NODESEND_VSWHERE = $vswhere
$env:NODESEND_WINDOWS_SDK = $WindowsSdk
$env:RC = $rc
$bundleArg = if ($Bundle -eq 'all') { 'nsis,msi' } else { $Bundle }
$profile = if ($Fast) { 'debug' } else { 'release' }
$profileDir = Join-Path $projectRoot "src-tauri/target/$profile"
# nodesend-cli 属于 cli feature，不应进入 GUI 安装包。修复前的 tauri-cli 在
# src/bin 扫描时会忽略 required-features（tauri-apps/tauri#15325），把上一次
# 构建残留的可执行文件重新打进安装包；打包前先清掉，让产物只可能来自本次构建。
if (-not $Dev) {
    Get-ChildItem -LiteralPath $profileDir -Filter 'nodesend-cli*' -File -ErrorAction SilentlyContinue | Remove-Item -Force
}
$command = if ($Dev) { "`"$msvcScript`" npm.cmd run tauri -- dev" } else { "`"$msvcScript`" npm.cmd run tauri -- build --bundles $bundleArg" }
if ($Fast -and -not $Dev) { $command += ' --debug --config src-tauri/tauri.windows.fast.json' }
Push-Location $projectRoot
try {
    & cmd.exe /d /s /c $command
    if ($LASTEXITCODE -ne 0) { throw "Windows command failed (exit $LASTEXITCODE)." }
} finally {
    Pop-Location
}

if ($Dev) { exit 0 }
$strayCli = @(Get-ChildItem -LiteralPath $profileDir -Filter 'nodesend-cli*' -File -ErrorAction SilentlyContinue)
if ($strayCli.Count) {
    throw ("nodesend-cli was produced while packaging the GUI; it must stay behind the cli feature:`n - " + (($strayCli | Select-Object -ExpandProperty FullName) -join "`n - "))
}
$bundleRoot = Join-Path $profileDir 'bundle'
$bundleKinds = if ($Bundle -eq 'all') { @('nsis', 'msi') } else { @($Bundle) }
$artifacts = @(foreach ($kind in $bundleKinds) {
    $extension = if ($kind -eq 'nsis') { '*.exe' } else { '*.msi' }
    Get-ChildItem -LiteralPath (Join-Path $bundleRoot $kind) -Filter $extension -File -ErrorAction Stop
})
# Keep Tauri's semver (0.1.1-build.80) for internal package metadata, but use
# the public release label (v0.1.1.80) in distributable artifact filenames.
$versionData = Get-Content -LiteralPath (Join-Path $projectRoot 'version.json') -Raw | ConvertFrom-Json
$publicVersion = "v$($versionData.version).$($versionData.build)"
$internalVersion = "$($versionData.version)-build.$($versionData.build)"
foreach ($artifact in $artifacts) {
    if ($artifact.Name.Contains($internalVersion)) {
        $newName = $artifact.Name.Replace($internalVersion, $publicVersion)
        $newPath = Join-Path $artifact.DirectoryName $newName
        $signature = "$($artifact.FullName).sig"

        # Repeated builds may leave the same public-version filename behind.
        # Remove the stale destination first; Rename-Item cannot overwrite it.
        if (Test-Path -LiteralPath $newPath) {
            Remove-Item -LiteralPath $newPath -Force
        }
        if (Test-Path -LiteralPath "$newPath.sig") {
            Remove-Item -LiteralPath "$newPath.sig" -Force
        }

        Rename-Item -LiteralPath $artifact.FullName -NewName $newName
        if (Test-Path -LiteralPath $signature) {
            Rename-Item -LiteralPath $signature -NewName "$newName.sig"
        }
    }
}
# Keep only the artifacts for the version built in this run. Older public-version
# filenames are not renamed above, so remove them and their signatures explicitly.
foreach ($kind in $bundleKinds) {
    $extension = if ($kind -eq 'nsis') { '*.exe' } else { '*.msi' }
    $kindDirectory = Join-Path $bundleRoot $kind
    Get-ChildItem -LiteralPath $kindDirectory -Filter $extension -File -ErrorAction SilentlyContinue |
        Where-Object { $_.Name -like 'NodeSend_*' -and $_.Name -notlike "*${publicVersion}*" } |
        ForEach-Object {
            $staleSignature = "$($_.FullName).sig"
            Remove-Item -LiteralPath $_.FullName -Force
            if (Test-Path -LiteralPath $staleSignature -PathType Leaf) {
                Remove-Item -LiteralPath $staleSignature -Force
            }
            Write-Output "Removed stale bundle: $($_.Name)"
        }
}
$artifacts = @(foreach ($kind in $bundleKinds) {
    $extension = if ($kind -eq 'nsis') { '*.exe' } else { '*.msi' }
    Get-ChildItem -LiteralPath (Join-Path $bundleRoot $kind) -Filter $extension -File -ErrorAction Stop
})
# Build outputs can inherit a Low mandatory label from the workspace. NSIS
# then runs at Low integrity and cannot create files in a normal user TEMP.
# Normalize only installer artifacts; never relabel the workspace or TEMP.
foreach ($artifact in $artifacts) {
    if ($artifact.Extension -ne '.exe') { continue }
    & icacls.exe $artifact.FullName /setintegritylevel M
    if ($LASTEXITCODE -ne 0) {
        throw "Installer was built, but its integrity label could not be set to Medium: $($artifact.FullName). Run the build from a normal user terminal."
    }
}
Write-Output 'Windows bundles:'
$artifacts | Select-Object FullName, Length, LastWriteTime

# Generate the stable updater manifest from the signed NSIS installer. The
# manifest URL itself is served from GitHub raw (see update-sources.json),
# while the installer is downloaded from the GitHub Release by default.
# Override the download host with -UpdateDownloadBaseUrl or
# NODESEND_UPDATE_DOWNLOAD_BASE_URL for an intranet/CDN mirror.
if (-not $Fast -and ($bundleKinds -contains 'nsis')) {
    $installerName = "NodeSend_$($publicVersion)_x64-setup.exe"
    $installer = @($artifacts | Where-Object { $_.Extension -eq '.exe' -and $_.Name -eq $installerName })
    if ($installer.Count -gt 0) {
        $installer = $installer[0]
        $signaturePath = "$($installer.FullName).sig"
        if (-not (Test-Path -LiteralPath $signaturePath -PathType Leaf)) {
            throw "Cannot generate updater manifest: installer signature was not found: $signaturePath"
        }
        $signatureText = (Get-Content -LiteralPath $signaturePath -Raw).Trim()
        if (-not $signatureText) { throw "Cannot generate updater manifest: installer signature is empty: $signaturePath" }

        $downloadBase = if ($UpdateDownloadBaseUrl.Trim()) {
            $UpdateDownloadBaseUrl.Trim().TrimEnd('/')
        } elseif ($env:NODESEND_UPDATE_DOWNLOAD_BASE_URL -and $env:NODESEND_UPDATE_DOWNLOAD_BASE_URL.Trim()) {
            $env:NODESEND_UPDATE_DOWNLOAD_BASE_URL.Trim().TrimEnd('/')
        } else {
            "https://github.com/picdupe/NodeSend/releases/download/v$($versionData.version)"
        }
        $downloadUri = $null
        if (-not [Uri]::TryCreate("$downloadBase/$($installer.Name)", [UriKind]::Absolute, [ref]$downloadUri) -or $downloadUri.Scheme -notin @('http', 'https')) {
            throw 'Updater download URL must be a valid HTTP or HTTPS URL. Set -UpdateDownloadBaseUrl or NODESEND_UPDATE_DOWNLOAD_BASE_URL.'
        }

        $manifestPath = Join-Path $projectRoot 'updates/stable/latest.json'
        $manifestDirectory = Split-Path -Parent $manifestPath
        New-Item -ItemType Directory -Path $manifestDirectory -Force | Out-Null

        # Keep platform entries produced by other OS build scripts when they
        # target the exact same release version/build. A different version
        # starts a clean manifest so stale platform artifacts are not retained.
        $platforms = [ordered]@{}
        if (Test-Path -LiteralPath $manifestPath -PathType Leaf) {
            try {
                $existingManifest = Get-Content -LiteralPath $manifestPath -Raw | ConvertFrom-Json
                if ([string]$existingManifest.version -eq $internalVersion -and $null -ne $existingManifest.platforms) {
                    foreach ($property in $existingManifest.platforms.PSObject.Properties) {
                        $platforms[$property.Name] = $property.Value
                    }
                    Write-Output "Merging updater manifest for existing version/build: $internalVersion"
                }
            } catch {
                throw "Cannot merge existing updater manifest '$manifestPath'. Fix or remove the invalid JSON before building. $($_.Exception.Message)"
            }
        }
        $platforms['windows-x86_64'] = [ordered]@{
            signature = $signatureText
            url = $downloadUri.AbsoluteUri
        }
        $manifest = [ordered]@{
            version = $internalVersion
            notes = "优化机械盘传输及大文件夹传输"
            platforms = $platforms
        }
        $manifest | ConvertTo-Json -Depth 8 | Set-Content -LiteralPath $manifestPath -Encoding utf8
        Write-Output "Updater manifest generated: $manifestPath"
        Write-Output "Installer download URL: $($downloadUri.AbsoluteUri)"
    } else {
        Write-Output 'Updater manifest skipped: no renamed NSIS installer was found.'
    }
}
