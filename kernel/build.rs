fn main() {
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap();
    let linker_script = format!("{manifest_dir}/linker-x86_64.ld");
    println!("cargo:rustc-link-arg=-T{linker_script}");
    println!("cargo:rustc-link-arg=-no-pie");
    println!("cargo:rerun-if-changed={linker_script}");
    println!("cargo:rerun-if-changed={manifest_dir}/../target/user/programs/first/first");
}
