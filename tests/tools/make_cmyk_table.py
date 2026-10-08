"""Make the DeviceCMYK to RGB table of crates/qingpdf-core/src/render/cmyk.bin.

    python tests/tools/make_cmyk_table.py [--check]

Draws a page of 9 x 9 x 9 x 9 one-point swatches (every colour whose four inks are multiples of 1/8), renders it
with PDFium at one pixel to the point, and writes the colour of each swatch: 6561 entries of three bytes, the cyan
index running slowest and the black fastest. The renderer looks a colour up by interpolating between the 16
entries around it.

PDFium is the reference the renderer's colours are compared against (what the other viewers show for DeviceCMYK is the
conversion of Adobe's SWOP profile, and PDFium keeps a table of it); the numbers written are measurements of its
output, not code. `--check` renders 2000 random colours too, and reports how far the table's interpolation is from
PDFium on them (and how far the 3a polynomial was).

Needs pypdfium2, numpy.
"""

import os
import random
import sys

import numpy as np
import pypdfium2 as pdfium

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from pdfmaker import Pdf  # noqa: E402

HERE = os.path.dirname(os.path.abspath(__file__))
OUT = os.path.normpath(os.path.join(HERE, "..", "..", "crates", "qingpdf-core", "src", "render", "cmyk.bin"))
N = 9


def render(swatches, cols):
    """Colours of swatches (each (c, m, y, k)) laid out `cols` to a row, a point each; returns their RGB."""
    rows = (len(swatches) + cols - 1) // cols
    pdf = Pdf()
    content = []
    for i, (c, m, y, k) in enumerate(swatches):
        x, row = i % cols, i // cols
        content.append(f"{c:.4f} {m:.4f} {y:.4f} {k:.4f} k {x} {rows - 1 - row} 1 1 re f")
    pdf.page(cols, rows, " ".join(content))
    path = os.path.join(os.environ.get("TEMP", "."), "cmyk_table_swatches.pdf")
    pdf.save(path)
    doc = pdfium.PdfDocument(path)
    img = np.asarray(doc[0].render(scale=1.0, may_draw_forms=False, draw_annots=False).to_pil().convert("RGB"), dtype=np.uint8)
    doc.close()
    return [tuple(img[i // cols, i % cols]) for i in range(len(swatches))]


def poly(c, m, y, k):
    """The polynomial of step 3a (pdf.js's coefficients)."""
    r = (255 + c * (-4.387332384609988 * c + 54.48615194189176 * m + 18.82290502165302 * y + 212.25662451639585 * k - 285.2331026137004)
         + m * (1.7149763477362134 * m - 5.6096736904047315 * y - 17.873870861415444 * k - 5.497006427196366)
         + y * (-2.5217340131683033 * y - 21.248923337353073 * k + 17.5119270841813)
         + k * (-21.86122147463605 * k - 189.48180835922747))
    g = (255 + c * (8.841041422036149 * c + 60.118027045597366 * m + 6.871425592049007 * y + 31.159100130055922 * k - 79.2970844816548)
         + m * (-15.310361306967817 * m + 17.575251261109482 * y + 131.35250912493976 * k - 190.9453302588951)
         + y * (4.444339102852739 * y + 9.8632861493405 * k - 24.86741582555878)
         + k * (-20.737325471181034 * k - 187.8045370971972))
    b = (255 + c * (0.8842522430003296 * c + 8.078677503112928 * m + 30.89978309703729 * y - 0.23883238689178934 * k - 14.183576799673286)
         + m * (10.49593273432072 * m + 63.02378494754052 * y + 50.606957656360734 * k - 112.23884253719253)
         + y * (0.03296041114873217 * y + 115.60384449646641 * k - 193.58209356861505)
         + k * (-22.33816807309886 * k - 180.12613974708367))
    return [min(255, max(0, v)) for v in (r, g, b)]


def lookup(table, c, m, y, k):
    """Quadrilinear interpolation in the table (the way the renderer does it)."""
    idx, frac = [], []
    for v in (c, m, y, k):
        t = min(max(v, 0.0), 1.0) * (N - 1)
        i = min(int(t), N - 2)
        idx.append(i)
        frac.append(t - i)
    out = np.zeros(3)
    for corner in range(16):
        w, at = 1.0, []
        for a in range(4):
            hi = (corner >> a) & 1
            w *= frac[a] if hi else 1.0 - frac[a]
            at.append(idx[a] + hi)
        out += w * table[at[0], at[1], at[2], at[3]]
    return out


def main():
    grid = [(c / (N - 1), m / (N - 1), y / (N - 1), k / (N - 1)) for c in range(N) for m in range(N) for y in range(N) for k in range(N)]
    colours = render(grid, 81)
    table = np.array(colours, dtype=np.uint8).reshape(N, N, N, N, 3)
    with open(OUT, "wb") as f:
        f.write(table.tobytes())
    print(f"wrote {OUT}: {table.size} bytes")
    if "--check" in sys.argv:
        rng = random.Random(5)
        samples = [(rng.random(), rng.random(), rng.random(), rng.random()) for _ in range(2000)]
        ref = np.array(render(samples, 50), dtype=float)
        ours = np.array([lookup(table.astype(float), *s) for s in samples])
        old = np.array([poly(*s) for s in samples])
        for name, got in (("table", ours), ("3a polynomial", old)):
            d = np.abs(got - ref)
            print(f"{name:14s} against PDFium on 2000 random colours: mean {d.mean():.2f}, 99th percentile {np.percentile(d, 99):.1f}, worst {d.max():.0f}")


if __name__ == "__main__":
    main()
