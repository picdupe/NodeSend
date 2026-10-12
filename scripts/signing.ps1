param(
    [switch]$Force
)

$ErrorActionPreference = 'Stop'
$projectRoot = Split-Path $PSScriptRoot -Parent
$configPath = Join-Path $projectRoot 'src-tauri/tauri.conf.json'
$keyDirectory = Join-Path $projectRoot 'signing'
$keyPath = Join-Path $keyDirectory 'nodesend-updater.key'
$publicKeyPath = "$keyPath.pub"

if (-not (Test-Path -LiteralPath $configPath)) {
    throw "Tauri configuration was not found: $configPath"
}

New-Item -ItemType Directory -Path $keyDirectory -Force | Out-Null
$legacyKeyPath = Join-Path $env:LOCALAPPDATA 'NodeSend/signing/nodesend-updater.key'
if (-not (Test-Path -LiteralPath $keyPath) -and (Test-Path -LiteralPath $legacyKeyPath)) {
    Move-Item -LiteralPath $legacyKeyPath -Destination $keyPath
    Write-Host "Moved the existing updater key into the project signing directory: $keyPath"
}
$existingKey = Test-Path -LiteralPath $keyPath
if ($existingKey -and $Force) {
    # Replacing this key invalidates the trust relationship for existing installs.
    Remove-Item -LiteralPath $keyPath -Force
    if (Test-Path -LiteralPath $publicKeyPath) {
        Remove-Item -LiteralPath $publicKeyPath -Force
    }
    $existingKey = $false
}

# Keep signing material ignored by Git and restrict access to the current user.
if (-not $existingKey) {
    $publicKey = $null
} elseif (Test-Path -LiteralPath $publicKeyPath -PathType Leaf) {
    $publicKey = (Get-Content -LiteralPath $publicKeyPath -Raw -Encoding UTF8).Trim()
    if (-not $publicKey) { throw "The public key file is empty: $publicKeyPath. The private key was preserved." }
} else {
    Write-Host 'Recovering the matching public key from the existing private key; the private key will not be replaced.'
    $recoveryManifest = Join-Path $PSScriptRoot 'updater-key-recovery/Cargo.toml'
    $previousErrorActionPreference = $ErrorActionPreference
    $ErrorActionPreference = 'Continue'
    try {
        $recoveredOutput = @(& cargo run --quiet --manifest-path $recoveryManifest -- $keyPath $publicKeyPath 2>&1 | ForEach-Object { $_.ToString() })
        $recoveryExitCode = $LASTEXITCODE
    } finally {
        $ErrorActionPreference = $previousErrorActionPreference
    }
    if ($recoveryExitCode -ne 0) {
        throw ("Could not recover the public key. The private key was preserved. Recovery output:`n" + ($recoveredOutput -join "`n"))
    }
    $publicKey = (Get-Content -LiteralPath $publicKeyPath -Raw -Encoding UTF8).Trim()
    if (-not $publicKey) { throw "Public-key recovery returned an empty file: $publicKeyPath. The private key was preserved." }
}
$identity = [System.Security.Principal.WindowsIdentity]::GetCurrent().Name
& icacls.exe $keyDirectory /inheritance:r /grant:r "${identity}:(OI)(CI)F" | Out-Null
if ($LASTEXITCODE -ne 0) { throw 'Could not restrict permissions on the signing-key directory.' }

if (-not $existingKey) {
    Push-Location $projectRoot
    try {
    # PowerShell can promote native stderr warnings to terminating errors when
    # ErrorActionPreference is Stop. Tauri emits a password recommendation on
    # stderr even when key generation itself succeeds, so capture it without
    # aborting and then check the actual native exit code.
    $previousErrorActionPreference = $ErrorActionPreference
    $ErrorActionPreference = 'Continue'
    try {
        $output = @(& npm exec -- tauri signer generate --ci --write-keys $keyPath 2>&1 | ForEach-Object { $_.ToString() })
        $signerExitCode = $LASTEXITCODE
    } finally {
        $ErrorActionPreference = $previousErrorActionPreference
    }
        if ($signerExitCode -ne 0) {
            throw ("Tauri signing key generation failed:`n" + ($output -join "`n"))
        }
    } finally {
        Pop-Location
    }

    # Prefer the CLI-created .pub file. Fall back to parsing its console output.
    if (Test-Path -LiteralPath $publicKeyPath -PathType Leaf) {
        $publicKey = (Get-Content -LiteralPath $publicKeyPath -Raw -Encoding UTF8).Trim()
    } else {
        $matches = [regex]::Matches(($output -join "`n"), '(?<![A-Za-z0-9+/=])[A-Za-z0-9+/=]{100,}(?![A-Za-z0-9+/=])')
        $publicKey = if ($matches.Count -gt 0) { $matches[$matches.Count - 1].Value } else { $null }
    }
    if (-not $publicKey) {
        throw ("The private key was generated at '$keyPath', but the matching public key could not be found. Preserve the key and inspect the Tauri output:`n" + ($output -join "`n"))
    }
}

if (-not (Test-Path -LiteralPath $publicKeyPath)) {
    [System.IO.File]::WriteAllText($publicKeyPath, $publicKey + [Environment]::NewLine, [System.Text.UTF8Encoding]::new($false))
}

$config = Get-Content -LiteralPath $configPath -Raw | ConvertFrom-Json
$config.plugins.updater.pubkey = $publicKey
$json = $config | ConvertTo-Json -Depth 100
[System.IO.File]::WriteAllText($configPath, $json + [Environment]::NewLine, [System.Text.UTF8Encoding]::new($false))

[Environment]::SetEnvironmentVariable('TAURI_SIGNING_PRIVATE_KEY', $keyPath, 'User')
$env:TAURI_SIGNING_PRIVATE_KEY = $keyPath
$env:NODESEND_UPDATER_PUBLIC_KEY = $publicKey

Write-Host ''
Write-Host 'NodeSend updater signing is prepared.' -ForegroundColor Green
Write-Host "Private key: $keyPath"
Write-Host 'Public key: stored in src-tauri/tauri.conf.json and is safe to publish.'
Write-Host 'TAURI_SIGNING_PRIVATE_KEY is set for this process and saved to your user environment.'
Write-Host 'Open a new terminal before a later release build so it inherits the saved environment variable.'
Write-Host 'Back up the private key securely. If it is lost, already-installed clients will not trust packages signed by a replacement key.'
