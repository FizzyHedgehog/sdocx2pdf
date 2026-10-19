#!/usr/bin/env python3
"""Probe Samsung Notes `.spi` media files.

The format is undocumented. This tool only parses the parts we have
confirmed by inspection; it does not decode pixels.

Usage:
    spi_probe.py <file.spi> [--blocks] [--dump N] [--stats]

Options:
    --blocks   list every block with its header fields (default)
    --dump N   hexdump block N
    --stats    byte statistics for every block
"""

from __future__ import annotations

import argparse
import struct
import sys
from pathlib import Path

# 10-byte tail shared by every block header. Used to locate block starts.
BLOCK_TAIL = bytes.fromhex("4302e0a02c0000003fff")
FILE_HEADER_LEN = 28
BLOCK_HEADER_LEN = 20

# Block payload sizes that show up repeatedly for "flat" planes
# (constant chroma / fully opaque alpha). Useful as a sanity check.
KNOWN_FLAT_BLOCK_SIZES = {1133: "flat plane", 1105: "flat plane (alpha?)"}


def file_header(data: bytes) -> dict:
    if len(data) < FILE_HEADER_LEN:
        raise ValueError("file too short for a 28-byte header")
    h = data[:FILE_HEADER_LEN]
    return {
        "declared_header_len": struct.unpack_from("<I", h, 0x00)[0],
        "version": struct.unpack_from("<I", h, 0x04)[0],
        "field_08": struct.unpack_from("<I", h, 0x08)[0],
        "profile_0c": h[0x0C:0x10].hex(),
        "profile_10": h[0x10:0x14].hex(),
        "profile_14": h[0x14:0x18].hex(),
        "payload_len": struct.unpack_from("<I", h, 0x18)[0],
        "payload_actual": len(data) - FILE_HEADER_LEN,
    }


def find_blocks(data: bytes) -> list[tuple[int, int]]:
    """Return (start, end) for each block, end exclusive."""
    starts: list[int] = []
    i = 0
    while True:
        j = data.find(BLOCK_TAIL, i)
        if j < 0:
            break
        starts.append(j - (BLOCK_HEADER_LEN - len(BLOCK_TAIL)))
        i = j + 1

    out = []
    for k, s in enumerate(starts):
        e = starts[k + 1] if k + 1 < len(starts) else len(data)
        out.append((s, e))
    return out


def describe_block(data: bytes, index: int, start: int, end: int) -> dict:
    h = data[start : start + BLOCK_HEADER_LEN]
    payload = data[start + BLOCK_HEADER_LEN : end]
    return {
        "index": index,
        "offset": start,
        "total": end - start,
        "size": len(payload),
        "magic": struct.unpack_from("<H", h, 0)[0],
        "flag": struct.unpack_from("<H", h, 2)[0],
        "field_04": struct.unpack_from("<I", h, 4)[0],
        "field_08": struct.unpack_from("<H", h, 8)[0],
        "ff_pct": 100.0 * payload.count(0xFF) / max(len(payload), 1),
        "zero_pct": 100.0 * payload.count(0x00) / max(len(payload), 1),
        "flat": KNOWN_FLAT_BLOCK_SIZES.get(len(payload)),
    }


def print_blocks(data: bytes) -> None:
    fh = file_header(data)
    print(f"file size          {len(data)}")
    print(f"header len field   {fh['declared_header_len']}")
    print(f"version            {fh['version']}")
    print(f"field 0x08         {fh['field_08']} (0x{fh['field_08']:x})")
    print(f"profile 0x0c..0x18 {fh['profile_0c']} {fh['profile_10']} {fh['profile_14']}")
    print(f"payload len        {fh['payload_len']} (actual {fh['payload_actual']})")
    print()

    blocks = find_blocks(data)
    print(f"{'#':>2} {'offset':>7} {'size':>7} {'f04':>8} {'f08':>4} {'ff%':>5} {'00%':>5}  note")
    for i, (s, e) in enumerate(blocks):
        b = describe_block(data, i, s, e)
        note = b["flat"] or ""
        print(
            f"{b['index']:>2} {b['offset']:>7} {b['size']:>7} {b['field_04']:>8} "
            f"{b['field_08']:>4} {b['ff_pct']:>5.1f} {b['zero_pct']:>5.1f}  {note}"
        )


def dump_block(data: bytes, n: int, limit: int = 512) -> None:
    blocks = find_blocks(data)
    if n >= len(blocks):
        raise SystemExit(f"block {n} does not exist (found {len(blocks)})")
    s, e = blocks[n]
    payload = data[s + BLOCK_HEADER_LEN : e]
    print(f"block {n}: {len(payload)} bytes at offset {s + BLOCK_HEADER_LEN}")
    for base in range(0, min(len(payload), limit), 16):
        chunk = payload[base : base + 16]
        print(f"{base:06x}  {' '.join(f'{b:02x}' for b in chunk)}")


def main() -> int:
    ap = argparse.ArgumentParser(description="Inspect Samsung Notes .spi files")
    ap.add_argument("path", type=Path)
    ap.add_argument("--blocks", action="store_true", help="list blocks (default)")
    ap.add_argument("--dump", type=int, metavar="N", help="hexdump block N")
    ap.add_argument("--stats", action="store_true", help="same as --blocks")
    args = ap.parse_args()

    data = args.path.read_bytes()

    if args.dump is not None:
        dump_block(data, args.dump)
    else:
        print_blocks(data)
    return 0


if __name__ == "__main__":
    sys.exit(main())
