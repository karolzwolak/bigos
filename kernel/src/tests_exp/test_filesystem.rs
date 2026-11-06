use crate::{
    filesystem::{
        FileType, fat32::test_data::create_fat32_image, init_filesystem, sirius::get_sirius,
    },
    serial_println,
};

pub fn test_filesystem() {
    serial_println!("\nTesting Filesystem");

    let fat32_image_data = create_fat32_image();

    let image_slice = &*fat32_image_data;

    match init_filesystem(image_slice) {
        Ok(_) => {
            serial_println!("Filesystem initialized");

            // Try to read root directory
            {
                let mut sirius = get_sirius();
                match sirius.list_directory("/") {
                    Ok(entries) => {
                        serial_println!("Root directory opened");
                        serial_println!("  Found {} entries:", entries.len());
                        for entry in &entries {
                            serial_println!("   - {} ({} bytes)", entry.name, entry.size);

                            if entry.file_type == FileType::File {
                                let mut buffer = [0u8; 64];
                                match sirius.read_file(&entry.name, 0, &mut buffer) {
                                    Ok(contents) => {
                                        serial_println!(
                                            "    Read file contents: '{}' {}",
                                            contents,
                                            entry.size
                                        );
                                        let content_str = core::str::from_utf8(&buffer[..contents])
                                            .unwrap_or("not utf8?");
                                        serial_println!("    Content: '{}'", content_str);
                                    }
                                    Err(e) => {
                                        serial_println!("    Failed to read file: {:?}", e);
                                    }
                                }
                            }
                        }

                        serial_println!("FILE CREATION");
                        match sirius.create_file("/newfile.txt") {
                            Ok(node) => {
                                serial_println!("Created new file: {}", node.name);
                            }
                            Err(e) => {
                                serial_println!("Failed to create file: {:?}", e);
                            }
                        }

                        serial_println!("DIRECTORY CREATION");
                        match sirius.create_directory("/somedir") {
                            Ok(node) => {
                                serial_println!("Created new directory: {}", node.name);
                            }
                            Err(e) => {
                                serial_println!("Failed to create directory: {:?}", e);
                            }
                        }

                        serial_println!("FILE IN SUBDIR CREATION");
                        match sirius.create_file("/somedir/nested.txt") {
                            Ok(node) => {
                                serial_println!("Created new file: {}", node.name);
                            }
                            Err(e) => {
                                serial_println!("Failed to create file: {:?}", e);
                            }
                        }
                    }
                    Err(e) => serial_println!("Failed to open root: {:?}", e),
                }
            }

            {
                let mut sirius = get_sirius();
                match sirius.list_directory("/") {
                    Ok(entries) => {
                        serial_println!("Root directory opened");
                        serial_println!("  Found {} entries:", entries.len());
                        for entry in &entries {
                            serial_println!("   - {} ({} bytes)", entry.name, entry.size);

                            if entry.file_type == FileType::File {
                                let mut buffer = [0u8; 64];
                                match sirius.read_file(&entry.name, 0, &mut buffer) {
                                    Ok(contents) => {
                                        serial_println!(
                                            "    Read file contents: '{}' {}",
                                            contents,
                                            entry.size
                                        );
                                        let content_str = core::str::from_utf8(&buffer[..contents])
                                            .unwrap_or("not utf8?");
                                        serial_println!("    Content: '{}'", content_str);
                                    }
                                    Err(e) => {
                                        serial_println!("    Failed to read file: {:?}", e);
                                    }
                                }
                            }
                        }
                    }
                    Err(e) => serial_println!("Failed to open root: {:?}", e),
                }

                serial_println!("FILE DELETION");
                match sirius.delete("/newfile.txt") {
                    Ok(_) => {
                        serial_println!("  deleted");
                    }
                    Err(e) => {
                        serial_println!("  Failed to delete file: {:?}", e);
                    }
                }
                match sirius.delete("/somedir") {
                    Ok(_) => {
                        serial_println!("  deleted");
                    }
                    Err(e) => {
                        serial_println!("  Failed to delete file: {:?}", e);
                    }
                }
                match sirius.list_directory("/") {
                    Ok(entries) => {
                        serial_println!("Root directory opened");
                        serial_println!("  Found {} entries:", entries.len());
                        for entry in &entries {
                            serial_println!("   - {} ({} bytes)", entry.name, entry.size);

                            if entry.file_type == FileType::File {
                                let mut buffer = [0u8; 64];
                                match sirius.read_file(&entry.name, 0, &mut buffer) {
                                    Ok(contents) => {
                                        serial_println!(
                                            "    Read file contents: '{}' {}",
                                            contents,
                                            entry.size
                                        );
                                        let content_str = core::str::from_utf8(&buffer[..contents])
                                            .unwrap_or("not utf8?");
                                        serial_println!("    Content: '{}'", content_str);
                                    }
                                    Err(e) => {
                                        serial_println!("    Failed to read file: {:?}", e);
                                    }
                                }
                            }
                        }
                    }
                    Err(e) => serial_println!("Failed to open root: {:?}", e),
                }
            }
        }
        Err(e) => serial_println!("Failed to initialize filesystem: {}", e),
    }
}
