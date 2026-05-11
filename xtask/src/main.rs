use std::env;
use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

fn project_root() -> PathBuf {
    // CARGO_MANIFEST_DIR is <root>/xtask; its parent is the workspace root.
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask must be inside the workspace root")
        .to_path_buf()
}

fn run(cmd: &mut Command) {
    let status = cmd
        .status()
        .unwrap_or_else(|e| panic!("failed to run {:?}: {e}", cmd.get_program()));
    if !status.success() {
        panic!("{:?} exited with status {status}", cmd.get_program());
    }
}

/// Like `run` but discards stdout/stderr — used for noisy packaging tools.
fn run_quiet(cmd: &mut Command) {
    let status = cmd
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .unwrap_or_else(|e| panic!("failed to run {:?}: {e}", cmd.get_program()));
    if !status.success() {
        panic!("{:?} exited with status {status}", cmd.get_program());
    }
}

/// Resolve a tool: honour env override, then try bare name, then search llvm dirs.
fn find_tool(env_var: &str, bare: &str) -> String {
    if let Ok(v) = env::var(env_var) {
        return v;
    }
    if which(bare) {
        return bare.to_string();
    }
    // Search versioned LLVM installations
    for ver in (10u32..=30).rev() {
        let p = format!("/usr/lib/llvm{ver}/bin/{bare}");
        if std::path::Path::new(&p).exists() {
            return p;
        }
    }
    // Fall back to bare name and let the OS report the error
    bare.to_string()
}

fn which(prog: &str) -> bool {
    Command::new("sh")
        .args(["-c", &format!("command -v {prog}")])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn cc() -> String {
    find_tool("CC", "clang")
}
fn ld() -> String {
    find_tool("LD", "ld.lld")
}
fn ar() -> String {
    // Prefer llvm-ar; fall back to system ar (compatible for static archives)
    if let Ok(v) = env::var("AR") {
        return v;
    }
    if which("llvm-ar") {
        return "llvm-ar".to_string();
    }
    for ver in (10u32..=30).rev() {
        let p = format!("/usr/lib/llvm{ver}/bin/llvm-ar");
        if std::path::Path::new(&p).exists() {
            return p;
        }
    }
    "ar".to_string()
}

fn ensure_limine(root: &Path) {
    let limine_dir = root.join("target/limine");
    let limine_bin = limine_dir.join("limine");

    if !limine_bin.exists() {
        println!("Cloning limine bootloader…");
        run(Command::new("git").args([
            "clone",
            "https://github.com/limine-bootloader/limine.git",
            "--branch=v10.x-binary",
            "--depth=1",
            limine_dir.to_str().unwrap(),
        ]));
        run(Command::new("make").current_dir(&limine_dir));
    }
}

fn build_kernel(root: &Path, release: bool) {
    println!("Building kernel…");
    let mut cmd = Command::new("cargo");
    cmd.current_dir(root).args([
        "-Z",
        "build-std=core,alloc",
        "build",
        "--package",
        "kernel",
        "--target",
        "x86_64-unknown-none",
    ]);
    if release {
        cmd.arg("--release");
    }
    run(&mut cmd);
}

fn kernel_bin(root: &Path, release: bool) -> PathBuf {
    let profile_dir = if release { "release" } else { "debug" };
    root.join("target")
        .join("x86_64-unknown-none")
        .join(profile_dir)
        .join("kernel")
}

const GREEN: &str = "\x1b[32m";
const RED: &str = "\x1b[31m";
const YELLOW: &str = "\x1b[33m";
const BOLD: &str = "\x1b[1m";
const RESET: &str = "\x1b[0m";

// QEMU exit code for QemuExitCode::Success (0x10): (0x10 << 1) | 1 = 33
const TEST_SUCCESS_EXIT: i32 = 33;

/// Discover test binaries by scanning `kernel/src/bin/` for `test_*.rs` files.
/// No [[bin]] entries needed — Cargo auto-discovers everything in src/bin/.
fn discover_test_bins(root: &Path) -> Vec<String> {
    let bin_dir = root.join("kernel/src/bin");
    let mut names: Vec<String> = fs::read_dir(&bin_dir)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", bin_dir.display()))
        .filter_map(|entry| {
            let entry = entry.ok()?;
            let name = entry.file_name().into_string().ok()?;
            let stem = name.strip_suffix(".rs")?;
            stem.starts_with("test_").then(|| stem.to_string())
        })
        .collect();
    names.sort();
    names
}

/// Build all test binaries in a single cargo invocation to avoid redundant
/// library recompilation and duplicate warnings.
fn build_all_test_kernels(root: &Path, bins: &[String]) {
    let mut cmd = Command::new("cargo");
    cmd.current_dir(root).args([
        "-Z",
        "build-std=core,alloc",
        "build",
        "--package",
        "kernel",
        "--target",
        "x86_64-unknown-none",
    ]);
    for name in bins {
        cmd.args(["--bin", name]);
    }
    run(&mut cmd);
}

/// Package a pre-built test binary into a bootable ISO.
/// Suppresses xorriso/limine-install output — they're noisy and add no signal.
fn package_test_iso(root: &Path, test_name: &str) -> PathBuf {
    let iso_root = root.join(format!("target/test_iso_root_{test_name}"));
    if iso_root.exists() {
        fs::remove_dir_all(&iso_root).unwrap();
    }
    fs::create_dir_all(iso_root.join("boot/limine")).unwrap();
    fs::create_dir_all(iso_root.join("EFI/BOOT")).unwrap();

    let limine = root.join("target/limine");
    let bin = root
        .join("target/x86_64-unknown-none/debug")
        .join(test_name);

    fs::copy(&bin, iso_root.join("boot/kernel")).unwrap();
    fs::copy(
        root.join("bootloader/limine.conf"),
        iso_root.join("boot/limine/limine.conf"),
    )
    .unwrap();
    for f in [
        "limine-bios.sys",
        "limine-bios-cd.bin",
        "limine-uefi-cd.bin",
    ] {
        fs::copy(limine.join(f), iso_root.join("boot/limine").join(f)).unwrap();
    }
    fs::copy(
        limine.join("BOOTX64.EFI"),
        iso_root.join("EFI/BOOT/BOOTX64.EFI"),
    )
    .unwrap();
    fs::copy(
        limine.join("BOOTIA32.EFI"),
        iso_root.join("EFI/BOOT/BOOTIA32.EFI"),
    )
    .unwrap();

    let iso = root.join(format!("target/test_{test_name}.iso"));
    run_quiet(Command::new("xorriso").args([
        "-as",
        "mkisofs",
        "-b",
        "boot/limine/limine-bios-cd.bin",
        "-no-emul-boot",
        "-boot-load-size",
        "4",
        "-boot-info-table",
        "--efi-boot",
        "boot/limine/limine-uefi-cd.bin",
        "-efi-boot-part",
        "--efi-boot-image",
        "--protective-msdos-label",
        iso_root.to_str().unwrap(),
        "-o",
        iso.to_str().unwrap(),
    ]));
    run_quiet(
        Command::new(limine.join("limine"))
            .arg("bios-install")
            .arg(&iso),
    );
    fs::remove_dir_all(&iso_root).unwrap();
    iso
}

struct TestResult {
    passed: bool,
    exit_code: Option<i32>,
    serial: String,
}

fn run_test(iso: &Path, ovmf_code: &Path, ovmf_vars: &Path) -> TestResult {
    let out = Command::new("timeout")
        .arg("10")
        .arg("qemu-system-x86_64")
        .args([
            "-M",
            "q35",
            "-accel",
            "kvm",
            "-cpu",
            "qemu64,+tsc-deadline,+apic",
        ])
        .args([
            "-drive",
            &format!(
                "if=pflash,unit=0,format=raw,file={},readonly=on",
                ovmf_code.display()
            ),
        ])
        .args([
            "-drive",
            &format!("if=pflash,unit=1,format=raw,file={}", ovmf_vars.display()),
        ])
        .args(["-cdrom", iso.to_str().unwrap()])
        .args(["-device", "isa-debug-exit,iobase=0xf4,iosize=0x04"])
        .args(["-serial", "stdio", "-display", "none", "-no-reboot"])
        .args(["-m", "256M"])
        // Capture serial (stdout). Discard QEMU's own stderr noise.
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .unwrap_or_else(|e| panic!("failed to spawn qemu: {e}"));

    let exit_code = out.status.code();
    TestResult {
        passed: exit_code == Some(TEST_SUCCESS_EXIT),
        exit_code,
        serial: String::from_utf8_lossy(&out.stdout).into_owned(),
    }
}

fn run_tests(root: &Path) {
    let bins = discover_test_bins(root);
    ensure_limine(root);
    build_user(root);
    println!("{BOLD}Building test kernels…{RESET}");
    build_all_test_kernels(root, &bins);

    let (ovmf_code, ovmf_vars) = ovmf(root);

    println!();
    let mut passed_names: Vec<&str> = Vec::new();
    let mut failed_names: Vec<&str> = Vec::new();

    for name in &bins {
        print!("  {BOLD}{name}{RESET} ... ");
        std::io::stdout().flush().unwrap();

        let iso = package_test_iso(root, name);
        let result = run_test(&iso, &ovmf_code, &ovmf_vars);

        if result.passed {
            println!("{GREEN}ok{RESET}");
            passed_names.push(name);
        } else {
            println!(
                "{RED}FAILED{RESET} (exit code: {})",
                result
                    .exit_code
                    .map_or("none (timeout)".to_string(), |c| c.to_string())
            );
            if result.serial.is_empty() {
                println!("  {YELLOW}(no serial output — kernel may not have started){RESET}");
            } else {
                println!("  {YELLOW}---- serial output ----{RESET}");
                for line in result.serial.lines() {
                    println!("  {line}");
                }
                println!("  {YELLOW}-----------------------{RESET}");
            }
            failed_names.push(name);
        }
    }

    println!();
    if failed_names.is_empty() {
        println!(
            "{GREEN}{BOLD}test result: ok.{RESET} {} passed; 0 failed",
            passed_names.len()
        );
    } else {
        println!("{BOLD}tests:{RESET}");
        for name in &passed_names {
            println!("  {GREEN}ok{RESET}    {name}");
        }
        for name in &failed_names {
            println!("  {RED}FAILED{RESET} {name}");
        }
        println!();
        println!(
            "{RED}{BOLD}test result: FAILED.{RESET} {} passed; {} failed",
            passed_names.len(),
            failed_names.len()
        );
        std::process::exit(1);
    }
}

fn cflags() -> Vec<&'static str> {
    vec![
        "--target=x86_64-unknown-elf",
        "-ffreestanding",
        "-nostdlib",
        "-nostdinc",
        "-fno-builtin",
        "-fno-stack-protector",
        "-fno-pie",
        "-mno-red-zone",
        "-m64",
        "-Wall",
        "-Wextra",
        "-I.",
        "-g",
    ]
}

fn compile_c(src: &Path, out: &Path, extra_flags: &[&str]) {
    let mut cmd = Command::new(cc());
    cmd.args(cflags())
        .args(extra_flags)
        .arg("-c")
        .arg(src)
        .arg("-o")
        .arg(out);
    run(&mut cmd);
}

fn build_user(root: &Path) {
    println!("Building user programs…");
    let user_dir = root.join("user");
    let build_dir = root.join("target/user");

    fs::create_dir_all(build_dir.join("libc")).unwrap();

    let crt0_o = build_dir.join("crt0.o");
    compile_c(&user_dir.join("crt0.c"), &crt0_o, &[]);

    let syscall_o = build_dir.join("libc/syscall.o");
    compile_c(&user_dir.join("libc/syscall.c"), &syscall_o, &[]);

    let libc_a = build_dir.join("libc.a");
    run(Command::new(ar())
        .args(["rcs"])
        .arg(&libc_a)
        .arg(&syscall_o));

    // Each subdirectory of user/programs/ that contains a <name>.c file is
    // treated as a program.  To add a new program, just create
    // user/programs/<name>/<name>.c — no changes to xtask needed.

    let programs_dir = user_dir.join("programs");
    let entries = fs::read_dir(&programs_dir)
        .unwrap_or_else(|e| panic!("can't read {}: {e}", programs_dir.display()));

    for entry in entries {
        let entry = entry.unwrap();
        if !entry.file_type().unwrap().is_dir() {
            continue;
        }
        let name = entry.file_name();
        let name = name.to_str().unwrap();
        let src = entry.path().join(format!("{name}.c"));
        if !src.exists() {
            continue;
        }

        let out_dir = build_dir.join("programs").join(name);
        fs::create_dir_all(&out_dir).unwrap();

        let obj = out_dir.join(format!("{name}.o"));
        compile_c(&src, &obj, &[]);

        run(Command::new(ld())
            .args(["-T", "linker.ld", "-nostdlib", "-static", "-no-pie"])
            .arg("-o")
            .arg(out_dir.join(name))
            .arg(&crt0_o)
            .arg(&obj)
            .arg(&libc_a)
            .current_dir(&user_dir));
    }
}

fn build_iso(root: &Path) {
    ensure_limine(root);
    build_user(root);
    build_kernel(root, false);

    println!("Building ISO image…");
    let iso_root = root.join("target/iso_root");

    // Remove and recreate staging directory
    if iso_root.exists() {
        fs::remove_dir_all(&iso_root).unwrap();
    }
    fs::create_dir_all(iso_root.join("boot/limine")).unwrap();
    fs::create_dir_all(iso_root.join("EFI/BOOT")).unwrap();

    let limine = root.join("target/limine");
    let kernel_bin = kernel_bin(root, false);

    fs::copy(&kernel_bin, iso_root.join("boot/kernel")).unwrap();
    fs::copy(
        root.join("bootloader/limine.conf"),
        iso_root.join("boot/limine/limine.conf"),
    )
    .unwrap();

    for f in [
        "limine-bios.sys",
        "limine-bios-cd.bin",
        "limine-uefi-cd.bin",
    ] {
        fs::copy(limine.join(f), iso_root.join("boot/limine").join(f)).unwrap();
    }
    fs::copy(
        limine.join("BOOTX64.EFI"),
        iso_root.join("EFI/BOOT/BOOTX64.EFI"),
    )
    .unwrap();
    fs::copy(
        limine.join("BOOTIA32.EFI"),
        iso_root.join("EFI/BOOT/BOOTIA32.EFI"),
    )
    .unwrap();

    let iso_path = root.join("target/template-x86_64.iso");

    run(Command::new("xorriso")
        .args([
            "-as",
            "mkisofs",
            "-b",
            "boot/limine/limine-bios-cd.bin",
            "-no-emul-boot",
            "-boot-load-size",
            "4",
            "-boot-info-table",
            "--efi-boot",
            "boot/limine/limine-uefi-cd.bin",
            "-efi-boot-part",
            "--efi-boot-image",
            "--protective-msdos-label",
        ])
        .arg(&iso_root)
        .arg("-o")
        .arg(&iso_path));

    run(Command::new(root.join("target/limine/limine"))
        .arg("bios-install")
        .arg(&iso_path));

    fs::remove_dir_all(&iso_root).unwrap();
    println!("ISO ready: {}", iso_path.display());
}

fn build_hdd(root: &Path) {
    ensure_limine(root);
    build_user(root);
    build_kernel(root, false);

    println!("Building HDD image…");
    let hdd = root.join("target/template-x86_64.hdd");
    let limine = root.join("target/limine");
    let kernel_bin = kernel_bin(root, false);

    if hdd.exists() {
        fs::remove_file(&hdd).unwrap();
    }

    run(Command::new("dd")
        .args(["if=/dev/zero", "bs=1M", "count=0", "seek=64"])
        .arg(format!("of={}", hdd.display())));

    run(Command::new("parted")
        .args(["-s"])
        .arg(&hdd)
        .args(["mklabel", "gpt"]));
    run(Command::new("parted")
        .args(["-s"])
        .arg(&hdd)
        .args(["mkpart", "primary", "2048s", "4095s"]));
    run(Command::new("parted")
        .args(["-s"])
        .arg(&hdd)
        .args(["set", "1", "bios_grub", "on"]));
    run(Command::new("parted")
        .args(["-s"])
        .arg(&hdd)
        .args(["mkpart", "ESP", "fat32", "4096s", "100%"]));
    run(Command::new("parted")
        .args(["-s"])
        .arg(&hdd)
        .args(["set", "2", "esp", "on"]));

    run(Command::new(limine.join("limine"))
        .arg("bios-install")
        .arg(&hdd));

    run(Command::new("mkfs.fat")
        .args(["-F", "32", "--offset=4096"])
        .arg(&hdd));

    let hdd_str = hdd.to_str().unwrap();
    let hdd_at_2m = format!("{hdd_str}@@2M");

    for dir in ["::/EFI", "::/EFI/BOOT", "::/boot", "::/boot/limine"] {
        run(Command::new("mmd").arg("-i").arg(&hdd_at_2m).arg(dir));
    }

    run(Command::new("mcopy")
        .args(["-i"])
        .arg(&hdd_at_2m)
        .arg(&kernel_bin)
        .arg("::/boot"));
    run(Command::new("mcopy")
        .args(["-i"])
        .arg(&hdd_at_2m)
        .arg(root.join("bootloader/limine.conf"))
        .arg("::/boot/limine"));
    run(Command::new("mcopy")
        .args(["-i"])
        .arg(&hdd_at_2m)
        .arg(limine.join("limine-uefi-cd.bin"))
        .arg("::/boot/limine"));
    run(Command::new("mcopy")
        .args(["-i"])
        .arg(&hdd_at_2m)
        .arg(limine.join("limine-bios.sys"))
        .arg("::/boot/limine"));
    run(Command::new("mcopy")
        .args(["-i"])
        .arg(&hdd_at_2m)
        .arg(limine.join("BOOTX64.EFI"))
        .arg("::/EFI/BOOT"));
    run(Command::new("mcopy")
        .args(["-i"])
        .arg(&hdd_at_2m)
        .arg(limine.join("BOOTIA32.EFI"))
        .arg("::/EFI/BOOT"));

    println!("HDD ready: {}", hdd.display());
}

fn fat32_image(root: &Path) {
    let img = root.join("target/test_disk_image.fat32.img");
    run(Command::new(root.join("scripts/create_fat32_image.sh"))
        .arg(&img)
        .arg("16")
        .current_dir(root));
}

fn qemu_flags() -> Vec<String> {
    env::var("QEMUFLAGS")
        .unwrap_or_else(|_| "-m 2G".to_string())
        .split_whitespace()
        .map(str::to_string)
        .collect()
}

fn ensure_ovmf(root: &Path) {
    let ovmf_dir = root.join("target/ovmf");
    let code_dst = ovmf_dir.join("ovmf-code-x86_64.fd");
    let vars_dst = ovmf_dir.join("ovmf-vars-x86_64.fd");

    if code_dst.exists() && vars_dst.exists() {
        return;
    }

    fs::create_dir_all(&ovmf_dir).unwrap();

    // (dir, code_filename, vars_filename) candidates ordered by preference.
    let candidates: &[(&str, &str, &str)] = &[
        // Debian / Ubuntu (ovmf package)
        ("/usr/share/OVMF", "OVMF_CODE_4M.fd", "OVMF_VARS_4M.fd"),
        ("/usr/share/OVMF", "OVMF_CODE.fd", "OVMF_VARS.fd"),
        // Arch Linux (edk2-ovmf package)
        ("/usr/share/edk2/x64", "OVMF_CODE.4m.fd", "OVMF_VARS.4m.fd"),
        ("/usr/share/edk2/x64", "OVMF_CODE.fd", "OVMF_VARS.fd"),
        ("/usr/share/edk2-ovmf/x64", "OVMF_CODE.fd", "OVMF_VARS.fd"),
        // Fedora / RHEL (edk2-ovmf package)
        ("/usr/share/edk2/ovmf", "OVMF_CODE.fd", "OVMF_VARS.fd"),
        // openSUSE (qemu-ovmf-x86_64 package)
        (
            "/usr/share/qemu",
            "ovmf-x86_64-code.bin",
            "ovmf-x86_64-vars.bin",
        ),
    ];

    for (dir, code_name, vars_name) in candidates {
        let base = Path::new(dir);
        let code_path = base.join(code_name);
        let vars_path = base.join(vars_name);
        if code_path.exists() && vars_path.exists() {
            println!("Found OVMF in {dir}, cop  ing to target/ovmf/…");
            fs::copy(&code_path, &code_dst).unwrap();
            fs::copy(&vars_path, &vars_dst).unwrap();
            return;
        }
    }

    panic!(
        "OVMF firmware not found. Install it with your package manager:\n\
         \n  Debian/Ubuntu:  sudo apt install ovmf\
         \n  Arch Linux:     sudo pacman -S edk2-ovmf\
         \n  Fedora/RHEL:    sudo dnf install edk2-ovmf\
         \n  openSUSE:       sudo zypper install qemu-ovmf-x86_64"
    );
}

fn ovmf(root: &Path) -> (PathBuf, PathBuf) {
    ensure_ovmf(root);
    let ovmf_dir = root.join("target/ovmf");
    (
        ovmf_dir.join("ovmf-code-x86_64.fd"),
        ovmf_dir.join("ovmf-vars-x86_64.fd"),
    )
}

fn run_iso(root: &Path) {
    build_iso(root);
    let (code, vars) = ovmf(root);
    let iso = root.join("target/template-x86_64.iso");

    let mut cmd = Command::new("qemu-system-x86_64");
    cmd.args([
        "-M",
        "q35",
        "-accel",
        "kvm",
        "-cpu",
        "qemu64,+tsc-deadline,+apic",
    ])
    .args([
        "-drive",
        &format!(
            "if=pflash,unit=0,format=raw,file={},readonly=on",
            code.display()
        ),
    ])
    .args([
        "-drive",
        &format!("if=pflash,unit=1,format=raw,file={}", vars.display()),
    ])
    .args(["-cdrom", iso.to_str().unwrap()])
    .args(["-device", "isa-debug-exit,iobase=0xf4,iosize=0x04"])
    .args(["-serial", "stdio", "-no-reboot"])
    .args(["-monitor", "telnet:127.0.0.1:1234,server,nowait"])
    .args(qemu_flags());
    run(&mut cmd);
}

fn run_hdd(root: &Path) {
    build_hdd(root);
    let (code, vars) = ovmf(root);
    let hdd = root.join("target/template-x86_64.hdd");
    let fat32 = root.join("target/test_disk_image.fat32.img");

    let mut cmd = Command::new("qemu-system-x86_64");
    cmd.args(["-M", "q35"])
        .args([
            "-drive",
            &format!(
                "if=pflash,unit=0,format=raw,file={},readonly=on",
                code.display()
            ),
        ])
        .args([
            "-drive",
            &format!("if=pflash,unit=1,format=raw,file={}", vars.display()),
        ])
        .args(["-hda", hdd.to_str().unwrap()])
        .args(["-hdb", fat32.to_str().unwrap()])
        .args(["-device", "isa-debug-exit,iobase=0xf4,iosize=0x04"])
        .args(["-serial", "stdio", "-no-reboot"])
        .args(qemu_flags());
    run(&mut cmd);
}

fn run_fs(root: &Path) {
    build_iso(root);
    let (code, vars) = ovmf(root);
    let iso = root.join("target/template-x86_64.iso");
    let fat32 = root.join("target/test_disk_image.fat32.img");

    let mut cmd = Command::new("qemu-system-x86_64");
    cmd.args(["-M", "q35"])
        .args([
            "-drive",
            &format!(
                "if=pflash,unit=0,format=raw,file={},readonly=on",
                code.display()
            ),
        ])
        .args([
            "-drive",
            &format!("if=pflash,unit=1,format=raw,file={}", vars.display()),
        ])
        .args(["-cdrom", iso.to_str().unwrap()])
        .args(["-drive", &format!("file={},format=raw", fat32.display())])
        .args(["-device", "isa-debug-exit,iobase=0xf4,iosize=0x04"])
        .args(["-serial", "stdio", "-no-reboot"])
        .args(qemu_flags());
    run(&mut cmd);
}

fn run_bios(root: &Path) {
    build_iso(root);
    let iso = root.join("target/template-x86_64.iso");

    let mut cmd = Command::new("qemu-system-x86_64");
    cmd.args(["-M", "q35", "-cdrom", iso.to_str().unwrap(), "-boot", "d"])
        .args(qemu_flags());
    run(&mut cmd);
}

fn clean(root: &Path) {
    // 'cargo clean' removes target/ entirely (kernel, user/target, images)
    run(Command::new("cargo").arg("clean").current_dir(root));
    // Also remove xtask's own build artefacts (not part of the workspace target)
    let xtask_target = root.join("xtask").join("target");
    if xtask_target.exists() {
        fs::remove_dir_all(&xtask_target)
            .unwrap_or_else(|e| panic!("failed to remove {}: {e}", xtask_target.display()));
    }
}

fn fmt(root: &Path, extra: &[String]) {
    for dir in [root, &root.join("xtask")] {
        run(Command::new("cargo")
            .args(["fmt", "--all"])
            .args(extra)
            .current_dir(dir));
    }
}

/// Split `extra` on `--`, returning (cargo_args, lint_args).
fn split_extra(extra: &[String]) -> (&[String], &[String]) {
    if let Some(pos) = extra.iter().position(|a| a == "--") {
        (&extra[..pos], &extra[pos + 1..])
    } else {
        (extra, &[])
    }
}

fn clippy(root: &Path, extra: &[String]) {
    let (cargo_args, lint_args) = split_extra(extra);
    for dir in [root, &root.join("xtask")] {
        run(Command::new("cargo")
            .args(["clippy", "--all-features"])
            .args(cargo_args)
            .arg("--")
            .args(["-D", "warnings"])
            .args(lint_args)
            .current_dir(dir));
    }
}

fn print_usage() {
    eprintln!(
        "Usage: cargo xtask <TASK>

Tasks:
  build        Build kernel and user programs
  iso          Build bootable ISO image (default)
  hdd          Build bootable HDD image
  run          Build ISO and launch in QEMU
  run-hdd      Build HDD image and launch in QEMU
  run-fs       Build ISO + FAT32 test disk and launch in QEMU
  run-bios     Build ISO and launch in QEMU (BIOS mode, no OVMF)
  test         Build and run all integration tests in QEMU
  fat32-image  Create test FAT32 disk image
  clean        Remove all build artefacts
  fmt          Format all code (workspace + xtask)  [extra args forwarded to cargo fmt]
  clippy       Lint all code (workspace + xtask)  [cargo args | -- lint flags]

Environment variables:
  QEMUFLAGS    Extra flags appended to QEMU (default: -m 2G)
"
    );
}

fn main() {
    let root = project_root();
    let mut args = env::args().skip(1);
    let task = args.next();
    let extra: Vec<String> = args.collect();

    match task.as_deref() {
        Some("build") => {
            build_user(&root);
            build_kernel(&root, false);
        }
        Some("iso") | None => build_iso(&root),
        Some("hdd") => build_hdd(&root),
        Some("run") => run_iso(&root),
        Some("run-hdd") => run_hdd(&root),
        Some("run-fs") => run_fs(&root),
        Some("run-bios") => run_bios(&root),
        Some("test") => run_tests(&root),
        Some("fat32-image") => fat32_image(&root),
        Some("clean") => clean(&root),
        Some("fmt") => fmt(&root, &extra),
        Some("clippy") => clippy(&root, &extra),
        Some(other) => {
            eprintln!("Unknown task: {other}\n");
            print_usage();
            std::process::exit(1);
        }
    }
}
