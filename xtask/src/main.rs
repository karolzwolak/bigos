use std::env;
use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

fn project_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask must be inside workspace root")
        .to_path_buf()
}

fn kernel_max_cores(root: &Path) -> u8 {
    let path = root.join("kernel/src/lib.rs");
    let content =
        fs::read_to_string(&path).unwrap_or_else(|e| panic!("can't read {}: {e}", path.display()));
    for line in content.lines() {
        let t = line.trim();
        if t.starts_with("pub const MAX_CORES") {
            if let Some(eq) = t.find('=') {
                let after = &t[eq + 1..];
                let digits: String = after.chars().filter(|c| c.is_ascii_digit()).collect();
                if let Ok(v) = digits.parse::<u8>() {
                    if v >= 1 && v <= 64 {
                        return v;
                    }
                }
            }
        }
    }
    4
}

fn qemu_smp_arg(root: &Path) -> String {
    let cores = kernel_max_cores(root);
    format!("cores={},threads=1", cores)
}

fn run(cmd: &mut Command) {
    let status = cmd
        .status()
        .unwrap_or_else(|e| panic!("failed to run {:?}: {e}", cmd.get_program()));
    if !status.success() {
        panic!("{:?} exited with status {status}", cmd.get_program());
    }
}

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

fn find_tool(env_var: &str, bare: &str) -> String {
    if let Ok(v) = env::var(env_var) {
        return v;
    }
    if which(bare) {
        return bare.to_string();
    }
    for ver in (10u32..=30).rev() {
        let p = format!("/usr/lib/llvm{ver}/bin/{bare}");
        if Path::new(&p).exists() {
            return p;
        }
    }
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

fn ensure_limine(root: &Path) {
    let limine_dir = root.join("target/limine");
    let limine_bin = limine_dir.join("limine");
    if !limine_bin.exists() {
        if limine_dir.exists() {
            fs::remove_dir_all(&limine_dir).unwrap();
        }
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

fn limine_conf_path(root: &Path) -> PathBuf {
    let candidates = [
        root.join("limine.conf"),
        root.join("bootloader/limine.conf"),
    ];
    for p in &candidates {
        if p.exists() {
            return p.clone();
        }
    }
    candidates[0].clone()
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

const TEST_SUCCESS_EXIT: i32 = 33;
const TEST_FAILED_EXIT: i32 = 35;
const UNEXPECTED_EXIT: i32 = 0;

fn discover_test_bins(root: &Path) -> Vec<String> {
    let bin_dir = root.join("kernel/src/bin");
    let Ok(entries) = fs::read_dir(&bin_dir) else {
        return Vec::new();
    };
    let mut names: Vec<String> = entries
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

fn discover_usertest_bins(root: &Path) -> Vec<String> {
    let bin_dir = root.join("kernel/src/bin");
    let Ok(entries) = fs::read_dir(&bin_dir) else {
        return Vec::new();
    };
    let mut names: Vec<String> = entries
        .filter_map(|entry| {
            let entry = entry.ok()?;
            let name = entry.file_name().into_string().ok()?;
            let stem = name.strip_suffix(".rs")?;
            (stem.starts_with("test_user") || stem.starts_with("test_usermem"))
                .then(|| stem.to_string())
        })
        .collect();
    names.sort();
    names
}

/// Splits the --release switch off the test name filters, test kernels then build with the release profile
fn split_release_flag(args: &[String]) -> (bool, Vec<String>) {
    let release = args.iter().any(|arg| arg == "--release");
    let filters = args
        .iter()
        .filter(|arg| *arg != "--release")
        .cloned()
        .collect();

    (release, filters)
}

fn build_all_test_kernels(root: &Path, bins: &[String], release: bool) {
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
    for name in bins {
        cmd.args(["--bin", name]);
    }
    run(&mut cmd);
}

fn package_test_iso(root: &Path, test_name: &str, release: bool) -> PathBuf {
    let iso_root = root.join(format!("target/test_iso_root_{test_name}"));
    if iso_root.exists() {
        fs::remove_dir_all(&iso_root).unwrap();
    }
    fs::create_dir_all(iso_root.join("boot/limine")).unwrap();
    fs::create_dir_all(iso_root.join("EFI/BOOT")).unwrap();
    let limine = root.join("target/limine");
    let profile_dir = if release { "release" } else { "debug" };
    let bin = root
        .join("target/x86_64-unknown-none")
        .join(profile_dir)
        .join(test_name);
    fs::copy(&bin, iso_root.join("boot/kernel")).unwrap();
    fs::copy(
        limine_conf_path(root),
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

fn save_test_logs(root: &Path, test_name: &str, serial: &str, com2_raw_path: Option<&Path>) {
    let test_logs_root = root.join("test_logs");
    let test_dir = test_logs_root.join(test_name);
    let _ = fs::create_dir_all(&test_dir);
    // Also keep legacy flat log for convenience
    let _ = fs::create_dir_all(&test_logs_root);
    let _ = fs::write(test_logs_root.join(format!("{}.log", test_name)), serial);
    // Merge COM2 raw if present
    let mut combined = serial.to_string();
    if let Some(p) = com2_raw_path {
        if let Ok(com2) = fs::read_to_string(p) {
            if !com2.trim().is_empty() {
                combined.push_str("\n");
                combined.push_str(&com2);
            }
        }
    }
    // ANSI strip
    let ansi_re = regex_lite_strip_ansi(&combined);
    // Write all.txt
    let _ = fs::write(test_dir.join("all.txt"), &ansi_re);
    // Also write flat combined
    let _ = fs::write(test_logs_root.join(format!("{}.log", test_name)), &ansi_re);
    // Split per core / per pid like log_splitter.py
    let max_cores = kernel_max_cores(root) as usize;
    let mut core_writers: Vec<Option<std::fs::File>> = (0..max_cores).map(|_| None).collect();
    for i in 0..max_cores {
        let path = test_dir.join(format!("core_{}.txt", i));
        if let Ok(f) = std::fs::File::create(&path) {
            core_writers[i] = Some(f);
        }
    }
    let mut pid_writers: std::collections::HashMap<u32, std::fs::File> =
        std::collections::HashMap::new();
    for line in ansi_re.lines() {
        // core split: ^[Core N
        if line.starts_with("[Core ") {
            if let Some(core_id) = parse_core_id(line) {
                if core_id < max_cores {
                    if let Some(f) = core_writers[core_id].as_mut() {
                        let _ = std::io::Write::write_all(f, format!("{}\n", line).as_bytes());
                    }
                }
            }
        }
        // pid split: [pid=N]
        if let Some(pid) = parse_pid(line) {
            let entry = pid_writers.entry(pid).or_insert_with(|| {
                std::fs::File::create(test_dir.join(format!("userspace_pid_{}.txt", pid))).unwrap()
            });
            let cleaned = line
                .replacen(&format!("[pid={}]", pid), "", 1)
                .trim()
                .to_string();
            let _ = std::io::Write::write_all(entry, format!("{}\n", cleaned).as_bytes());
        }
    }
}

fn regex_lite_strip_ansi(s: &str) -> String {
    // lite ANSI strip without regex crate: remove \x1b[...m
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            if chars.peek() == Some(&'[') {
                chars.next();
                // consume until 'm' or letter
                for ch in chars.by_ref() {
                    if ch.is_ascii_alphabetic() {
                        break;
                    }
                }
                continue;
            }
        }
        out.push(c);
    }
    out
}

fn parse_core_id(line: &str) -> Option<usize> {
    // "[Core N |" or "[Core N]"
    let rest = line.strip_prefix("[Core ")?;
    let end = rest.find(|c: char| !c.is_ascii_digit())?;
    rest[..end].parse().ok()
}

fn parse_pid(line: &str) -> Option<u32> {
    let start = line.find("[pid=")?;
    let rest = &line[start + 5..];
    let end = rest.find(']')?;
    rest[..end].parse().ok()
}

fn run_test_with_timeout(
    root: &Path,
    iso: &Path,
    ovmf_code: &Path,
    ovmf_vars: &Path,
    timeout_secs: &str,
) -> TestResult {
    let smp = qemu_smp_arg(root);
    // Prepare test_logs path for COM2 capture (file backend)
    // We don't yet know test_name here, so use generic temp file and let caller handle saving.
    // Keep stdio for COM1, file for COM2 to preserve both.
    let com2_tmp = root
        .join("target")
        .join(format!("com2_{}.log", std::process::id()));
    let com2_arg = format!("file:{}", com2_tmp.display());
    let out = Command::new("timeout")
        .arg(timeout_secs)
        .arg("qemu-system-x86_64")
        .args([
            "-M",
            "q35",
            "-accel",
            "kvm",
            "-smp",
            &smp,
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
        .args([
            "-serial",
            "stdio",
            "-serial",
            &com2_arg,
            "-display",
            "none",
            "-no-reboot",
        ])
        .args(["-m", "256M"])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .unwrap_or_else(|e| panic!("failed to spawn qemu: {e}"));
    let exit_code = out.status.code();
    let mut serial = String::from_utf8_lossy(&out.stdout).into_owned();
    // Merge COM2 file (userspace) if present — QEMU file backend for -serial 2
    if let Ok(com2) = fs::read_to_string(&com2_tmp) {
        if !com2.trim().is_empty() {
            serial.push_str("\n");
            serial.push_str(&com2);
        }
        let _ = fs::remove_file(&com2_tmp);
    } else {
        let _ = fs::remove_file(&com2_tmp);
    }
    TestResult {
        passed: exit_code == Some(TEST_SUCCESS_EXIT),
        exit_code,
        serial,
    }
}

fn run_test(root: &Path, iso: &Path, ovmf_code: &Path, ovmf_vars: &Path) -> TestResult {
    run_test_with_timeout(root, iso, ovmf_code, ovmf_vars, "15")
}

fn build_user_rust_only(root: &Path) {
    println!("Building Rust userspace (for usertest)…");
    let rustspace_dir = root.join("user/rustspace");
    if !rustspace_dir.join("Cargo.toml").exists() {
        eprintln!("user/rustspace/Cargo.toml not found — nothing to build");
        return;
    }
    let _ = Command::new("cargo")
        .args(["fmt"])
        .current_dir(&rustspace_dir)
        .status();
    let status = Command::new("cargo")
        .args([
            "+nightly",
            "build",
            "-Z",
            "build-std=core,alloc",
            "-Z",
            "json-target-spec",
            "--target",
            "x86_64-user.json",
        ])
        .current_dir(&rustspace_dir)
        .status();
    if let Ok(s) = status {
        if !s.success() {
            panic!("Rust userspace build failed");
        }
    } else {
        panic!("failed to spawn cargo for rustspace");
    }
}

fn run_tests(root: &Path, args: &[String]) {
    let (release, filters) = split_release_flag(args);
    let all_bins = discover_test_bins(root);
    if all_bins.is_empty() {
        eprintln!("No test binaries found in kernel/src/bin/test_*.rs");
        eprintln!("MofuOS in-kernel tests live in kernel/src/tests_exp/ - run via normal boot.");
        std::process::exit(1);
    }
    let bins: Vec<String> = if filters.is_empty() {
        all_bins
    } else {
        all_bins
            .into_iter()
            .filter(|name| filters.iter().any(|f| name.contains(f.as_str())))
            .collect()
    };
    if bins.is_empty() {
        eprintln!("No tests matched the given filter(s).");
        std::process::exit(1);
    }
    ensure_limine(root);
    build_user(root);
    println!("{BOLD}Building test kernels…{RESET}");
    build_all_test_kernels(root, &bins, release);
    let (ovmf_code, ovmf_vars) = ovmf(root);
    println!();
    let mut passed_names: Vec<&str> = Vec::new();
    let mut failed_names: Vec<(&str, Option<i32>)> = Vec::new();
    let smp = qemu_smp_arg(root);
    println!(
        "{BOLD}SMP cores:{RESET} {} (from kernel/src/lib.rs MAX_CORES)",
        kernel_max_cores(root)
    );
    println!("{BOLD}QEMU smp:{RESET} {}", smp);
    for name in &bins {
        print!("  {BOLD}{name}{RESET} ... ");
        std::io::stdout().flush().unwrap();
        let iso = package_test_iso(root, name, release);
        let result = run_test(root, &iso, &ovmf_code, &ovmf_vars);
        // Save logs like log_splitter → test_logs/<name>/all.txt + core_N.txt + userspace
        save_test_logs(root, name, &result.serial, None);
        if result.passed {
            println!("{GREEN}ok{RESET} (log: test_logs/{}/all.txt)", name);
            let serial = result.serial.trim();
            if !serial.is_empty() {
                println!("  {YELLOW}---- serial output ----{RESET}");
                for line in serial.lines() {
                    println!("  {line}");
                }
                println!("  {YELLOW}-----------------------{RESET}");
            }
            passed_names.push(name);
        } else {
            let code_str = match result.exit_code {
                None => "none (timeout)".to_string(),
                Some(UNEXPECTED_EXIT) => format!("{} (unexpected exit)", UNEXPECTED_EXIT),
                Some(TEST_FAILED_EXIT) => format!("{} (kernel reported failure)", TEST_FAILED_EXIT),
                Some(c) => c.to_string(),
            };
            println!(
                "{RED}FAILED{RESET} (exit code: {code_str}) (log: test_logs/{}/all.txt)",
                name
            );
            let serial = result.serial.trim();
            if !serial.is_empty() {
                println!("  {YELLOW}---- serial output ----{RESET}");
                for line in serial.lines() {
                    println!("  {line}");
                }
                println!("  {YELLOW}-----------------------{RESET}");
            }
            failed_names.push((name, result.exit_code));
        }
    }
    println!();
    if failed_names.is_empty() {
        println!(
            "{GREEN}{BOLD}test result: ok.{RESET} {} passed; 0 failed (logs in test_logs/)",
            passed_names.len()
        );
    } else {
        println!("{BOLD}tests:{RESET}");
        for name in &passed_names {
            println!("  {GREEN}ok{RESET}    {name}");
        }
        for (name, exit_code) in &failed_names {
            let code_str = match exit_code {
                None => "none (timeout)".to_string(),
                Some(UNEXPECTED_EXIT) => format!("{} (unexpected exit)", UNEXPECTED_EXIT),
                Some(TEST_FAILED_EXIT) => format!("{} (kernel reported failure)", TEST_FAILED_EXIT),
                Some(c) => c.to_string(),
            };
            println!("  {RED}FAILED{RESET} {name} (exit code: {code_str})");
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
    let rustspace_dir = user_dir.join("rustspace");
    if rustspace_dir.join("Cargo.toml").exists() {
        println!("Building Rust userspace…");
        let _ = Command::new("cargo")
            .args(["fmt"])
            .current_dir(&rustspace_dir)
            .status();
        let status = Command::new("cargo")
            .args([
                "+nightly",
                "build",
                "-Z",
                "build-std=core,alloc",
                "-Z",
                "json-target-spec",
                "--target",
                "x86_64-user.json",
            ])
            .current_dir(&rustspace_dir)
            .status();
        if let Ok(s) = status {
            if !s.success() {
                eprintln!("warning: Rust userspace build failed (non-fatal)");
            }
        }
    }
}

fn build_iso(root: &Path) {
    ensure_limine(root);
    build_user(root);
    build_kernel(root, false);
    println!("Building ISO image…");
    let iso_root = root.join("target/iso_root");
    if iso_root.exists() {
        fs::remove_dir_all(&iso_root).unwrap();
    }
    fs::create_dir_all(iso_root.join("boot/limine")).unwrap();
    fs::create_dir_all(iso_root.join("EFI/BOOT")).unwrap();
    let limine = root.join("target/limine");
    let kernel_bin = kernel_bin(root, false);
    if !kernel_bin.exists() {
        panic!(
            "kernel binary not found at {} - build failed?",
            kernel_bin.display()
        );
    }
    fs::copy(&kernel_bin, iso_root.join("boot/kernel")).unwrap();
    fs::copy(
        limine_conf_path(root),
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
    // Compat copy to repo root for legacy scripts expecting template-x86_64.iso
    let _ = fs::copy(&iso_path, root.join("template-x86_64.iso"));
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
        .arg(limine_conf_path(root))
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
    let _ = fs::copy(&hdd, root.join("template-x86_64.hdd"));
    println!("HDD ready: {}", hdd.display());
}

fn ensure_ata_disk(root: &Path) -> PathBuf {
    let ata_path = root.join("storage/ata_disk.img");
    if ata_path.exists() {
        return ata_path;
    }
    println!("Creating ATA disk image…");
    let template = root.join("disk_templates/fat32_os_disk_template_default");
    run(Command::new("bash")
        .args([
            "scripts/create_ata_disk.sh",
            "-i",
            template.to_str().unwrap(),
            "-o",
            "ata_disk.img",
            "-s",
            "64",
        ])
        .current_dir(root));
    ata_path
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
    // stupid btw
    let candidates: &[(&str, &str, &str)] = &[
        ("/usr/share/OVMF", "OVMF_CODE_4M.fd", "OVMF_VARS_4M.fd"),
        ("/usr/share/OVMF", "OVMF_CODE.fd", "OVMF_VARS.fd"),
        ("/usr/share/edk2/x64", "OVMF_CODE.4m.fd", "OVMF_VARS.4m.fd"),
        ("/usr/share/edk2/x64", "OVMF_CODE.fd", "OVMF_VARS.fd"),
        ("/usr/share/edk2-ovmf/x64", "OVMF_CODE.fd", "OVMF_VARS.fd"),
        ("/usr/share/edk2/ovmf", "OVMF_CODE.fd", "OVMF_VARS.fd"),
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
            println!("Found OVMF in {dir}, copying to target/ovmf/…");
            fs::copy(&code_path, &code_dst).unwrap();
            fs::copy(&vars_path, &vars_dst).unwrap();
            return;
        }
    }
    // Fallback: legacy ovmf/ at repo root (MofuOS old layout)
    let legacy_code = root.join("ovmf/ovmf-code-x86_64.fd");
    let legacy_vars = root.join("ovmf/ovmf-vars-x86_64.fd");
    if legacy_code.exists() && legacy_vars.exists() {
        println!("Using legacy ovmf/ directory");
        fs::copy(&legacy_code, &code_dst).unwrap();
        fs::copy(&legacy_vars, &vars_dst).unwrap();
        return;
    }
    panic!("OVMF firmware not found");
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
    let ata = ensure_ata_disk(root);
    let (code, vars) = ovmf(root);
    let iso = root.join("target/template-x86_64.iso");
    let socket1 = "/tmp/mofuos_com1.sock";
    let socket2 = "/tmp/mofuos_com2.sock";
    let _ = fs::create_dir_all(root.join("logs"));
    let _ = fs::remove_file(socket1);
    let _ = fs::remove_file(socket2);
    // Spawn log splitter in background
    let log_script = root.join("scripts/log_splitter.py");
    let mut log_child = Command::new("python3")
        .arg(&log_script)
        .arg(socket1)
        .arg(socket2)
        .spawn()
        .expect("failed to spawn log_splitter.py");
    std::thread::sleep(std::time::Duration::from_millis(200));
    let smp = qemu_smp_arg(root);
    println!(
        "SMP cores: {} (from kernel/src/lib.rs MAX_CORES)",
        kernel_max_cores(root)
    );
    let mut cmd = Command::new("qemu-system-x86_64");

    cmd.args([
        "-M",
        "q35",
        "-accel",
        "kvm",
        "-smp",
        &smp,
        "-cpu",
        "host,+tsc-deadline,+apic",
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
    .args(["-device", "piix3-ide,id=ide"])
    .args(["-device", "ide-hd,drive=ata0,bus=ide.0,unit=0"])
    .args([
        "-drive",
        &format!("file={},format=raw,id=ata0,if=none", ata.display()),
    ])
    .args(["-device", "isa-debug-exit,iobase=0xf4,iosize=0x04"])
    .args(["-serial", &format!("unix:{socket1},server")])
    .args(["-serial", &format!("unix:{socket2},server,nowait")])
    .args(["-no-reboot"])
    .args(["-monitor", "telnet:127.0.0.1:1234,server,nowait"])
    .args(qemu_flags());

    let status = cmd.status().expect("failed to run qemu");
    let _ = log_child.kill();
    if !status.success() {
        if let Some(code) = status.code() {
            std::process::exit(code);
        }
    }
}

fn run_nologs(root: &Path) {
    build_iso(root);
    let ata = ensure_ata_disk(root);
    let (code, vars) = ovmf(root);
    let iso = root.join("target/template-x86_64.iso");
    let smp = qemu_smp_arg(root);
    println!(
        "SMP cores: {} (from kernel/src/lib.rs MAX_CORES)",
        kernel_max_cores(root)
    );
    let mut cmd = Command::new("qemu-system-x86_64");
    cmd.args([
        "-M",
        "q35",
        "-accel",
        "kvm",
        "-smp",
        &smp,
        "-cpu",
        "host,+tsc-deadline,+apic",
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
    .args(["-device", "piix3-ide,id=ide"])
    .args(["-device", "ide-hd,drive=ata0,bus=ide.0,unit=0"])
    .args([
        "-drive",
        &format!("file={},format=raw,id=ata0,if=none", ata.display()),
    ])
    .args(["-device", "isa-debug-exit,iobase=0xf4,iosize=0x04"])
    .args(["-no-reboot"])
    .args(["-monitor", "telnet:127.0.0.1:1234,server,nowait"])
    .args(qemu_flags());
    run(&mut cmd);
}

fn clean(root: &Path) {
    run(Command::new("cargo").arg("clean").current_dir(root));
    let xtask_target = root.join("xtask").join("target");
    if xtask_target.exists() {
        fs::remove_dir_all(&xtask_target)
            .unwrap_or_else(|e| panic!("failed to remove {}: {e}", xtask_target.display()));
    }
    // Legacy artefacts at repo root (from old GNUmakefile)
    for p in [
        "template-x86_64.iso",
        "template-x86_64.hdd",
        "test_disk_image.fat32.img",
    ] {
        let _ = fs::remove_file(root.join(p));
    }
}

fn fmt(root: &Path, extra: &[String]) {
    for dir in [root, &root.join("xtask")] {
        run(Command::new("cargo")
            .args(["fmt", "--all"])
            .args(extra)
            .current_dir(dir));
    }
    // Also fmt rustspace separately (outside workspace)
    let rustspace = root.join("user/rustspace");
    if rustspace.join("Cargo.toml").exists() {
        let _ = Command::new("cargo")
            .args(["fmt", "--all"])
            .args(extra)
            .current_dir(&rustspace)
            .status();
    }
}

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

fn run_usertests(root: &Path, args: &[String]) {
    let (release, filters) = split_release_flag(args);
    let all_bins = discover_usertest_bins(root);
    if all_bins.is_empty() {
        eprintln!("No usertest binaries found in kernel/src/bin/test_user*.rs or test_usermem*.rs");
        std::process::exit(1);
    }
    let bins: Vec<String> = if filters.is_empty() {
        all_bins
    } else {
        all_bins
            .into_iter()
            .filter(|name| filters.iter().any(|f| name.contains(f.as_str())))
            .collect()
    };
    if bins.is_empty() {
        eprintln!("No usertests matched the given filter(s).");
        std::process::exit(1);
    }
    ensure_limine(root);
    build_user_rust_only(root);
    println!("{BOLD}Building usertest kernels…{RESET}");
    build_all_test_kernels(root, &bins, release);
    let (ovmf_code, ovmf_vars) = ovmf(root);
    println!();
    let mut passed_names: Vec<&str> = Vec::new();
    let mut failed_names: Vec<(&str, Option<i32>)> = Vec::new();
    let smp = qemu_smp_arg(root);
    println!(
        "{BOLD}SMP cores:{RESET} {} (from kernel/src/lib.rs MAX_CORES)",
        kernel_max_cores(root)
    );
    println!("{BOLD}QEMU smp:{RESET} {}", smp);
    for name in &bins {
        print!("  {BOLD}{name}{RESET} ... ");
        std::io::stdout().flush().unwrap();
        let iso = package_test_iso(root, name, release);
        let result = run_test_with_timeout(root, &iso, &ovmf_code, &ovmf_vars, "30");
        save_test_logs(root, name, &result.serial, None);
        if result.passed {
            println!("{GREEN}ok{RESET} (log: test_logs/{}/all.txt)", name);
            let serial = result.serial.trim();
            if !serial.is_empty() {
                println!("  {YELLOW}---- serial output ----{RESET}");
                for line in serial.lines() {
                    println!("  {line}");
                }
                println!("  {YELLOW}-----------------------{RESET}");
            }
            passed_names.push(name);
        } else {
            let code_str = match result.exit_code {
                None => "none (timeout)".to_string(),
                Some(UNEXPECTED_EXIT) => format!("{} (unexpected exit)", UNEXPECTED_EXIT),
                Some(TEST_FAILED_EXIT) => format!("{} (kernel reported failure)", TEST_FAILED_EXIT),
                Some(c) => c.to_string(),
            };
            println!(
                "{RED}FAILED{RESET} (exit code: {code_str}) (log: test_logs/{}/all.txt)",
                name
            );
            let serial = result.serial.trim();
            if !serial.is_empty() {
                println!("  {YELLOW}---- serial output ----{RESET}");
                for line in serial.lines() {
                    println!("  {line}");
                }
                println!("  {YELLOW}-----------------------{RESET}");
            }
            failed_names.push((name, result.exit_code));
        }
    }
    println!();
    if failed_names.is_empty() {
        println!(
            "{GREEN}{BOLD}usertest result: ok.{RESET} {} passed; 0 failed (logs in test_logs/)",
            passed_names.len()
        );
    } else {
        println!("{BOLD}usertests:{RESET}");
        for name in &passed_names {
            println!("  {GREEN}ok{RESET}    {name}");
        }
        for (name, exit_code) in &failed_names {
            let code_str = match exit_code {
                None => "none (timeout)".to_string(),
                Some(UNEXPECTED_EXIT) => format!("{} (unexpected exit)", UNEXPECTED_EXIT),
                Some(TEST_FAILED_EXIT) => format!("{} (kernel reported failure)", TEST_FAILED_EXIT),
                Some(c) => c.to_string(),
            };
            println!("  {RED}FAILED{RESET} {name} (exit code: {code_str})");
        }
        println!();
        println!(
            "{RED}{BOLD}usertest result: FAILED.{RESET} {} passed; {} failed",
            passed_names.len(),
            failed_names.len()
        );
        std::process::exit(1);
    }
}

fn print_usage() {
    eprintln!(
        "Usage: cargo xtask <TASK>\n\nTasks:\n  build        Build kernel and user programs\n  iso          Build bootable ISO image (default)\n  hdd          Build bootable HDD image\n  run          Build ISO + ATA disk and launch in QEMU (sockets + log_splitter)\n  run-nologs   Build ISO and launch in QEMU (stdio, no log splitter)\n   ata-disk     Create ATA disk image (storage/ata_disk.img)\n  test [--release] [filter…]  Build and run integration tests (kernel/src/bin/test_*.rs)\n  usertest [--release] [filter…] Build and run userspace integration tests (kernel/src/bin/test_user*.rs, test_usermem*.rs)\n  clean        Remove all build artefacts\n  fmt          Format all code (workspace + xtask + rustspace)  [extra args forwarded]\n  clippy       Lint all code (workspace + xtask)  [cargo args | -- lint flags]\n\nEnvironment variables:\n  QEMUFLAGS    Extra flags appended to QEMU (default: -m 2G)\n  CC, LD, AR   Tool overrides for user program build\n"
    );
}

fn main() {
    let root = project_root();
    let mut args = env::args().skip(1);
    let task = args.next();
    let extra: Vec<String> = args.collect();
    match task.as_deref() {
        Some("--help") | Some("-h") | Some("help") => {
            print_usage();
            return;
        }
        Some("build") => {
            build_user(&root);
            build_kernel(&root, false);
        }
        Some("iso") | None => build_iso(&root),
        Some("hdd") => build_hdd(&root),
        Some("run") => run_iso(&root),
        Some("run-nologs") => run_nologs(&root),
        Some("ata-disk") => {
            ensure_ata_disk(&root);
        }
        Some("test") => run_tests(&root, &extra),
        Some("usertest") => run_usertests(&root, &extra),
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
