#![no_std]
#![no_main]

extern crate alloc;
extern crate kernel;

use core::panic::PanicInfo;
use kernel::{
    LIMINE_BASE_REVISION,
    filesystem::{fat32::test_data::create_fat32_image, init_filesystem, sirius::get_sirius},
    testing::{test_case, test_panic_handler},
};
use limine::{
    BaseRevision, RequestsEndMarker, RequestsStartMarker,
    request::{HhdmRequest, MemmapRequest},
};

#[used]
#[unsafe(link_section = ".requests_start_marker")]
static _START: RequestsStartMarker = RequestsStartMarker::new();
#[used]
#[unsafe(link_section = ".requests")]
static BASE_REVISION: BaseRevision = BaseRevision::with_revision(LIMINE_BASE_REVISION);
#[used]
#[unsafe(link_section = ".requests")]
static HHDM_REQUEST: HhdmRequest = HhdmRequest::new();
#[used]
#[unsafe(link_section = ".requests")]
static MEMORY_MAP_REQUEST: MemmapRequest = MemmapRequest::new();
#[used]
#[unsafe(link_section = ".requests_end_marker")]
static _END: RequestsEndMarker = RequestsEndMarker::new();

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    test_panic_handler(info)
}

#[unsafe(no_mangle)]
extern "C" fn kmain() -> ! {
    assert!(BASE_REVISION.is_supported());
    let hhdm_offset = HHDM_REQUEST.response().expect("no HHDM").offset;
    let memory_map = MEMORY_MAP_REQUEST
        .response()
        .expect("no memory map")
        .entries();
    kernel::testing::init_with_heap(hhdm_offset, memory_map);

    let image = create_fat32_image();
    init_filesystem(&*image).expect("filesystem init failed");

    kernel::testing::run_all_tests()
}

#[test_case]
fn test_init_and_list() {
    let entries = get_sirius()
        .list_directory("/")
        .expect("list_directory failed");
    assert!(!entries.is_empty(), "root directory is empty");
    let names: alloc::vec::Vec<_> = entries.iter().map(|e| e.name.as_str()).collect();
    assert!(
        names.contains(&"HELLO.TXT") || names.iter().any(|n| n.to_uppercase() == "HELLO.TXT"),
        "HELLO.TXT not found in {:?}",
        names
    );
}

#[test_case]
fn test_read_file() {
    let mut buf = [0u8; 64];
    let read = get_sirius()
        .read_file("HELLO.TXT", 0, &mut buf)
        .expect("read_file failed");
    assert!(read > 0, "read 0 bytes");
    let text = core::str::from_utf8(&buf[..read]).expect("file content not UTF-8");
    assert!(text.contains("BigOS"), "unexpected content: {}", text);
}

#[test_case]
fn test_create_and_delete() {
    {
        let mut sirius = get_sirius();
        sirius
            .create_file("/newfile.txt")
            .expect("create_file failed");
        sirius
            .create_directory("/newdir")
            .expect("create_directory failed");
    }
    {
        let mut sirius = get_sirius();
        sirius
            .delete("/newfile.txt")
            .expect("delete newfile.txt failed");
        sirius.delete("/newdir").expect("delete newdir failed");
    }
    let entries = get_sirius()
        .list_directory("/")
        .expect("list after delete failed");
    let names: alloc::vec::Vec<_> = entries.iter().map(|e| e.name.as_str()).collect();
    assert!(
        names
            .iter()
            .all(|name| name.to_uppercase() != "NEWFILE.TXT"),
        "newfile.txt still present after delete"
    );
}
