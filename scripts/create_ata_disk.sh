#!/usr/bin/env bash
# Usage: ./scripts/create_ata_disk.sh -i [input_template] -o [output_file] -s [size_mb]
set -e

INPUT_TEMPLATE=""
OUTPUT_FILENAME="ata_disk.img"
SIZE_MB=256

while getopts "i:o:s:h" opt; do
    case $opt in
        i)
            INPUT_TEMPLATE="$OPTARG"
            ;;
        o)
            OUTPUT_FILENAME="$OPTARG"
            ;;
        s)
            SIZE_MB="$OPTARG"
            ;;
        h)
            echo "Usage: $0 -i <input_template_dir> -o <output_filename> -s <size_mb>"
            echo "  -i  Input template directory (required)"
            echo "  -o  Output filename (default: ata_disk.img)"
            echo "  -s  Size in MB (default: 256)"
            echo "  -h  Show this help message"
            exit 0
            ;;
        \?)
            echo "Invalid option: -$OPTARG" >&2
            exit 1
            ;;
        :)
            echo "Option -$OPTARG requires an argument." >&2
            exit 1
            ;;
    esac
done

if [ -z "$INPUT_TEMPLATE" ]; then
    echo "Error: Input template directory is required. Use -i to specify." >&2
    exit 1
fi

if [ ! -d "$INPUT_TEMPLATE" ]; then
    echo "Error: Input template directory '$INPUT_TEMPLATE' does not exist." >&2
    exit 1
fi

OUT_DIR="storage"
mkdir -p "${OUT_DIR}"
OUTPUT="${OUT_DIR}/${OUTPUT_FILENAME}"

echo "Creating ${SIZE_MB}MB FAT32 disk image: ${OUTPUT}"
echo "Using template directory: ${INPUT_TEMPLATE}"

dd if=/dev/zero of="${OUTPUT}" bs=1M count="${SIZE_MB}" status=none
mkfs.fat -F 32 -n "MOFUOS" "${OUTPUT}"

ITEM_COUNT=0

copy_to_image() {
    local source_dir="$1"
    local target_dir="$2"
    
    if [ "$target_dir" != "::/" ] && [ "$target_dir" != "::" ]; then
        local display_path="${target_dir#::/}"
        echo "  Creating directory: ${display_path}"
        mmd -i "${OUTPUT}" "${target_dir}" 2>/dev/null || true
    fi
    
    for item in "${source_dir}"/*; do
        if [ -f "$item" ]; then
            local filename=$(basename "$item")
            local display_path="${target_dir#::/}"
            if [ -n "$display_path" ]; then
                echo "  Copied ${display_path}/${filename}"
            else
                echo "  Copied ${filename}"
            fi
            mcopy -i "${OUTPUT}" "$item" "${target_dir}/${filename}"
            ITEM_COUNT=$((ITEM_COUNT + 1))
        elif [ -d "$item" ]; then
            local dirname=$(basename "$item")
            local new_target="${target_dir}/${dirname}"
            copy_to_image "$item" "$new_target"
        fi
    done
}

if [ -d "${INPUT_TEMPLATE}" ]; then
    copy_to_image "${INPUT_TEMPLATE}" "::"
    
    if [ $ITEM_COUNT -eq 0 ]; then
        echo "  No files or directories found in template directory"
    else
        echo "  Copied $ITEM_COUNT item(s) total"
    fi
fi

echo "Done: ${OUTPUT} ($(du -sh "${OUTPUT}" | cut -f1))"

exit 0