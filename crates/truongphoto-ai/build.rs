fn main() {
    #[cfg(target_os = "macos")]
    {
        // Statically linked ONNX Runtime CoreML EP references `___isPlatformVersionAtLeast`,
        // which lives in Xcode's `libclang_rt.osx.a` runtime archive.
        if let Ok(output) = std::process::Command::new("clang")
            .arg("--print-resource-dir")
            .output()
        {
            if output.status.success() {
                let resource_dir = String::from_utf8_lossy(&output.stdout).trim().to_string();
                let darwin_lib_dir = std::path::Path::new(&resource_dir).join("lib/darwin");
                if darwin_lib_dir.join("libclang_rt.osx.a").is_file() {
                    println!(
                        "cargo:rustc-link-search=native={}",
                        darwin_lib_dir.display()
                    );
                    println!("cargo:rustc-link-lib=static=clang_rt.osx");
                }
            }
        }
    }
}
