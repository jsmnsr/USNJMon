use std::{env, fs, path::PathBuf, process::Command};

fn run(command: &mut Command) {
    let status = command
        .status()
        .unwrap_or_else(|e| panic!("cannot execute {command:?}: {e}"));
    assert!(status.success(), "{command:?} failed with {status}");
}

fn main() {
    println!("cargo:rerun-if-changed=resources/messages.mc");
    println!("cargo:rerun-if-changed=resources/provider.man");
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
    let resources = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap()).join("resources");
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
    let profile = out.ancestors().nth(3).unwrap();
    for (source, stem, directory, dll_name, variable) in [
        (
            "messages.mc",
            "messages",
            "legacy",
            "usn-eventlog-messages.dll",
            "USN_MESSAGE_DLL",
        ),
        (
            "provider.man",
            "provider",
            "provider",
            "usn-eventlog-provider.dll",
            "USN_PROVIDER_DLL",
        ),
    ] {
        let destination = out.join(directory);
        fs::create_dir_all(&destination).unwrap();
        run(Command::new(tool("mc.exe"))
            .arg("-U")
            .arg("-h")
            .arg(&destination)
            .arg("-r")
            .arg(&destination)
            .arg(resources.join(source)));
        let res = destination.join(format!("{stem}.res"));
        run(Command::new(tool("rc.exe"))
            .current_dir(&destination)
            .arg("/nologo")
            .arg("/fo")
            .arg(&res)
            .arg(destination.join(format!("{stem}.rc"))));
        let dll = destination.join(dll_name);
        run(Command::new(&linker)
            .args(["-flavor", "link", "/dll", "/noentry", "/machine:x64"])
            .arg(format!("/out:{}", dll.display()))
            .arg(res));
        fs::copy(&dll, profile.join(dll_name)).unwrap();
        println!("cargo:rustc-env={variable}={}", dll.display());
    }
    fs::copy(
        resources.join("provider.man"),
        profile.join("usn-eventlog.man"),
    )
    .unwrap();
    println!(
        "cargo:rustc-env=USN_PROVIDER_HEADER={}",
        out.join("provider").join("provider.h").display()
    );
}
