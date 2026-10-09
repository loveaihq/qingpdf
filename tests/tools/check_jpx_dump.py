"""Have OpenJPEG (through Pillow) decode the streams the JPEG 2000 tests make, and compare with our decoder.

    set JPX_DUMP_DIR=some\\folder
    cargo test -p qingpdf-core --lib jpx
    python tests/tools/check_jpx_dump.py some\\folder

The tests write `name.j2k` / `name.jp2` (made by the test encoder in `render/jpx/test_enc.rs`, which has no
wavelet transform: the subband coefficients are made up, so the pictures are noise) and `name.raw` (a line
"width height channels", then what our decoder made of it, 8 bits per channel, interleaved). Every stream is
decoded by OpenJPEG, an independent decoder, and the two must agree: exactly for reversible streams, within
a level or two for irreversible ones (one level for the conversion of 12 and 16 bits to 8). Streams OpenJPEG through
Pillow takes differently (chroma at lower resolution, sYCC, channel definitions, palettes, samples of 1 to 4 bits) are listed apart.
"""

import os
import sys

import numpy as np
from PIL import Image

folder = sys.argv[1]
same = diff = other = 0
for name in sorted(os.listdir(folder)):
    base, ext = os.path.splitext(name)
    if ext not in (".j2k", ".jp2"):
        continue
    with open(os.path.join(folder, base + ".raw"), "rb") as f:
        head, _, body = f.read().partition(b"\n")
    w, h, n = (int(v) for v in head.split())
    ours = np.frombuffer(body, dtype=np.uint8).reshape(h, w, n)
    try:
        im = Image.open(os.path.join(folder, name))
        im.load()
    except Exception as e:  # noqa: BLE001
        print(f"  {base}: OpenJPEG cannot decode it ({e})")
        other += 1
        continue
    theirs = np.asarray(im)
    if theirs.dtype == np.uint16:
        theirs = (theirs >> 8).astype(np.uint8)
    if theirs.ndim == 2:
        theirs = theirs[..., None]
    if base.startswith(("subsampled", "cdef", "baseline", "palette", "depth1_", "depth2_", "depth4_")):
        # OpenJPEG through Pillow does not take chroma at lower resolution, channel definitions or palettes the way
        # the PDF's rules (and ours) do, and it shifts samples of fewer than 8 bits up (1 becomes 128) where we scale them
        # to the full range (1 becomes 255); these are not comparable.
        print(f"  {base}: not comparable ({im.mode})")
        other += 1
        continue
    if theirs.shape != ours.shape:
        print(f"  {base}: shapes differ, ours {ours.shape} theirs {theirs.shape} ({im.mode})")
        other += 1
        continue
    # Pillow cuts 16-bit samples to their high byte, we round: the same pictures, the 8-bit conversion differs by one.
    d = np.abs(ours.astype(int) - theirs.astype(int))
    limit = 0
    if "irreversible" in base or base == "ict":
        limit = 2
    elif "_12" in base or base.startswith(("depth10", "depth12", "depth16")):
        limit = 1
    if d.max() <= limit:
        same += 1
    else:
        diff += 1
        print(f"  {base}: DIFFERENT, max {d.max()}, mean {d.mean():.3f} ({im.mode})")
print(f"{same} streams agree, {diff} differ, {other} not comparable")
sys.exit(1 if diff else 0)
