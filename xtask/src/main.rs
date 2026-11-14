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
        if t.starts_with("pub const MAX_CORES")
            && let Some(eq) = t.find('=')
        {
            let after = &t[eq + 1..];
            let digits: String = after.chars().filter(|c| c.is_ascii_digit()).collect();
            if let Ok(v) = digits.parse::<u8>()
                && (1..=64).contains(&v)
            {
                return v;
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

const DISK_IMAGE_SIZE_MB: u32 = 64;
const DEFAULT_DISK_TEMPLATE: &str = "disk_templates/default";
const TEST_DISK_TEMPLATE: &str = "disk_templates/test";
const DEFAULT_DISK_IMAGE: &str = "storage/disk.img";
const TEST_DISK_IMAGE: &str = "storage/test_disk.img";
const TEST_DISK_PATTERN_BYTES: u32 = 100_000;

/// newest modification time of any file or directory under path, including path itself
fn newest_modification_time(path: &Path) -> std::time::SystemTime {
    let mut newest = fs::metadata(path)
        .and_then(|meta| meta.modified())
        .unwrap_or(std::time::UNIX_EPOCH);

    if let Ok(entries) = fs::read_dir(path) {
        for entry in entries.flatten() {
            newest = newest.max(newest_modification_time(&entry.path()));
        }
    }

    newest
}

const USER_BINS_DIR: &str = "target/user_bins";
const DOOM_WAD_SOURCE: &str = "storage/doom1.wad";
const DOOM_WAD_DISK_NAME: &str = "DOOM1.WAD";
const DISK_STAGING_DIR: &str = "target/disk_staging";
const DISK_BIN_DIR: &str = "BIN";
const HEADER_ONLY_ELF_NAME: &str = "HDRONLY.ELF";
const TRUNCATED_ELF_NAME: &str = "TRUNC.ELF";
const TRUNCATION_SOURCE_BIN: &str = "rust1st";
const HEADER_ONLY_ELF_BYTES: usize = 32;
const ELF64_PHOFF_OFFSET: usize = 0x20;
const ELF64_PHENTSIZE_OFFSET: usize = 0x36;
const ELF64_PHNUM_OFFSET: usize = 0x38;
const ELF64_PH_TYPE_OFFSET: usize = 0;
const ELF64_PH_OFFSET_OFFSET: usize = 8;
const ELF64_PH_FILESZ_OFFSET: usize = 32;
const PT_LOAD: u32 = 1;

fn read_le_u16(bytes: &[u8], offset: usize) -> usize {
    u16::from_le_bytes(bytes[offset..offset + 2].try_into().unwrap()) as usize
}

fn read_le_u32(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap())
}

fn read_le_u64(bytes: &[u8], offset: usize) -> usize {
    u64::from_le_bytes(bytes[offset..offset + 8].try_into().unwrap()) as usize
}

/// Length that cuts the ELF in the middle of its first PT_LOAD with file data, after the
/// program header table, so the header parses and only the segment bounds check can fail
fn truncated_elf_length(elf: &[u8]) -> usize {
    let phoff = read_le_u64(elf, ELF64_PHOFF_OFFSET);
    let phentsize = read_le_u16(elf, ELF64_PHENTSIZE_OFFSET);
    let phnum = read_le_u16(elf, ELF64_PHNUM_OFFSET);
    let table_end = phoff + phentsize * phnum;

    for index in 0..phnum {
        let header = phoff + index * phentsize;
        let filesz = read_le_u64(elf, header + ELF64_PH_FILESZ_OFFSET);
        if read_le_u32(elf, header + ELF64_PH_TYPE_OFFSET) != PT_LOAD || filesz == 0 {
            continue;
        }

        let segment_end = read_le_u64(elf, header + ELF64_PH_OFFSET_OFFSET) + filesz;
        let cut = segment_end - filesz / 2;
        assert!(
            cut > table_end && cut < segment_end,
            "the truncated ELF must keep the program header table and lose segment data"
        );

        return cut;
    }

    panic!("{TRUNCATION_SOURCE_BIN} has no PT_LOAD segment with file data");
}

fn copy_directory_recursive(source: &Path, destination: &Path) {
    fs::create_dir_all(destination)
        .unwrap_or_else(|e| panic!("can't create {}: {e}", destination.display()));

    for entry in fs::read_dir(source)
        .unwrap_or_else(|e| panic!("can't read {}: {e}", source.display()))
        .flatten()
    {
        let target = destination.join(entry.file_name());
        if entry.path().is_dir() {
            copy_directory_recursive(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), &target)
                .unwrap_or_else(|e| panic!("can't copy {}: {e}", entry.path().display()));
        }
    }
}

fn stage_disk_contents(root: &Path, template_dir: &Path, staging_dir: &Path, is_test: bool) {
    if staging_dir.exists() {
        fs::remove_dir_all(staging_dir).unwrap();
    }
    copy_directory_recursive(template_dir, staging_dir);

    let bin_dir = staging_dir.join(DISK_BIN_DIR);
    fs::create_dir_all(&bin_dir).unwrap();
    let stripped_dir = root.join(USER_BINS_DIR);
    for name in user_bin_names(root) {
        fs::copy(stripped_dir.join(&name), bin_dir.join(&name))
            .unwrap_or_else(|e| panic!("stripped program {name} is missing: {e}"));
    }

    // the WAD is user supplied
    let wad_source = root.join(DOOM_WAD_SOURCE);
    if wad_source.exists() {
        fs::copy(&wad_source, staging_dir.join(DOOM_WAD_DISK_NAME))
            .unwrap_or_else(|e| panic!("can't copy {}: {e}", wad_source.display()));
    }

    if is_test {
        let source = fs::read(stripped_dir.join(TRUNCATION_SOURCE_BIN)).unwrap();
        fs::write(
            bin_dir.join(HEADER_ONLY_ELF_NAME),
            &source[..HEADER_ONLY_ELF_BYTES],
        )
        .unwrap();
        fs::write(
            bin_dir.join(TRUNCATED_ELF_NAME),
            &source[..truncated_elf_length(&source)],
        )
        .unwrap();
    }
}

/// builds the FAT32 image from the staged template, unless the image is newer than the template,
/// the script and the stripped programs
fn ensure_disk_image(root: &Path, template: &str, output: &str, pattern_bytes: u32) {
    let template_dir = root.join(template);
    let output_path = root.join(output);
    let script = root.join("scripts/create_disk_image.sh");

    let image_time = fs::metadata(&output_path).and_then(|meta| meta.modified());
    let script_time = newest_modification_time(&script);
    let template_time = newest_modification_time(&template_dir)
        .max(script_time)
        .max(newest_modification_time(&root.join(USER_BINS_DIR)))
        .max(newest_modification_time(&root.join(DOOM_WAD_SOURCE)));

    let image_stem = output_path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .expect("disk image path has a file name");
    let staging_dir = root.join(DISK_STAGING_DIR).join(image_stem);
    // a removed WAD leaves no newer file behind, compare against the last staging instead
    let wad_changed =
        staging_dir.join(DOOM_WAD_DISK_NAME).exists() != root.join(DOOM_WAD_SOURCE).exists();
    if !wad_changed && image_time.is_ok_and(|image_time| image_time >= template_time) {
        return;
    }

    stage_disk_contents(root, &template_dir, &staging_dir, pattern_bytes > 0);

    let mut cmd = Command::new(&script);
    cmd.current_dir(root)
        .arg("-i")
        .arg(&staging_dir)
        .arg("-o")
        .arg(&output_path)
        .args(["-s", &DISK_IMAGE_SIZE_MB.to_string()]);
    if pattern_bytes > 0 {
        cmd.args(["-p", &pattern_bytes.to_string()]);
    }
    run_quiet(&mut cmd);
}

const DISK_AIO_ENV: &str = "BIGOS_DISK_AIO";
const DEFAULT_DISK_AIO: &str = "io_uring";
const TEST_DISK_COPY_DIR: &str = "target/test_disks";
const FSCK_TEST_PREFIXES: [&str; 2] = ["test_fs", "test_user_fs"];

/// QEMU arguments that attach the disk image as a modern virtio-blk PCI device
fn disk_qemu_args(image: &Path) -> Vec<String> {
    let aio = env::var(DISK_AIO_ENV).unwrap_or_else(|_| DEFAULT_DISK_AIO.to_string());

    vec![
        "-drive".to_string(),
        format!(
            "file={},if=none,format=raw,cache=unsafe,aio={aio},id=disk0",
            image.display()
        ),
        "-device".to_string(),
        "virtio-blk-pci,drive=disk0,disable-legacy=on".to_string(),
    ]
}

/// Copies the pristine test image so writes never leak between tests or runs
fn prepare_test_disk(root: &Path, test_name: &str) -> PathBuf {
    let copy_dir = root.join(TEST_DISK_COPY_DIR);
    fs::create_dir_all(&copy_dir).expect("failed to create the test disk directory");

    let copy_path = copy_dir.join(format!("{test_name}.img"));
    fs::copy(root.join(TEST_DISK_IMAGE), &copy_path).expect("failed to copy the test disk image");

    copy_path
}

/// Returns the fsck report when the image has errors
fn fsck_report_if_dirty(image: &Path) -> Option<String> {
    let output = Command::new("fsck.fat")
        .arg("-n")
        .arg(image)
        .output()
        .unwrap_or_else(|e| panic!("failed to run fsck.fat: {e}"));
    if output.status.success() {
        return None;
    }

    Some(String::from_utf8_lossy(&output.stdout).into_owned())
}

fn build_disk_images(root: &Path) {
    ensure_disk_image(root, DEFAULT_DISK_TEMPLATE, DEFAULT_DISK_IMAGE, 0);
    ensure_disk_image(
        root,
        TEST_DISK_TEMPLATE,
        TEST_DISK_IMAGE,
        TEST_DISK_PATTERN_BYTES,
    );
}

const GREEN: &str = "\x1b[32m";
const RED: &str = "\x1b[31m";
const YELLOW: &str = "\x1b[33m";
const BOLD: &str = "\x1b[1m";
const RESET: &str = "\x1b[0m";

/// Tests that need storage/doom1.wad on the test image, reported as skipped without it
const TESTS_NEEDING_WAD: [&str; 1] = ["test_user_doom"];

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
    if let Some(p) = com2_raw_path
        && let Ok(com2) = fs::read_to_string(p)
        && !com2.trim().is_empty()
    {
        combined.push('\n');
        combined.push_str(&com2);
    }
    // ANSI strip
    let ansi_re = regex_lite_strip_ansi(&combined);
    // Write all.txt
    let _ = fs::write(test_dir.join("all.txt"), &ansi_re);
    // Also write flat combined
    let _ = fs::write(test_logs_root.join(format!("{}.log", test_name)), &ansi_re);
    // Split per core / per pid like log_splitter.py
    let max_cores = kernel_max_cores(root) as usize;
    let mut core_writers: Vec<Option<std::fs::File>> = (0..max_cores)
        .map(|i| std::fs::File::create(test_dir.join(format!("core_{i}.txt"))).ok())
        .collect();
    let mut pid_writers: std::collections::HashMap<u32, std::fs::File> =
        std::collections::HashMap::new();
    for line in ansi_re.lines() {
        // core split: ^[Core N
        if line.starts_with("[Core ")
            && let Some(core_id) = parse_core_id(line)
            && core_id < max_cores
            && let Some(f) = core_writers[core_id].as_mut()
        {
            let _ = std::io::Write::write_all(f, format!("{}\n", line).as_bytes());
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
        if c == '\x1b' && chars.peek() == Some(&'[') {
            chars.next();
            // consume until 'm' or letter
            for ch in chars.by_ref() {
                if ch.is_ascii_alphabetic() {
                    break;
                }
            }
            continue;
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
    disk_image: &Path,
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
        .args(disk_qemu_args(disk_image))
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
            serial.push('\n');
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

fn run_test(root: &Path, name: &str, iso: &Path, ovmf_code: &Path, ovmf_vars: &Path) -> TestResult {
    let disk_image = prepare_test_disk(root, name);
    let mut result = run_test_with_timeout(root, iso, ovmf_code, ovmf_vars, &disk_image, "30");

    let needs_fsck = FSCK_TEST_PREFIXES
        .iter()
        .any(|prefix| name.starts_with(prefix));
    if needs_fsck && let Some(report) = fsck_report_if_dirty(&disk_image) {
        result.passed = false;
        result.serial.push_str("\nfsck.fat reported errors:\n");
        result.serial.push_str(&report);
    }

    // a failed test keeps its image for debugging
    if result.passed {
        let _ = fs::remove_file(&disk_image);
    }

    result
}

fn run_tests(root: &Path, args: &[String]) {
    let (release, filters) = split_release_flag(args);
    let all_bins = discover_test_bins(root);
    if all_bins.is_empty() {
        eprintln!("No test binaries found in kernel/src/bin/test_*.rs");
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
    let has_wad = root.join(DOOM_WAD_SOURCE).exists();
    let (bins, skipped_names): (Vec<String>, Vec<String>) = bins
        .into_iter()
        .partition(|name| has_wad || !TESTS_NEEDING_WAD.contains(&name.as_str()));
    ensure_limine(root);
    build_user(root);
    build_disk_images(root);
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
    for name in &skipped_names {
        println!("  {BOLD}{name}{RESET} ... {YELLOW}skipped{RESET} (no {DOOM_WAD_SOURCE})");
    }
    for name in &bins {
        print!("  {BOLD}{name}{RESET} ... ");
        std::io::stdout().flush().unwrap();
        let iso = package_test_iso(root, name, release);
        let result = run_test(root, name, &iso, &ovmf_code, &ovmf_vars);
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
            "{GREEN}{BOLD}test result: ok.{RESET} {} passed; 0 failed; {} skipped (logs in test_logs/)",
            passed_names.len(),
            skipped_names.len()
        );
    } else {
        println!("{BOLD}tests:{RESET}");
        for name in &passed_names {
            println!("  {GREEN}ok{RESET}    {name}");
        }
        for name in &skipped_names {
            println!("  {YELLOW}skipped{RESET} {name}");
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
            "{RED}{BOLD}test result: FAILED.{RESET} {} passed; {} failed; {} skipped",
            passed_names.len(),
            failed_names.len(),
            skipped_names.len()
        );
        std::process::exit(1);
    }
}

/// A standalone userspace cargo package built for the x86_64-user target
struct UserCrate {
    dir: &'static str,
    /// relative to dir
    target_spec: &'static str,
    /// its build script compiles C
    needs_clang: bool,
}

impl UserCrate {
    /// cargo subcommand in the crate directory with the userspace target flags
    fn cargo(&self, root: &Path, subcommand: &str) -> Command {
        let mut cmd = Command::new("cargo");
        cmd.arg(subcommand)
            .args(USER_TARGET_ARGS)
            .args(["--target", self.target_spec])
            .current_dir(root.join(self.dir));

        cmd
    }

    fn manifest(&self) -> String {
        format!("{}/{CARGO_MANIFEST}", self.dir)
    }

    fn bin_source_dir(&self) -> String {
        format!("{}/{USER_BIN_SOURCE_SUBDIR}", self.dir)
    }
}

// rustspace first, doom depends on it
const USER_CRATES: [UserCrate; 2] = [
    UserCrate {
        dir: "user/rustspace",
        target_spec: "x86_64-user.json",
        needs_clang: false,
    },
    UserCrate {
        dir: "user/doom",
        target_spec: "../rustspace/x86_64-user.json",
        needs_clang: true,
    },
];

const USER_TARGET_ARGS: [&str; 4] = ["-Z", "build-std=core,alloc", "-Z", "json-target-spec"];
const USER_BUILD_SUBDIR: &str = "target/x86_64-user/debug";
const USER_BIN_SOURCE_SUBDIR: &str = "src/bin";
const CARGO_MANIFEST: &str = "Cargo.toml";

const MAX_BIN_NAME_LENGTH: usize = 8;
const BIN_SECTION_HEADER: &str = "[[bin]]";
const NAME_KEY: &str = "name";

/// Names of every userspace program, over all user crates
fn user_bin_names(root: &Path) -> Vec<String> {
    USER_CRATES
        .iter()
        .flat_map(|user_crate| crate_bin_names(root, user_crate))
        .collect()
}

/// Names of the programs of one user crate, from the [[bin]] tables of its manifest
fn crate_bin_names(root: &Path, user_crate: &UserCrate) -> Vec<String> {
    let path = root.join(user_crate.manifest());
    let content =
        fs::read_to_string(&path).unwrap_or_else(|e| panic!("can't read {}: {e}", path.display()));

    let mut names = Vec::new();
    let mut in_bin_table = false;
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            in_bin_table = trimmed == BIN_SECTION_HEADER;
            continue;
        }
        if !in_bin_table {
            continue;
        }
        let Some((key, value)) = trimmed.split_once('=') else {
            continue;
        };
        if key.trim() == NAME_KEY {
            names.push(value.trim().trim_matches('"').to_string());
        }
    }

    names
}

fn is_valid_bin_name(name: &str) -> bool {
    let length_ok = (1..=MAX_BIN_NAME_LENGTH).contains(&name.len());
    let chars_ok = name
        .bytes()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'_');

    length_ok && chars_ok
}

/// Stops the build when a program name cannot be a FAT 8.3 short name or when
/// a source file in src/bin has no [[bin]] entry (cargo would build it under its file name and
/// it would miss the disk image)
fn check_user_bin_names(root: &Path) {
    for user_crate in &USER_CRATES {
        let declared = crate_bin_names(root, user_crate);
        let source_dir_name = user_crate.bin_source_dir();
        let source_dir = root.join(&source_dir_name);
        for entry in fs::read_dir(&source_dir)
            .unwrap_or_else(|e| panic!("can't read {}: {e}", source_dir.display()))
            .flatten()
        {
            let file_name = entry.file_name().to_string_lossy().into_owned();
            if let Some(stem) = file_name.strip_suffix(".rs")
                && !declared.iter().any(|name| name == stem)
            {
                eprintln!(
                    "error: {source_dir_name}/{file_name} has no [[bin]] entry named '{stem}' in {}",
                    user_crate.manifest()
                );
                std::process::exit(1);
            }
        }
    }

    let declared = user_bin_names(root);
    for (index, name) in declared.iter().enumerate() {
        // all programs share /BIN on the disk image
        if declared[..index].contains(name) {
            eprintln!("error: userspace program name '{name}' is declared by two user crates");
            std::process::exit(1);
        }
    }

    for name in declared {
        if !is_valid_bin_name(&name) {
            eprintln!(
                "error: userspace program name '{name}' is invalid: names must be 1 to {MAX_BIN_NAME_LENGTH} characters of a-z, 0-9 or _ so they fit a FAT 8.3 short name (no long file name support)"
            );
            std::process::exit(1);
        }
    }
}

/// Rustup llvm-objcopy, so no system package is needed
fn llvm_objcopy_path() -> PathBuf {
    let sysroot = Command::new("rustc")
        .args(["--print", "sysroot"])
        .output()
        .expect("failed to run rustc");
    let version = Command::new("rustc")
        .arg("-vV")
        .output()
        .expect("failed to run rustc");
    let version = String::from_utf8_lossy(&version.stdout);
    let host = version
        .lines()
        .find_map(|line| line.strip_prefix("host: "))
        .expect("rustc -vV has no host line");

    PathBuf::from(String::from_utf8_lossy(&sysroot.stdout).trim())
        .join("lib/rustlib")
        .join(host)
        .join("bin/llvm-objcopy")
}

/// Writes target/user_bins/<name> without debug info, the debug builds are about 5 MB each
fn strip_user_bins(root: &Path) {
    let objcopy = llvm_objcopy_path();
    let output_dir = root.join(USER_BINS_DIR);
    fs::create_dir_all(&output_dir).unwrap();

    for user_crate in &USER_CRATES {
        let build_dir = root.join(user_crate.dir).join(USER_BUILD_SUBDIR);
        for name in crate_bin_names(root, user_crate) {
            let input = build_dir.join(&name);
            let output = output_dir.join(&name);
            let input_time = fs::metadata(&input)
                .and_then(|meta| meta.modified())
                .unwrap_or_else(|e| panic!("{} was not built: {e}", input.display()));
            let output_time = fs::metadata(&output).and_then(|meta| meta.modified());
            if output_time.is_ok_and(|output_time| output_time >= input_time) {
                continue;
            }

            run_quiet(
                Command::new(&objcopy)
                    .arg("--strip-debug")
                    .arg(&input)
                    .arg(&output),
            );
        }
    }
}

/// Stops the build with a clear message when clang is not on PATH
fn ensure_clang(user_crate: &UserCrate) {
    let found = Command::new("clang")
        .arg("--version")
        .output()
        .is_ok_and(|output| output.status.success());
    if !found {
        eprintln!(
            "error: {} compiles C and needs clang on PATH (apt install clang)",
            user_crate.dir
        );
        std::process::exit(1);
    }
}

fn build_user(root: &Path) {
    check_user_bin_names(root);
    println!("Building user programs…");
    for user_crate in &USER_CRATES {
        println!("Building {}…", user_crate.dir);
        if user_crate.needs_clang {
            ensure_clang(user_crate);
        }
        let _ = Command::new("cargo")
            .args(["fmt"])
            .current_dir(root.join(user_crate.dir))
            .status();
        run(&mut user_crate.cargo(root, "build"));
    }
    strip_user_bins(root);
}

fn build_iso(root: &Path) {
    ensure_limine(root);
    build_user(root);
    build_disk_images(root);
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
    build_disk_images(root);
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
    let socket1 = "/tmp/bigos_com1.sock";
    let socket2 = "/tmp/bigos_com2.sock";
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
    .args(disk_qemu_args(&root.join(DEFAULT_DISK_IMAGE)))
    .args(["-device", "isa-debug-exit,iobase=0xf4,iosize=0x04"])
    .args(["-serial", &format!("unix:{socket1},server")])
    .args(["-serial", &format!("unix:{socket2},server,nowait")])
    .args(["-no-reboot"])
    .args(["-monitor", "telnet:127.0.0.1:1234,server,nowait"])
    .args(qemu_flags());

    let status = cmd.status().expect("failed to run qemu");
    let _ = log_child.kill();
    let _ = log_child.wait();
    if !status.success()
        && let Some(code) = status.code()
    {
        std::process::exit(code);
    }
}

fn run_nologs(root: &Path) {
    build_iso(root);
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
    .args(disk_qemu_args(&root.join(DEFAULT_DISK_IMAGE)))
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
    // the user crates are standalone workspaces
    for user_crate in &USER_CRATES {
        run(Command::new("cargo")
            .args(["fmt", "--all"])
            .args(extra)
            .current_dir(root.join(user_crate.dir)));
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
    build_user(root);
    for dir in [root, &root.join("xtask")] {
        run(Command::new("cargo")
            .args(["clippy", "--all-features"])
            .args(cargo_args)
            .arg("--")
            .args(["-D", "warnings"])
            .args(lint_args)
            .current_dir(dir));
    }
    for user_crate in &USER_CRATES {
        run(user_crate
            .cargo(root, "clippy")
            .args(cargo_args)
            .arg("--")
            .args(["-D", "warnings"])
            .args(lint_args));
    }
}

fn print_usage() {
    eprintln!(
        "Usage: cargo xtask <TASK>\n\nTasks:\n  build        Build kernel and user programs\n  iso          Build bootable ISO image (default)\n  hdd          Build bootable HDD image\n  run          Build ISO and launch in QEMU (sockets + log_splitter)\n  run-nologs   Build ISO and launch in QEMU (stdio, no log splitter)\n  test [--release] [filter…]  Build and run integration tests (kernel/src/bin/test_*.rs)\n  disk         Build the user programs and the FAT32 disk images in storage/ when out of date\n  clean        Remove all build artefacts\n  fmt          Format all code (workspace + xtask + user crates)  [extra args forwarded]\n  clippy       Lint all code (workspace + xtask + user crates)  [cargo args | -- lint flags]\n\nUser crates: user/rustspace, user/doom (needs clang). Put the shareware DOOM1.WAD at\n{DOOM_WAD_SOURCE} to stage it as /{DOOM_WAD_DISK_NAME} on the disk images\n\nEnvironment variables:\n  QEMUFLAGS    Extra flags appended to QEMU (default: -m 2G)\n  CC, LD, AR   Tool overrides for user program build\n"
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
        }
        Some("build") => {
            build_user(&root);
            build_disk_images(&root);
            build_kernel(&root, false);
        }
        Some("iso") | None => build_iso(&root),
        Some("hdd") => build_hdd(&root),
        Some("run") => run_iso(&root),
        Some("run-nologs") => run_nologs(&root),
        Some("test") => run_tests(&root, &extra),
        Some("disk") => {
            build_user(&root);
            build_disk_images(&root);
        }
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
