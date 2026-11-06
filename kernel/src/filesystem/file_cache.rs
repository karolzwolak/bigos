use alloc::string::String;
use alloc::vec::Vec;
use core::fmt;

use crate::data_structures::hash_map_fx::FxHashMap;
use crate::filesystem::FileNode;
use crate::filesystem::fat32::{FileNodeHandle, INVALID_NODE_HANDLE};
use crate::filesystem::sirius::FileSystemError;
use crate::{interrupts, serial_println_core};

pub const FS_CACHE_SIZE: usize = 16 * 1024 * 1024;
//pub const FS_CACHE_SIZE: usize = 32 * 1024;
pub const FS_CACHE_MAP_FILE_COUNT: usize = 1024;

const DEBUG_LOGS: bool = true;

macro_rules! serial_println_core {
    ($($arg:tt)*) => {
        if DEBUG_LOGS {
            $crate::serial_println_core!($($arg)*);
        }
    };
}

pub struct CacheStats {
    pub total_files: usize,
    pub total_bytes: usize,
    pub max_bytes: usize,
    pub dirty_files: usize,
}

pub trait CacheFilesystemDriver {
    /// Read the entire file into out, out must be pre-sized to file_size
    /// Returns the number of bytes actually written
    fn read_file_into(
        &mut self,
        node_id: FileNodeHandle,
        out: &mut [u8],
    ) -> Result<usize, FileSystemError>;

    fn write_file(
        &mut self,
        node_id: FileNodeHandle,
        offset: usize,
        data: &[u8],
    ) -> Result<usize, FileSystemError>;

    fn file_size(&mut self, node_id: FileNodeHandle) -> Result<usize, FileSystemError>;

    fn read_dir(&mut self, node_id: FileNodeHandle) -> Result<Vec<String>, FileSystemError>;

    fn get_node(&mut self, node_id: FileNodeHandle) -> Result<FileNode, FileSystemError>;
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[repr(u8)]
pub enum CacheImportance {
    Minimal = 0,
    Low = 1,
    Normal = 2,
    High = 3,
    VeryHigh = 4,
    Critical = 5,
    Resident = 6,
}

impl fmt::Display for CacheImportance {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            CacheImportance::Minimal => "Minimal",
            CacheImportance::Low => "Low",
            CacheImportance::Normal => "Normal",
            CacheImportance::High => "High",
            CacheImportance::VeryHigh => "VeryHigh",
            CacheImportance::Critical => "Critical",
            CacheImportance::Resident => "Resident",
        };
        write!(f, "{}", s)
    }
}

impl CacheImportance {
    pub fn from_u8(value: u8) -> Option<Self> {
        match value {
            0 => Some(Self::Minimal),
            1 => Some(Self::Low),
            2 => Some(Self::Normal),
            3 => Some(Self::High),
            4 => Some(Self::VeryHigh),
            5 => Some(Self::Critical),
            6 => Some(Self::Resident),
            _ => None,
        }
    }

    fn eviction_priority(&self) -> u64 {
        match self {
            Self::Minimal => 0,
            Self::Low => 1,
            Self::Normal => 2,
            Self::High => 3,
            Self::VeryHigh => 4,
            Self::Critical => 5,
            Self::Resident => u64::MAX,
        }
    }
}

/// Per-file metadata
struct CachedFile {
    arena_offset: usize,
    len: usize,
    last_access: u64,
    is_dirty: bool,
    pinned: bool,
    importance: CacheImportance,
}

impl CachedFile {
    fn new(arena_offset: usize, len: usize, tick: u64, importance: CacheImportance) -> Self {
        Self {
            arena_offset,
            len,
            last_access: tick,
            is_dirty: false,
            pinned: false,
            importance,
        }
    }
}

/// On eviction the entry is removed from the map and its arena region is marked free
/// The arena is compacted when a new file would exceed max_memory but there is
/// enough free space fragmented across evicted regions
pub struct FileCache<D: CacheFilesystemDriver> {
    pub driver: D,
    arena: Vec<u8>,
    arena_push_offset: usize,
    current_memory_used: usize,
    max_memory: usize,
    access_tick: u64,
    files: FxHashMap<FileNodeHandle, CachedFile>,

    // TODO: need something better, this is now for correctness of file stats, so they dont get stale data if we dont flush
    file_nodes: FxHashMap<FileNodeHandle, FileNode>,

    directory_children: FxHashMap<FileNodeHandle, Vec<FileNodeHandle>>,
    directory_hints: FxHashMap<FileNodeHandle, CacheImportance>,
}

impl<D: CacheFilesystemDriver> FileCache<D> {
    pub fn new(driver: D, max_memory: usize) -> Self {
        let mut arena = Vec::with_capacity(max_memory);
        arena.resize(max_memory, 0u8);
        Self {
            driver,
            arena,
            arena_push_offset: 0,
            current_memory_used: 0,
            max_memory,
            access_tick: 0,
            files: FxHashMap::with_capacity(FS_CACHE_MAP_FILE_COUNT),
            file_nodes: FxHashMap::with_capacity(FS_CACHE_MAP_FILE_COUNT),
            directory_children: FxHashMap::default(),
            directory_hints: FxHashMap::default(),
        }
    }

    fn arena_push_alloc(&mut self, len: usize) -> Option<usize> {
        if self.arena_push_offset + len <= self.max_memory {
            let offset = self.arena_push_offset;
            self.arena_push_offset += len;
            Some(offset)
        } else {
            None
        }
    }

    /// i hate this but its tough to do something here
    fn compact(&mut self, sort_last: FileNodeHandle) {
        let mut live: Vec<(FileNodeHandle, usize, usize)> = self
            .files
            .iter()
            .map(|(&id, f)| (id, f.arena_offset, f.len))
            .collect();
        live.sort_by_key(
            |&(id, offset, _)| {
                if id != sort_last { offset } else { usize::MAX }
            },
        );

        let mut write_offset = 0usize;
        for (id, old_offset, len) in live {
            if old_offset != write_offset {
                self.arena
                    .copy_within(old_offset..old_offset + len, write_offset);
                if let Some(f) = self.files.get_mut(id) {
                    f.arena_offset = write_offset;
                }
            }
            write_offset += len;
        }
        self.arena_push_offset = write_offset;
        serial_println_core!(
            "file_cache: compacted arena, used={}/{}",
            self.arena_push_offset,
            self.max_memory
        );
    }

    fn ensure_space(
        &mut self,
        needed: usize,
        file_to_end: FileNodeHandle,
        force_eviction: bool,
    ) -> bool {
        while self.arena_push_offset + needed > self.max_memory {
            serial_println_core!(
                "file_cache: arena full, need={} used={}/{}",
                needed,
                self.current_memory_used,
                self.max_memory
            );
            let evicted_size = self.evict_one_keep_alive(file_to_end, force_eviction);
            serial_println_core!(
                "file_cache: evicted {} bytes, new used={}/{}",
                evicted_size,
                self.current_memory_used,
                self.max_memory
            );
            if evicted_size != 0 {
                if self.current_memory_used + needed - evicted_size <= self.max_memory {
                    // we have evicted enough, need to compact now to move the push offset
                    serial_println_core!(
                        "file_cache: evicted enough, compacting arena to make room for {} bytes",
                        needed
                    );
                    self.compact(file_to_end);
                }
            } else {
                serial_println_core!(
                    "file_cache: arena full, cannot evict more files, needed={} used={}/{}",
                    needed,
                    self.current_memory_used,
                    self.max_memory
                );
                return false;
            }
        }
        true
    }

    /// Read `len` bytes starting at `offset` from the cached file directly into `out`
    /// Loads from disk on miss. Returns bytes copied.
    pub fn read_file_range(
        &mut self,
        node_id: FileNodeHandle,
        offset: usize,
        out: &mut [u8],
    ) -> Result<usize, FileSystemError> {
        self.access_tick = self.access_tick.wrapping_add(1);

        serial_println_core!(
            "file_cache: read node={:#x} offset={} len={}; arena_push_offset={} current_memory_used={}/{}",
            node_id,
            offset,
            out.len(),
            self.arena_push_offset,
            self.current_memory_used,
            self.max_memory
        );

        if let Some(cached) = self.files.get_mut(node_id) {
            cached.last_access = self.access_tick;
            let file_len = cached.len;
            if offset >= file_len {
                return Ok(0);
            }
            let copy_len = out.len().min(file_len - offset);
            let src_off = cached.arena_offset + offset;
            out[..copy_len].copy_from_slice(&self.arena[src_off..src_off + copy_len]);
            serial_println_core!(
                "file_cache: hit node={:#x} offset={} copy={}",
                node_id,
                offset,
                copy_len
            );
            return Ok(copy_len);
        }

        // cache miss: load the whole file
        serial_println_core!("file_cache: miss node={:#x}", node_id);
        let file_size = self.driver.file_size(node_id)?;
        serial_println_core!(
            "file_cache: miss node={:#x} file_size={}",
            node_id,
            file_size
        );

        if file_size == 0 {
            return Ok(0);
        }

        if !self.ensure_space(file_size, INVALID_NODE_HANDLE, true) {
            serial_println_core!(
                "file_cache: arena full, cannot ensure space for node={:#x} size={}",
                node_id,
                file_size
            );
            //TODO: read file into the buffer from disk
            return Err(FileSystemError::CacheFull);
        }

        let arena_off = match self.arena_push_alloc(file_size) {
            Some(off) => off,
            None => {
                // arena full even after eviction, read bypassing cache
                //TODO: this can just randomly allocate a very big buffer and that should not ever happen
                serial_println_core!("file_cache: arena full, bypass read node={:#x}", node_id);
                let mut tmp = alloc::vec![0u8; file_size];
                let read = self.driver.read_file_into(node_id, &mut tmp)?;
                let end = out.len().min(read.saturating_sub(offset));
                if end > 0 {
                    out[..end].copy_from_slice(&tmp[offset..offset + end]);
                }
                return Ok(end);
            }
        };

        let mut arena_slice = &mut self.arena[arena_off..arena_off + file_size];
        serial_println_core!(
            "file_cache: loading node={:#x} from driver into arena at arena_offset={} file_size={}, arena_slice.len()={}",
            node_id,
            arena_off,
            file_size,
            arena_slice.len()
        );
        let read_bytes = self.driver.read_file_into(node_id, arena_slice)?;
        let importance = self.get_effective_importance(node_id);
        self.files.insert(
            node_id,
            CachedFile::new(arena_off, read_bytes, self.access_tick, importance),
        );
        self.file_nodes
            .insert(node_id, self.driver.get_node(node_id)?);
        self.current_memory_used += read_bytes;

        serial_println_core!(
            "file_cache: loaded node={:#x} read_bytes={} mem={}/{}",
            node_id,
            read_bytes,
            self.current_memory_used,
            self.max_memory
        );

        if offset >= read_bytes {
            serial_println_core!(
                "file_cache: read node={:#x} offset={} beyond file size {}",
                node_id,
                offset,
                read_bytes
            );
            return Ok(0);
        }
        let copy_len = out.len().min(read_bytes - offset);
        out[..copy_len]
            .copy_from_slice(&self.arena[arena_off + offset..arena_off + offset + copy_len]);
        Ok(copy_len)
    }

    pub fn write_file(
        &mut self,
        node_id: FileNodeHandle,
        offset: usize,
        data: &[u8],
    ) -> Result<usize, FileSystemError> {
        self.access_tick = self.access_tick.wrapping_add(1);

        serial_println_core!(
            "file_cache: write node={:#x} offset={} len={}; arena_push_offset={} current_memory_used={}/{}",
            node_id,
            offset,
            data.len(),
            self.arena_push_offset,
            self.current_memory_used,
            self.max_memory
        );

        // ensure file is in cache before patching
        if !self.files.contains_key(node_id) {
            serial_println_core!("file_cache: write miss node={:#x}", node_id);
            let file_size = self.driver.file_size(node_id).unwrap_or(0);
            if file_size > 0 {
                if !self.ensure_space(file_size, INVALID_NODE_HANDLE, true) {
                    return Err(FileSystemError::CacheFull);
                }
                if let Some(arena_off) = self.arena_push_alloc(file_size) {
                    let read = self.driver.read_file_into(
                        node_id,
                        &mut self.arena[arena_off..arena_off + file_size],
                    )?;
                    let importance = self.get_effective_importance(node_id);
                    self.files.insert(
                        node_id,
                        CachedFile::new(arena_off, read, self.access_tick, importance),
                    );

                    let mut node = self.driver.get_node(node_id)?;
                    if let Some(node) = self.file_nodes.get_mut(node_id) {
                        *node = node.clone();
                    } else {
                        self.file_nodes.insert(node_id, node);
                    }

                    self.current_memory_used += read;
                }
            } else {
                serial_println_core!("file_cache: write miss, file_size == 0");
                // new/empty file: allocate space for the incoming write
                let needed = offset + data.len();
                if !self.ensure_space(needed, INVALID_NODE_HANDLE, true) {
                    return Err(FileSystemError::CacheFull);
                }
                if let Some(arena_off) = self.arena_push_alloc(needed) {
                    self.arena[arena_off..arena_off + needed].fill(0);
                    let importance = self.get_effective_importance(node_id);
                    self.files.insert(
                        node_id,
                        CachedFile::new(arena_off, 0, self.access_tick, importance),
                    );

                    let mut node = self.driver.get_node(node_id)?;
                    if let Some(node) = self.file_nodes.get_mut(node_id) {
                        *node = node.clone();
                    } else {
                        self.file_nodes.insert(node_id, node);
                    }

                    self.current_memory_used += 0;
                }
            }
        }

        let in_file_write_end = offset + data.len();

        serial_println_core!(
            "file_cache: write node={:#x} offset={} len={} in_file_write_end={}",
            node_id,
            offset,
            data.len(),
            in_file_write_end
        );

        let Some(file) = self.files.get_mut(node_id) else {
            return Err(FileSystemError::FileNotFound);
        };
        let file_old_len = file.len;
        let file_old_arena_offset = file.arena_offset;
        let needs_grow = file_old_len < in_file_write_end;
        serial_println_core!(
            "file_cache: write node={:#x} needs_grow={}",
            node_id,
            needs_grow
        );

        if needs_grow {
            let new_len = in_file_write_end.max(file_old_len);
            let extra = new_len - file_old_len;

            let at_tail = file_old_arena_offset + file_old_len == self.arena_push_offset;

            if at_tail && self.arena_push_offset + extra <= self.max_memory {
                // file is at the top of the arena, just extend in-place
                serial_println_core!(
                    "file_cache: write node={:#x} growing in-place at tail",
                    node_id
                );
                self.arena[self.arena_push_offset..self.arena_push_offset + extra].fill(0);
                self.arena_push_offset += extra;
                self.current_memory_used += extra;
                file.len = new_len;
            } else if self.max_memory - self.arena_push_offset >= new_len {
                // arena tail has enough contiguous free space to hold the grown file:
                // copy old data to the tail, zero-fill the extension, no compaction.
                serial_println_core!(
                    "file_cache: write node={:#x} growing by copying to tail",
                    node_id
                );
                let new_off = self.arena_push_offset;
                self.arena.copy_within(
                    file_old_arena_offset..file_old_arena_offset + file_old_len,
                    new_off,
                );
                self.arena[new_off + file_old_len..new_off + new_len].fill(0);
                self.arena_push_offset += new_len;
                self.current_memory_used += extra;
                file.arena_offset = new_off;
                file.len = new_len;
            } else {
                // no room at the tail; compact with sort_last so this file ends up
                // at the arena tail with its data intact, then extend in-place.
                serial_println_core!(
                    "file_cache: write node={:#x} growing by compacting to tail",
                    node_id
                );
                if !self.ensure_space(new_len - file_old_len, node_id, true) {
                    return Err(FileSystemError::CacheFull);
                }

                if self.arena_push_offset + extra <= self.max_memory {
                    // Refetch fresh values after ensure_space - arena_offset may have changed
                    if let Some(file) = self.files.get_mut(node_id) {
                        debug_assert_eq!(
                            file.arena_offset + file.len,
                            self.arena_push_offset,
                            "file must be at arena tail after sort_last compact"
                        );
                        self.arena[self.arena_push_offset..self.arena_push_offset + extra].fill(0);
                        self.arena_push_offset += extra;
                        self.current_memory_used += extra;
                        file.len = new_len;
                    }
                }
            }
        }

        if let Some(file) = self.files.get_mut(node_id) {
            let dst_off = file.arena_offset + offset;
            serial_println_core!(
                "file_cache: write copy_from_slice data: node={:#x} offset={} len={} dst_off={} file_len={}",
                node_id,
                offset,
                data.len(),
                dst_off,
                file.len
            );
            self.arena[dst_off..dst_off + data.len()].copy_from_slice(data);
            if in_file_write_end > file.len {
                self.current_memory_used += in_file_write_end - file.len;
                file.len = in_file_write_end;
            }
            file.is_dirty = true;
            file.last_access = self.access_tick;

            let mut node = self.driver.get_node(node_id)?;
            if let Some(node) = self.file_nodes.get_mut(node_id) {
                *node = node.clone();
                node.size = file.len;
                node.modified_time = interrupts::tsc_timestamp_us() as u32; //TODO:
            } else {
                self.file_nodes.insert(node_id, node);
            }

            serial_println_core!(
                "file_cache: write node={:#x} offset={} len={} dirty",
                node_id,
                offset,
                data.len()
            );
            Ok(data.len())
        } else {
            Err(FileSystemError::IoError)
        }
    }

    pub fn get_node_cached(
        &mut self,
        node_id: FileNodeHandle,
    ) -> Result<FileNode, FileSystemError> {
        serial_println_core!("file_cache: get_node_cached node={:#x}", node_id);
        if let Some(node) = self.file_nodes.get(node_id) {
            serial_println_core!(
                "file_cache: get_node_cached hit node={:#x}, size: {}",
                node_id,
                node.size
            );
            Ok(node.clone())
        } else {
            let node = self.driver.get_node(node_id)?;
            self.file_nodes.insert(node_id, node.clone());
            serial_println_core!(
                "file_cache: get_node_cached miss node={:#x}, inserting, size: {}",
                node_id,
                node.size
            );
            Ok(node)
        }
    }

    pub fn flush_file(&mut self, node_id: FileNodeHandle) -> Result<(), FileSystemError> {
        if let Some(cached) = self.files.get_mut(node_id) {
            if cached.is_dirty {
                let off = cached.arena_offset;
                let len = cached.len;
                self.driver
                    .write_file(node_id, 0, &self.arena[off..off + len])?;
                cached.is_dirty = false;
                serial_println_core!("file_cache: flushed node={:#x}", node_id);
            }
        }
        Ok(())
    }

    pub fn flush_nodes(&mut self, node_ids: &[FileNodeHandle]) -> Result<(), FileSystemError> {
        for &node_id in node_ids {
            if let Some(cached) = self.files.get_mut(node_id) {
                if cached.is_dirty {
                    let off = cached.arena_offset;
                    let len = cached.len;
                    self.driver
                        .write_file(node_id, 0, &self.arena[off..off + len])?;
                    cached.is_dirty = false;
                    serial_println_core!("file_cache: flush_nodes: flushed node={:#x}", node_id);
                }
            }
        }
        Ok(())
    }

    pub fn flush_all(&mut self) -> Result<(), FileSystemError> {
        let dirty_ids: Vec<FileNodeHandle> = self
            .files
            .iter()
            .filter(|(_, f)| f.is_dirty)
            .map(|(&id, _)| id)
            .collect();

        for node_id in dirty_ids {
            if let Some(cached) = self.files.get_mut(node_id) {
                if cached.is_dirty {
                    let off = cached.arena_offset;
                    let len = cached.len;
                    self.driver
                        .write_file(node_id, 0, &self.arena[off..off + len])?;
                    cached.is_dirty = false;
                    serial_println_core!("file_cache: flush_all: flushed node={:#x}", node_id);
                }
            }
        }
        Ok(())
    }

    pub fn pin_file(&mut self, node_id: FileNodeHandle) -> Result<(), FileSystemError> {
        if !self.files.contains_key(node_id) {
            // load into cache by doing a zero-length probe read
            let file_size = self.driver.file_size(node_id)?;
            if file_size > 0 {
                let mut dummy = [0u8; 0];
                self.read_file_range(node_id, 0, &mut dummy)?;
            }
        }
        if let Some(cached) = self.files.get_mut(node_id) {
            cached.pinned = true;
            cached.importance = CacheImportance::Resident;
            serial_println_core!("file_cache: pinned node={:#x}", node_id);
        }
        Ok(())
    }

    pub fn unpin_file(&mut self, node_id: FileNodeHandle) -> Result<(), FileSystemError> {
        if let Some(cached) = self.files.get_mut(node_id) {
            cached.pinned = false;
            cached.importance = CacheImportance::Normal;
            serial_println_core!("file_cache: unpinned node={:#x}", node_id);
        }
        Ok(())
    }

    pub fn reserve_cache(
        &mut self,
        node_id: FileNodeHandle,
        importance: CacheImportance,
    ) -> Result<(), FileSystemError> {
        self.directory_hints.insert(node_id, importance);
        if let Some(children) = self.directory_children.get(node_id) {
            let children: Vec<FileNodeHandle> = children.clone();
            for child_id in children {
                if let Some(cached) = self.files.get_mut(child_id) {
                    if !cached.pinned {
                        cached.importance = importance;
                    }
                }
            }
        }
        Ok(())
    }

    pub fn evict_directory(&mut self, node_id: FileNodeHandle) -> Result<usize, FileSystemError> {
        let mut freed = 0usize;
        if let Some(children) = self.directory_children.remove(node_id) {
            for child_id in children {
                if let Some(cached) = self.files.get(child_id) {
                    if !cached.pinned {
                        freed += cached.len;
                        self.files.remove(child_id);
                        self.current_memory_used -= freed;
                    }
                }
            }
        }
        self.directory_hints.remove(node_id);
        Ok(freed)
    }

    pub fn register_file_in_directory(&mut self, dir_id: FileNodeHandle, file_id: FileNodeHandle) {
        self.directory_children
            .entry(dir_id)
            .or_insert_with(Vec::new)
            .push(file_id);
    }

    pub fn is_cached(&self, node_id: FileNodeHandle) -> bool {
        self.files.contains_key(node_id)
    }

    pub fn stats(&self) -> CacheStats {
        let dirty_files = self.files.values().filter(|f| f.is_dirty).count();
        CacheStats {
            total_files: self.files.len(),
            total_bytes: self.current_memory_used,
            max_bytes: self.max_memory,
            dirty_files,
        }
    }

    pub fn invalidate(&mut self, node_id: FileNodeHandle) {
        if let Some(cached) = self.files.remove(node_id) {
            self.current_memory_used -= cached.len;
            serial_println_core!("file_cache: invalidated node={:#x}", node_id);
        }
        self.file_nodes.remove(node_id);
    }

    pub fn clear(&mut self) {
        self.files.clear();
        self.directory_children.clear();
        self.directory_hints.clear();
        self.current_memory_used = 0;
        self.arena_push_offset = 0;
    }

    fn get_effective_importance(&self, node_id: FileNodeHandle) -> CacheImportance {
        self.directory_hints
            .get(node_id)
            .copied()
            .unwrap_or(CacheImportance::Normal)
    }

    /// Returns the size of the evicted file
    fn evict_one(&mut self) -> usize {
        let mut best_score: u64 = 0;
        let mut best_id: FileNodeHandle = INVALID_NODE_HANDLE;

        for (&node_id, cached) in self.files.iter() {
            if cached.pinned {
                continue;
            }
            let priority = cached.importance.eviction_priority();
            if priority == u64::MAX {
                continue;
            }
            let age = self.access_tick.wrapping_sub(cached.last_access);
            let score = priority.saturating_mul(age.max(1));
            if score > best_score {
                best_id = node_id;
                best_score = score;
            }
        }

        if best_id == INVALID_NODE_HANDLE {
            return 0;
        }

        // flush before eviction if dirty
        if let Some(cached) = self.files.get(best_id) {
            if cached.is_dirty {
                let off = cached.arena_offset;
                let len = cached.len;
                if self
                    .driver
                    .write_file(best_id, 0, &self.arena[off..off + len])
                    .is_err()
                {
                    serial_println_core!(
                        "file_cache: evict flush failed for node={:#x}, skipping",
                        best_id
                    );
                    return 0;
                }
            }
        }

        if let Some(cached) = self.files.remove(best_id) {
            self.current_memory_used -= cached.len;
            serial_println_core!("file_cache: evicted node={:#x} len={}", best_id, cached.len);
            return cached.len;
        }

        0
    }

    /// Returns the size of the evicted file
    fn evict_one_keep_alive(
        &mut self,
        file_to_keep: FileNodeHandle,
        force_eviction: bool,
    ) -> usize {
        serial_println_core!(
            "file_cache: evict_one_keep_alive: file_to_keep={:#x} force_eviction={}",
            file_to_keep,
            force_eviction
        );
        let mut best_score: u64 = 0;
        let mut best_id: FileNodeHandle = INVALID_NODE_HANDLE;

        for (&node_id, cached) in self.files.iter() {
            if (cached.pinned && !force_eviction) || node_id == file_to_keep {
                continue;
            }
            let priority = cached.importance.eviction_priority();
            if priority == u64::MAX && !force_eviction {
                continue;
            }
            let age = self.access_tick.wrapping_sub(cached.last_access);
            let score = priority.saturating_mul(age.max(1));
            if score > best_score {
                best_id = node_id;
                best_score = score;
            }
        }

        if best_id == INVALID_NODE_HANDLE {
            serial_println_core!("file_cache: evict_one_keep_alive: no suitable file to evict");
            return 0;
        }

        // flush before eviction if dirty
        if let Some(cached) = self.files.get(best_id) {
            serial_println_core!(
                "file_cache: evict_one_keep_alive: best_id={:#x} is_dirty={} importance={}",
                best_id,
                cached.is_dirty,
                cached.importance
            );
            if cached.is_dirty {
                let off = cached.arena_offset;
                let len = cached.len;
                serial_println_core!(
                    "file_cache: evict_one_keep_alive: flushing dirty file best_id={:#x} len={}",
                    best_id,
                    len
                );
                if self
                    .driver
                    .write_file(best_id, 0, &self.arena[off..off + len])
                    .is_err()
                {
                    serial_println_core!(
                        "file_cache: evict flush failed for node={:#x}, skipping",
                        best_id
                    );
                    return 0;
                }
            }
        }

        if let Some(cached) = self.files.remove(best_id) {
            self.current_memory_used -= cached.len;
            serial_println_core!("file_cache: evicted node={:#x} len={}", best_id, cached.len);
            return cached.len;
        }

        serial_println_core!("file_cache: evict_one_keep_alive: failed to evict file");

        0
    }
}
