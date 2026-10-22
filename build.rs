use std::env;
use std::fs;
use std::path::PathBuf;
use std::process::Command;

fn run(command: &mut Command) {
    let status = command
        .status()
        .unwrap_or_else(|error| panic!("cannot execute {command:?}: {error}"));
    assert!(status.success(), "{command:?} failed with {status}");
}

fn main() {
    println!("cargo:rerun-if-changed=resources/provider.man");
    println!("cargo:rerun-if-env-changed=WINDOWS_SDK_BIN");
    if env::var("CARGO_CFG_TARGET_OS").expect("Cargo sets target OS") != "windows" {
        return;
    }
    assert_eq!(
        env::var("CARGO_CFG_TARGET_ARCH").expect("Cargo sets architecture"),
        "x86_64",
        "only Windows x64 is supported"
    );
    let sdk = env::var_os("WINDOWS_SDK_BIN").map(PathBuf::from);
    let tool = |name: &str| {
        sdk.as_ref()
            .map_or_else(|| PathBuf::from(name), |path| path.join(name))
    };
    let out = PathBuf::from(env::var_os("OUT_DIR").expect("Cargo sets OUT_DIR"));
    let resources =
        PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("Cargo sets manifest directory"))
            .join("resources");
    let sysroot = Command::new(env::var_os("RUSTC").expect("Cargo sets RUSTC"))
        .args(["--print", "sysroot"])
        .output()
        .expect("query Rust sysroot");
    assert!(sysroot.status.success(), "rustc could not report sysroot");
    let linker = PathBuf::from(
        String::from_utf8(sysroot.stdout)
            .expect("sysroot is UTF-8")
            .trim(),
    )
    .join("lib")
    .join("rustlib")
    .join(env::var("HOST").expect("Cargo sets host"))
    .join("bin")
    .join("rust-lld.exe");
    let profile = out
        .ancestors()
        .nth(3)
        .expect("Cargo OUT_DIR has a profile ancestor");
    run(Command::new(tool("mc.exe"))
        .arg("-U")
        .arg("-h")
        .arg(&out)
        .arg("-r")
        .arg(&out)
        .arg(resources.join("provider.man")));
    let res = out.join("provider.res");
    run(Command::new(tool("rc.exe"))
        .current_dir(&out)
        .arg("/nologo")
        .arg("/fo")
        .arg(&res)
        .arg(out.join("provider.rc")));
    let dll = out.join("usnjmon-provider.dll");
    run(Command::new(&linker)
        .args(["-flavor", "link", "/dll", "/noentry", "/machine:x64"])
        .arg(format!("/out:{}", dll.display()))
        .arg(res));
    fs::copy(&dll, profile.join("usnjmon-provider.dll"))
        .expect("copy provider DLL to build output");
    fs::copy(resources.join("provider.man"), profile.join("usnjmon.man"))
        .expect("copy provider manifest");
    println!("cargo:rustc-env=USN_PROVIDER_DLL={}", dll.display());
    println!(
        "cargo:rustc-env=USN_PROVIDER_HEADER={}",
        out.join("provider.h").display()
    );
}
