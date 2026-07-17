fn main() {
    let target = std::env::var("TARGET").expect("TARGET should be set");
    if target.contains("windows-msvc") {
        println!("cargo:rustc-link-arg=/FORCE:MULTIPLE");
    } else if target.contains("linux") || target.contains("android") {
        println!("cargo:rustc-link-arg=-Wl,--allow-multiple-definition");
    }
}
