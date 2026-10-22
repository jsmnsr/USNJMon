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
and in-memory native message resources. CI runs both targets.

Distribute `usnjmon.exe`, `usnjmon-provider.dll` and `usnjmon.man` from
`target\<target>\release` together with `scripts` and `config.example.json`.
Only the executable is needed for offline conversion; the DLL/manifest are
needed for provider/service installation and Event Viewer General descriptions.
No legacy provider is built or installed by the renamed product. Old installed
resources are deliberately retained for old history.

## Rust style

Follow the applicable general rules in
[Google's Rust Style Guide](https://google.github.io/styleguide/rustguide.html):
standard stable rustfmt, 100-column maximum, snake_case modules, one primary
abstraction per leaf module, explicit imports from one module per statement,
qualified ordinary cross-module function calls, and named multi-field structures.
Use borrows unless ownership/sharing is required, descriptive `expect` messages
for genuine invariants, and propagated contextual errors for runtime failures.
Tests may use `unwrap` and `use super::*`.

Unsafe code is limited to native adapters, with `SAFETY:` comments explaining
lifetimes, buffer sizes and ownership. Clippy rejects undocumented unsafe blocks
and production `unwrap`; Rust rejects implicit unsafe operations inside unsafe
functions. Shared `Arc` cancellation state is required for SCM/worker threads.
Use stable Rust: Google's Chromium GN/flat-crate infrastructure and
Fuchsia-specific policies do not apply to this Cargo Windows application.

The library modules are internal application/test boundaries, not a stable
public library API.

## Validation boundaries

Tests use synthetic records and owned disposable files only. Offline conversion
is exercised through the native Windows Event Log reader across multiple chunks,
including XPath selection and named-field Unicode round trips. The tests never
register providers, install/start services, read host journals or write live logs.

See [validation](validation.md) for explicitly authorized deployment acceptance
checks and [references](references.md) for native API/format decisions.
