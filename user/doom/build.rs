use std::env;
use std::path::{Path, PathBuf};

include!("src/screen.rs");

const C_TARGET: &str = "x86_64-unknown-none-elf";
const LIBC_INCLUDE_DIR: &str = "libc/include";
const LIBC_SOURCE_DIR: &str = "libc/src";
const LIBCTEST_SOURCE_DIR: &str = "libctest";
const DOOM_SOURCE_DIR: &str = "doomgeneric/doomgeneric";
const DOOM_SUBMODULE_DIR: &str = "doomgeneric";
// the platform backends, sound and the files bigOS replaces are not compiled
const DOOM_EXCLUDED_PREFIXES: [&str; 1] = ["doomgeneric_"];
const DOOM_EXCLUDED_PARTS: [&str; 2] = ["_allegro", "_sdl"];
const DOOM_EXCLUDED_FILES: [&str; 3] = ["i_main.c", "doomdef.c", "icon.c"];
const COMPILER: &str = "clang";
const LIBRARY_NAME: &str = "doomc";
const DOOM_LIBRARY_NAME: &str = "doomgeneric";

const COMMON_FLAGS: [&str; 10] = [
    "-ffreestanding",
    "-nostdlibinc",
    "-fno-pic",
    "-fno-pie",
    "-mcmodel=small",
    "-mno-red-zone",
    "-mno-mmx",
    "-fno-stack-protector",
    "-std=gnu99",
    "-O2",
];

fn c_files_in(directory: &str) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(directory)
        .unwrap_or_else(|error| panic!("can't read {directory}: {error}"))
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|extension| extension == "c"))
        .collect();
    files.sort();

    files
}

fn base_build() -> cc::Build {
    let mut build = cc::Build::new();
    // cargo's target is the custom x86_64-user spec, cc cannot map it
    build
        .compiler(COMPILER)
        .target(C_TARGET)
        .host(&env::var("HOST").expect("cargo sets HOST"))
        .opt_level(2)
        .debug(false)
        .pic(false)
        .flag(format!("--target={C_TARGET}"))
        .include(LIBC_INCLUDE_DIR);
    for flag in COMMON_FLAGS {
        build.flag(flag);
    }

    build
}

fn is_compiled_doom_file(path: &Path) -> bool {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default();

    !DOOM_EXCLUDED_PREFIXES
        .iter()
        .any(|prefix| name.starts_with(prefix))
        && !DOOM_EXCLUDED_PARTS.iter().any(|part| name.contains(part))
        && !DOOM_EXCLUDED_FILES.contains(&name)
}

fn require_doom_submodule() {
    if !Path::new(DOOM_SOURCE_DIR).is_dir() {
        panic!(
            "the doomgeneric submodule is not initialized ({DOOM_SUBMODULE_DIR} is empty). \
             Run `cargo xtask doom` from the repository root, or `git submodule update --init user/doom/doomgeneric`"
        );
    }
}

fn main() {
    require_doom_submodule();

    println!("cargo:rerun-if-changed={LIBC_INCLUDE_DIR}");
    println!("cargo:rerun-if-changed={LIBC_SOURCE_DIR}");
    println!("cargo:rerun-if-changed={LIBCTEST_SOURCE_DIR}");
    println!("cargo:rerun-if-changed={DOOM_SOURCE_DIR}");
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=src/screen.rs");

    let mut libc_build = base_build();
    libc_build.warnings(true).extra_warnings(true);
    for file in c_files_in(LIBC_SOURCE_DIR)
        .into_iter()
        .chain(c_files_in(LIBCTEST_SOURCE_DIR))
    {
        libc_build.file(file);
    }
    libc_build.compile(LIBRARY_NAME);

    // vendored code, warnings in it are not ours to fix
    let mut doom_build = base_build();
    doom_build
        .warnings(false)
        .flag("-w")
        .define("DOOMGENERIC_RESX", DOOM_SCREEN_WIDTH.to_string().as_str())
        .define("DOOMGENERIC_RESY", DOOM_SCREEN_HEIGHT.to_string().as_str());
    for file in c_files_in(DOOM_SOURCE_DIR)
        .into_iter()
        .filter(|file| is_compiled_doom_file(file))
    {
        doom_build.file(file);
    }
    doom_build.compile(DOOM_LIBRARY_NAME);
}
