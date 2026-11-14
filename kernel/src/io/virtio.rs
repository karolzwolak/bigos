use crate::boot_info::boot_info;
use crate::io::disk::{DiskOpError, DiskOpResult, SECTOR_SIZE_BYTES};
use crate::io::pci::{
    self, BAR_COUNT, PciBar, PciDeviceInfo, PciLocation, VIRTIO_BLK_MODERN_DEVICE_ID,
    VIRTIO_VENDOR_ID,
};
use crate::memory::allocator::HEAP_POINTER;
use crate::memory::paging::map_mmio_range;
use crate::serial_println;
use core::cell::UnsafeCell;
use core::cmp::min;
use core::ptr::NonNull;
use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use virtio_drivers::device::blk::VirtIOBlk;
use virtio_drivers::transport::pci::PciTransport;
use virtio_drivers::transport::pci::bus::{ConfigurationAccess, DeviceFunction, PciRoot};
use virtio_drivers::{BufferDirection, Hal, PAGE_SIZE, PhysAddr};

const DMA_QUEUE_PAGES: usize = 16;
const DMA_QUEUE_BYTES: usize = DMA_QUEUE_PAGES * PAGE_SIZE;

pub const MAX_REQUEST_SECTORS: usize = 128;
const DMA_BOUNCE_BYTES: usize = MAX_REQUEST_SECTORS * SECTOR_SIZE_BYTES;

const MAX_BAR_MAP_BYTES: u64 = 16 * 1024 * 1024;

#[repr(C, align(4096))]
struct DmaPool {
    queue_area: [u8; DMA_QUEUE_BYTES],
    bounce: [u8; DMA_BOUNCE_BYTES],
}

struct DmaPoolCell(UnsafeCell<DmaPool>);

// SAFETY: queue_area is handed out once through the atomic page counter and the bounce
// buffer is only touched by the single VirtioBlk instance, see VIRTIO_BLK_CLAIMED
unsafe impl Sync for DmaPoolCell {}

static DMA_POOL: DmaPoolCell = DmaPoolCell(UnsafeCell::new(DmaPool {
    queue_area: [0; DMA_QUEUE_BYTES],
    bounce: [0; DMA_BOUNCE_BYTES],
}));
static DMA_QUEUE_NEXT_PAGE: AtomicUsize = AtomicUsize::new(0);
static VIRTIO_BLK_CLAIMED: AtomicBool = AtomicBool::new(false);

fn kernel_image_virt_to_phys(virt_address: u64) -> u64 {
    let info = boot_info();

    virt_address - info.kernel_virtual_base + info.kernel_physical_base
}

fn dma_virt_to_phys(virt_address: u64) -> u64 {
    let info = boot_info();

    if virt_address >= info.kernel_virtual_base {
        return kernel_image_virt_to_phys(virt_address);
    }

    assert!(
        virt_address >= info.hhdm_offset && virt_address < HEAP_POINTER as u64,
        "virtio: buffer {:#x} is not DMA addressable",
        virt_address
    );

    virt_address - info.hhdm_offset
}

fn bounce_buffer(byte_count: usize) -> &'static mut [u8] {
    debug_assert!(byte_count <= DMA_BOUNCE_BYTES);

    // SAFETY: see the Sync impl of DmaPoolCell
    unsafe {
        let base = (&raw mut (*DMA_POOL.0.get()).bounce).cast::<u8>();
        core::slice::from_raw_parts_mut(base, byte_count)
    }
}

pub struct BigosHal;

// SAFETY: queue pages come from a static, page aligned, zeroed pool and are never handed
// out twice. share() returns the physical address of memory that is contiguous in RAM
unsafe impl Hal for BigosHal {
    fn dma_alloc(pages: usize, _direction: BufferDirection) -> (PhysAddr, NonNull<u8>) {
        let first_page = DMA_QUEUE_NEXT_PAGE.fetch_add(pages, Ordering::Relaxed);
        assert!(
            first_page + pages <= DMA_QUEUE_PAGES,
            "virtio: DMA queue pool exhausted"
        );

        // SAFETY: the page range is exclusively ours, it was reserved above
        let virt_pointer = unsafe {
            let base = (&raw mut (*DMA_POOL.0.get()).queue_area).cast::<u8>();
            let pointer = base.add(first_page * PAGE_SIZE);
            core::ptr::write_bytes(pointer, 0, pages * PAGE_SIZE);
            pointer
        };
        let phys_address = dma_virt_to_phys(virt_pointer as u64);

        (phys_address, NonNull::new(virt_pointer).unwrap())
    }

    unsafe fn dma_dealloc(_paddr: PhysAddr, _vaddr: NonNull<u8>, _pages: usize) -> i32 {
        // the queue lives as long as the kernel
        0
    }

    unsafe fn mmio_phys_to_virt(paddr: PhysAddr, _size: usize) -> NonNull<u8> {
        let virt_address = paddr + boot_info().hhdm_offset;

        NonNull::new(virt_address as *mut u8).unwrap()
    }

    unsafe fn share(buffer: NonNull<[u8]>, _direction: BufferDirection) -> PhysAddr {
        dma_virt_to_phys(buffer.as_ptr().cast::<u8>() as u64)
    }

    unsafe fn unshare(_paddr: PhysAddr, _buffer: NonNull<[u8]>, _direction: BufferDirection) {}
}

/// PCI configuration access through the legacy 0xCF8 / 0xCFC ports
pub struct PortIoCam;

impl PortIoCam {
    fn location(device_function: DeviceFunction) -> PciLocation {
        PciLocation::new(
            device_function.bus,
            device_function.device,
            device_function.function,
        )
    }
}

impl ConfigurationAccess for PortIoCam {
    fn read_word(&self, device_function: DeviceFunction, register_offset: u8) -> u32 {
        pci::config_read_dword(Self::location(device_function), register_offset)
    }

    fn write_word(&mut self, device_function: DeviceFunction, register_offset: u8, data: u32) {
        pci::config_write_dword(Self::location(device_function), register_offset, data);
    }

    unsafe fn unsafe_clone(&self) -> Self {
        PortIoCam
    }
}

pub struct VirtioBlk {
    device: VirtIOBlk<BigosHal, PciTransport>,
    pub capacity_sectors: u64,
    pub requests_completed: u64,
}

impl VirtioBlk {
    /// Finds the first modern virtio-blk function on the PCI bus
    pub fn find_and_init() -> DiskOpResult<Self> {
        let info = pci::find_device(VIRTIO_VENDOR_ID, VIRTIO_BLK_MODERN_DEVICE_ID)
            .ok_or(DiskOpError::DeviceNotFound)?;

        Self::init(info)
    }

    fn init(info: PciDeviceInfo) -> DiskOpResult<Self> {
        let already_claimed = VIRTIO_BLK_CLAIMED.swap(true, Ordering::AcqRel);
        debug_assert!(!already_claimed, "virtio-blk initialized twice");

        serial_println!(
            "virtio-blk: found at {:02x}:{:02x}.{}",
            info.location.bus,
            info.location.device,
            info.location.function
        );

        map_memory_bars(info.location);
        pci::enable_memory_and_bus_master(info.location);

        let device_function = DeviceFunction {
            bus: info.location.bus,
            device: info.location.device,
            function: info.location.function,
        };
        let mut root = PciRoot::new(PortIoCam);
        let transport = PciTransport::new::<BigosHal, _>(&mut root, device_function)
            .map_err(|_| DiskOpError::DeviceNotFound)?;
        let device =
            VirtIOBlk::<BigosHal, _>::new(transport).map_err(|_| DiskOpError::DeviceNotFound)?;
        let capacity_sectors = device.capacity();
        serial_println!("virtio-blk: {} sectors", capacity_sectors);

        Ok(Self {
            device,
            capacity_sectors,
            requests_completed: 0,
        })
    }

    pub fn sector_count(&self) -> u64 {
        self.capacity_sectors
    }

    fn is_range_valid(&self, start_sector: u64, byte_count: usize) -> bool {
        let sector_count = (byte_count / SECTOR_SIZE_BYTES) as u64;

        start_sector
            .checked_add(sector_count)
            .is_some_and(|end_sector| end_sector <= self.capacity_sectors)
    }

    /// The caller buffer is never handed to the device, every request goes through the bounce buffer
    pub fn read_sectors(&mut self, start_sector: u64, buffer: &mut [u8]) -> DiskOpResult<()> {
        debug_assert!(buffer.len().is_multiple_of(SECTOR_SIZE_BYTES));
        if !self.is_range_valid(start_sector, buffer.len()) {
            return Err(DiskOpError::InvalidSector);
        }

        let mut sector = start_sector;
        let mut done_bytes = 0;
        while done_bytes < buffer.len() {
            let chunk_bytes = min(buffer.len() - done_bytes, DMA_BOUNCE_BYTES);

            let bounce = bounce_buffer(chunk_bytes);
            self.device
                .read_blocks(sector as usize, bounce)
                .map_err(|_| DiskOpError::ReadError)?;
            buffer[done_bytes..done_bytes + chunk_bytes].copy_from_slice(bounce);
            self.requests_completed += 1;

            sector += (chunk_bytes / SECTOR_SIZE_BYTES) as u64;
            done_bytes += chunk_bytes;
        }

        Ok(())
    }

    pub fn write_sectors(&mut self, start_sector: u64, buffer: &[u8]) -> DiskOpResult<()> {
        debug_assert!(buffer.len().is_multiple_of(SECTOR_SIZE_BYTES));
        if !self.is_range_valid(start_sector, buffer.len()) {
            return Err(DiskOpError::InvalidSector);
        }

        let mut sector = start_sector;
        let mut done_bytes = 0;
        while done_bytes < buffer.len() {
            let chunk_bytes = min(buffer.len() - done_bytes, DMA_BOUNCE_BYTES);

            let bounce = bounce_buffer(chunk_bytes);
            bounce.copy_from_slice(&buffer[done_bytes..done_bytes + chunk_bytes]);
            self.device
                .write_blocks(sector as usize, bounce)
                .map_err(|_| DiskOpError::WriteError)?;
            self.requests_completed += 1;

            sector += (chunk_bytes / SECTOR_SIZE_BYTES) as u64;
            done_bytes += chunk_bytes;
        }

        Ok(())
    }

    pub fn flush(&mut self) -> DiskOpResult<()> {
        self.device.flush().map_err(|_| DiskOpError::WriteError)
    }
}

fn map_memory_bars(location: PciLocation) {
    let bars = pci::read_bars(location);
    debug_assert_eq!(bars.len(), BAR_COUNT as usize);

    for bar in bars {
        if let PciBar::Memory {
            address,
            size_bytes,
            ..
        } = bar
        {
            debug_assert!(size_bytes <= MAX_BAR_MAP_BYTES);
            if address != 0 {
                map_mmio_range(address, size_bytes);
            }
        }
    }
}
