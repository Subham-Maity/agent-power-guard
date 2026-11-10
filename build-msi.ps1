$ErrorActionPreference = "Stop"

$wixBin = "C:\Program Files (x86)\WiX Toolset v3.14\bin"
$candle = Join-Path $wixBin "candle.exe"
$light  = Join-Path $wixBin "light.exe"

if (!(Test-Path $candle) -or !(Test-Path $light)) {
    Write-Error "WiX Toolset v3.14 not found in $wixBin"
    exit 1
}

$version = (Get-Content Cargo.toml | Select-String '^version = "(.*)"').Matches.Groups[1].Value
if (!$version) { $version = "1.1.0" }

$exePath = if (Test-Path "target\x86_64-pc-windows-msvc\release\agent-power-guard.exe") {
    (Resolve-Path "target\x86_64-pc-windows-msvc\release\agent-power-guard.exe").Path
} else {
    (Resolve-Path "target\release\agent-power-guard.exe").Path
}
$outDir = (Resolve-Path ".").Path + "\target\installer"
if (!(Test-Path $outDir)) {
    New-Item -ItemType Directory -Path $outDir -Force | Out-Null
}

$wixObj = Join-Path $outDir "main.wixobj"
$msiOut = Join-Path $outDir "AgentPowerGuard-$version-x64.msi"

Write-Host "Compiling WiX source..."
& $candle -arch x64 "-dSourceExe=$exePath" -out $wixObj "wix\main.wxs"
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }

Write-Host "Linking MSI installer..."
& $light -ext WixUIExtension -out $msiOut $wixObj
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }

Write-Host "MSI successfully created at: $msiOut"
Get-Item $msiOut | Select-Object Name, Length, LastWriteTime | Format-Table -AutoSize
