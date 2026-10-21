use std::{env, fs, path::PathBuf, process::Command};

fn run(command: &mut Command) {
    let status = command
        .status()
        .unwrap_or_else(|e| panic!("cannot execute {command:?}: {e}"));
    assert!(status.success(), "{command:?} failed with {status}");
}

fn main() {
    println!("cargo:rerun-if-changed=resources/messages.mc");
    println!("cargo:rerun-if-env-changed=WINDOWS_SDK_BIN");
    if env::var("CARGO_CFG_TARGET_OS").unwrap() != "windows" {
        return;
    }
    assert_eq!(
        env::var("CARGO_CFG_TARGET_ARCH").unwrap(),
        "x86_64",
        "only Windows x64 is supported"
    );
    let sdk = env::var_os("WINDOWS_SDK_BIN").map(PathBuf::from);
    let tool = |name: &str| {
        sdk.as_ref()
            .map(|p| p.join(name))
            .unwrap_or_else(|| name.into())
    };
    let out = PathBuf::from(env::var_os("OUT_DIR").unwrap());
    let source = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap())
        .join("resources")
        .join("messages.mc");
    run(Command::new(tool("mc.exe"))
        .arg("-U")
        .arg("-h")
        .arg(&out)
        .arg("-r")
        .arg(&out)
        .arg(source));
    let res = out.join("messages.res");
    run(Command::new(tool("rc.exe"))
        .current_dir(&out)
        .arg("/nologo")
        .arg("/fo")
        .arg(&res)
        .arg(out.join("messages.rc")));
    let sysroot = Command::new(env::var_os("RUSTC").unwrap())
        .args(["--print", "sysroot"])
        .output()
        .unwrap();
    assert!(sysroot.status.success());
    let linker = PathBuf::from(String::from_utf8(sysroot.stdout).unwrap().trim())
        .join("lib")
        .join("rustlib")
        .join(env::var("HOST").unwrap())
        .join("bin")
        .join("rust-lld.exe");
    let dll = out.join("usn-eventlog-messages.dll");
    run(Command::new(linker)
        .args(["-flavor", "link", "/dll", "/noentry", "/machine:x64"])
        .arg(format!("/out:{}", dll.display()))
        .arg(res));
    fs::copy(
        &dll,
        out.ancestors()
            .nth(3)
            .unwrap()
            .join("usn-eventlog-messages.dll"),
    )
    .unwrap();
    println!("cargo:rustc-env=USN_MESSAGE_DLL={}", dll.display());
}
