use std::env;
use std::path::PathBuf;

fn main() {
    let target = env::var("TARGET").expect("TARGET should be set");
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let libs_dir = manifest_dir
        .parent()
        .expect("storage-support should live under crates/")
        .parent()
        .expect("workspace root should exist")
        .join("libs")
        .join(&target);

    println!("cargo:rerun-if-env-changed=TARGET");
    println!("cargo:rerun-if-changed={}", libs_dir.display());
    println!("cargo:rustc-link-search=native={}", libs_dir.display());
    println!("cargo:rustc-link-lib=static=torchnexus_agent_storage");
    if target.contains("windows-msvc") {
        println!("cargo:rustc-link-arg=/FORCE:MULTIPLE");
    } else if target.contains("linux") || target.contains("android") {
        println!("cargo:rustc-link-arg=-Wl,--allow-multiple-definition");
    }
}
