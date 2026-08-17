use std::path::{Path, PathBuf};

fn main() {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let vendor = manifest.join("vendor").join("vosk");

    // 1) Tell the MSVC linker where the Vosk import lib lives (vosk.lib / libvosk.lib).
    println!("cargo:rustc-link-search=native={}", vendor.display());

    // 2) Copy ALL Vosk runtime DLLs next to the final binary so it loads at runtime.
    //    libvosk.dll is a MinGW build and depends on the libstdc++/libgcc/winpthread DLLs.
    //    OUT_DIR = target/<profile>/build/<pkg>/out  ->  ancestors[3] = target/<profile>
    const RUNTIME_DLLS: [&str; 4] = [
        "libvosk.dll",
        "libstdc++-6.dll",
        "libgcc_s_seh-1.dll",
        "libwinpthread-1.dll",
    ];
    if let Ok(out) = std::env::var("OUT_DIR") {
        if let Some(target_dir) = Path::new(&out).ancestors().nth(3) {
            for dll in RUNTIME_DLLS {
                let src = vendor.join(dll);
                let dst = target_dir.join(dll);
                if src.exists() {
                    if let Err(e) = std::fs::copy(&src, &dst) {
                        println!("cargo:warning=failed to copy {dll}: {e}");
                    }
                } else {
                    println!("cargo:warning={dll} not found in vendor/vosk");
                }
            }
        }
    }

    println!("cargo:rerun-if-changed=vendor/vosk/libvosk.dll");
    tauri_build::build();
}
