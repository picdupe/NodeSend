param(
    [switch]$Check,
    [switch]$Debug,
    [switch]$Fast,
    [ValidateSet('aarch64', 'armv7', 'i686', 'x86_64')]
    [string]$Target = 'aarch64',
    [string]$JavaHome = 'D:\Program Files\Java\jdk-21.0.12.1'
)

$ErrorActionPreference = 'Stop'
$fastBuild = $Fast -or $Debug
$projectRoot = Split-Path $PSScriptRoot -Parent
$properties = Join-Path $projectRoot 'src-tauri/gen/android/local.properties'
if (Test-Path -LiteralPath $properties) {
    $sdkLine = Get-Content -LiteralPath $properties | Where-Object { $_ -match '^sdk\.dir=' } | Select-Object -First 1
}
if ($sdkLine) {
    $sdkRoot = $sdkLine.Substring('sdk.dir='.Length).Trim()
} else {
    $sdkRoot = if ($env:ANDROID_SDK_ROOT) { $env:ANDROID_SDK_ROOT } elseif ($env:ANDROID_HOME) { $env:ANDROID_HOME } else { 'D:\Users\picdupe\AppData\Local\Android\Sdk' }
    $sdkRoot = $sdkRoot -replace '\\', '/'
    Set-Content -LiteralPath $properties -Value "sdk.dir=$sdkRoot" -Encoding ASCII
    Write-Output "Created Android local.properties: sdk.dir=$sdkRoot"
}
$missing = @()
if (-not (Test-Path "$JavaHome/bin/java.exe")) { $missing += "Java: $JavaHome" }
if (Test-Path "$JavaHome/release") {
    $javaRelease = Get-Content -LiteralPath "$JavaHome/release"
    if (-not ($javaRelease -match '^JAVA_VERSION="21(?:\.|\")')) {
        $missing += "JDK 21 required: $JavaHome"
    }
}
if (-not (Test-Path "$sdkRoot/platforms/android-36/android.jar")) {
    $missing += 'Android SDK Platform 36'
}
if (-not (Test-Path "$sdkRoot/build-tools/35.0.0/aapt2.exe")) { $missing += 'Android SDK Build-Tools 35.0.0' }
$gradleConfig = Get-Content -Raw (Join-Path $projectRoot 'src-tauri/gen/android/app/build.gradle.kts')
if ($gradleConfig -match 'ndkVersion\s*=\s*"([\d.]+)"') {
    $ndkVersion = $Matches[1]
} else {
    $ndkVersion = (Get-ChildItem -LiteralPath (Join-Path $sdkRoot 'ndk') -Directory -ErrorAction SilentlyContinue |
        Sort-Object Name -Descending | Select-Object -First 1 -ExpandProperty Name)
    if (-not $ndkVersion) { throw 'No Android NDK installation found in the selected SDK.' }
    Write-Output "Using installed NDK: $ndkVersion"
}
$ndkRoot = Join-Path $sdkRoot "ndk/$ndkVersion"
foreach ($component in @('source.properties', 'toolchains/llvm/prebuilt/windows-x86_64/bin/clang.exe', 'toolchains/llvm/prebuilt/windows-x86_64/sysroot/usr/include/sys/cdefs.h')) {
    $file = Get-Item -LiteralPath (Join-Path $ndkRoot $component) -ErrorAction SilentlyContinue
    if (-not $file -or $file.Length -eq 0) { $missing += "NDK $ndkVersion : $component missing or empty" }
}
Write-Output "Android SDK: $sdkRoot"
Write-Output "Android API: 36; NDK: $ndkRoot; Java: $JavaHome"
if ($missing.Count) {
    throw ("Missing build prerequisites in the selected SDK:`n - " + ($missing -join "`n - "))
}
if ($Check) { Write-Output 'Android build prerequisites found.'; exit 0 }

# Scope the Android toolchain to this build; never inherit a retired NDK path.
$buildTemp = Join-Path $projectRoot 'src-tauri/target/android-build-tmp'
New-Item -ItemType Directory -Force -Path $buildTemp | Out-Null
# LLVM uses TMP/TEMP/TMPDIR for assembly intermediates. Keep all three in a
# writable project directory instead of inheriting an inaccessible user temp.
$tempProbe = Join-Path $buildTemp ([Guid]::NewGuid().ToString() + '.tmp')
try {
    [IO.File]::WriteAllText($tempProbe, 'NodeSend build temp check')
} finally {
    if (Test-Path -LiteralPath $tempProbe) { Remove-Item -LiteralPath $tempProbe -Force }
}
$environment = @{
    TMP = $buildTemp
    TEMP = $buildTemp
    TMPDIR = $buildTemp
    ANDROID_HOME = $sdkRoot
    ANDROID_SDK_ROOT = $sdkRoot
    ANDROID_NDK_HOME = $ndkRoot
    NDK_HOME = $ndkRoot
    NDK_ROOT = $ndkRoot
    JAVA_HOME = $JavaHome
    PATH = "$JavaHome\bin;$env:PATH"
}
$signingEnv = Join-Path $projectRoot 'signing/.env'
# Both variants must use the SAME signing identity to support replacement installs.
& {
    $signingKeys = @('NODESEND_KEYSTORE', 'NODESEND_KEY_ALIAS', 'NODESEND_STORE_PASSWORD', 'NODESEND_KEY_PASSWORD')
    foreach ($key in $signingKeys) {
        $environment[$key] = [Environment]::GetEnvironmentVariable($key, 'Process')
    }
    if (Test-Path -LiteralPath $signingEnv) {
        foreach ($line in Get-Content -LiteralPath $signingEnv -Encoding UTF8) {
            if ($line -match '^\s*(#|$)') { continue }
            if ($line -notmatch '^\s*(NODESEND_[A-Z_]+)\s*=(.*)$') { throw 'Invalid entry in signing/.env.' }
            $key = $Matches[1]
            $value = $Matches[2].Trim()
            if ($key -notin $signingKeys) { throw "Unsupported signing setting: $key" }
            if ($value.Length -ge 2 -and (($value.StartsWith('"') -and $value.EndsWith('"')) -or ($value.StartsWith("'") -and $value.EndsWith("'")))) {
                $value = $value.Substring(1, $value.Length - 2)
            }
            $environment[$key] = $value
        }
    }
    if (-not $environment.NODESEND_KEY_ALIAS) { $environment.NODESEND_KEY_ALIAS = 'nodesend' }
    if (-not $environment.NODESEND_KEY_PASSWORD) { $environment.NODESEND_KEY_PASSWORD = $environment.NODESEND_STORE_PASSWORD }
    foreach ($key in $signingKeys) {
        if (-not $environment[$key]) { throw "Fill $key in signing/.env before building either variant; debug signing would prevent upgrades." }
    }
    if (-not [IO.Path]::IsPathRooted($environment.NODESEND_KEYSTORE)) {
        $environment.NODESEND_KEYSTORE = Join-Path $projectRoot $environment.NODESEND_KEYSTORE
    }
    if (-not (Test-Path -LiteralPath $environment.NODESEND_KEYSTORE -PathType Leaf)) { throw 'Signing keystore not found; check signing/.env.' }
}
$previous = @{}
foreach ($key in $environment.Keys) {
    $previous[$key] = [Environment]::GetEnvironmentVariable($key, 'Process')
    [Environment]::SetEnvironmentVariable($key, $environment[$key], 'Process')
}
Push-Location $projectRoot
try {
    & (Join-Path $PSScriptRoot 'icons.ps1')
    & npm.cmd run build
    if ($LASTEXITCODE -ne 0) { throw 'Frontend build failed.' }
    $arguments = @('run', 'tauri', '--', 'android', 'build', '--target', $Target, '--config', 'src-tauri/tauri.android.package.json')
    if ($fastBuild) { $arguments += @('--debug', '--apk') }
    & npm.cmd @arguments
    if ($LASTEXITCODE -ne 0) { throw "Android build failed (exit $LASTEXITCODE)." }

    $variant = if ($fastBuild) { 'debug' } else { 'release' }
    Write-Output 'Android artifacts:'
    Get-ChildItem -LiteralPath (Join-Path $projectRoot 'src-tauri/gen/android/app/build/outputs') -Recurse -File -ErrorAction SilentlyContinue |
        Where-Object { $_.Extension -in '.apk', '.aab' -and $_.FullName -match "/$variant/|${variant}" } |
        Select-Object FullName, Length, LastWriteTime
} finally {
    Pop-Location
    foreach ($key in $previous.Keys) {
        [Environment]::SetEnvironmentVariable($key, $previous[$key], 'Process')
    }
}

