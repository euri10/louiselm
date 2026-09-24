//! Compile the Linux `x86_64` Sender guard into the measured launcher artifact.

use std::{env, error::Error, path::PathBuf, process::Command};

fn main() -> Result<(), Box<dyn Error>> {
    if env::var("CARGO_CFG_TARGET_OS")? != "linux" || env::var("CARGO_CFG_TARGET_ARCH")? != "x86_64"
    {
        return Err("the Sender guard build requires the supported Linux x86_64 target".into());
    }
    // The source includes only these fixed Linux UAPI trees. Watching the
    // directories also catches replaced headers and newly included files.
    for input in [
        "build.rs",
        "src/launch_supervisor/sender_guard",
        "/usr/include/linux",
        "/usr/include/asm-generic",
        "/usr/include/x86_64-linux-gnu/asm",
    ] {
        println!("cargo::rerun-if-changed={input}");
    }
    println!("cargo::rerun-if-env-changed=PATH");
    let output = PathBuf::from(env::var_os("OUT_DIR").ok_or("Cargo OUT_DIR is missing")?)
        .join("sender-guard.bpf.o");
    let status = Command::new("clang")
        .args([
            "-target",
            "bpfel",
            "-D__TARGET_ARCH_x86",
            "-g",
            "-O2",
            "-Wall",
            "-Werror",
            "-I",
            "/usr/include/x86_64-linux-gnu",
            "-c",
            "src/launch_supervisor/sender_guard/lifecycle.bpf.c",
            "-o",
        ])
        .arg(output)
        .status()
        .map_err(|error| {
            format!("Sender guard: cannot run clang ({error}); install clang and linux-libc-dev")
        })?;
    if !status.success() {
        return Err(format!("Sender guard compilation failed ({status}); clang needs the BPF target and linux-libc-dev headers").into());
    }
    Ok(())
}
