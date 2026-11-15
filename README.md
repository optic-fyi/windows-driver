# opticdrv

Optic's Windows kernel driver, written in Rust on KMDF ([windows-drivers-rs](https://github.com/microsoft/windows-drivers-rs)). Proprietary, see `LICENSE`.

It currently enforces nothing. It loads, creates `\\.\OpticDrv` (SYSTEM and Administrators only) and keeps a heartbeat with the Optic service:

- The service calls `DeviceIoControl(IOCTL_OPTIC_HEARTBEAT = 0x226000)` at least every 10 s with no input and a 16 byte output (`HeartbeatReply { sequence: u64, missed: u64 }`, see `src/ioctl.rs`).
- A WDF timer fires every 10 s and logs through `DbgPrint` whether a ping arrived (`opticdrv: heartbeat #N: service alive|MISSED`).

## Build

Needs Rust stable (`x86_64-pc-windows-msvc`), LLVM/clang, and the Windows SDK + WDK 10.0.22621. See `.github/workflows/build.yml`.

```
cargo build --release --target x86_64-pc-windows-msvc
copy target\x86_64-pc-windows-msvc\release\opticdrv.dll opticdrv.sys
```

## Releasing

Run the **Release** workflow (`workflow_dispatch`, main only). It builds, signs with the fixed test certificate (fails if the secrets are missing) and publishes a release `v<version>-<sha7>` with `opticdrv.sys`, `opticdrv.sys.sha256` and `optic-driver-test.cer`. The monorepo's Windows app build downloads the **latest** release, verifies the hash and embeds the driver, so protect `main` and bump `version` in `Cargo.toml` as needed.

## Signing (test certificate)

CI signs with a fixed self-signed certificate (`optic-driver-test.cer`, subject `CN=Optic Driver Test Signing`, valid to 2036) taken from the repo secrets `DRIVER_SIGNING_PFX_BASE64` and `DRIVER_SIGNING_PFX_PASSWORD`. The same certificate is used on every run; CI fails if the signature does not match the committed `.cer`. Replace it with a real EV/attestation-signed flow before shipping.

To load it on a test machine (elevated):

```
bcdedit /set testsigning on          & rem reboot afterwards
certutil -addstore Root optic-driver-test.cer
certutil -addstore TrustedPublisher optic-driver-test.cer
sc create opticdrv type= kernel binPath= C:\path\to\opticdrv.sys
sc start opticdrv
```
