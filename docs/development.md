# Build and contribute

Supported targets are `x86_64-pc-windows-msvc` and `x86_64-pc-windows-gnu`,
on Windows 10/11 or Server 2016+ x64.

Install Rust **1.99.1** with rustfmt/Clippy and the Windows 10/11 SDK (`mc.exe`,
`rc.exe`). MSVC also requires Visual Studio C++ Build Tools; GNU requires MinGW
binutils (`dlltool` and its assembler) or an absolute
`RUSTFLAGS=-C dlltool=<path-to-llvm-dlltool.exe>` setting.

```powershell
rustup toolchain install 1.99.1 --profile minimal --component rustfmt,clippy
rustup override set 1.99.1
rustup target add x86_64-pc-windows-msvc
.\scripts\Build.ps1
# Or: .\scripts\Build.ps1 -Target x86_64-pc-windows-gnu
```

The wrapper discovers the SDK, or accepts `WINDOWS_SDK_BIN` pointing to
`bin\<version>\x64`. It checks formatting, Clippy, tests, release compilation,
and in-memory native message resources. Clippy and tests cover every workspace
package. CI runs both Windows targets and the portable packages on Linux.

Distribute `usnjmon.exe`, `usnjmon-provider.dll` and `usnjmon.man` from
`target\<target>\release` together with `scripts` and `config.example.json`.
Only the executable is needed for offline conversion; the DLL/manifest are
needed for provider/service installation and Event Viewer General descriptions.
No legacy provider is built or installed by the renamed product. Old installed
resources are deliberately retained for old history.

Use `scripts\Package.ps1` and `scripts\Test-Package.ps1` for a versioned Windows
x64 MSVC ZIP with checksums, build metadata, the MIT license and dependency notices. The
[release guide](releases.md) describes the manual-only publishing workflow.

## Rust style

Follow [Microsoft's Pragmatic Rust Guidelines](https://microsoft.github.io/rust-guidelines/)
for application and library design. Apply their spirit and safety, performance
and maintainability goals; document justified exceptions when a guideline does
not fit this Cargo Windows application.

The following are local project conventions, not a summary of Microsoft's
guidelines: stable rustfmt, 100-column maximum, snake_case modules, one primary
abstraction per leaf module, explicit imports from one module per statement,
qualified ordinary cross-module function calls, and named multi-field structures.
Use borrows unless ownership/sharing is required, descriptive `expect` messages
for genuine invariants, and propagated contextual errors for runtime failures.
Tests may use `unwrap` and `use super::*`.

Unsafe code is limited to native adapters, with `SAFETY:` comments explaining
lifetimes, buffer sizes and ownership. Clippy rejects undocumented unsafe blocks
and production `unwrap`; Rust rejects implicit unsafe operations inside unsafe
functions. Shared `Arc` cancellation state is required for SCM/worker threads.
Use stable Rust and the pinned toolchain documented above.

## Package and module boundaries

The root Cargo workspace keeps common dependency versions, edition, MSRV,
license, version and lint settings in `Cargo.toml`. The three packages are
unpublished application components, not stable public libraries:

| Package | Responsibility | Dependencies on other workspace packages |
| --- | --- | --- |
| `usnjmon` (root) | CLI, configuration, atomic filesystem checkpoints, Windows adapters and provider build resources | `usnjmon-core`, `usnjmon-evtx` |
| `crates\usnjmon-core` | USN parsing, extracted-stream reading, event/schema mapping, reconciliation, delivery, path policy and cancellation | None |
| `crates\usnjmon-evtx` | Bounded EVTX framing, checksums and typed Binary XML encoding | None |

The two leaf packages forbid unsafe code and require neither Windows libraries
nor SDK tools. The EVTX writer deliberately knows nothing about USN records or
the application's event schema. Its public input is a typed XML element tree.
The application's converter maps core schema fields into that tree.

Within the core, `event` owns the envelope and `Sink` contract, and `checkpoint`
owns cursor validation and the `Store` contract. `engine` separates reconciliation
from batch delivery; `filter` separates path policy from the sink adapter; `schema`
separates record fields, operational notices and native scalar encoding.
The application supplies filesystem persistence and native sinks. Import shared
types directly from their owning package rather than re-exporting them through
the application.

Windows modules separate collector supervision from lane-local polling,
conversion publication from document construction, ETW delivery from registration
checks/resource formatting, and volume identity/querying from path lookup and
overlapped I/O. Request buffers, event handles and `OVERLAPPED` storage remain
within a single I/O function until completion or cancellation is drained.
Implementation modules and worker state are private; only entry points and
cross-package contracts are exposed.

Run the portable packages independently on any supported Rust host:

```powershell
cargo test --locked -p usnjmon-core -p usnjmon-evtx
cargo clippy --locked -p usnjmon-core -p usnjmon-evtx --all-targets -- -D warnings
```

The core's `test-util` feature exposes synthetic journal fixtures only for
dependent tests. Normal application builds do not enable it. Native round-trip
tests remain with the application; the portable writer also checks a deterministic
multi-chunk byte fingerprint captured from the pre-refactor implementation.
The provider manifest remains the single source for schema field-order tests.

These boundaries follow Microsoft's guidance on smaller crates, balanced modules,
mockable I/O and shared workspace settings. Configuration and native adapters
remain in the application because they have no independent consumer. Additional
crates for each service/volume operation would add APIs without isolating a new
dependency. Events still use the existing JSON envelope, and errors remain
contextual `anyhow` results: typed-event redesign and a stable SDK are separate
changes, not prerequisites for this refactor.

## Validation boundaries

Tests use synthetic records and owned disposable files only. Offline conversion
is exercised through the native Windows Event Log reader across multiple chunks,
including XPath selection and named-field Unicode round trips. The tests never
register providers, install/start services, read host journals or write live logs.

See [validation](validation.md) for explicitly authorized deployment acceptance
checks.
