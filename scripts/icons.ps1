param([switch]$Force)
$ErrorActionPreference = 'Stop'
$projectRoot = Split-Path $PSScriptRoot -Parent
function Get-IconHash([string]$Path) {
    $hasher = [Security.Cryptography.SHA256]::Create()
    $stream = [IO.File]::OpenRead($Path)
    try { return [BitConverter]::ToString($hasher.ComputeHash($stream)).Replace('-', '') }
    finally { $stream.Dispose(); $hasher.Dispose() }
}
# Avoid touching every Android resource and invalidating Gradle on each build.
$stamp = Join-Path $projectRoot 'src-tauri/icons/.build-fingerprint.json'
$inputs = @('scripts/icons.ps1', 'package-lock.json', 'src-tauri/icons/logo.svg', 'src-tauri/icons/mobile-logo.svg')
$fingerprint = ($inputs | ForEach-Object { Get-IconHash (Join-Path $projectRoot $_) }) -join ':'
if (-not $Force -and (Test-Path -LiteralPath $stamp)) {
    try {
        $cached = Get-Content -LiteralPath $stamp -Raw | ConvertFrom-Json
        $valid = $cached.input -eq $fingerprint -and @($cached.outputs).Count -gt 0
        foreach ($output in $cached.outputs) {
            $file = Join-Path $projectRoot $output.path
            if (-not (Test-Path -LiteralPath $file -PathType Leaf) -or (Get-IconHash $file) -ne $output.hash) { $valid = $false; break }
        }
        if ($valid) { Write-Output 'Icons unchanged; reusing generated resources.'; return }
    } catch { Write-Verbose 'Icon cache unavailable; regenerating.' }
}
Push-Location $projectRoot
try {
    & npm.cmd run tauri -- icon src-tauri/icons/logo.svg --output src-tauri/icons
    if ($LASTEXITCODE -ne 0) { throw 'Icon generation failed.' }
    # Generate Android separately with the reduced effective mark; desktop
    # resources above continue to use the full-size logo.
    $mobileOutput = Join-Path $projectRoot 'src-tauri/mobile-icons'
    if (Test-Path $mobileOutput) { Remove-Item $mobileOutput -Recurse -Force }
    New-Item -ItemType Directory -Force -Path (Join-Path $mobileOutput 'android') | Out-Null
    & npm.cmd run tauri -- icon src-tauri/icons/mobile-logo.svg --output $mobileOutput
    if ($LASTEXITCODE -ne 0) { throw 'Mobile icon generation failed.' }
    # Tauri writes Android icons directly into the initialized Android project.
    # Mirror those generated resources back, never overwrite them with old copies.
    $mobileResources = Join-Path $mobileOutput 'android'
    $resources = Join-Path $projectRoot 'src-tauri/gen/android/app/src/main/res'
    $mirror = Join-Path $projectRoot 'src-tauri/icons/android'
    Get-ChildItem -LiteralPath $mobileResources -Recurse -File |
        Where-Object { $_.Name -match '^ic_launcher.*\.(png|xml)$' } |
        ForEach-Object {
            $relative = $_.FullName.Substring($mobileResources.Length + 1)
            $androidDestination = Join-Path $resources $relative
            New-Item -ItemType Directory -Force -Path (Split-Path $androidDestination -Parent) | Out-Null
            Copy-Item -LiteralPath $_.FullName -Destination $androidDestination -Force
            $destination = Join-Path $mirror $relative
            New-Item -ItemType Directory -Force -Path (Split-Path $destination -Parent) | Out-Null
            Copy-Item -LiteralPath $_.FullName -Destination $destination -Force
            if ([Convert]::ToBase64String([IO.File]::ReadAllBytes($_.FullName)) -ne [Convert]::ToBase64String([IO.File]::ReadAllBytes($destination))) {
                throw "Icon mismatch: $relative"
            }
        }
    $outputRoots = @('src-tauri/icons', 'src-tauri/gen/android/app/src/main/res')
    $outputs = @(foreach ($root in $outputRoots) {
        Get-ChildItem -LiteralPath (Join-Path $projectRoot $root) -Recurse -File |
            Where-Object { $_.Extension -in @('.png', '.ico', '.icns') -or $_.Name -match '^ic_launcher.*\.xml$' } |
            ForEach-Object { @{ path = $_.FullName.Substring($projectRoot.Length + 1); hash = (Get-IconHash $_.FullName) } }
    })
    @{ input = $fingerprint; outputs = $outputs } | ConvertTo-Json -Depth 4 | Set-Content -LiteralPath $stamp -Encoding UTF8
} finally { Pop-Location }
