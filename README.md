# opticdrv

Optic's Windows kernel driver, written in Rust on KMDF ([windows-drivers-rs](https://github.com/microsoft/windows-drivers-rs)). Proprietary, see `LICENSE`.

It loads, creates `\\.\OpticDrv` and supervises the Optic user-mode service over a heartbeat:

- The service calls `DeviceIoControl(IOCTL_OPTIC_HEARTBEAT = 0x226000)` at least every 10 s with no input and a 16 byte output (`HeartbeatReply { sequence: u64, missed: u64 }`, see `crates/opticdrv-core/src/ioctl.rs`).
- The service queries `DeviceIoControl(IOCTL_OPTIC_DRIVER_STATUS = 0x226004)` for a 24 byte supervision snapshot (`DriverStatus { version: u32, tamper_events: u32, sequence: u64, missed_total: u64 }`).
- A WDF timer fires every 10 s and logs through `DbgPrint` whether a ping arrived (`opticdrv: heartbeat #N: service alive|MISSED`). Three consecutive misses (`MISSED_THRESHOLD`) persist a tamper record to the driver's `Parameters` registry key and write one `opticdrv` event-log entry (event `0x1001`).

## Layout

| Path | What |
|---|---|
| `crates/opticdrv-core` | `no_std`, WDK-free logic: IOCTL numbers, wire format, heartbeat counters. Fully tested on any host. |
| `src/lib.rs` (`opticdrv`) | Thin KMDF shell: device, symlink, queue, timer. Only calls into core. No logic worth testing lives here. |
| `crates/opticdrv-core/tests/protocol_golden.rs` | Pins the service <-> driver wire contract. |

Rule of thumb: if it can be written without `wdk_sys`, it belongs in `opticdrv-core` with a test.

## Develop

Any OS, no WDK needed (this is what the `fmt` and `core` CI checks run):

```
cargo fmt --all --check
cargo clippy --all-targets -- -D warnings
cargo test
```

Windows only, needs Rust (`x86_64-pc-windows-msvc`), LLVM/clang and the Windows SDK + WDK 10.0.26100 (see the `driver` job in `.github/workflows/build.yml`):

```
cargo build -p opticdrv --release --target x86_64-pc-windows-msvc
copy target\x86_64-pc-windows-msvc\release\opticdrv.dll opticdrv.sys
```

The CRT is linked statically (`.cargo/config.toml`), which `wdk-build` requires.

### Tests

Every test has a comment directly above it stating the bug it catches. Unit tests sit next to the code, `tests/` holds the golden wire contract and `proptest` property tests of the heartbeat. A change to `protocol_golden.rs` is a breaking protocol change for the service: do it on purpose.

### Changing the IOCTL protocol

Edit `crates/opticdrv-core/src/ioctl.rs`, update the golden test and this README, then update the service in the monorepo in the same release window. Bump `OPTIC_PROTOCOL_VERSION` whenever the wire meaning changes; the service treats a version it does not know as untrusted.

## Supervision design

Threat model: the supervised machine's other users are untrusted, including a user with local administrator rights. Standard users are fully contained (they cannot open the device, stop services, or write the install). Against an administrator the driver provides detection and evidence, not prevention: an administrator can always disable supervision with enough deliberate steps (boot options, offline disk edits), so every such step is recorded instead.

How the pieces fit:

- **Two locks on the channel.** The device ACL admits only `LocalSystem` and Administrators (see `DEVICE_SDDL`), and every request is then checked for a session-0 caller named `optic.exe` (`verify_caller`). Session 0 is the load-bearing check: interactive processes run in session 1+, whatever they are named. A renamed binary running as a service would still pass; creating one needs administrator rights.
- **One outage, one record.** The threshold action fires exactly once per outage (the third consecutive miss), so a long outage writes one registry value and one event-log entry, not a stream. The persisted `TamperEvents` counter survives reboots and is reloaded at startup, so post-reboot status reports stay truthful.
- **Best-effort reporting, never a crash.** Registry and event-log writes can fail (resource pressure, key ACLs); they log via `DbgPrint` and move on. A watchdog must never take the machine down.
- **Recovery lives in user mode, not here.** The driver records and reports; restarting and repairing the service is the installer's scheduled repair task (it runs `optic.exe --repair` every 5 minutes as SYSTEM). A kernel driver has no supported API to start a service, and reaching for one would be exactly the kind of fragile kernel trick this codebase avoids.

Deliberate non-goals (revisit with care, each needs its own design review):

- Process-handle filtering (`ObRegisterCallbacks`) to block termination of the service process. Powerful, but a bug here destabilizes the whole machine; the current detect-and-repair loop is preferred until it proves insufficient.
- Refusing driver unload. Non-PnP drivers are stopped through the SCM like any service; an administrator stopping the driver is recorded (the next repair run reinstalls and restarts it, and the gap shows in the tamper record).
- Earlier load (`SERVICE_SYSTEM_START`). The driver currently starts automatically with the system services; boot-start would cover the pre-logon window but raises the cost of every bug.

### Workflow

`main` is protected by a ruleset: every change goes through a pull request, and the `fmt`, `core` and `driver` checks must pass. Dependabot proposes cargo and Actions updates weekly (the `wdk*` crates are grouped, bump them together and expect bindings changes).

## Releasing

Run the **Release** workflow (`workflow_dispatch`, main only). It builds, signs with the fixed test certificate (fails if the secrets are missing) and publishes a release `v<version>-<first 12 hex of the sha256>` with `opticdrv.sys`, `opticdrv.sys.sha256` and `optic-driver-test.cer`. The release notes contain the full SHA-256 and the `gh secret set OPTIC_DRIVER_SHA256 ...` command that makes the monorepo's Windows app build use that release. The monorepo never takes "latest": it only uses the release whose hash is in that secret, so publishing a release changes nothing until the secret is updated.

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
