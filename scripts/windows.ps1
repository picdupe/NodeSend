param(
    [switch]$Check,
    [switch]$Dev,
    [switch]$Fast,
    [ValidateSet('all', 'nsis', 'msi')]
    [string]$Bundle = 'all',
    [string]$WindowsSdk = '10.0.22621.0'
)

$ErrorActionPreference = 'Stop'
$projectRoot = Split-Path $PSScriptRoot -Parent
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

# msvc.cmd imports vcvarsall's INCLUDE/LIB/PATH and explicitly sets RC for
# tauri-winres. Run the complete build in that same cmd.exe environment.
$env:NODESEND_VSWHERE = $vswhere
$env:NODESEND_WINDOWS_SDK = $WindowsSdk
$env:RC = $rc
$bundleArg = if ($Bundle -eq 'all') { 'nsis,msi' } else { $Bundle }
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
$profile = if ($Fast) { 'debug' } else { 'release' }
$bundleRoot = Join-Path $projectRoot "src-tauri/target/$profile/bundle"
$bundleKinds = if ($Bundle -eq 'all') { @('nsis', 'msi') } else { @($Bundle) }
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
