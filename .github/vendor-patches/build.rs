use cmake;

fn main() {
    // ---------
    // - CMAKE -
    // ---------
    let out_dir = cmake::Config::new("cmake")
        .build_target("lib_abl_link_c")
        .build();

    // MSVC (windows + msvc env): Visual Studio puts output in OUT_DIR/build/{Debug,Release,...}
    // MinGW / GNU on Windows: Makefile generator puts output directly in OUT_DIR/build
    // Non-Windows: Output directly to OUT_DIR/build
    #[cfg(all(target_os = "windows", not(target_env = "gnu")))]
    let build_dir = out_dir
        .join("build")
        .join(cmake::Config::new("cmake").get_profile());

    #[cfg(any(not(target_os = "windows"), target_env = "gnu"))]
    let build_dir = out_dir.join("build");

    println!("cargo:rustc-link-search=native={}", build_dir.display());
    println!("cargo:rustc-link-lib=static=lib_abl_link_c");

    // MACOS: Link standard C++ lib, to prevent linker errors
    #[cfg(target_os = "macos")]
    println!("cargo:rustc-link-lib=c++");

    // LINUX/GNU: Link standard C++ lib
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    println!("cargo:rustc-link-lib=stdc++");

    // WINDOWS + MinGW: Link C++ and networking libs required by Ableton Link
    // Static-link libstdc++ and libgcc so the binary is self-contained.
    #[cfg(all(target_os = "windows", target_env = "gnu"))]
    {
        // Point the linker at the MinGW static libs directory.
        // Try the well-known GCC lib path first; fall back to dynamic linking
        // if the static archive is not found.
        let gcc_lib =
            std::path::PathBuf::from(std::env::var("MINGW_LIB_DIR").unwrap_or_else(|_| {
                // Auto-detect: ask gcc where its libs live
                let out = std::process::Command::new("gcc")
                    .args(["-print-file-name=libstdc++.a"])
                    .output();
                match out {
                    Ok(o) if o.status.success() => {
                        let p = String::from_utf8_lossy(&o.stdout).trim().to_string();
                        let path = std::path::PathBuf::from(&p);
                        if path.exists() {
                            path.parent().unwrap().to_string_lossy().to_string()
                        } else {
                            String::new()
                        }
                    }
                    _ => String::new(),
                }
            }));
        if gcc_lib.exists() {
            println!("cargo:rustc-link-search=native={}", gcc_lib.display());
            println!("cargo:rustc-link-lib=static=stdc++");
            println!("cargo:rustc-link-lib=static=gcc");
        } else {
            // Fallback: dynamic linking (user will need libstdc++-6.dll on PATH)
            println!("cargo:rustc-link-lib=stdc++");
        }
        println!("cargo:rustc-link-lib=iphlpapi");
    }

    // WINDOWS + MSVC: Link networking libs required by Ableton Link
    #[cfg(all(target_os = "windows", not(target_env = "gnu")))]
    {
        println!("cargo:rustc-link-lib=iphlpapi");
    }

    let out_dir_path = std::path::PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let bindings_dest = out_dir_path.join("link_bindings.rs");

    let manifest_dir = std::path::PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let bindings_src = manifest_dir.join("link_bindings.rs");

    if bindings_src.exists() {
        std::fs::copy(&bindings_src, &bindings_dest)
            .expect("Failed to copy link_bindings.rs to OUT_DIR");
    } else {
        println!(
            "cargo:warning=Pre-generated link_bindings.rs not found at {}",
            bindings_src.display()
        );
    }
}
