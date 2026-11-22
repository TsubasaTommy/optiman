//! Builds the JPEG XL reference library (libjxl, BSD-3-Clause) from the pinned git submodule
//! and links it statically. Only the encoder/decoder/threads libraries are built.

use std::path::{Path, PathBuf};
use std::{env, fs};

const SUBMODULE: &str = "third_party/libjxl";
const NESTED: [&str; 3] = ["third_party/highway", "third_party/brotli", "third_party/skcms"];

/// Application manifest for both executables: common controls v6 (themed settings window),
/// per-monitor DPI awareness v2, long paths, Windows 10/11 behaviour and no elevation.
const MANIFEST: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<assembly xmlns="urn:schemas-microsoft-com:asm.v1" manifestVersion="1.0">
  <dependency>
    <dependentAssembly>
      <assemblyIdentity type="win32" name="Microsoft.Windows.Common-Controls" version="6.0.0.0"
        processorArchitecture="*" publicKeyToken="6595b64144ccf1df" language="*"/>
    </dependentAssembly>
  </dependency>
  <compatibility xmlns="urn:schemas-microsoft-com:compatibility.v1">
    <application>
      <supportedOS Id="{8e0f7a12-bfb3-4fe8-b9a5-48fd50a15a9a}"/>
    </application>
  </compatibility>
  <application xmlns="urn:schemas-microsoft-com:asm.v3">
    <windowsSettings>
      <dpiAware xmlns="http://schemas.microsoft.com/SMI/2005/WindowsSettings">true/pm</dpiAware>
      <dpiAwareness xmlns="http://schemas.microsoft.com/SMI/2016/WindowsSettings">PerMonitorV2</dpiAwareness>
      <longPathAware xmlns="http://schemas.microsoft.com/SMI/2016/WindowsSettings">true</longPathAware>
    </windowsSettings>
  </application>
  <trustInfo xmlns="urn:schemas-microsoft-com:asm.v3">
    <security>
      <requestedPrivileges>
        <requestedExecutionLevel level="asInvoker" uiAccess="false"/>
      </requestedPrivileges>
    </security>
  </trustInfo>
</assembly>
"#;

fn embed_windows_manifest() {
    let out = PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR"));
    let path = out.join("app.manifest");
    fs::write(&path, MANIFEST).expect("write manifest");
    println!("cargo:rustc-link-arg-bins=/MANIFEST:EMBED");
    println!("cargo:rustc-link-arg-bins=/MANIFESTINPUT:{}", path.display());
    println!("cargo:rustc-link-arg-bins=/MANIFESTUAC:NO");
}

fn main() {
    let src = Path::new(SUBMODULE);
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed=OIF_SKIP_LIBJXL");
    if cfg_msvc() {
        embed_windows_manifest();
    }
    // Type-checking another target (e.g. `cargo check --target x86_64-pc-windows-msvc` on a Mac)
    // does not link, so the native library build can be skipped.
    if env::var_os("OIF_SKIP_LIBJXL").is_some() {
        return;
    }
    println!("cargo:rerun-if-changed={SUBMODULE}/lib/include");
    println!("cargo:rerun-if-changed={SUBMODULE}/CMakeLists.txt");
    for nested in NESTED {
        if !src.join(nested).join("CMakeLists.txt").exists() && !src.join(nested).join("src").exists() {
            panic!("libjxl のソースが見つかりません。次を実行してください: scripts/fetch-libjxl.sh");
        }
    }

    let mut cfg = cmake::Config::new(src);
    cfg.profile("Release");
    for (key, value) in [
        ("BUILD_SHARED_LIBS", "OFF"),
        ("BUILD_TESTING", "OFF"),
        ("CMAKE_POSITION_INDEPENDENT_CODE", "ON"),
        ("JPEGXL_ENABLE_TOOLS", "OFF"),
        ("JPEGXL_ENABLE_DEVTOOLS", "OFF"),
        ("JPEGXL_ENABLE_EXAMPLES", "OFF"),
        ("JPEGXL_ENABLE_BENCHMARK", "OFF"),
        ("JPEGXL_ENABLE_FUZZERS", "OFF"),
        ("JPEGXL_ENABLE_DOXYGEN", "OFF"),
        ("JPEGXL_ENABLE_MANPAGES", "OFF"),
        ("JPEGXL_ENABLE_JNI", "OFF"),
        ("JPEGXL_ENABLE_SJPEG", "OFF"),
        ("JPEGXL_ENABLE_OPENEXR", "OFF"),
        ("JPEGXL_ENABLE_VIEWERS", "OFF"),
        ("JPEGXL_ENABLE_PLUGINS", "OFF"),
        ("JPEGXL_ENABLE_TCMALLOC", "OFF"),
        ("JPEGXL_ENABLE_SKCMS", "ON"),
        ("JPEGXL_ENABLE_TRANSCODE_JPEG", "ON"),
        ("JPEGXL_ENABLE_BOXES", "ON"),
        ("JPEGXL_BUNDLE_LIBPNG", "OFF"),
        ("JPEGXL_FORCE_SYSTEM_BROTLI", "OFF"),
        ("JPEGXL_FORCE_SYSTEM_HWY", "OFF"),
        ("JPEGXL_FORCE_SYSTEM_LCMS2", "OFF"),
        ("JPEGXL_WARNINGS_AS_ERRORS", "OFF"),
        // Highway/brotli install targets are not needed; libraries are picked up from the build tree.
        ("HWY_ENABLE_INSTALL", "OFF"),
        ("HWY_ENABLE_CONTRIB", "OFF"),
        ("HWY_ENABLE_EXAMPLES", "OFF"),
        ("HWY_ENABLE_TESTS", "OFF"),
        ("BROTLI_DISABLE_TESTS", "ON"),
    ] {
        cfg.define(key, value);
    }
    if let Ok(n) = std::thread::available_parallelism() {
        cfg.env("CMAKE_BUILD_PARALLEL_LEVEL", n.get().to_string());
    }
    let dst = cfg.build();

    // Find each static library wherever CMake put it (install dir or build tree).
    let wanted = ["jxl", "jxl_threads", "jxl_cms", "hwy", "brotlienc", "brotlidec", "brotlicommon"];
    let mut found = Vec::new();
    collect_libs(&dst, &wanted, &mut found);
    let mut dirs: Vec<PathBuf> = Vec::new();
    for name in wanted {
        let Some((_, path)) = found.iter().find(|(n, _)| *n == name) else {
            panic!("libjxl のビルド結果に {name} が見つかりません ({})", dst.display());
        };
        let dir = path.parent().unwrap().to_path_buf();
        if !dirs.contains(&dir) {
            println!("cargo:rustc-link-search=native={}", dir.display());
            dirs.push(dir);
        }
    }
    for name in wanted {
        let (_, path) = found.iter().find(|(n, _)| *n == name).unwrap();
        let stem = path.file_stem().unwrap().to_string_lossy();
        let link_name = stem.strip_prefix("lib").filter(|_| !cfg_msvc()).unwrap_or(&stem);
        println!("cargo:rustc-link-lib=static={link_name}");
    }

    // C++ runtime for the statically linked libjxl.
    match env::var("CARGO_CFG_TARGET_OS").as_deref() {
        Ok("macos" | "ios") => println!("cargo:rustc-link-lib=c++"),
        Ok("linux") => println!("cargo:rustc-link-lib=stdc++"),
        _ => {} // MSVC links its C++ runtime automatically
    }
}

fn cfg_msvc() -> bool {
    env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc")
}

/// Recursively finds `lib<name>.a` / `<name>.lib` (also `-static` variants).
fn collect_libs(dir: &Path, wanted: &[&'static str], found: &mut Vec<(&'static str, PathBuf)>) {
    let Ok(entries) = fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_libs(&path, wanted, found);
            continue;
        }
        let file = path.file_name().unwrap().to_string_lossy().into_owned();
        for &name in wanted {
            let candidates = [
                format!("lib{name}.a"),
                format!("{name}.lib"),
                format!("lib{name}-static.a"),
                format!("{name}-static.lib"),
            ];
            if candidates.contains(&file) && !found.iter().any(|(n, _)| *n == name) {
                found.push((name, path.clone()));
            }
        }
    }
}
