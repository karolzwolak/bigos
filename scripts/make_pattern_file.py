#!/usr/bin/env python3
"""
Write a file of an exact byte size filled with a repeating ASCII pattern

Examples:
    scripts/make_pattern_file.py disk_templates/default/FILES/ABCD4M.TXT 4194304
    scripts/make_pattern_file.py disk_templates/default/FILES/ABCD8M.TXT 8388608
    scripts/make_pattern_file.py out.txt 1000 --pattern 0123456789 --line-length 0
"""

import argparse
import math
import sys
from pathlib import Path

DEFAULT_PATTERN = "ABCD"
DEFAULT_LINE_LENGTH = 60
NO_LINE_BREAKS = 0
NEWLINE = b"\n"
WRITE_CHUNK_BYTES = 1 << 20


def parse_size(text: str) -> int:
    size = int(text, 0)
    if size < 0:
        raise argparse.ArgumentTypeError("size must not be negative")
    return size


def repeating_unit(pattern: bytes, line_length: int) -> bytes:
    if line_length == NO_LINE_BREAKS:
        return pattern

    pattern_chars = math.lcm(len(pattern), line_length)
    stream = pattern * (pattern_chars // len(pattern))
    lines = [stream[start : start + line_length] for start in range(0, pattern_chars, line_length)]

    return NEWLINE.join(lines) + NEWLINE


def write_pattern_file(path: Path, size_bytes: int, pattern: bytes, line_length: int) -> None:
    unit = repeating_unit(pattern, line_length)
    chunk = unit * max(1, WRITE_CHUNK_BYTES // len(unit))

    path.parent.mkdir(parents=True, exist_ok=True)
    with path.open("wb") as file:
        remaining = size_bytes
        while remaining > 0:
            write_len = min(remaining, len(chunk))
            file.write(chunk[:write_len])
            remaining -= write_len


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("output", type=Path, help="file to create or overwrite")
    parser.add_argument(
        "size",
        type=parse_size,
        help="size in bytes with newlines, decimal or hex",
    )
    parser.add_argument(
        "--pattern",
        default=DEFAULT_PATTERN,
        help=f"ASCII text to repeat (default {DEFAULT_PATTERN})",
    )
    parser.add_argument(
        "--line-length",
        type=int,
        default=DEFAULT_LINE_LENGTH,
        help=f"pattern characters per line (default {DEFAULT_LINE_LENGTH}, 0 for one long line)",
    )
    args = parser.parse_args()

    if not args.pattern or not args.pattern.isascii() or "\n" in args.pattern:
        parser.error("the pattern must be non-empty ASCII without newlines")
    if args.line_length < 0:
        parser.error("the line length must not be negative")

    write_pattern_file(args.output, args.size, args.pattern.encode("ascii"), args.line_length)
    print(f"wrote {args.size} bytes to {args.output}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
