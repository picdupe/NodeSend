# Repair this machine's Windows SDK 22621 from its signed installer cache.
# Run elevated. Does not uninstall any SDK or automatically restart Windows.
param([string[]]$Only)

$ErrorActionPreference = 'Stop'
if ($Only) { $Only = ($Only -join ',').Split(',') }
$repairLogDir = Join-Path $PSScriptRoot '../sdk-repair-logs'
New-Item -ItemType Directory -Path $repairLogDir -Force | Out-Null
$repairLogDir = (Resolve-Path $repairLogDir).Path
$products = @(
    @('DB0A2082-E91C-2D58-7D65-19113BEDB221', 'Windows SDK for Windows Store Apps Tools-x86_en-us.msi'),
    @('716B691E-C18D-7C81-1890-929DA4A90EEE', 'Windows SDK for Windows Store Apps-x86_en-us.msi'),
    @('A5980723-EA0A-9FF0-2F17-D3CA231D037C', 'Windows SDK Modern Versioned Developer Tools-x86_en-us.msi'),
    @('1B9DE5A3-4B45-E518-3E41-ED155A646D8F', 'Windows SDK EULA-x86_en-us.msi'),
    @('E46FFDDB-220B-543F-63E4-4C31EAE2B3EA', 'Universal CRT Headers Libraries and Sources-x86_en-us.msi'),
    @('B13573B3-86DE-950D-07DD-71ECE767F8E0', 'Windows SDK for Windows Store Apps Headers-x86_en-us.msi'),
    @('293DAAA3-1A33-3825-B6A7-6C9A7FC8F847', 'Windows SDK Desktop Headers x86-x86_en-us.msi'),
    @('1F079CA3-A855-D075-2DBD-013FDCE1A711', 'Windows SDK Desktop Headers x64-x86_en-us.msi'),
    @('538F2EF7-8807-5E59-3621-E606FE025D15', 'Windows SDK for Windows Store Apps Libs-x86_en-us.msi'),
    @('A82C1991-2667-0151-896F-C92C3FCD8AA1', 'Windows SDK Desktop Libs x64-x86_en-us.msi'),
    @('2DBDB095-D491-1983-F74F-EF988CA60CF7', 'Windows SDK Desktop Tools x64-x86_en-us.msi')
)
$installer = New-Object -ComObject WindowsInstaller.Installer
$resultsPath = Join-Path $repairLogDir 'results.json'
$results = if (Test-Path -LiteralPath $resultsPath) { @(Get-Content -LiteralPath $resultsPath -Raw | ConvertFrom-Json) } else { @() }
foreach ($product in $products) {
    if ($Only -and $product[0] -notin $Only) { continue }
    $id = '{' + $product[0] + '}'
    $msi = 'C:\ProgramData\Package Cache\' + $id + 'v10.1.22621.5040\Installers\' + $product[1]
    if (-not (Test-Path -LiteralPath $msi)) { $msi = Join-Path $repairLogDir $product[1] }
    if (-not (Test-Path -LiteralPath $msi)) { throw "Missing cached installer: $msi" }
    $signature = Get-AuthenticodeSignature -LiteralPath $msi
    if ($signature.Status -ne 'Valid' -or $signature.SignerCertificate.Subject -notmatch 'O=Microsoft Corporation') {
        throw "Invalid Microsoft signature: $msi"
    }
    $state = $installer.ProductState($id)
    $mode = if ($state -eq 5) { '/fa' } else { '/i' }
    $log = Join-Path $repairLogDir ($product[0] + '.log')
    Write-Output "Repairing $($product[1]) (installed state $state)"
    $arguments = "$mode `"$msi`" /qn /norestart KITSROOT=`"D:\Windows Kits\10`" /L*v `"$log`""
    $process = Start-Process -FilePath "$env:SystemRoot\System32\msiexec.exe" -ArgumentList $arguments -WindowStyle Hidden -PassThru -Wait
    $results += [pscustomobject]@{Package=$product[1]; ExitCode=$process.ExitCode}
    $results | ConvertTo-Json | Set-Content -LiteralPath $resultsPath -Encoding UTF8
    if ($process.ExitCode -notin @(0,3010)) { throw "MSI failed with $($process.ExitCode): $log" }
}
Write-Output 'SDK core repair complete.'
