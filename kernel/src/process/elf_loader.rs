use crate::serial_println;
use alloc::vec::Vec;
use elf::ElfBytes;

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
    pub in_memory_size: u64, // includes the bss section
    pub data: Vec<u8>,
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
}

impl From<elf::ParseError> for ElfLoadError {
    fn from(err: elf::ParseError) -> Self {
        ElfLoadError::ParseError(err)
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
            curr_seg.in_memory_size = new_end - curr_end;

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
    pub fn from_elf_data(elf_data: &[u8]) -> Result<Self, ElfLoadError> {
        serial_println!("ElfLoadInfo: from_elf_data(): loading elf");
        let elf = ElfBytes::<elf::endian::AnyEndian>::minimal_parse(elf_data)?;
        serial_println!("ElfLoadInfo: from_elf_data(): elf parsed");

        match elf.ehdr.e_machine {
            elf::abi::EM_X86_64 => {}
            machine => {
                serial_println!(
                    "ELF Error: Expected x86_64 ({}), got {}",
                    elf::abi::EM_X86_64,
                    machine
                );
                return Err(ElfLoadError::InvalidArch);
            }
        }

        let entry_point = elf.ehdr.e_entry;
        serial_println!("ELF: Entry point: {:#x}", entry_point);

        // Parse program headers and find loadable segments
        let mut min_vaddr = u64::MAX;
        let mut max_vaddr = u64::MIN;
        let mut segments = Vec::new();

        let segments_iter = elf.segments().ok_or(ElfLoadError::InvalidHeader)?;

        for segment in segments_iter {
            if segment.p_type == elf::abi::PT_LOAD {
                let vaddr = segment.p_vaddr;
                let in_file_size = segment.p_filesz;
                let in_memory_size = segment.p_memsz;
                let offset = segment.p_offset;

                serial_println!(
                    "ELF: Found LOAD segment: vaddr={:#x}, in_file_size={}, in_memory_size={}",
                    vaddr,
                    in_file_size,
                    in_memory_size
                );

                min_vaddr = core::cmp::min(vaddr, min_vaddr);
                max_vaddr = core::cmp::max(vaddr + in_memory_size, max_vaddr);

                let segment_data = if in_file_size > 0 {
                    let offset_usize = offset as usize;
                    let in_file_size_usize = in_file_size as usize;

                    if offset_usize + in_file_size_usize > elf_data.len() {
                        return Err(ElfLoadError::ReadError);
                    }

                    elf_data[offset_usize..offset_usize + in_file_size_usize].to_vec()
                } else {
                    Vec::new()
                };

                segments.push(LoadSegment {
                    vaddr,
                    in_file_size,
                    in_memory_size,
                    data: segment_data,
                });
            }
        }

        if segments.is_empty() {
            return Err(ElfLoadError::NoLoadableSegments);
        }
        let segments = merge_segments(segments);

        serial_println!(
            "ELF: Memory range: {:#x} - {:#x} (size: {:#x} bytes)",
            min_vaddr,
            max_vaddr,
            max_vaddr - min_vaddr
        );

        Ok(ElfLoadInfo {
            entry_point,
            min_vaddr,
            max_vaddr,
            segments,
        })
    }
}
