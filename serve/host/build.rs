//! Embeds a `longPathAware` application manifest in the Windows binaries (`kith`, `kith-serve`, `kith-servew`), so
//! paths past 260 characters (agent worktrees, `node_modules`) work. A no-op on every other target.
//!
//! No manifest crate: the MSVC linker takes the manifest as an input file. (`MinGW` targets are not shipped.)

use std::path::PathBuf;

const MANIFEST: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<assembly xmlns="urn:schemas-microsoft-com:asm.v1" manifestVersion="1.0">
  <application xmlns="urn:schemas-microsoft-com:asm.v3">
    <windowsSettings xmlns:ws2="http://schemas.microsoft.com/SMI/2016/WindowsSettings">
      <ws2:longPathAware>true</ws2:longPathAware>
    </windowsSettings>
  </application>
</assembly>
"#;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let target_env = std::env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default();
    if target_os != "windows" || target_env != "msvc" {
        return;
    }
    // Windows gives a main thread 1 MiB, Unix 8: the host's start-up (its router tables and store) overflows the smaller one.
    println!("cargo:rustc-link-arg-bins=/STACK:8388608");
    let Some(out) = std::env::var_os("OUT_DIR") else { return };
    let path = PathBuf::from(out).join("kith.manifest");
    if std::fs::write(&path, MANIFEST).is_err() {
        println!("cargo:warning=could not write the longPathAware manifest");
        return;
    }
    println!("cargo:rustc-link-arg-bins=/MANIFEST:EMBED");
    println!("cargo:rustc-link-arg-bins=/MANIFESTINPUT:{}", path.display());
}
