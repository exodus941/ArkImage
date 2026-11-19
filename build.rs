//! Windows: embed the app icon; for a DLL build (VCPKGRS_DYNAMIC=1), copy the vcpkg runtime DLLs next to the exe.

use std::{env, fs, path::PathBuf};

fn main() {
    println!("cargo:rerun-if-env-changed=VCPKG_ROOT");
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo::rustc-check-cfg=cfg(lottie)");
    if env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        // rlottie (Lottie playback), static, from scripts/build-rlottie-macos.sh. Without it
        // the app builds and simply does not open Lottie files.
        println!("cargo:rerun-if-env-changed=RLOTTIE_LIB_DIR");
        let dir = env::var("RLOTTIE_LIB_DIR").map(PathBuf::from).unwrap_or_else(|_| {
            PathBuf::from(env::var("HOME").unwrap_or_default()).join("Library/Caches/rlottie/build")
        });
        if dir.join("librlottie.a").exists() {
            println!("cargo:rerun-if-changed={}", dir.join("librlottie.a").display());
            println!("cargo:rustc-link-search=native={}", dir.display());
            println!("cargo:rustc-link-lib=static=rlottie");
            println!("cargo:rustc-link-lib=dylib=c++");
            println!("cargo:rustc-cfg=lottie");
        } else {
            println!("cargo:warning=rlottie not found in {}; Lottie is off (run scripts/build-rlottie-macos.sh)", dir.display());
        }
        return;
    }
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    // App icon in the exe (Explorer, taskbar pins, Alt+Tab). Made by examples/make_icon.rs.
    println!("cargo:rerun-if-changed=assets/icon.ico");
    let mut res = winresource::WindowsResource::new();
    res.set_icon("assets/icon.ico");
    // Common Controls 6: TaskDialogIndirect (silent message boxes, src/dialog.rs) exists
    // only there; without it the exe does not start.
    res.set_manifest(
        r#"<assembly xmlns="urn:schemas-microsoft-com:asm.v1" manifestVersion="1.0">
<dependency><dependentAssembly><assemblyIdentity type="win32" name="Microsoft.Windows.Common-Controls" version="6.0.0.0" processorArchitecture="*" publicKeyToken="6595b64144ccf1df" language="*"/></dependentAssembly></dependency>
<trustInfo xmlns="urn:schemas-microsoft-com:asm.v3"><security><requestedPrivileges><requestedExecutionLevel level="asInvoker" uiAccess="false"/></requestedPrivileges></security></trustInfo>
</assembly>"#,
    );
    if let Err(e) = res.compile() {
        println!("cargo:warning=exe icon not embedded: {e}");
    }
    println!("cargo:rerun-if-env-changed=VCPKGRS_DYNAMIC");
    let Ok(root) = env::var("VCPKG_ROOT") else {
        println!("cargo:warning=VCPKG_ROOT not set; HEIC needs libheif from vcpkg");
        return;
    };
    // rlottie (Lottie playback): static, from the same vcpkg tree; the C API is in src/lottie.rs.
    println!("cargo:rustc-cfg=lottie");
    let triplet = env::var("VCPKGRS_TRIPLET").unwrap_or_else(|_| "x64-windows-static-md".into());
    let lib = PathBuf::from(&root).join("installed").join(&triplet).join("lib");
    println!("cargo:rustc-link-search=native={}", lib.display());
    println!("cargo:rustc-link-lib=static=rlottie");
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
