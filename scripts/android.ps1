param(
    [switch]$Check,
    [switch]$Debug,
    [switch]$Fast,
    [Alias('Target')]
    [ValidateSet('arm64-v8a', 'armeabi-v7a', 'x86_64', 'universal')]
    [string]$Architecture = '',
    [string]$JavaHome = ''
)

$ErrorActionPreference = 'Stop'
$fastBuild = $Fast -or $Debug
$architectureMap = [ordered]@{
    'arm64-v8a' = 'aarch64'
    'armeabi-v7a' = 'armv7'
    'x86_64' = 'x86_64'
    'universal' = ''
}
$architectures = if ($Architecture) { @($Architecture) } else { @($architectureMap.Keys) }
$projectRoot = Split-Path $PSScriptRoot -Parent
$versionScript = Join-Path $PSScriptRoot 'version.py'
$python = Get-Command python -ErrorAction SilentlyContinue
if (-not $python) { throw 'Python 3 is required to synchronize the release version. Install Python and ensure python.exe is on PATH.' }
$versionAction = & $python.Source $versionScript sync
if ($LASTEXITCODE -ne 0) { throw 'Could not synchronize NodeSend version metadata.' }
Write-Output $versionAction
$properties = Join-Path $projectRoot 'src-tauri/gen/android/local.properties'
$sdkRoot = $null
if (Test-Path -LiteralPath $properties) {
    $sdkLine = Get-Content -LiteralPath $properties | Where-Object { $_ -match '^sdk\.dir=' } | Select-Object -First 1
    if ($sdkLine) { $sdkRoot = $sdkLine.Substring('sdk.dir='.Length).Trim() }
}
if (-not $sdkRoot) {
    $sdkRoot = if ($env:ANDROID_SDK_ROOT) { $env:ANDROID_SDK_ROOT } elseif ($env:ANDROID_HOME) { $env:ANDROID_HOME } else { $null }
    if (-not $sdkRoot) { throw 'Android SDK path is not configured. Set ANDROID_SDK_ROOT or ANDROID_HOME, or provide sdk.dir in src-tauri/gen/android/local.properties.' }
    $sdkRoot = $sdkRoot -replace '\\', '/'
    New-Item -ItemType Directory -Force -Path (Split-Path -Parent $properties) | Out-Null
    Set-Content -LiteralPath $properties -Value "sdk.dir=$sdkRoot" -Encoding ASCII
    Write-Output "Created Android local.properties: sdk.dir=$sdkRoot"
}
$sdkRoot = (Resolve-Path -LiteralPath $sdkRoot).Path
$JavaHome = if ($JavaHome) { $JavaHome } elseif ($env:JAVA_HOME) { $env:JAVA_HOME } else {
    $javaCommand = Get-Command java.exe -ErrorAction SilentlyContinue | Select-Object -First 1
    if ($javaCommand) { Split-Path (Split-Path $javaCommand.Source -Parent) -Parent } else { $null }
}
if (-not $JavaHome) { throw 'JDK 21 was not found. Set JAVA_HOME or add the JDK bin directory to PATH.' }
$JavaHome = (Resolve-Path -LiteralPath $JavaHome).Path
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
    $variant = if ($fastBuild) { 'debug' } else { 'release' }
    $outputRoot = Join-Path $projectRoot 'src-tauri/gen/android/app/build/outputs'
    $releaseRoot = Join-Path $projectRoot 'dist/Android'
    # Windows paths are case-insensitive, so New-Item -Force does not correct
    # the on-disk casing of an existing `dist/android` directory. Rename it
    # through a temporary sibling to ensure the published directory is `Android`.
    $distRoot = Join-Path $projectRoot 'dist'
    $existingReleaseRoot = Get-ChildItem -LiteralPath $distRoot -Directory -ErrorAction SilentlyContinue |
        Where-Object { $_.Name -ieq 'android' } | Select-Object -First 1
    if ($existingReleaseRoot -and $existingReleaseRoot.Name -cne 'Android') {
        $temporaryReleaseRoot = Join-Path $distRoot ('.android-case-' + [Guid]::NewGuid().ToString('N'))
        Move-Item -LiteralPath $existingReleaseRoot.FullName -Destination $temporaryReleaseRoot
        Move-Item -LiteralPath $temporaryReleaseRoot -Destination $releaseRoot
    }
    New-Item -ItemType Directory -Force -Path $releaseRoot | Out-Null
    $builtApks = @{}
    foreach ($architectureName in $architectures) {
        $rustTarget = $architectureMap[$architectureName]
        $arguments = @('run', 'tauri', '--', 'android', 'build', '--config', 'src-tauri/tauri.android.package.json')
        if ($rustTarget) {
            $arguments += @('--target', $rustTarget, '--split-per-abi')
        }
        if ($fastBuild) { $arguments += @('--debug', '--apk') }
        Write-Output "Building Android architecture: $architectureName"
        & npm.cmd @arguments
        if ($LASTEXITCODE -ne 0) { throw "Android build failed for '$architectureName' (exit $LASTEXITCODE)." }

        $apkFiles = @(Get-ChildItem -LiteralPath $outputRoot -Recurse -File -Filter '*.apk' -ErrorAction SilentlyContinue |
            Where-Object { $_.FullName -match "(?i)[/\\]$variant[/\\]" -and $_.Name -notlike 'NodeSend_*' })
        if ($architectureName -eq 'universal') {
            $candidates = @($apkFiles | Where-Object { $_.FullName -match '(?i)[/\\]apk[/\\]universal[/\\]' })
        } else {
            $pathPattern = switch ($architectureName) {
                'arm64-v8a' { '(?i)[/\\](arm64-v8a|aarch64|arm64)[/\\]' }
                'armeabi-v7a' { '(?i)[/\\](armeabi-v7a|armv7|arm)[/\\]' }
                'x86_64' { '(?i)[/\\]x86_64[/\\]' }
            }
            $candidates = @($apkFiles | Where-Object { $_.FullName -match $pathPattern })
        }
        if ($candidates.Count -ne 1) {
            throw "Expected exactly one $variant APK for '$architectureName', found $($candidates.Count). Inspect $outputRoot."
        }

        Add-Type -AssemblyName System.IO.Compression.FileSystem
        $archive = [System.IO.Compression.ZipFile]::OpenRead($candidates[0].FullName)
        try {
            $nativeAbis = @($archive.Entries | ForEach-Object {
                if ($_.FullName -match '^lib/([^/]+)/[^/]+\.so$') { $Matches[1] }
            } | Sort-Object -Unique)
        } finally {
            $archive.Dispose()
        }
        $expectedAbi = switch ($architectureName) {
            'arm64-v8a' { 'arm64-v8a' }
            'armeabi-v7a' { 'armeabi-v7a' }
            'x86_64' { 'x86_64' }
        }
        if ($architectureName -eq 'universal') {
            $requiredAbis = @('arm64-v8a', 'armeabi-v7a', 'x86_64')
            $missingAbis = @($requiredAbis | Where-Object { $_ -notin $nativeAbis })
            if ($missingAbis.Count -gt 0) {
                throw "The APK in the universal output directory is not a verified universal package; missing native ABIs: $($missingAbis -join ', '). Found: $($nativeAbis -join ', ')."
            }
        } elseif ($expectedAbi -notin $nativeAbis) {
            throw "APK architecture mismatch for '$architectureName'. Expected ABI '$expectedAbi'; found: $($nativeAbis -join ', ')."
        }

        $versionData = Get-Content -LiteralPath (Join-Path $projectRoot 'version.json') -Raw | ConvertFrom-Json
        $publicVersion = "v$($versionData.version).$($versionData.build)"
        $suffix = switch ($architectureName) {
            'arm64-v8a' { '-arm64-v8a' }
            'armeabi-v7a' { '-armeabi-v7a' }
            'x86_64' { '-x86_64' }
            'universal' { '' }
        }
        $apkName = if ($fastBuild) { "NodeSend_$($publicVersion)$suffix-debug.apk" } else { "NodeSend_$($publicVersion)$suffix-release.apk" }
        $publishedApk = Join-Path $releaseRoot $apkName
        Copy-Item -LiteralPath $candidates[0].FullName -Destination $publishedApk -Force
        $builtApks[$architectureName] = $publishedApk
        Write-Output "Android APK ($architectureName): $publishedApk"
    }

    # Update the legacy Android updater entry with the ARM64 build when it is
    # part of this run. Never point it at a universal APK merely because its
    # output directory happens to be named universal.
    if (-not $fastBuild -and $builtApks.ContainsKey('arm64-v8a')) {
        $versionData = Get-Content -LiteralPath (Join-Path $projectRoot 'version.json') -Raw | ConvertFrom-Json
        $downloadBase = if ($env:NODESEND_UPDATE_DOWNLOAD_BASE_URL -and $env:NODESEND_UPDATE_DOWNLOAD_BASE_URL.Trim()) {
            $env:NODESEND_UPDATE_DOWNLOAD_BASE_URL.Trim().TrimEnd('/')
        } else {
            "https://github.com/picdupe/NodeSend/releases/download/v$($versionData.version)"
        }
        $downloadUri = $null
        $arm64Name = Split-Path -Leaf $builtApks['arm64-v8a']
        if (-not [Uri]::TryCreate("$downloadBase/$arm64Name", [UriKind]::Absolute, [ref]$downloadUri) -or $downloadUri.Scheme -ne 'https') {
            throw 'Android updater download URL must be HTTPS. Set NODESEND_UPDATE_DOWNLOAD_BASE_URL to a valid HTTPS base URL.'
        }
        $python = Get-Command python -ErrorAction SilentlyContinue
        if (-not $python) { throw 'Python 3 is required to generate Android update metadata.' }
        & $python.Source (Join-Path $PSScriptRoot 'android-update-manifest.py') --channel stable --apk $builtApks['arm64-v8a'] --url $downloadUri.AbsoluteUri
        if ($LASTEXITCODE -ne 0) { throw 'Could not merge Android APK metadata into updates/stable/latest.json.' }
        Write-Output "Android updater download URL: $($downloadUri.AbsoluteUri)"
    }
} finally {
    Pop-Location
    foreach ($key in $previous.Keys) {
        [Environment]::SetEnvironmentVariable($key, $previous[$key], 'Process')
    }
}

