//! Windows: embed the app icon; for a DLL build (VCPKGRS_DYNAMIC=1), copy the vcpkg runtime DLLs next to the exe.

use std::{env, fs, path::PathBuf};

fn main() {
    println!("cargo:rerun-if-env-changed=VCPKG_ROOT");
    println!("cargo:rerun-if-changed=build.rs");
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    // App icon in the exe (Explorer, taskbar pins, Alt+Tab). Made by examples/make_icon.rs.
    println!("cargo:rerun-if-changed=assets/icon.ico");
    let mut res = winresource::WindowsResource::new();
    res.set_icon("assets/icon.ico");
    if let Err(e) = res.compile() {
        println!("cargo:warning=exe icon not embedded: {e}");
    }
    println!("cargo:rerun-if-env-changed=VCPKGRS_DYNAMIC");
    let Ok(root) = env::var("VCPKG_ROOT") else {
        println!("cargo:warning=VCPKG_ROOT not set; HEIC needs libheif from vcpkg");
        return;
    };
    // The default build links libheif statically (one exe; see .cargo/config.toml).
    // Only a DLL build (VCPKGRS_DYNAMIC=1) needs the DLLs beside the exe.
    if env::var_os("VCPKGRS_DYNAMIC").is_none() {
        return;
    }
    let bin = PathBuf::from(root).join("installed").join("x64-windows").join("bin");
    let out = PathBuf::from(env::var("OUT_DIR").unwrap());
    // OUT_DIR = <target>/<profile>/build/<pkg>[/<hash>]/out; the profile dir holds "build".
    let Some(profile_dir) = out
        .ancestors()
        .find(|a| a.file_name().and_then(|n| n.to_str()) == Some("build"))
        .and_then(|b| b.parent())
    else {
        return;
    };
    let Ok(rd) = fs::read_dir(&bin) else { return };
    for e in rd.flatten() {
        let p = e.path();
        if p.extension().and_then(|x| x.to_str()) == Some("dll") {
            let _ = fs::copy(&p, profile_dir.join(p.file_name().unwrap()));
        }
    }
}
