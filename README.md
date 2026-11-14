sup

## Build

`./build.ps1` needs Visual Studio with the C++ workload; the WDK comes from NuGet unless
`-SkipRestore`. Output is unsigned `opticdrv/x64/Release/opticdrv.sys`. Not yet used by the Optic
Windows app.
