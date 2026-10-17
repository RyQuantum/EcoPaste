"""Writes an RGB PNG of pseudo-random pixels (incompressible, so it stays large).

Usage: python3 gen_png.py <out.png> <width> <height>
"""

import random
import struct
import sys
import zlib
from pathlib import Path


def _chunk(kind: bytes, data: bytes) -> bytes:
    body = kind + data
    return struct.pack(">I", len(data)) + body + struct.pack(">I", zlib.crc32(body) & 0xFFFFFFFF)


def write_png(out: Path, width: int, height: int) -> None:
    rng = random.Random(width * 31 + height)
    rows = b"".join(b"\x00" + rng.randbytes(width * 3) for _ in range(height))
    png = (
        b"\x89PNG\r\n\x1a\n"
        + _chunk(b"IHDR", struct.pack(">IIBBBBB", width, height, 8, 2, 0, 0, 0))
        + _chunk(b"IDAT", zlib.compress(rows, 9))
        + _chunk(b"IEND", b"")
    )
    Path(out).write_bytes(png)


if __name__ == "__main__":
    write_png(Path(sys.argv[1]), int(sys.argv[2]), int(sys.argv[3]))
