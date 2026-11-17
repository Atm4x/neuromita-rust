param(
    [Parameter(Mandatory)]
    [string] $BinaryDirectory,
    [Parameter(Mandatory)]
    [string] $OutputDirectory
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$repository = Split-Path $PSScriptRoot
Push-Location $repository
try {
    $metadata = cargo metadata --locked --offline --no-deps --format-version 1
    if ($LASTEXITCODE -ne 0) { throw 'Cargo metadata failed' }
    $package = (ConvertFrom-Json -InputObject $metadata).packages |
        Where-Object name -eq 'neuromita'

    $binary = (Resolve-Path -LiteralPath $BinaryDirectory).Path
    $output = New-Item -ItemType Directory -Path $OutputDirectory -Force
    $root = New-Item -ItemType Directory -Path (Join-Path $output.FullName "NeuroMita-v$($package.version)") -Force
    $profile = New-Item -ItemType Directory -Path (Join-Path $root.FullName 'UserData') -Force
    $executable = Join-Path $binary 'neuromita.exe'
    $nativeAsset = Join-Path $output.FullName 'neuromita-gui-windows-x86_64.exe'
    $portableAsset = Join-Path $output.FullName "NeuroMita-v$($package.version)-windows-x86_64.zip"
    if ((Test-Path -LiteralPath $nativeAsset) -or (Test-Path -LiteralPath $portableAsset)) {
        throw 'Release assets already exist in this output directory'
    }

    $reportedVersion = & $executable --version | Out-String
    if ($LASTEXITCODE -ne 0 -or $reportedVersion.Trim() -ne "NeuroMita $($package.version)") {
        throw 'Desktop executable version differs from Cargo.toml'
    }

    Copy-Item -LiteralPath $executable -Destination $root.FullName
    Copy-Item -LiteralPath (Join-Path $repository 'Prompts') -Destination $profile.FullName -Recurse
    Copy-Item -LiteralPath (Join-Path $repository 'licenses') -Destination $root.FullName -Recurse
    $chessLicense = Join-Path $root.FullName 'licenses/cozy-chess-LICENSE'
    Copy-Item -LiteralPath (Join-Path $repository 'third_party/cozy-chess/LICENSE') -Destination $chessLicense
    Copy-Item -LiteralPath $executable -Destination $nativeAsset
    Compress-Archive -LiteralPath $root.FullName -DestinationPath $portableAsset -CompressionLevel Optimal
    Get-FileHash -Algorithm SHA256 -LiteralPath $nativeAsset, $portableAsset
} finally {
    Pop-Location
}
