$ErrorActionPreference = 'Stop'
$projectRoot = Split-Path $PSScriptRoot -Parent
$versionScript = Join-Path $PSScriptRoot 'version.py'
$python = Get-Command python -ErrorAction SilentlyContinue
if (-not $python) { throw 'Python 3 is required to increment the release build number.' }

$currentVersion = Get-Content -LiteralPath (Join-Path $projectRoot 'version.json') -Raw | ConvertFrom-Json
$changelogPath = Join-Path $projectRoot 'CHANGELOG.md'
if (-not (Test-Path -LiteralPath $changelogPath -PathType Leaf)) { throw "Changelog not found: $changelogPath" }
$currentVersionPattern = '(?m)^## \[' + [regex]::Escape([string]$currentVersion.version) + '\] - \d{4}-\d{2}-\d{2}$'
if (-not (Select-String -LiteralPath $changelogPath -Pattern $currentVersionPattern -Quiet)) {
    throw "Release notes for version $($currentVersion.version) were not found in $changelogPath."
}

$changelog = Get-Content -LiteralPath $changelogPath -Raw -Encoding UTF8
$sectionPattern = '(?ms)^## \[' + [regex]::Escape([string]$currentVersion.version) + '\] - \d{4}-\d{2}-\d{2}\s*\r?\n(.*?)(?=^## \[|\z)'
$section = [regex]::Match($changelog, $sectionPattern)
if (-not $section.Success) { throw "Release notes for version $($currentVersion.version) were not found in $changelogPath." }
$notes = $section.Groups[1].Value.Trim()

function Get-SavedCredential([string]$HostName) {
    $request = 'protocol=https' + [Environment]::NewLine + 'host=' + $HostName + [Environment]::NewLine + [Environment]::NewLine
    $lines = $request | git credential fill 2>$null
    if ($LASTEXITCODE -ne 0) { throw "No saved Git Credential Manager credential for $HostName." }
    $values = @{}
    foreach ($line in $lines) {
        $separator = $line.IndexOf('=')
        if ($separator -gt 0) { $values[$line.Substring(0, $separator)] = $line.Substring($separator + 1) }
    }
    if (-not $values.password) { throw "No API token is available for $HostName in Git Credential Manager." }
    return $values
}

function New-ApiClient([string]$Token, [bool]$UseProxy) {
    $handler = [System.Net.Http.HttpClientHandler]::new()
    $handler.UseProxy = $UseProxy
    $client = [System.Net.Http.HttpClient]::new($handler)
    $null = $client.DefaultRequestHeaders.UserAgent.ParseAdd('NodeSend-Release-Publisher')
    $null = $client.DefaultRequestHeaders.Accept.ParseAdd('application/vnd.github+json')
    if ($Token) { $client.DefaultRequestHeaders.Authorization = [System.Net.Http.Headers.AuthenticationHeaderValue]::new('Bearer', $Token) }
    return @{ Client = $client; Handler = $handler }
}

function Send-JsonRequest($Client, [string]$Method, [string]$Uri, $Body = $null) {
    $request = [System.Net.Http.HttpRequestMessage]::new([System.Net.Http.HttpMethod]::new($Method), $Uri)
    if ($null -ne $Body) {
        $json = $Body | ConvertTo-Json -Depth 20 -Compress
        $request.Content = [System.Net.Http.StringContent]::new($json, [System.Text.Encoding]::UTF8, 'application/json')
    }
    try {
        $response = $Client.SendAsync($request).GetAwaiter().GetResult()
        if (-not $response.IsSuccessStatusCode) { throw "Release API request failed: $Method ($([int]$response.StatusCode))." }
        $text = $response.Content.ReadAsStringAsync().GetAwaiter().GetResult()
        if ($text) { return ($text | ConvertFrom-Json) }
    } finally { $request.Dispose() }
}

function Get-PlainNote([string]$Line) {
    $value = $Line.Trim() -replace '^[-*+]\s+', ''
    $value = [regex]::Replace($value, '\[([^\]]+)\]\([^)]+\)', '$1')
    $value = $value -replace '`', ''
    $value = $value -replace '\*\*|__|\*|_', ''
    return $value.Trim()
}

function Get-NoteWords([string]$Line) {
    $plain = (Get-PlainNote $Line).ToLowerInvariant() -replace '[^\p{L}\p{Nd}]+', ' '
    return @($plain.Split(' ', [StringSplitOptions]::RemoveEmptyEntries) | ForEach-Object {
        switch ($_) { 'improved' { 'improve' } 'added' { 'add' } 'updated' { 'update' } 'fixed' { 'fix' } default { $_ } }
    } | Sort-Object -Unique)
}

function Test-SameNote([string]$Left, [string]$Right) {
    $leftWords = @(Get-NoteWords $Left)
    $rightWords = @(Get-NoteWords $Right)
    if ($leftWords.Count -eq 0 -or $rightWords.Count -eq 0) { return $false }
    $intersection = @($leftWords | Where-Object { $_ -in $rightWords }).Count
    $union = @($leftWords + $rightWords | Sort-Object -Unique).Count
    return ($intersection / [double]$union) -ge 0.8
}

function Get-NewReleaseNotes([string]$ChangelogSection, [string]$PublishedBody) {
    $publishedBullets = @([regex]::Matches($PublishedBody, '(?m)^\s*[-*+]\s+.+$') | ForEach-Object Value)
    $newBullets = @([regex]::Matches($ChangelogSection, '(?m)^\s*[-*+]\s+.+$') | ForEach-Object Value | Where-Object {
        $candidate = $_
        -not (@($publishedBullets | Where-Object { Test-SameNote $candidate $_ }).Count -gt 0)
    })
    return (($newBullets | ForEach-Object { Get-PlainNote $_ }) -join [Environment]::NewLine).Trim()
}

$savedCredentials = @{
    github = Get-SavedCredential 'github.com'
    gitcode = Get-SavedCredential 'gitcode.com'
}

$tag = "v$($currentVersion.version)"
$targets = @('github', 'gitcode')
$clients = @{}
foreach ($target in $targets) {
    if ($target -eq 'github') {
        $clients.github = New-ApiClient $savedCredentials.github.password $true
        $base = 'https://api.github.com/repos/picdupe/NodeSend'
        $release = Send-JsonRequest $clients.github.Client 'GET' "$base/releases/tags/$tag"
        if ($release.tag_name -ne $tag) { throw 'GitHub returned a different release tag.' }
    } else {
        $clients.gitcode = New-ApiClient $null $false
        $token = [uri]::EscapeDataString($savedCredentials.gitcode.password)
        $base = 'https://api.gitcode.com/api/v5/repos/picdupe/NodeSend'
        $release = Send-JsonRequest $clients.gitcode.Client 'GET' "$base/releases/$tag`?access_token=$token"
        if ($release.tag_name -ne $tag) { throw 'GitCode returned a different release tag.' }
        $clients.gitcode.Token = $token
    }
    $clients[$target].Base = $base
    $clients[$target].Release = $release
}

$updateNotes = Get-NewReleaseNotes $notes ([string]$clients.github.Release.body)
$gitcodeNotes = Get-NewReleaseNotes $notes ([string]$clients.gitcode.Release.body)
if ($updateNotes -cne $gitcodeNotes) {
    throw 'GitHub and GitCode release descriptions produce different update notes. Synchronize both release descriptions before building.'
}
if ([string]::IsNullOrWhiteSpace($updateNotes)) {
    throw "No new release-note entries were found in the $($currentVersion.version) changelog section; refusing to increment the build number."
}
Write-Output 'New updater notes:'
$updateNotes -split '\r?\n' | ForEach-Object { Write-Output "  $_" }

Write-Output 'Incrementing the build number once.'
& $python.Source $versionScript increment
if ($LASTEXITCODE -ne 0) { throw 'Could not increment and synchronize the release build number.' }

$versionData = Get-Content -LiteralPath (Join-Path $projectRoot 'version.json') -Raw | ConvertFrom-Json
$version = [string]$versionData.version
$build = [int]$versionData.build
$tag = "v$version"
$publicVersion = "$version.$build"
$manifestPath = Join-Path $projectRoot 'updates/stable/latest.json'
$installerName = "NodeSend_${publicVersion}_x64-setup.exe"
$androidName = "NodeSend_v${publicVersion}-arm64-v8a-release.apk"
$bundleDir = Join-Path $projectRoot 'src-tauri/target/release/bundle/nsis'
$files = @(
    @{ Name = $installerName; Path = Join-Path $bundleDir $installerName; Kind = 'installer' },
    @{ Name = "$installerName.sig"; Path = Join-Path $bundleDir "$installerName.sig"; Kind = 'signature' },
    @{ Name = $androidName; Path = Join-Path $projectRoot "dist/Android/$androidName"; Kind = 'android' }
)

Write-Output 'Building the Windows release bundle.'
$pwsh = Get-Command pwsh -ErrorAction SilentlyContinue
if (-not $pwsh) { throw 'PowerShell 7 (pwsh) is required to run the platform build scripts.' }
$windowsBuildScript = Join-Path $PSScriptRoot 'windows.ps1'
$androidBuildScript = Join-Path $PSScriptRoot 'android.ps1'
Push-Location $projectRoot
try {
    & $pwsh.Source -NoProfile -ExecutionPolicy Bypass -File $windowsBuildScript -Bundle nsis -Channel stable
    if ($LASTEXITCODE -ne 0) { throw "Windows release build failed (exit $LASTEXITCODE)." }

    Write-Output 'Building the Android ARM64 release package.'
    & $pwsh.Source -NoProfile -ExecutionPolicy Bypass -File $androidBuildScript -Architecture arm64-v8a -Channel stable
    if ($LASTEXITCODE -ne 0) { throw "Android ARM64 release build failed (exit $LASTEXITCODE)." }
} finally {
    Pop-Location
}

if (-not (Test-Path -LiteralPath $manifestPath -PathType Leaf)) { throw "Updater manifest not found: $manifestPath" }
foreach ($file in $files) {
    if (-not (Test-Path -LiteralPath $file.Path -PathType Leaf)) { throw "Build artifact not found: $($file.Path)" }
    $file.Size = (Get-Item -LiteralPath $file.Path).Length
    $file.Sha256 = (Get-FileHash -LiteralPath $file.Path -Algorithm SHA256).Hash.ToLowerInvariant()
}

$manifest = Get-Content -LiteralPath $manifestPath -Raw -Encoding UTF8 | ConvertFrom-Json
if ($manifest.version -ne "$version-build.$build") { throw 'Updater manifest version does not match version.json.' }
if ($manifest.platforms.'windows-x86_64'.signature -ne (Get-Content -LiteralPath $files[1].Path -Raw).Trim()) {
    throw 'Windows installer signature does not match updates/stable/latest.json.'
}
if ($manifest.android.sha256.ToLowerInvariant() -ne $files[2].Sha256) { throw 'Android APK SHA-256 does not match updates/stable/latest.json.' }
if (-not $manifest.platforms.'windows-x86_64'.url.EndsWith($installerName, [StringComparison]::Ordinal) -or
    -not $manifest.android.url.EndsWith($androidName, [StringComparison]::Ordinal)) {
    throw 'Updater manifest download URLs do not match the build artifacts.'
}
$manifest.notes = $updateNotes
$manifest | ConvertTo-Json -Depth 30 | Set-Content -LiteralPath $manifestPath -Encoding UTF8
$manifest = Get-Content -LiteralPath $manifestPath -Raw -Encoding UTF8 | ConvertFrom-Json
if ($manifest.notes -cne $updateNotes) { throw 'Updater notes do not match the new changelog entries.' }

Write-Output "Release: $tag"
Write-Output "Notes:   $changelogPath [$version]"
Write-Output 'Validated release artifacts:'
$files | ForEach-Object { Write-Output ("  {0}  {1:N0} bytes  SHA-256 {2}" -f $_.Name, $_.Size, $_.Sha256) }

function Send-FileRequest($Client, [string]$Method, [string]$Uri, [string]$Path, [hashtable]$Headers = @{}) {
    $request = [System.Net.Http.HttpRequestMessage]::new([System.Net.Http.HttpMethod]::new($Method), $Uri)
    $stream = [System.IO.File]::OpenRead($Path)
    try {
        $content = [System.Net.Http.StreamContent]::new($stream)
        $content.Headers.ContentType = [System.Net.Http.Headers.MediaTypeHeaderValue]::Parse('application/octet-stream')
        $request.Content = $content
        foreach ($name in $Headers.Keys) {
            if ($name -eq 'Content-Type') { $content.Headers.ContentType = [System.Net.Http.Headers.MediaTypeHeaderValue]::Parse([string]$Headers[$name]) }
            else { $null = $request.Headers.TryAddWithoutValidation([string]$name, [string]$Headers[$name]) }
        }
        $response = $Client.SendAsync($request).GetAwaiter().GetResult()
        if (-not $response.IsSuccessStatusCode) { throw "Release attachment upload failed: $([IO.Path]::GetFileName($Path)) ($([int]$response.StatusCode))." }
        return $response
    } finally { $request.Dispose(); $stream.Dispose() }
}

function Get-ManagedOldAssets($Assets, [string[]]$CurrentNames) {
    $patterns = @(
        '^NodeSend_\d+\.\d+\.\d+\.\d+_x64-setup\.exe$',
        '^NodeSend_\d+\.\d+\.\d+\.\d+_x64-setup\.exe\.sig$',
        '^NodeSend_v\d+\.\d+\.\d+\.\d+-arm64-v8a-release\.apk$'
    )
    return @($Assets | Where-Object {
        $assetName = [string]$_.name
        $isManaged = $false
        foreach ($pattern in $patterns) { if ($assetName -match $pattern) { $isManaged = $true; break } }
        $isManaged -and $assetName -notin $CurrentNames
    })
}

$currentNames = @($files | ForEach-Object Name)
try {
    foreach ($target in $targets) {
        $release = $clients[$target].Release
        foreach ($file in $files) {
            if (@($release.assets | Where-Object name -eq $file.Name).Count -gt 0) {
                throw "Target attachment already exists on $($target): $($file.Name). Increase the build number before publishing another build."
            }
        }
    }

    foreach ($target in $targets) {
        $entry = $clients[$target]
        foreach ($file in $files) {
            if ($target -eq 'github') {
                $uploadBase = ([string]$entry.Release.upload_url) -replace '\{\?name,label\}$', ''
                $uploadUri = $uploadBase + '?name=' + [uri]::EscapeDataString($file.Name)
                $null = Send-FileRequest $entry.Client 'POST' $uploadUri $file.Path
            } else {
                $url = "$($entry.Base)/releases/$tag/upload_url`?access_token=$($entry.Token)&file_name=$([uri]::EscapeDataString($file.Name))"
                $info = Send-JsonRequest $entry.Client 'GET' $url
                $uploadHeaders = @{}
                foreach ($property in $info.headers.PSObject.Properties) { $uploadHeaders[$property.Name] = [string]$property.Value }
                $null = Send-FileRequest $entry.Client 'PUT' ([string]$info.url) $file.Path $uploadHeaders
            }
        }
    }

    foreach ($target in $targets) {
        $entry = $clients[$target]
        if ($target -eq 'github') { $updated = Send-JsonRequest $entry.Client 'GET' "$($entry.Base)/releases/tags/$tag" }
        else { $updated = Send-JsonRequest $entry.Client 'GET' "$($entry.Base)/releases/$tag`?access_token=$($entry.Token)" }
        foreach ($file in $files) {
            $asset = @($updated.assets | Where-Object name -eq $file.Name)
            if ($asset.Count -ne 1 -or [long]$asset[0].size -ne [long]$file.Size) { throw "Uploaded asset verification failed on $($target): $($file.Name)." }
        }
        if ($target -eq 'github') {
            $oldAssets = Get-ManagedOldAssets $updated.assets $currentNames
            foreach ($asset in $oldAssets) { $null = Send-JsonRequest $entry.Client 'DELETE' "$($entry.Base)/releases/assets/$($asset.id)" }
            $null = Send-JsonRequest $entry.Client 'PATCH' "$($entry.Base)/releases/$($updated.id)" @{ body = $notes }
        } else {
            $oldAssets = Get-ManagedOldAssets $updated.assets $currentNames
            foreach ($asset in $oldAssets) {
                $deleteUri = "$($entry.Base)/releases/$tag/attach_files/$($asset.id)`?access_token=$($entry.Token)"
                $null = Send-JsonRequest $entry.Client 'DELETE' $deleteUri
            }
            $null = Send-JsonRequest $entry.Client 'PATCH' "$($entry.Base)/releases/$tag`?access_token=$($entry.Token)" @{ name = [string]$updated.name; body = $notes }
        }
    }

    Write-Output "Updated existing release $tag on: $($targets -join ', ')"
    Write-Output 'Uploaded the current Windows installer, signature, and Android ARM64 APK; replaced older matching attachments and synchronized the local release notes.'
} finally {
    foreach ($entry in $clients.Values) {
        if ($entry.Client) { $entry.Client.Dispose() }
        if ($entry.Handler) { $entry.Handler.Dispose() }
    }
}
