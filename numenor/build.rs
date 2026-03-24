use std::path::{Path, PathBuf};

/// Discover the llvm-ar bundled with the current nightly toolchain.
///
/// Cargo sets RUSTC to the full path of the rustc binary, e.g.:
///   /Users/bd/.rustup/toolchains/nightly-aarch64-apple-darwin/bin/rustc
///
/// From that we derive:
///   {toolchain_root}/lib/rustlib/{host}/bin/llvm-ar
///
/// On macOS the system `ar` cannot create ELF symbol tables, so rust-lld
/// fails to resolve symbols from cross-compiled C/C++ archives.  Using
/// llvm-ar produces a proper ELF archive index that rust-lld can scan.
fn find_llvm_ar() -> Option<PathBuf> {
    let rustc = std::env::var("RUSTC").ok()?;
    // rustc path: <toolchain_root>/bin/rustc → parent.parent = toolchain_root
    let toolchain_root = Path::new(&rustc).parent()?.parent()?;
    let host = std::env::var("HOST").ok()?;
    let llvm_ar = toolchain_root
        .join("lib/rustlib")
        .join(&host)
        .join("bin/llvm-ar");
    if llvm_ar.exists() {
        Some(llvm_ar)
    } else {
        eprintln!("cargo:warning=llvm-ar not found at {:?}; falling back to system ar", llvm_ar);
        None
    }
}

fn main() {
    let vendor_path = std::fs::canonicalize(Path::new("../vendor/llama.cpp")).unwrap();
    let src_path = vendor_path.join("src");
    let ggml_path = vendor_path.join("ggml/src");
    let ggml_cpu_path = vendor_path.join("ggml/src/ggml-cpu");
    let ggml_cpu_arm_path = ggml_cpu_path.join("arch/arm");

    let llvm_path = Path::new("../vendor/llvm-project");
    let libcxx_path = llvm_path.join("libcxx");
    let libcxxabi_path = llvm_path.join("libcxxabi");

    // Use llvm-ar if available (required on macOS to create ELF-indexed archives
    // that rust-lld can read; macOS system ar produces empty symbol tables for
    // cross-compiled ELF objects).
    let llvm_ar = find_llvm_ar();

    // --- C Build (ggml core + ggml-cpu C) ---
    let mut c_build = cc::Build::new();
    if let Some(ref ar) = llvm_ar {
        c_build.archiver(ar);
    }
    c_build
        .cpp(false)
        .target("aarch64-unknown-none")
        .flag("-std=c11")
        .flag("-O3")
        .flag("-ffunction-sections")
        .flag("-fdata-sections")
        .flag("-w") // Suppress warnings
        .include("src/cpp/include/c") 
        .include(vendor_path.join("include"))
        .include(vendor_path.join("ggml/include"))
        .include("src/cpp/include/overrides")
        .include(vendor_path.join("src"))
        .include(vendor_path.join("ggml/src"))
        .include(ggml_cpu_path.clone())
        .define("_LIBCPP_DISABLE_VISIBILITY_ANNOTATIONS", None)
        .define("_LIBCPP_DISABLE_AVAILABILITY", None)
        .define("GGML_USE_CPU", None)
        .define("GGML_SCHED_MAX_COPIES", Some("4"))
        .define("GGML_VERSION", Some("\"stub\""))
        .define("GGML_COMMIT", Some("\"stub\""))
        .file(ggml_path.join("ggml.c"))
        .file(ggml_path.join("ggml-alloc.c"))
        .file(ggml_path.join("ggml-quants.c"));

    // Add C files from ggml-cpu
    for entry in std::fs::read_dir(&ggml_cpu_path).expect("Failed to read ggml-cpu") {
        let entry = entry.expect("Failed to read entry");
        let path = entry.path();
        if let Some(ext) = path.extension().and_then(|s| s.to_str()) {
             if ext == "c" {
                 if path.is_file() {
                    c_build.file(path);
                 }
             }
        }

    // Add C files from ggml-cpu/arch/arm
    if ggml_cpu_arm_path.exists() {
        for entry in std::fs::read_dir(&ggml_cpu_arm_path).expect("Failed to read ggml-cpu/arch/arm") {
            let entry = entry.expect("Failed to read entry");
            let path = entry.path();
            if let Some(ext) = path.extension().and_then(|s| s.to_str()) {
                 if ext == "c" {
                     if path.is_file() {
                        c_build.file(path);
                     }
                 }
            }
        }
    }
    }
    
    c_build.compile("ggml");

    // --- C++ Build (llama.cpp + runtime + ggml-cpu C++) ---
    let mut cpp_build = cc::Build::new();
    if let Some(ref ar) = llvm_ar {
        cpp_build.archiver(ar);
    }
    cpp_build
        .cpp(true)
        .target("aarch64-unknown-none") // Critical for cross-compilation
        .flag("-std=c++20")
        .flag("-O3")
        .flag("-ffunction-sections")
        .flag("-fdata-sections")
        .flag("-w") // Suppress warnings
        .include("src/cpp/include/cpp")
        .include(libcxx_path.join("include"))
        .include(libcxxabi_path.join("include"))
        .include("src/cpp/include/c")
        .include(vendor_path.join("include"))
        .include(vendor_path.join("ggml/include"))
        .include("src/cpp/include/overrides")
        .include(vendor_path.join("src"))
        .include(vendor_path.join("ggml/src"))
        .include(ggml_cpu_path.clone())
        .define("_LIBCPP_DISABLE_VISIBILITY_ANNOTATIONS", None)
        .define("_LIBCPP_DISABLE_AVAILABILITY", None)
        .define("_LIBCPP_BUILDING_LIBRARY", None)
        .define("_LIBCPP_HAS_NO_THREADS", None)
        .define("_LIBCPP_PSTL_BACKEND_SERIAL", None)
        .define("GGML_USE_CPU", None)
        .define("GGML_SCHED_MAX_COPIES", Some("4"))
        .define("GGML_VERSION", Some("\"stub\"")) 
        .define("GGML_COMMIT", Some("\"stub\""))
        .define("LLAMA_NO_MMAP", None)
        .define("GGML_NO_MMAP", None)
        .cpp_link_stdlib(None) // Disable linking against libstdc++ (we provide our own via libcxx sources)
        
        // Runtime
        .file("src/cpp/shim.cpp")
        .file("src/cpp/llm_engine.cpp")
        .file("src/cpp/backend_reg.cpp") // Custom backend registry logic
        .file("src/cpp/chrono_stub.cpp") // Bare-metal chrono::system_clock::now()

        .file("src/cpp/errno.c")
        
        // LibCxx Runtime
        .file(libcxx_path.join("src/new.cpp"))
        .file(libcxx_path.join("src/memory.cpp"))
        .file(libcxx_path.join("src/system_error.cpp"))
        .file(libcxx_path.join("src/stdexcept.cpp"))
        .file(libcxx_path.join("src/functional.cpp"))
        .file(libcxx_path.join("src/hash.cpp"))
        .file(libcxx_path.join("src/string.cpp"))


        .file(libcxx_path.join("src/new_helpers.cpp"))
        .file(libcxx_path.join("src/typeinfo.cpp")) // RTTI

        // .file(libcxxabi_path.join("src/cxa_virtual.cpp")) // Stubbed in shim
        .file(libcxxabi_path.join("src/private_typeinfo.cpp")) // RTTI


        // GGML C++ Files
        .file(ggml_path.join("ggml-backend.cpp"))
        // .file(ggml_path.join("ggml-backend-reg.cpp")) // Requires filesystem
        // .file(ggml_path.join("ggml-backend-reg.cpp")) // Requires filesystem/dlfcn
        .file(ggml_path.join("gguf.cpp"));

    // Add C++ files from src/
    for entry in std::fs::read_dir(&src_path).expect("Failed to read llama.cpp/src") {
        let entry = entry.expect("Failed to read entry");
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) == Some("cpp") {
             let name = path.file_name().unwrap().to_str().unwrap();
             if name != "llama-quant.cpp" && name != "llama-model-saver.cpp" {
                 cpp_build.file(path);
             }
        }
    }

    // Add C++ files from src/models (Architectures)
    let models_path = src_path.join("models");
    if models_path.exists() {
        for entry in std::fs::read_dir(&models_path).expect("Failed to read llama.cpp/src/models") {
            let entry = entry.expect("Failed to read entry");
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) == Some("cpp") {
                 cpp_build.file(path);
            }
        }
    }

    // Add C++ files from ggml-cpu

    for entry in std::fs::read_dir(&ggml_cpu_path).expect("Failed to read ggml-cpu") {
        let entry = entry.expect("Failed to read entry");
        let path = entry.path();
        if let Some(ext) = path.extension().and_then(|s| s.to_str()) {
             if ext == "cpp" { // ONLY CPP
                 if path.is_file() {
                    cpp_build.file(path);
                 }
             }
        }
    }

    // Add C++ files from ggml-cpu/arch/arm
    if ggml_cpu_arm_path.exists() {
        for entry in std::fs::read_dir(&ggml_cpu_arm_path).expect("Failed to read ggml-cpu/arch/arm") {
            let entry = entry.expect("Failed to read entry");
            let path = entry.path();
            if let Some(ext) = path.extension().and_then(|s| s.to_str()) {
                 if ext == "cpp" {
                     if path.is_file() {
                        cpp_build.file(path);
                     }
                 }
            }
        }
    }

    cpp_build.compile("numenor_cpp");
        
    println!("cargo:rerun-if-changed=src/cpp/llm_engine.cpp");
    println!("cargo:rerun-if-changed=src/cpp/shim.cpp");
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=../vendor/llama.cpp/ggml/src/ggml.c");
    println!("cargo:rerun-if-changed=../vendor/llama.cpp/src/llama.cpp");
    println!("cargo:rerun-if-changed=src/cpp/llm_engine.h");
    println!("cargo:rerun-if-changed=../vendor/llama.cpp");
}
