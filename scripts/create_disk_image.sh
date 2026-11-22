#!/usr/bin/env bash
# Usage: ./scripts/create_disk_image.sh -i <template_dir> -o <output_file> [-s <size_mb>] [-p <pattern_bytes>] [-x <dir>]... [-r <file>]...
set -euo pipefail

VOLUME_LABEL="BIGOS"
PATTERN_PERIOD=251
PATTERN_FILE_NAME="BIG.BIN"

INPUT_TEMPLATE=""
OUTPUT=""
SIZE_MB=64
PATTERN_BYTES=0
EXECUTABLE_DIRS=()
READ_ONLY_FILES=()

usage() {
    echo "Usage: $0 -i <template_dir> -o <output_file> [-s <size_mb>] [-p <pattern_bytes>] [-x <dir>]... [-r <file>]..."
    echo "  -i  Template directory copied into the image root (required)"
    echo "  -o  Output image path (required)"
    echo "  -s  Image size in MiB (default: ${SIZE_MB})"
    echo "  -p  Also create ${PATTERN_FILE_NAME} of this many bytes, byte i = i % ${PATTERN_PERIOD}"
    echo "  -x  Make every file in this template directory r-x (FAT ReadOnly + System, bigOS reads System as execute)"
    echo "  -r  Make this template file r-- (FAT ReadOnly)"
    echo "  -h  Show this help message"
}

while getopts "i:o:s:p:x:r:h" opt; do
    case $opt in
        i) INPUT_TEMPLATE="$OPTARG" ;;
        o) OUTPUT="$OPTARG" ;;
        s) SIZE_MB="$OPTARG" ;;
        p) PATTERN_BYTES="$OPTARG" ;;
        x) EXECUTABLE_DIRS+=("$OPTARG") ;;
        r) READ_ONLY_FILES+=("$OPTARG") ;;
        h) usage; exit 0 ;;
        *) usage >&2; exit 1 ;;
    esac
done

if [ -z "$INPUT_TEMPLATE" ] || [ -z "$OUTPUT" ]; then
    usage >&2
    exit 1
fi

if [ ! -d "$INPUT_TEMPLATE" ]; then
    echo "Error: template directory '$INPUT_TEMPLATE' does not exist." >&2
    exit 1
fi

mkdir -p "$(dirname "$OUTPUT")"

echo "Creating ${SIZE_MB}MiB FAT32 disk image: ${OUTPUT}"
echo "Using template directory: ${INPUT_TEMPLATE}"

rm -f "$OUTPUT"
dd if=/dev/zero of="$OUTPUT" bs=1M count="$SIZE_MB" status=none
mkfs.fat -F 32 -n "$VOLUME_LABEL" "$OUTPUT" > /dev/null

ITEM_COUNT=0

copy_to_image() {
    local source_dir="$1"
    local target_dir="$2"

    if [ "$target_dir" != "::" ]; then
        echo "  Creating directory: ${target_dir#::/}"
        mmd -i "$OUTPUT" "$target_dir"
    fi

    local item
    for item in "$source_dir"/*; do
        [ -e "$item" ] || continue
        local name
        name=$(basename "$item")
        if [ -f "$item" ]; then
            echo "  Copied ${target_dir#::}/${name}"
            mcopy -i "$OUTPUT" "$item" "${target_dir}/${name}"
            ITEM_COUNT=$((ITEM_COUNT + 1))
        elif [ -d "$item" ]; then
            copy_to_image "$item" "${target_dir}/${name}"
        fi
    done
}

copy_to_image "$INPUT_TEMPLATE" "::"

for dir in "${EXECUTABLE_DIRS[@]+"${EXECUTABLE_DIRS[@]}"}"; do
    for item in "$INPUT_TEMPLATE/$dir"/*; do
        [ -f "$item" ] || continue
        mattrib -i "$OUTPUT" +r +s "::/${dir}/$(basename "$item")"
    done
    echo "  Marked the files in ${dir} r-x"
done

for file in "${READ_ONLY_FILES[@]+"${READ_ONLY_FILES[@]}"}"; do
    mattrib -i "$OUTPUT" +r "::/${file}"
    echo "  Marked ${file} r--"
done

if [ "$PATTERN_BYTES" -gt 0 ]; then
    PATTERN_TMP=$(mktemp -d)
    trap 'rm -rf "$PATTERN_TMP"' EXIT

    for ((byte = 0; byte < PATTERN_PERIOD; byte++)); do
        printf "$(printf '\\%03o' "$byte")"
    done > "${PATTERN_TMP}/period"

    repeat_count=$((PATTERN_BYTES / PATTERN_PERIOD + 1))
    for ((i = 0; i < repeat_count; i++)); do
        cat "${PATTERN_TMP}/period"
    done | head -c "$PATTERN_BYTES" > "${PATTERN_TMP}/${PATTERN_FILE_NAME}"

    echo "  Generated ${PATTERN_FILE_NAME} (${PATTERN_BYTES} bytes)"
    mcopy -i "$OUTPUT" "${PATTERN_TMP}/${PATTERN_FILE_NAME}" "::/${PATTERN_FILE_NAME}"
    ITEM_COUNT=$((ITEM_COUNT + 1))
fi

echo "Done: ${OUTPUT} (${ITEM_COUNT} file(s), $(du -sh "$OUTPUT" | cut -f1))"
