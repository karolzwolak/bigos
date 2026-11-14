use crate::filesystem::fat32::FileNodeHandle;
use crate::filesystem::vfs::{FileSystemError, VFS, Vfs};
use crate::memory::paging::PAGE_SIZE;
use crate::process::PROCESS_USER_VADDR_ALLOC_START;
use alloc::vec::Vec;
use elf::abi::{EI_NIDENT, ELFMAGIC, EM_X86_64, ET_EXEC, PT_LOAD};
use elf::endian::AnyEndian;
use elf::file::{Class, ELF64_EHDR_TAILSIZE, FileHeader, parse_ident};
use elf::segment::{ProgramHeader, SegmentTable};

/// Debug-stripped copy of the init program, the fallback when /BIN/shell cannot be loaded
pub const SHELL_ELF: &[u8] = include_bytes!("../../../target/user_bins/shell");
/// Debug-stripped PID 1, embedded so it exists even when no disk is attached
pub const BIGPROC_ELF: &[u8] = include_bytes!("../../../target/user_bins/bigproc");

const ELF64_HEADER_SIZE: usize = EI_NIDENT + ELF64_EHDR_TAILSIZE;
const PROGRAM_HEADER_SIZE: usize = 56;
pub const MAX_PROGRAM_HEADERS: usize = 16;
const PROGRAM_TABLE_MAX_BYTES: usize = MAX_PROGRAM_HEADERS * PROGRAM_HEADER_SIZE;
const BYTES_PER_MIB: u64 = 1024 * 1024;

/// the lowest page stays unmapped so a null access faults
pub const USER_IMAGE_MIN_VADDR: u64 = PAGE_SIZE as u64;
/// the image has to end before the process heap window starts
pub const USER_IMAGE_MAX_END_VADDR: u64 = PROCESS_USER_VADDR_ALLOC_START;
pub const MAX_ELF_IMAGE_BYTES: u64 = 64 * BYTES_PER_MIB;

#[derive(Debug)]
pub struct ElfLoadInfo {
    pub entry_point: u64,
    pub min_vaddr: u64,
    pub max_vaddr: u64,
    pub segments: Vec<LoadSegment>,
}

#[derive(Debug)]
pub struct LoadSegment {
    pub vaddr: u64,
    pub in_file_size: u64,
    pub in_memory_size: u64,
    pub flags: u32,
    pub data: Vec<u8>,
}

#[repr(u32)]
pub enum ElfLoadFlags {
    Executable = 1,
    Writable = 2,
    Readable = 4,
}

#[derive(Debug)]
pub enum ElfLoadError {
    InvalidMagic,
    InvalidHeader,
    InvalidArch,
    InvalidType,
    NoLoadableSegments,
    ReadError,
    ParseError(elf::ParseError),
    FileSystem(FileSystemError),
    NoFilesystem,
    /// the file ends before the header, the program header table or a segment does
    Truncated,
    TooManyProgramHeaders,
    SegmentOutOfBounds,
    ImageTooLarge,
}

impl From<elf::ParseError> for ElfLoadError {
    fn from(err: elf::ParseError) -> Self {
        ElfLoadError::ParseError(err)
    }
}

impl From<FileSystemError> for ElfLoadError {
    fn from(err: FileSystemError) -> Self {
        ElfLoadError::FileSystem(err)
    }
}

/// Checks the first ELF64_HEADER_SIZE bytes of a file, none of the values can be trusted
fn parse_file_header(bytes: &[u8]) -> Result<FileHeader<AnyEndian>, ElfLoadError> {
    if bytes.len() < ELF64_HEADER_SIZE {
        return Err(ElfLoadError::Truncated);
    }
    if bytes[..ELFMAGIC.len()] != ELFMAGIC {
        return Err(ElfLoadError::InvalidMagic);
    }

    let ident = parse_ident::<AnyEndian>(&bytes[..EI_NIDENT])?;
    let header = FileHeader::parse_tail(ident, &bytes[EI_NIDENT..ELF64_HEADER_SIZE])?;

    if header.class != Class::ELF64 || !matches!(header.endianness, AnyEndian::Little) {
        return Err(ElfLoadError::InvalidHeader);
    }
    if header.e_machine != EM_X86_64 {
        return Err(ElfLoadError::InvalidArch);
    }
    if header.e_type != ET_EXEC {
        return Err(ElfLoadError::InvalidType);
    }
    if header.e_phentsize as usize != PROGRAM_HEADER_SIZE {
        return Err(ElfLoadError::InvalidHeader);
    }
    if header.e_phnum as usize > MAX_PROGRAM_HEADERS {
        return Err(ElfLoadError::TooManyProgramHeaders);
    }

    Ok(header)
}

fn program_table_bytes(header: &FileHeader<AnyEndian>) -> usize {
    header.e_phnum as usize * PROGRAM_HEADER_SIZE
}

/// Returns the PT_LOAD headers after checking each of them against the file size and the user
/// address space, and the entry point against the executable ones
fn checked_load_headers(
    header: &FileHeader<AnyEndian>,
    table: SegmentTable<AnyEndian>,
    file_size: u64,
) -> Result<Vec<ProgramHeader>, ElfLoadError> {
    let mut load_headers = Vec::new();
    let mut image_bytes = 0u64;
    let mut is_entry_executable = false;

    for program_header in table.iter() {
        if program_header.p_type != PT_LOAD || program_header.p_memsz == 0 {
            continue;
        }

        let file_end = program_header
            .p_offset
            .checked_add(program_header.p_filesz)
            .ok_or(ElfLoadError::SegmentOutOfBounds)?;
        let memory_end = program_header
            .p_vaddr
            .checked_add(program_header.p_memsz)
            .ok_or(ElfLoadError::SegmentOutOfBounds)?;
        if program_header.p_filesz > program_header.p_memsz
            || file_end > file_size
            || program_header.p_vaddr < USER_IMAGE_MIN_VADDR
            || memory_end > USER_IMAGE_MAX_END_VADDR
        {
            return Err(ElfLoadError::SegmentOutOfBounds);
        }

        image_bytes = image_bytes
            .checked_add(program_header.p_memsz)
            .filter(|&bytes| bytes <= MAX_ELF_IMAGE_BYTES)
            .ok_or(ElfLoadError::ImageTooLarge)?;

        let is_executable = program_header.p_flags & ElfLoadFlags::Executable as u32 != 0;
        let contains_entry = (program_header.p_vaddr..memory_end).contains(&header.e_entry);
        is_entry_executable |= is_executable && contains_entry;

        load_headers.push(program_header);
    }

    if load_headers.is_empty() {
        return Err(ElfLoadError::NoLoadableSegments);
    }
    if !is_entry_executable {
        return Err(ElfLoadError::SegmentOutOfBounds);
    }

    Ok(load_headers)
}

impl LoadSegment {
    fn from_header(program_header: &ProgramHeader, data: Vec<u8>) -> Self {
        debug_assert!(data.len() as u64 == program_header.p_filesz);

        Self {
            vaddr: program_header.p_vaddr,
            in_file_size: program_header.p_filesz,
            in_memory_size: program_header.p_memsz,
            flags: program_header.p_flags,
            data,
        }
    }
}

fn merge_segments(segments: Vec<LoadSegment>) -> Vec<LoadSegment> {
    let mut merged = Vec::<LoadSegment>::with_capacity(segments.len());
    let mut sorted = segments;
    sorted.sort_by_key(|s| s.vaddr);

    let mut sorted_iter = sorted.into_iter();
    let mut curr_seg = sorted_iter.next().unwrap();

    for next in sorted_iter {
        let curr_end = curr_seg.vaddr + curr_seg.in_memory_size;
        if next.vaddr <= curr_end {
            let next_end = next.vaddr + next.in_memory_size;
            let new_end = core::cmp::max(next_end, curr_end);
            curr_seg.in_memory_size = new_end - curr_seg.vaddr;
            curr_seg.flags |= next.flags;

            if next.in_file_size > 0 {
                let offset_in_curr = (next.vaddr - curr_seg.vaddr) as usize;
                let merged_data_size = offset_in_curr + next.data.len();
                if curr_seg.data.len() < merged_data_size {
                    curr_seg.data.resize(merged_data_size, 0)
                }
                curr_seg.data[offset_in_curr..merged_data_size].copy_from_slice(&next.data);
            }

            let file_end = next.vaddr + next.in_file_size;
            if file_end > curr_seg.vaddr + curr_seg.in_file_size {
                curr_seg.in_file_size = file_end - curr_seg.vaddr;
            }
        } else {
            merged.push(curr_seg);
            curr_seg = next;
        }
    }

    merged.push(curr_seg);

    merged
}

impl ElfLoadInfo {
    /// Parses an ELF that is fully in memory
    pub fn from_elf_data(elf_data: &[u8]) -> Result<Self, ElfLoadError> {
        let header = parse_file_header(elf_data)?;

        let table_start =
            usize::try_from(header.e_phoff).map_err(|_| ElfLoadError::SegmentOutOfBounds)?;
        let table_end = table_start
            .checked_add(program_table_bytes(&header))
            .ok_or(ElfLoadError::SegmentOutOfBounds)?;
        let table_bytes = elf_data
            .get(table_start..table_end)
            .ok_or(ElfLoadError::Truncated)?;
        let table = SegmentTable::new(header.endianness, header.class, table_bytes);

        let load_headers = checked_load_headers(&header, table, elf_data.len() as u64)?;

        let mut segments = Vec::with_capacity(load_headers.len());
        for program_header in &load_headers {
            // the bounds were checked against the length of elf_data
            let start = program_header.p_offset as usize;
            let end = start + program_header.p_filesz as usize;
            segments.push(LoadSegment::from_header(
                program_header,
                elf_data[start..end].to_vec(),
            ));
        }

        Self::from_segments(header.e_entry, segments)
    }

    /// Loads an ELF through the VFS and never reads more than the headers and the segments
    pub fn from_vfs(vfs: &mut Vfs, path: &str) -> Result<Self, ElfLoadError> {
        let node = vfs.open_file(path)?;
        let file_size = node.size as u64;

        let mut header_bytes = [0u8; ELF64_HEADER_SIZE];
        read_exact_at(vfs, node.node_id, 0, &mut header_bytes)?;
        let header = parse_file_header(&header_bytes)?;

        let mut table_buffer = [0u8; PROGRAM_TABLE_MAX_BYTES];
        let table_bytes = &mut table_buffer[..program_table_bytes(&header)];
        let table_offset =
            usize::try_from(header.e_phoff).map_err(|_| ElfLoadError::SegmentOutOfBounds)?;
        read_exact_at(vfs, node.node_id, table_offset, table_bytes)?;
        let table = SegmentTable::new(header.endianness, header.class, table_bytes);

        let load_headers = checked_load_headers(&header, table, file_size)?;

        let mut segments = Vec::with_capacity(load_headers.len());
        for program_header in &load_headers {
            // filesz is at most memsz, and the sum of memsz is bounded by MAX_ELF_IMAGE_BYTES
            let mut data = Vec::new();
            data.try_reserve_exact(program_header.p_filesz as usize)
                .map_err(|_| ElfLoadError::ImageTooLarge)?;
            data.resize(program_header.p_filesz as usize, 0);

            read_exact_at(
                vfs,
                node.node_id,
                program_header.p_offset as usize,
                &mut data,
            )?;
            segments.push(LoadSegment::from_header(program_header, data));
        }

        Self::from_segments(header.e_entry, segments)
    }

    /// Shared tail of both loaders, the segments are already bounds checked
    fn from_segments(entry_point: u64, segments: Vec<LoadSegment>) -> Result<Self, ElfLoadError> {
        if segments.is_empty() {
            return Err(ElfLoadError::NoLoadableSegments);
        }

        let min_vaddr = segments.iter().map(|segment| segment.vaddr).min().unwrap();
        let max_vaddr = segments
            .iter()
            .map(|segment| segment.vaddr + segment.in_memory_size)
            .max()
            .unwrap();

        Ok(ElfLoadInfo {
            entry_point,
            min_vaddr,
            max_vaddr,
            segments: merge_segments(segments),
        })
    }
}

/// A short read means the file is shorter than its headers claim
fn read_exact_at(
    vfs: &mut Vfs,
    node_id: FileNodeHandle,
    offset: usize,
    buffer: &mut [u8],
) -> Result<(), ElfLoadError> {
    if buffer.is_empty() {
        return Ok(());
    }

    let read = vfs.read_node(node_id, offset, buffer)?;
    if read == buffer.len() {
        Ok(())
    } else {
        Err(ElfLoadError::Truncated)
    }
}

/// Loads an ELF from the filesystem. The VFS lock is taken and released inside, so the caller
/// holds no lock on return and can take PROCESS_MANAGER next (never hold both)
pub fn load_elf_from_path(path: &str) -> Result<ElfLoadInfo, ElfLoadError> {
    let vfs = VFS.get().ok_or(ElfLoadError::NoFilesystem)?;
    let mut vfs_guard = vfs.lock();

    ElfLoadInfo::from_vfs(&mut vfs_guard, path)
}
