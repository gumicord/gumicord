"""Application icon, generated without dependencies.

A white chat bubble on a dark rounded square. Rendered at the requested
size directly (no scaling step), so every shipped size is crisp. Run:
    python3 packaging/make-icon.py 256 packaging/linux/icon.png
    python3 packaging/make-icon.py 1024 /tmp/icon-1024.png
"""

import struct
import sys
import zlib


def rounded_rect(w, h, r):
    """Coverage mask (0..1) of a centered rounded rect via distance field."""

    def cover(x, y):
        cx = min(max(x, r), w - 1 - r)
        cy = min(max(y, r), h - 1 - r)
        dx, dy = x - cx, y - cy
        d = (dx * dx + dy * dy) ** 0.5
        # 1px antialiased edge.
        return min(max(r + 0.5 - d, 0.0), 1.0)

    return cover


def bubble(w, h):
    """Coverage mask of a speech bubble: rounded body plus tail."""

    def cover(x, y):
        # Body occupies the upper ~72%.
        bw, bh = w * 0.68, h * 0.52
        bx, by = (w - bw) / 2, h * 0.14
        r = bh * 0.28
        cx = min(max(x, bx + r), bx + bw - r)
        cy = min(max(y, by + r), by + bh - r)
        body = r + 0.5 - ((x - cx) ** 2 + (y - cy) ** 2) ** 0.5
        # Tail: triangle from the body's bottom-left down to the left.
        tx, ty = bx + bw * 0.22, by + bh
        tail = 0.0
        if y >= ty:
            half = (bw * 0.16) * max(0.0, 1.0 - (y - ty) / (h * 0.16))
            if abs(x - tx) <= half:
                tail = 1.0
        return min(max(max(body, tail), 0.0), 1.0)

    return cover


def render(size):
    bg = (35, 40, 58)
    fg = (245, 247, 252)
    plate = rounded_rect(size, size, size * 0.22)
    mark = bubble(size, size)
    out = bytearray()
    for y in range(size):
        for x in range(size):
            bg_a = plate(x + 0.5, y + 0.5)
            fg_a = mark(x + 0.5, y + 0.5)
            r = bg[0] * bg_a * (1 - fg_a) + fg[0] * fg_a
            g = bg[1] * bg_a * (1 - fg_a) + fg[1] * fg_a
            b = bg[2] * bg_a * (1 - fg_a) + fg[2] * fg_a
            a = max(bg_a, fg_a)
            out += bytes(
                (
                    round(r),
                    round(g),
                    round(b),
                    round(a * 255),
                )
            )
    return bytes(out)


def write_png(path, size, pixels):
    def chunk(kind, data):
        c = struct.pack(">I", len(data)) + kind + data
        return c + struct.pack(">I", zlib.crc32(kind + data) & 0xFFFFFFFF)

    ihdr = struct.pack(">IIBBBBB", size, size, 8, 6, 0, 0, 0)
    raw = b"".join(
        b"\x00" + pixels[y * size * 4 : (y + 1) * size * 4] for y in range(size)
    )
    with open(path, "wb") as f:
        f.write(b"\x89PNG\r\n\x1a\n")
        f.write(chunk(b"IHDR", ihdr))
        f.write(chunk(b"IDAT", zlib.compress(raw, 9)))
        f.write(chunk(b"IEND", b""))


if __name__ == "__main__":
    size = int(sys.argv[1])
    write_png(sys.argv[2], size, render(size))
    print(f"wrote {sys.argv[2]} ({size}x{size})")
