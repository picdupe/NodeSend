$ErrorActionPreference = 'Stop'
$projectRoot = Split-Path $PSScriptRoot -Parent
$versionScript = Join-Path $PSScriptRoot 'version.py'
$releaseCommitFiles = @(
    'CHANGELOG.md',
    'version.json',
    'package.json',
    'package-lock.json',
    'src-tauri/tauri.conf.json',
    'src-tauri/Cargo.toml',
    'src-tauri/Cargo.lock',
    'scripts/nsis-hooks.nsh',
    'updates/stable/latest.json'
)
$git = Get-Command git -ErrorAction SilentlyContinue
if (-not $git) { throw 'Git is required to commit and push the updater manifest before publishing.' }
$branch = (& $git.Source -C $projectRoot branch --show-current).Trim()
if ($LASTEXITCODE -ne 0 -or $branch -ne 'main') { throw "Release publishing must run from the main branch (current: '$branch')." }
& $git.Source -C $projectRoot diff --cached --quiet
if ($LASTEXITCODE -ne 0) { throw 'The Git index has staged changes. Commit or unstage them before running the release script.' }
$originalHead = (& $git.Source -C $projectRoot rev-parse HEAD).Trim()
if ($LASTEXITCODE -ne 0 -or -not $originalHead) { throw 'Could not determine the current Git commit.' }

function Get-RemoteMainHead([string]$Remote, [string]$ExpectedHead) {
    if ($Remote -eq 'gitcode') {
        & $git.Source -C $projectRoot -c http.proxy= fetch $Remote '+refs/heads/main:refs/remotes/gitcode/main'
    } else {
        & $git.Source -C $projectRoot fetch $Remote '+refs/heads/main:refs/remotes/origin/main'
    }
    if ($LASTEXITCODE -ne 0) { throw "Could not fetch $Remote/main before release." }
    $remoteHead = (& $git.Source -C $projectRoot rev-parse "refs/remotes/$Remote/main").Trim()
    if ($LASTEXITCODE -ne 0 -or -not $remoteHead) { throw "Could not read $Remote/main after fetching." }
    & $git.Source -C $projectRoot merge-base --is-ancestor $remoteHead $ExpectedHead
    if ($LASTEXITCODE -ne 0) { throw "$Remote/main contains commits that are not in the local release branch. Integrate them before publishing." }
    return $remoteHead
}

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

function Send-JsonRequest($Client, [string]$Method, [string]$Uri, $Body = $null, [switch]$AllowNotFound) {
    $request = [System.Net.Http.HttpRequestMessage]::new([System.Net.Http.HttpMethod]::new($Method), $Uri)
    if ($null -ne $Body) {
        $json = $Body | ConvertTo-Json -Depth 20 -Compress
        $request.Content = [System.Net.Http.StringContent]::new($json, [System.Text.Encoding]::UTF8, 'application/json')
    }
    try {
        $response = $Client.SendAsync($request).GetAwaiter().GetResult()
        if (-not $response.IsSuccessStatusCode) {
            if ($AllowNotFound -and [int]$response.StatusCode -eq 404) { return $null }
            $details = $response.Content.ReadAsStringAsync().GetAwaiter().GetResult()
            if ($details.Length -gt 1200) { $details = $details.Substring(0, 1200) }
            throw "Release API request failed: $Method $($request.RequestUri.Host) ($([int]$response.StatusCode)): $details"
        }
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
        $latestRelease = Send-JsonRequest $clients.github.Client 'GET' "$base/releases/latest" $null -AllowNotFound
        $release = Send-JsonRequest $clients.github.Client 'GET' "$base/releases/tags/$tag" $null -AllowNotFound
        $hasCurrentRelease = $null -ne $release
        if ($null -eq $release) {
            $release = [pscustomobject]@{ tag_name = $tag; body = ''; assets = @() }
            $clients.github.NeedsCreate = $true
        }
        if ($release.tag_name -ne $tag) { throw 'GitHub returned a different release tag.' }
        $clients.github.LatestRelease = $latestRelease
        $clients.github.HasCurrentRelease = $hasCurrentRelease
    } else {
        $clients.gitcode = New-ApiClient $null $false
        $token = [uri]::EscapeDataString($savedCredentials.gitcode.password)
        $base = 'https://api.gitcode.com/api/v5/repos/picdupe/NodeSend'
        $latestRelease = Send-JsonRequest $clients.gitcode.Client 'GET' "$base/releases/latest`?access_token=$token" $null -AllowNotFound
        $release = Send-JsonRequest $clients.gitcode.Client 'GET' "$base/releases/$tag`?access_token=$token" $null -AllowNotFound
        $hasCurrentRelease = $null -ne $release
        if ($null -eq $release) {
            $release = [pscustomobject]@{ tag_name = $tag; body = ''; assets = @() }
            $clients.gitcode.NeedsCreate = $true
        }
        if ($release.tag_name -ne $tag) { throw 'GitCode returned a different release tag.' }
        $clients.gitcode.Token = $token
        $clients.gitcode.LatestRelease = $latestRelease
        $clients.gitcode.HasCurrentRelease = $hasCurrentRelease
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
    throw "No new release-note entries were found in the $($currentVersion.version) changelog section."
}
Write-Output 'New updater notes:'
$updateNotes -split '\r?\n' | ForEach-Object { Write-Output "  $_" }

Write-Output 'Checking the remote main branches before building.'
$originHead = Get-RemoteMainHead 'origin' $originalHead
$gitcodeHead = Get-RemoteMainHead 'gitcode' $originalHead

$githubLatestTag = if ($clients.github.LatestRelease) { [string]$clients.github.LatestRelease.tag_name } else { '' }
$gitcodeLatestTag = if ($clients.gitcode.LatestRelease) { [string]$clients.gitcode.LatestRelease.tag_name } else { '' }
$partialCurrentRelease = [bool]$clients.github.HasCurrentRelease -ne [bool]$clients.gitcode.HasCurrentRelease
if ($githubLatestTag -ine $gitcodeLatestTag -and -not $partialCurrentRelease) {
    $githubLabel = if ($githubLatestTag) { $githubLatestTag } else { '(none)' }
    $gitcodeLabel = if ($gitcodeLatestTag) { $gitcodeLatestTag } else { '(none)' }
    throw "The latest release tags differ between GitHub ($githubLabel) and GitCode ($gitcodeLabel). Synchronize both release pages before building."
}

$latestTagForVersion = $githubLatestTag
if ($partialCurrentRelease) {
    $latestTagForVersion = if ($clients.github.HasCurrentRelease) { $githubLatestTag } else { $gitcodeLatestTag }
    if ($latestTagForVersion -ine $tag) {
        throw "A partial release exists for $tag, but that repository's latest release is $latestTagForVersion. Refusing to resume against an older release."
    }
    Write-Output "Detected a partially published $tag release; preserving build $($currentVersion.build) while completing both release pages."
}

$remoteVersionText = $latestTagForVersion -replace '^v', ''
if ($remoteVersionText -and $remoteVersionText -notmatch '^\d+\.\d+\.\d+$') {
    throw "The latest release tag is not a semantic product version: $latestTagForVersion."
}
$remoteVersion = if ($remoteVersionText) { [version]$remoteVersionText } else { $null }
$localVersion = [version][string]$currentVersion.version

$assetBuilds = @()
$assetPattern = '^NodeSend_(?:v)?' + [regex]::Escape([string]$currentVersion.version) + '\.(\d+)(?:_x64-setup\.exe(?:\.sig)?|_x64_(?:zh-CN|zh-TW|en-US|ja-JP)\.msi|-arm64-v8a-release\.apk)$'
foreach ($target in $targets) {
    foreach ($asset in @($clients[$target].Release.assets)) {
        $assetName = [string]$asset.name
        $assetMatch = [regex]::Match($assetName, $assetPattern)
        if ($assetMatch.Success) {
            $assetBuilds += [int]$assetMatch.Groups[1].Value
        }
    }
}

$publishedBuild = $null
if ($assetBuilds.Count -gt 0) {
    $publishedBuild = ($assetBuilds | Measure-Object -Maximum).Maximum
    Write-Output "Found published build $publishedBuild in release attachment names."
} elseif ($githubLatestTag -or $gitcodeLatestTag) {
    $remoteManifestBuilds = @()
    foreach ($remoteHead in @($originHead, $gitcodeHead)) {
        $remoteManifestJson = & $git.Source -C $projectRoot show "${remoteHead}:updates/stable/latest.json" 2>$null
        if ($LASTEXITCODE -ne 0 -or -not $remoteManifestJson) { continue }
        try { $remoteManifest = ($remoteManifestJson -join [Environment]::NewLine) | ConvertFrom-Json } catch { continue }
        $manifestMatch = [regex]::Match([string]$remoteManifest.version, '^' + [regex]::Escape([string]$currentVersion.version) + '-build\.(\d+)$')
        if ($manifestMatch.Success) { $remoteManifestBuilds += [int]$manifestMatch.Groups[1].Value }
    }
    if ($remoteManifestBuilds.Count -gt 0) {
        $publishedBuild = ($remoteManifestBuilds | Measure-Object -Maximum).Maximum
        Write-Output "Release attachments did not expose a build number; using remote latest.json build $publishedBuild."
    } elseif ($remoteVersion -and $remoteVersion -eq [version][string]$currentVersion.version) {
        throw 'Could not determine the published build from release attachment names or remote latest.json; refusing to guess and increment.'
    }
}

$resumeBuild = $null
if ($null -ne $publishedBuild) {
    $localBuild = [int]$currentVersion.build
    $localManifestPath = Join-Path $projectRoot 'updates/stable/latest.json'
    $localManifestBuild = $null
    if (Test-Path -LiteralPath $localManifestPath -PathType Leaf) {
        $localManifest = Get-Content -LiteralPath $localManifestPath -Raw -Encoding UTF8 | ConvertFrom-Json
        $localManifestMatch = [regex]::Match([string]$localManifest.version, '^' + [regex]::Escape([string]$currentVersion.version) + '-build\.(\d+)$')
        if ($localManifestMatch.Success) { $localManifestBuild = [int]$localManifestMatch.Groups[1].Value }
    }
    if ($null -ne $localManifestBuild -and $localManifestBuild -gt $publishedBuild -and $localManifestBuild -le $localBuild) {
        $resumeBuild = $localManifestBuild
    } elseif ($localBuild -gt $publishedBuild) {
        $resumeBuild = $localBuild
    }
}
if ($remoteVersion -and $remoteVersion -gt $localVersion) {
    throw "Remote release $latestTagForVersion is newer than local version $($currentVersion.version). Update version.json before publishing."
}

if ($partialCurrentRelease) {
    & $python.Source $versionScript sync
} elseif ($remoteVersion -and $remoteVersion -eq $localVersion) {
    if ($null -ne $resumeBuild) {
        if ([int]$currentVersion.build -ne $resumeBuild) {
            $versionPath = Join-Path $projectRoot 'version.json'
            $currentVersion.build = $resumeBuild
            [System.IO.File]::WriteAllText($versionPath, (($currentVersion | ConvertTo-Json) + "`n"), [System.Text.UTF8Encoding]::new($false))
        }
        Write-Output "A release build $resumeBuild is already prepared locally; reusing it."
        & $python.Source $versionScript sync
    } else {
        if ($null -ne $publishedBuild -and [int]$currentVersion.build -le $publishedBuild) {
            $currentVersion.build = $publishedBuild
            $versionPath = Join-Path $projectRoot 'version.json'
            [System.IO.File]::WriteAllText($versionPath, (($currentVersion | ConvertTo-Json) + "`n"), [System.Text.UTF8Encoding]::new($false))
        }
        $publishedLabel = if ($null -ne $publishedBuild) { " after published build $publishedBuild" } else { '' }
        Write-Output "Remote release $latestTagForVersion already uses version $($currentVersion.version); incrementing the build number$publishedLabel."
        & $python.Source $versionScript increment
    }
} else {
    $remoteLabel = if ($latestTagForVersion) { $latestTagForVersion } else { '(none)' }
    Write-Output "Remote latest release is $remoteLabel; keeping build $($currentVersion.build) for new version $($currentVersion.version)."
    & $python.Source $versionScript sync
}
if ($LASTEXITCODE -ne 0) { throw 'Could not synchronize the release build number.' }

$versionData = Get-Content -LiteralPath (Join-Path $projectRoot 'version.json') -Raw | ConvertFrom-Json
$version = [string]$versionData.version
$build = [int]$versionData.build
$tag = "v$version"
$publicVersion = "$version.$build"
$manifestPath = Join-Path $projectRoot 'updates/stable/latest.json'
$installerName = "NodeSend_${publicVersion}_x64-setup.exe"
$androidName = "NodeSend_v${publicVersion}-arm64-v8a-release.apk"
$bundleDir = Join-Path $projectRoot 'src-tauri/target/release/bundle/nsis'
$msiBundleDir = Join-Path $projectRoot 'src-tauri/target/release/bundle/msi'
$files = @(
    @{ Name = $installerName; Path = Join-Path $bundleDir $installerName; Kind = 'installer' },
    @{ Name = "$installerName.sig"; Path = Join-Path $bundleDir "$installerName.sig"; Kind = 'signature' },
    @{ Name = $androidName; Path = Join-Path $projectRoot "dist/Android/$androidName"; Kind = 'android' },
    @{ Name = "NodeSend_${publicVersion}_x64_zh-CN.msi"; Path = Join-Path $msiBundleDir "NodeSend_${publicVersion}_x64_zh-CN.msi"; Kind = 'msi' },
    @{ Name = "NodeSend_${publicVersion}_x64_zh-TW.msi"; Path = Join-Path $msiBundleDir "NodeSend_${publicVersion}_x64_zh-TW.msi"; Kind = 'msi' },
    @{ Name = "NodeSend_${publicVersion}_x64_en-US.msi"; Path = Join-Path $msiBundleDir "NodeSend_${publicVersion}_x64_en-US.msi"; Kind = 'msi' },
    @{ Name = "NodeSend_${publicVersion}_x64_ja-JP.msi"; Path = Join-Path $msiBundleDir "NodeSend_${publicVersion}_x64_ja-JP.msi"; Kind = 'msi' }
)

$resumePreparedRelease = $false
if ($partialCurrentRelease) {
    $missingArtifacts = @($files | Where-Object { -not (Test-Path -LiteralPath $_.Path -PathType Leaf) })
    if ($missingArtifacts.Count -gt 0) {
        throw "A partial $tag release already exists, but prepared artifacts are missing. Refusing to rebuild under an existing tag: $($missingArtifacts.Name -join ', ')."
    }
    $preparedManifest = Get-Content -LiteralPath $manifestPath -Raw -Encoding UTF8 | ConvertFrom-Json
    $apkHash = (Get-FileHash -LiteralPath $files[2].Path -Algorithm SHA256).Hash.ToLowerInvariant()
    $installerSignature = (Get-Content -LiteralPath $files[1].Path -Raw).Trim()
    $committedVersionJson = & $git.Source -C $projectRoot show 'HEAD:version.json'
    if ($LASTEXITCODE -ne 0) { throw 'Could not inspect the prepared release version in HEAD.' }
    $committedVersion = $committedVersionJson | ConvertFrom-Json
    $preparedVersion = "$($currentVersion.version)-build.$($currentVersion.build)"
    $manifestMatches =
        $preparedManifest.version -ceq $preparedVersion -and
        $preparedManifest.notes -ceq $updateNotes -and
        $preparedManifest.android.sha256.ToLowerInvariant() -ceq $apkHash -and
        $preparedManifest.platforms.'windows-x86_64'.signature -ceq $installerSignature -and
        $preparedManifest.platforms.'windows-x86_64'.url.EndsWith($installerName, [StringComparison]::Ordinal) -and
        $preparedManifest.android.url.EndsWith($androidName, [StringComparison]::Ordinal)
    if (-not $manifestMatches -or
        $committedVersion.version -cne $currentVersion.version -or
        [int]$committedVersion.build -ne [int]$currentVersion.build) {
        throw "A partial $tag release exists, but the prepared artifacts do not match the versioned manifest already pushed to main. Refusing to rebuild or move the existing tag."
    }
    $resumePreparedRelease = $true
    Write-Output "Reusing the validated $publicVersion artifacts and manifest already pushed to main."
}

if (-not $resumePreparedRelease) {
Write-Output 'Building the Windows release bundle.'
$pwsh = Get-Command pwsh -ErrorAction SilentlyContinue
if (-not $pwsh) { throw 'PowerShell 7 (pwsh) is required to run the platform build scripts.' }
$npm = Get-Command npm.cmd -ErrorAction SilentlyContinue
if (-not $npm) { throw 'npm.cmd was not found. Install Node.js and ensure npm is on PATH.' }
$androidBuildScript = Join-Path $PSScriptRoot 'android.ps1'
Push-Location $projectRoot
try {
    & $npm.Source run windows:build
    if ($LASTEXITCODE -ne 0) { throw "Windows release build failed (exit $LASTEXITCODE)." }

    Write-Output 'Building the Android ARM64 release package.'
    & $pwsh.Source -NoProfile -ExecutionPolicy Bypass -File $androidBuildScript -Architecture arm64-v8a -Channel stable
    if ($LASTEXITCODE -ne 0) { throw "Android ARM64 release build failed (exit $LASTEXITCODE)." }
} finally {
    Pop-Location
}
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
if (-not $resumePreparedRelease) {
    $manifest | ConvertTo-Json -Depth 30 | Set-Content -LiteralPath $manifestPath -Encoding UTF8
}
$manifest = Get-Content -LiteralPath $manifestPath -Raw -Encoding UTF8 | ConvertFrom-Json
if ($manifest.notes -cne $updateNotes) { throw 'Updater notes do not match the new changelog entries.' }

if ($resumePreparedRelease) {
    Write-Output 'Reusing the updater manifest commit already pushed to both repositories.'
} else {
Write-Output 'Preparing the updater manifest commit.'
& $git.Source -C $projectRoot add -- $releaseCommitFiles
if ($LASTEXITCODE -ne 0) { throw 'Could not stage the fixed release metadata file list.' }
$stagedFiles = @(& $git.Source -C $projectRoot diff --cached --name-only)
if ($LASTEXITCODE -ne 0) { throw 'Could not verify the staged release metadata file list.' }
$unexpectedStagedFiles = @($stagedFiles | Where-Object { $_ -notin $releaseCommitFiles })
if ($unexpectedStagedFiles.Count -gt 0) { throw "Unexpected staged files: $($unexpectedStagedFiles -join ', ')" }
& $git.Source -C $projectRoot diff --cached --quiet
if ($LASTEXITCODE -eq 0) { throw 'No release metadata changes were staged; refusing to amend an unchanged commit.' }
& $git.Source -C $projectRoot commit --amend --no-edit
if ($LASTEXITCODE -ne 0) { throw 'Could not amend the current commit with the updater manifest and version metadata.' }

Write-Output 'Pushing the amended commit to GitHub and GitCode before publishing release assets.'
& $git.Source -C $projectRoot push "--force-with-lease=refs/heads/main:$originHead" origin 'HEAD:refs/heads/main'
if ($LASTEXITCODE -ne 0) { throw 'Could not push the amended release commit to GitHub; release publishing stopped.' }
& $git.Source -C $projectRoot -c http.proxy= push "--force-with-lease=refs/heads/main:$gitcodeHead" gitcode 'HEAD:refs/heads/main'
if ($LASTEXITCODE -ne 0) { throw 'Could not push the amended release commit to GitCode; release publishing stopped.' }
}

foreach ($target in $targets) {
    $entry = $clients[$target]
    if (-not $entry.NeedsCreate) { continue }
    if ($target -eq 'github') {
        $entry.Release = Send-JsonRequest $entry.Client 'POST' "$($entry.Base)/releases" @{
            tag_name = $tag; target_commitish = 'main'; name = $tag; body = $notes;
            draft = $false; prerelease = $false
        }
    } else {
        $entry.Release = Send-JsonRequest $entry.Client 'POST' "$($entry.Base)/releases`?access_token=$($entry.Token)" @{
            tag_name = $tag; target_commitish = 'main'; name = $tag; body = $notes;
            prerelease = $false
        }
    }
    if ($entry.Release.tag_name -ne $tag) { throw "$target returned a different release tag after creation." }
    Write-Output "Created the $target release and tag $tag after pushing the release commit."
}

foreach ($target in $targets) {
    $entry = $clients[$target]
    if ([string]$entry.Release.body -ceq $notes) { continue }
    if ($target -eq 'github') {
        $entry.Release = Send-JsonRequest $entry.Client 'PATCH' "$($entry.Base)/releases/$($entry.Release.id)" @{ body = $notes }
    } else {
        $entry.Release = Send-JsonRequest $entry.Client 'PATCH' "$($entry.Base)/releases/$tag`?access_token=$($entry.Token)" @{ name = [string]$entry.Release.name; body = $notes }
    }
    Write-Output "Updated the $target release description from CHANGELOG.md."
}

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
        '^NodeSend_\d+\.\d+\.\d+\.\d+_x64_(zh-CN|zh-TW|en-US|ja-JP)\.msi$',
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
    Write-Output 'Uploaded the current NSIS installer, signature, four localized MSI packages, and Android ARM64 APK; replaced older matching attachments and synchronized the local release notes.'
} finally {
    foreach ($entry in $clients.Values) {
        if ($entry.Client) { $entry.Client.Dispose() }
        if ($entry.Handler) { $entry.Handler.Dispose() }
    }
}
