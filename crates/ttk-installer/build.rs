//! Embed `ttk.exe` into `ttk-setup.exe`.
//!
//! The binary to embed is named by `TTK_INSTALLER_PAYLOAD`, which
//! `scripts/build-installer.ps1` sets after building `ttk` in release mode.
//! Without it the installer is built with an empty payload, so that an
//! ordinary `cargo build` / `cargo test` of the workspace never depends on
//! build order; such an installer looks for a `ttk.exe` next to itself instead.

use std::path::PathBuf;

fn main() {
    println!("cargo:rerun-if-env-changed=TTK_INSTALLER_PAYLOAD");
    println!("cargo::rustc-check-cfg=cfg(ttk_payload)");

    let out = PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR"));
    let dest = out.join("payload.bin");

    match std::env::var("TTK_INSTALLER_PAYLOAD") {
        Ok(p) if !p.trim().is_empty() => {
            let src = PathBuf::from(p.trim());
            if !src.is_file() {
                panic!(
                    "TTK_INSTALLER_PAYLOAD points at {}, which is not a file",
                    src.display()
                );
            }
            println!("cargo:rerun-if-changed={}", src.display());
            std::fs::copy(&src, &dest).expect("copy the payload");
            println!("cargo:rustc-cfg=ttk_payload");
        }
        _ => {
            std::fs::write(&dest, b"").expect("write an empty payload");
        }
    }

    // Ask for no elevation, ever: everything the installer touches belongs to
    // the current user. Without this Windows may guess from the word "setup"
    // in the file name that the program wants administrator rights.
    let target_env = std::env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default();
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    if target_os == "windows" && target_env == "msvc" {
        println!("cargo:rustc-link-arg-bins=/MANIFEST:EMBED");
        println!("cargo:rustc-link-arg-bins=/MANIFESTUAC:level='asInvoker'");
    }
}
