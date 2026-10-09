"""Make the small JPEG 2000 files the decoder's tests are tried on (3c2-2).

Pillow (with the OpenJPEG it ships) writes them and reads them back: each `name.jp2` comes with
`name.raw`, what OpenJPEG decodes it to (8 bits per channel, interleaved; 16-bit samples are cut to
their high byte), and, for some, `name_r1.raw` and `name_r2.raw`, the same decoded at one and two
resolution levels fewer. The Rust tests (`render/jpx/tests.rs`) decode the files with our decoder and
compare. The test files are made by this script; the JPEG 2000 decoder itself is our own.

    python tests/tools/make_jpx_fixtures.py [output folder]
"""

import os
import sys

import numpy as np
from PIL import Image

HERE = os.path.dirname(os.path.abspath(__file__))
OUT = sys.argv[1] if len(sys.argv) > 1 else os.path.normpath(os.path.join(HERE, "..", "..", "crates", "qingpdf-core", "tests", "jpx_fixtures"))
os.makedirs(OUT, exist_ok=True)


def picture(w, h, channels, seed=1):
    """Smooth gradients, a few edges and a little noise: content in every subband."""
    rng = np.random.default_rng(seed)
    y, x = np.mgrid[0:h, 0:w]
    planes = []
    for c in range(channels):
        base = (x * (3 + c) + y * (2 + 2 * c) + 40 * c) % 256
        edge = np.where(((x // 9 + y // 7) % 2) == 0, 40, 0)
        noise = rng.integers(0, 14, size=(h, w))
        planes.append(np.clip(base * 0.6 + edge + noise, 0, 255))
    a = np.stack(planes, axis=-1).astype(np.uint8)
    return a[..., 0] if channels == 1 else a


def decode(path, reduce=0):
    im = Image.open(path)
    im.reduce = reduce
    im.load()
    return im


def raw_of(im):
    a = np.asarray(im)
    if a.dtype == np.uint16:
        a = (a >> 8).astype(np.uint8)
    return a.astype(np.uint8).tobytes()


def make(name, arr, mode=None, reduces=(), raw=True, **kw):
    im = Image.fromarray(arr) if mode is None else Image.fromarray(arr).convert(mode) if mode != "I;16" else Image.fromarray(arr)
    path = os.path.join(OUT, name + ".jp2")
    im.save(path, "JPEG2000", **kw)
    for r in (0, *reduces) if raw else ():
        try:
            d = decode(path, r)
        except OSError as e:
            print(f"  {name}: reduce {r} not decodable by Pillow ({e})")
            continue
        suffix = "" if r == 0 else f"_r{r}"
        with open(os.path.join(OUT, f"{name}{suffix}.raw"), "wb") as f:
            f.write(raw_of(d))
        if r == 0:
            print(f"{name}: {d.size[0]}x{d.size[1]} {d.mode}, {os.path.getsize(path)} bytes")


W, H = 61, 47
rgb = picture(W, H, 3)
gray = picture(W, H, 1, seed=2)
big = picture(130, 110, 3, seed=3)
mid = picture(72, 56, 3, seed=6)

make("rgb_lossless", rgb, reduces=(1, 2), irreversible=False, num_resolutions=4)
make("rgb_lossy", rgb, reduces=(1, 2), irreversible=True, quality_mode="rates", quality_layers=[12, 5, 1], num_resolutions=4)
make("gray_lossless", gray, reduces=(1, 3), irreversible=False, num_resolutions=5)
make("gray_lossy", gray, reduces=(1,), irreversible=True, quality_mode="dB", quality_layers=[36], num_resolutions=5)
make("rgb_tiles", mid, reduces=(1,), irreversible=False, num_resolutions=3, tile_size=(48, 40))
make("rgb_tiles_lossy", mid, reduces=(1,), irreversible=True, quality_mode="rates", quality_layers=[10, 3], num_resolutions=3, tile_size=(40, 40))
make("rgb_offset", rgb, irreversible=False, num_resolutions=3, offset=(3, 5), tile_size=(32, 32), tile_offset=(1, 2))
for i, order in enumerate(("LRCP", "RLCP", "RPCL", "PCRL", "CPRL")):
    # The same picture in every order (lossless): one expected file for all of them, `order.raw`.
    make(
        "order_" + order.lower(),
        mid,
        raw=False,
        irreversible=False,
        num_resolutions=4,
        progression=order,
        quality_mode="rates",
        quality_layers=[8, 3, 1],
        precinct_size=[(16, 16), (16, 16), (32, 32), (64, 64)],
        codeblock_size=(16, 16),
        tile_size=(40, 32),
    )
with open(os.path.join(OUT, "order.raw"), "wb") as f:
    f.write(raw_of(decode(os.path.join(OUT, "order_lrcp.jp2"))))
make("blocks_small", rgb, irreversible=False, num_resolutions=3, codeblock_size=(4, 8))
make("rgba_lossless", np.dstack([rgb, picture(W, H, 1, seed=9)]), mode="RGBA", irreversible=False, num_resolutions=3)
make("gray16", (picture(W, H, 1, seed=4).astype(np.uint16) * 257), mode="I;16", irreversible=False, num_resolutions=3)
make("plt", rgb, irreversible=False, num_resolutions=3, plt=True)
make("one_level", gray, irreversible=False, num_resolutions=1)
make("tiny", picture(5, 3, 3, seed=5), irreversible=False, num_resolutions=2)
