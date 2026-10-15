"""Pack crates/qingpdf-base/src/render/cmyk.bin into cmyk.z, the form the program carries.

    python tests/tools/pack_cmyk_table.py

The table is 9 x 9 x 9 x 9 colours of three bytes, cyan slowest and black fastest. Neighbouring colours are close, so the
file is stored as the difference of each entry from the one before it along the yellow axis, and then of each of those from
the one before it along the cyan axis, and compressed with zlib (19,683 bytes become about 7,600). The renderer undoes the
two differences after inflating it the first time a CMYK colour is converted; a unit test checks the result against cmyk.bin
byte for byte. Run this after `make_cmyk_table.py` has written a new cmyk.bin.
"""

import os
import zlib

HERE = os.path.dirname(os.path.abspath(__file__))
DIR = os.path.normpath(os.path.join(HERE, "..", "..", "crates", "qingpdf-base", "src", "render"))
N = 9
STRIDE_C, STRIDE_Y = 729 * 3, 9 * 3


def pack(raw: bytes) -> bytes:
    assert len(raw) == N ** 4 * 3
    # Difference along yellow (the entries with yellow index 0 stay as they are), computed from the original values.
    a = bytearray(raw)
    for i in range(len(raw)):
        if (i // STRIDE_Y) % N != 0:
            a[i] = (raw[i] - raw[i - STRIDE_Y]) & 255
    # Then along cyan, from the result of the first.
    b = bytearray(a)
    for i in range(STRIDE_C, len(a)):
        b[i] = (a[i] - a[i - STRIDE_C]) & 255
    return zlib.compress(bytes(b), 9)


def unpack(packed: bytes) -> bytes:
    t = bytearray(zlib.decompress(packed))
    for i in range(STRIDE_C, len(t)):
        t[i] = (t[i] + t[i - STRIDE_C]) & 255
    for i in range(len(t)):
        if (i // STRIDE_Y) % N != 0:
            t[i] = (t[i] + t[i - STRIDE_Y]) & 255
    return bytes(t)


if __name__ == "__main__":
    raw = open(os.path.join(DIR, "cmyk.bin"), "rb").read()
    packed = pack(raw)
    assert unpack(packed) == raw
    with open(os.path.join(DIR, "cmyk.z"), "wb") as f:
        f.write(packed)
    print(f"cmyk.bin {len(raw)} -> cmyk.z {len(packed)} bytes")
