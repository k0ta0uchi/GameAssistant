use std::env;
use std::fs;
use std::path::PathBuf;

fn main() {
    // `muda` (used by Tauri) calls TaskDialogIndirect, which is exported by
    // the version 6 Common Controls activation context only. Embed one
    // activation manifest into every Windows artifact so both the application
    // and Cargo-generated test harnesses load ComCtl32 v6 before `main`.
    #[cfg(windows)]
    {
        let manifest_dir = PathBuf::from(
            env::var_os("CARGO_MANIFEST_DIR").expect("manifest dir for test manifest"),
        );
        let resource = manifest_dir.join("resources").join("common-controls.rc");
        let manifest = manifest_dir
            .join("resources")
            .join("common-controls.manifest");
        if resource.is_file() {
            // Tauri's resource file is configured below without its own
            // manifest. This resource is therefore the single RT_MANIFEST for
            // both application and test artifacts; adding it to every target
            // is intentional and avoids duplicate-resource linker failures.
            embed_resource::compile_for_everything(&resource, embed_resource::NONE)
                .manifest_required()
                .expect("embed Common Controls v6 manifest");
            println!("cargo:rerun-if-changed={}", resource.display());
            println!("cargo:rerun-if-changed={}", manifest.display());
        }
    }

    // The optional server is embedded only when the release maintainer places
    // the pinned Windows binary in resources. Development and CI builds stay
    // self-contained without requiring a large binary checkout.
    let manifest_dir = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("manifest dir"));
    let runtime_dir = manifest_dir.join("resources").join("llama");
    let runtime_files = [
        "llama-server.exe",
        "llama.dll",
        "mtmd.dll",
        "ggml.dll",
        "ggml-base.dll",
        "ggml-vulkan.dll",
        "ggml-rpc.dll",
        "ggml-cpu-x64.dll",
        "ggml-cpu-alderlake.dll",
        "ggml-cpu-haswell.dll",
        "ggml-cpu-icelake.dll",
        "ggml-cpu-sandybridge.dll",
        "ggml-cpu-sapphirerapids.dll",
        "ggml-cpu-skylakex.dll",
        "ggml-cpu-sse42.dll",
        "libomp140.x86_64.dll",
        "libcurl-x64.dll",
        "LICENSE-curl",
        "LICENSE-httplib",
        "LICENSE-jsonhpp",
        "LICENSE-linenoise",
        "NOTICE-llama.cpp.txt",
    ];
    let out_dir = PathBuf::from(env::var_os("OUT_DIR").expect("out dir"));
    let generated = out_dir.join("llama_server_embed.rs");
    let mut entries = Vec::new();
    for file_name in runtime_files {
        let path = runtime_dir.join(file_name);
        if path.is_file() {
            let escaped = path
                .to_string_lossy()
                .replace('\\', "\\\\")
                .replace('"', "\\\"");
            entries.push(format!(
                "(\"{}\", include_bytes!(\"{}\"))",
                file_name, escaped
            ));
        }
        println!("cargo:rerun-if-changed={}", path.display());
    }
    let source = format!(
        "pub static LLAMA_RUNTIME_FILES: &[(&str, &[u8])] = &[{}];\n",
        entries.join(",")
    );
    fs::write(generated, source).expect("write llama server embed manifest");
    // The default Tauri resource also contains a Common Controls manifest.
    // Disable only that part and keep its icon/version resources, otherwise
    // the custom manifest above would be linked twice (CVTRES CVT1100/LNK1123).
    tauri_build::try_build(
        tauri_build::Attributes::new()
            .windows_attributes(tauri_build::WindowsAttributes::new_without_app_manifest()),
    )
    .expect("failed to run tauri build helpers")
}
