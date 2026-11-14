<#
.SYNOPSIS
  Builds opticdrv\x64\Release\opticdrv.sys with MSBuild. Needs Visual Studio (C++ and the
  WindowsKernelModeDriver toolset); the WDK itself is restored from NuGet unless -SkipRestore.
.DESCRIPTION
  Fails (non-zero exit) if MSBuild cannot be found, the build fails, or the .sys is not produced.
  The result is unsigned: test-sign it (bcdedit /set testsigning on; signtool sign ...) before
  loading it.
#>
param(
    # Build against an installed WDK instead of restoring the WDK NuGet packages.
    [switch]$SkipRestore
)
$ErrorActionPreference = 'Stop'

$project = Join-Path $PSScriptRoot 'opticdrv\opticdrv.vcxproj'
$output = Join-Path $PSScriptRoot 'opticdrv\x64\Release\opticdrv.sys'

$vswhere = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio\Installer\vswhere.exe'
if (-not (Test-Path $vswhere)) {
    throw "vswhere.exe not found; install Visual Studio with the 'Desktop development with C++' workload and the WDK."
}
$msbuild = & $vswhere -latest -requires Microsoft.Component.MSBuild -find 'MSBuild\**\Bin\MSBuild.exe' | Select-Object -First 1
if (-not $msbuild) {
    throw 'MSBuild not found via vswhere.'
}

# The WDK headers and libraries come from NuGet (opticdrv\packages.config), the same way
# microsoft/Windows-driver-samples builds in CI. Directory.Build.props picks the restored packages
# up; with -SkipRestore the build uses a WDK installed on this machine instead.
if (-not $SkipRestore) {
    $driverDir = Join-Path $PSScriptRoot 'opticdrv'
    $nuget = (Get-Command nuget.exe -ErrorAction SilentlyContinue).Source
    if (-not $nuget) {
        $nuget = Join-Path $env:TEMP 'nuget.exe'
        if (-not (Test-Path $nuget)) {
            Invoke-WebRequest -Uri 'https://dist.nuget.org/win-x86-commandline/latest/nuget.exe' -OutFile $nuget
        }
    }
    & $nuget restore (Join-Path $driverDir 'packages.config') -PackagesDirectory (Join-Path $driverDir 'packages') -NonInteractive
    if ($LASTEXITCODE -ne 0) {
        throw "nuget restore failed with exit code $LASTEXITCODE"
    }
}

if (Test-Path $output) { Remove-Item $output }

& $msbuild $project /p:Configuration=Release /p:Platform=x64 /nologo /verbosity:minimal
if ($LASTEXITCODE -ne 0) {
    throw "MSBuild failed with exit code $LASTEXITCODE"
}
if (-not (Test-Path $output)) {
    throw "Build reported success but $output was not produced."
}

Write-Host "Built $((Resolve-Path $output).Path)"
