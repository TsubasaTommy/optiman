//! Builds the JPEG XL reference library (libjxl, BSD-3-Clause) from the pinned git submodule
//! and links it statically. Only the encoder/decoder/threads libraries are built.

use std::fs::File;
use std::path::{Path, PathBuf};
use std::{env, fs};

const SUBMODULE: &str = "third_party/libjxl";
const NESTED: [&str; 3] = ["third_party/highway", "third_party/brotli", "third_party/skcms"];

/// CMake options for libjxl. They are part of the build cache key (see `cache_key`).
const CMAKE_OPTIONS: &[(&str, &str)] = &[
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
];

/// Bump to rebuild cached libjxl builds after a change that neither the options above nor
/// `libjxl_config` show (both are part of the cache key), e.g. a new version of the cmake crate.
const BUILD_REVISION: u32 = 1;

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

/// App icon of both executables (Explorer, taskbar, Start menu), rendered by
/// `scripts/render_icon.py`.
const ICON: &str = "assets/optiman.ico";

fn embed_windows_resources() {
    let out = PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR"));
    let path = out.join("app.manifest");
    fs::write(&path, MANIFEST).expect("write manifest");
    println!("cargo:rustc-link-arg-bins=/MANIFEST:EMBED");
    println!("cargo:rustc-link-arg-bins=/MANIFESTINPUT:{}", path.display());
    println!("cargo:rustc-link-arg-bins=/MANIFESTUAC:NO");

    println!("cargo:rerun-if-changed={ICON}");
    let ico = fs::read(ICON).expect("read the app icon");
    let path = out.join("icon.res");
    fs::write(&path, icon_res(&ico)).expect("write icon.res");
    // The MSVC linker takes compiled resource files as inputs.
    println!("cargo:rustc-link-arg-bins={}", path.display());
}

/// The icon as a compiled resource file: each image as an RT_ICON, plus the RT_GROUP_ICON
/// directory (ID 1, the one Explorer shows) that lists them.
fn icon_res(ico: &[u8]) -> Vec<u8> {
    const RT_ICON: u16 = 3;
    const RT_GROUP_ICON: u16 = 14;
    let u32_at = |i: usize| u32::from_le_bytes(ico[i..i + 4].try_into().expect("4 bytes")) as usize;
    let count = usize::from(u16::from_le_bytes([ico[4], ico[5]]));
    // A .res file starts with an empty entry.
    let mut res = [0, 0, 0, 0, 32, 0, 0, 0, 0xFF, 0xFF, 0, 0, 0xFF, 0xFF, 0, 0].to_vec();
    res.resize(32, 0);
    // The group directory is the .ico header with each entry's file offset replaced by an ID.
    let mut group = ico[..6].to_vec();
    for i in 0..count {
        let entry = 6 + 16 * i;
        let (len, offset) = (u32_at(entry + 8), u32_at(entry + 12));
        let id = u16::try_from(i + 1).expect("icon count");
        push_resource(&mut res, RT_ICON, id, &ico[offset..offset + len]);
        group.extend_from_slice(&ico[entry..entry + 12]);
        group.extend_from_slice(&id.to_le_bytes());
    }
    push_resource(&mut res, RT_GROUP_ICON, 1, &group);
    res
}

/// Appends one resource with a numeric type and ID, language neutral.
fn push_resource(res: &mut Vec<u8>, kind: u16, id: u16, data: &[u8]) {
    res.extend(u32::try_from(data.len()).expect("resource size").to_le_bytes()); // DataSize
    res.extend(32u32.to_le_bytes()); // HeaderSize
    for word in [0xFFFF, kind, 0xFFFF, id] {
        res.extend(u16::to_le_bytes(word)); // TYPE and NAME as ordinals
    }
    res.extend(0u32.to_le_bytes()); // DataVersion
    res.extend(0x1010u16.to_le_bytes()); // MemoryFlags: MOVEABLE | DISCARDABLE
    res.extend(0u16.to_le_bytes()); // LanguageId
    res.extend([0; 8]); // Version, Characteristics
    res.extend_from_slice(data);
    res.resize(res.len().next_multiple_of(4), 0);
}

fn main() {
    let src = Path::new(SUBMODULE);
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed=OPTIMAN_SKIP_LIBJXL");
    if cfg_msvc() {
        embed_windows_resources();
    }
    // Type-checking another target (e.g. `cargo check --target x86_64-pc-windows-msvc` on a Mac)
    // does not link, so the native library build can be skipped.
    if env::var_os("OPTIMAN_SKIP_LIBJXL").is_some() {
        return;
    }
    println!("cargo:rerun-if-changed={SUBMODULE}/lib/include");
    println!("cargo:rerun-if-changed={SUBMODULE}/CMakeLists.txt");
    for nested in NESTED {
        if !src.join(nested).join("CMakeLists.txt").exists() && !src.join(nested).join("src").exists() {
            panic!("libjxl のソースが見つかりません。次を実行してください: scripts/fetch-libjxl.sh");
        }
    }

    let mut cfg = libjxl_config(src);
    println!("cargo:rerun-if-env-changed=OPTIMAN_LIBJXL_CACHE_DIR");
    let dst = match env::var_os("OPTIMAN_LIBJXL_CACHE_DIR") {
        Some(dir) => cached_build(Path::new(&dir), &mut cfg),
        None => cfg.build(),
    };

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

    // C++ runtime for the statically linked libjxl (MSVC links its own automatically).
    if env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        println!("cargo:rustc-link-lib=c++");
    }
}

/// The CMake build of libjxl. This function's source text is part of the cache key, so every
/// change here rebuilds cached builds.
fn libjxl_config(src: &Path) -> cmake::Config {
    let mut cfg = cmake::Config::new(src);
    cfg.profile("Release");
    for (key, value) in CMAKE_OPTIONS {
        cfg.define(key, value);
    }
    if let Ok(n) = std::thread::available_parallelism() {
        cfg.env("CMAKE_BUILD_PARALLEL_LEVEL", n.get().to_string());
    }
    if cfg_msvc() {
        // The Visual Studio generator builds the files of one project serially unless asked.
        cfg.cflag("/MP").cxxflag("/MP");
    }
    cfg
}

/// Builds libjxl once per target and configuration into a shared directory instead of every
/// cargo profile's OUT_DIR (`check`, `clippy`, `test` and `--release` would each rebuild it).
/// The flags the cmake crate passes do not depend on the cargo profile, and the CMake build
/// type is fixed to Release, so the result is the same for all of them.
fn cached_build(root: &Path, cfg: &mut cmake::Config) -> PathBuf {
    let target = env::var("TARGET").expect("TARGET");
    let name = format!("{target}-{:016x}", cache_key(&target));
    let dir = root.join(&name);
    fs::create_dir_all(&dir).expect("create the libjxl cache directory");
    // Concurrent cargo invocations (e.g. clippy and test) wait for each other here.
    let lock = File::create(root.join(format!("{name}.lock"))).expect("create the libjxl cache lock");
    lock.lock().expect("lock the libjxl cache");
    let stamp = dir.join("complete");
    if !stamp.exists() {
        cfg.out_dir(&dir);
        cfg.build();
        fs::write(&stamp, name.as_bytes()).expect("write the libjxl cache stamp");
        remove_stale_builds(root, &target, &name);
    }
    dir
}

/// Deletes this target's builds for other keys, so the cache (which CI restores from an older key
/// and saves under the new one) keeps one build per target. Builds another cargo process holds
/// are left alone.
fn remove_stale_builds(root: &Path, target: &str, current: &str) {
    let Ok(entries) = fs::read_dir(root) else { return };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let is_key = |k: &str| k.len() == 16 && k.bytes().all(|b| b.is_ascii_hexdigit());
        let other_build = name != current
            && name.strip_prefix(target).and_then(|r| r.strip_prefix('-')).is_some_and(is_key)
            && entry.path().is_dir();
        if !other_build {
            continue;
        }
        let lock_path = root.join(format!("{name}.lock"));
        let Ok(lock) = File::create(&lock_path) else { continue };
        if lock.try_lock().is_ok() {
            let _ = fs::remove_dir_all(entry.path());
            drop(lock);
            let _ = fs::remove_file(lock_path);
        }
    }
}

/// Everything that changes the libjxl build: the options, `libjxl_config` and the build
/// revision, the library versions, the target and the compiler environment. Edits elsewhere in
/// this script keep the cached build.
fn cache_key(target: &str) -> u64 {
    // Without CRs, so a CRLF checkout gives the same key.
    let script = fs::read_to_string("build.rs").expect("read build.rs").replace('\r', "");
    let config_fn = script.split("fn libjxl_config").nth(1).and_then(|s| s.split("\n}\n").next());
    let config_fn = config_fn.expect("libjxl_config in build.rs");
    let mut data = format!("{BUILD_REVISION} {CMAKE_OPTIONS:?} msvc={} {config_fn}", cfg_msvc()).into_bytes();
    for file in [
        "lib/CMakeLists.txt",
        "third_party/highway/CMakeLists.txt",
        "third_party/brotli/CMakeLists.txt",
        "third_party/skcms/skcms.cc",
    ] {
        data.extend(fs::read(Path::new(SUBMODULE).join(file)).unwrap_or_default());
    }
    data.extend(target.as_bytes());
    for var in ["HOST", "CC", "CXX", "CFLAGS", "CXXFLAGS", "MACOSX_DEPLOYMENT_TARGET", "CMAKE_GENERATOR"] {
        data.extend(var.as_bytes());
        data.extend(env::var(var).unwrap_or_default().bytes());
        println!("cargo:rerun-if-env-changed={var}");
    }
    data.iter().fold(0xcbf2_9ce4_8422_2325u64, |h, &b| (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3))
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
