//! Embeds the icon (`assets/icon.ico`) in the Windows executable, for Explorer and shortcuts.
//!
//! Only for Windows targets with the GNU toolchain: `windres` (mingw's resource compiler, or
//! the one `WINDRES` names) compiles a one-line resource script to an object that is linked
//! in. Without `windres` the build goes on without the icon, with a warning. The window's own
//! icon is set at run time (`main.rs`), on every platform.

use std::path::PathBuf;
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=assets/icon.ico");
    println!("cargo:rerun-if-env-changed=WINDRES");
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let target_env = std::env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default();
    if target_os != "windows" || target_env != "gnu" {
        return;
    }

    let out = PathBuf::from(std::env::var("OUT_DIR").expect("cargo sets OUT_DIR"));
    let icon = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("cargo sets it"))
        .join("assets/icon.ico");
    let script = out.join("icon.rc");
    let object = out.join("icon.o");
    let icon_path = icon.display().to_string().replace('\\', "/");
    std::fs::write(&script, format!("1 ICON \"{icon_path}\"\n")).expect("writing icon.rc");

    let candidates = match std::env::var("WINDRES") {
        Ok(windres) => vec![windres],
        Err(_) => vec![
            "x86_64-w64-mingw32-windres".to_owned(),
            "windres".to_owned(),
        ],
    };
    for windres in candidates {
        let compiled = Command::new(&windres)
            .arg(&script)
            .args(["-O", "coff", "-o"])
            .arg(&object)
            .status();
        if compiled.is_ok_and(|status| status.success()) {
            println!("cargo:rustc-link-arg-bins={}", object.display());
            return;
        }
    }
    println!("cargo:warning=windres not found: rfdyno.exe gets no icon");
}
