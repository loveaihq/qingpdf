"""Make the synthetic test pages of step 3c (transparency, shadings, patterns, optional content, CMYK).

    python tests/tools/make_transparency_fixtures.py [--perf]

Writes tests/corpus/public/transparency/*.pdf (small files, one or two pages each) and, with --perf, the heavy
page the render speed test uses, tests/out/perf/transparency-heavy.pdf (not committed). Compare them with PDFium:

    python tests/tools/render_compare.py --engine-compare --only transparency/ --all

Everything here is written by the qingpdf project (MIT OR Apache-2.0); the files have no third-party content.
No fonts are used, so the pages show the transparency and the gradients and nothing else.
"""

import os
import struct
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from pdfmaker import Pdf, circle  # noqa: E402

HERE = os.path.dirname(os.path.abspath(__file__))
OUT = os.path.normpath(os.path.join(HERE, "..", "corpus", "public", "transparency"))
PERF = os.path.normpath(os.path.join(HERE, "..", "out", "perf"))


def nums(values):
    return " ".join(f"{v:g}" for v in values)


def exp_fn(c0, c1):
    return f"<< /FunctionType 2 /Domain [0 1] /C0 [{nums(c0)}] /C1 [{nums(c1)}] /N 1 >>"


def stitch_fn(colours):
    """A function through the colours, evenly spaced."""
    parts = [exp_fn(a, b) for a, b in zip(colours, colours[1:])]
    bounds = [i / len(parts) for i in range(1, len(parts))]
    return f"<< /FunctionType 3 /Domain [0 1] /Functions [{' '.join(parts)}] /Bounds [{nums(bounds)}] /Encode [{'0 1 ' * len(parts)}] >>"


def checker(x, y, w, h, size=10):
    """A grey checker board behind a cell, to see through to."""
    out = ["0.85 g"]
    for i in range(int(w // size)):
        for j in range(int(h // size)):
            if (i + j) % 2 == 0:
                out.append(f"{x + i * size} {y + j * size} {size} {size} re f")
    return " ".join(out) + " "


def stripes(x, y, w, h):
    """Three coloured bands and a grey one: a backdrop that shows what a blend mode does."""
    t = w / 3
    return (f"0.9 0.2 0.2 rg {x} {y} {t:.2f} {h} re f 0.2 0.7 0.3 rg {x + t:.2f} {y} {t:.2f} {h} re f "
            f"0.3 0.3 0.9 rg {x + 2 * t:.2f} {y} {t:.2f} {h} re f 0.5 g {x} {y} {w} {h / 4:.2f} re f ")


def form(pdf, bbox, content, resources="<< >>", group=None, matrix=None):
    entries = f"/Type /XObject /Subtype /Form /BBox [{nums(bbox)}] /Resources {resources}"
    if group is not None:
        entries += f" /Group << /S /Transparency {group} >>"
    if matrix:
        entries += f" /Matrix [{nums(matrix)}]"
    return pdf.stream(entries, content)


def save(pdf, name, out=OUT):
    os.makedirs(out, exist_ok=True)
    path = os.path.join(out, name)
    pdf.save(path)
    print(f"wrote {os.path.relpath(path)} ({os.path.getsize(path)} bytes)")


# --- blend modes ------------------------------------------------------------------------------------------

BLEND_MODES = ["Normal", "Multiply", "Screen", "Overlay", "Darken", "Lighten", "ColorDodge", "ColorBurn",
               "HardLight", "SoftLight", "Difference", "Exclusion", "Hue", "Saturation", "Color", "Luminosity"]


def blend_modes():
    pdf = Pdf()
    gstates = []
    for i, mode in enumerate(BLEND_MODES):
        gstates.append(f"/B{i} << /BM /{mode} >> /C{i} << /BM /{mode} /ca 0.6 >>")
    resources = f"<< /ExtGState << {' '.join(gstates)} >> >>"
    content = []
    for i in range(16):
        x, y = (i % 4) * 80, (3 - i // 4) * 80
        content.append(stripes(x, y, 80, 80))
        # An opaque rectangle and, over the stripes too, a half-transparent circle, both blended.
        content.append(f"q /B{i} gs 0.95 0.75 0.2 rg {x + 10} {y + 10} 60 60 re f Q ")
        content.append(f"q /C{i} gs 0.2 0.8 0.9 rg {circle(x + 40, y + 40, 22)} f Q ")
    pdf.page(320, 320, "".join(content), resources)
    save(pdf, "blend-modes.pdf")


# --- soft masks -------------------------------------------------------------------------------------------

def soft_masks():
    pdf = Pdf()
    # The masks: a gradient (luminosity), shapes of different opacity (alpha), a gradient with a backdrop colour and a transfer function.
    axial = f"<< /ShadingType 2 /ColorSpace /DeviceGray /Coords [0 0 120 0] /Function {exp_fn([1], [0])} /Extend [true true] >>"
    g_lum = form(pdf, [0, 0, 120, 120], "/Sh sh", f"<< /Shading << /Sh {axial} >> >>", group="/CS /DeviceGray")
    g_alpha = form(pdf, [0, 0, 120, 120], f"0 g {circle(60, 60, 50)} f /A gs 0 g {circle(60, 60, 25)} f",
                   "<< /ExtGState << /A << /ca 0.5 >> >> >>", group="")
    g_small = form(pdf, [20, 20, 100, 100], "0.5 g 20 20 80 80 re f 1 g 40 40 40 40 re f", group="/CS /DeviceGray")
    tr = "<< /FunctionType 2 /Domain [0 1] /C0 [1] /C1 [0] /N 1 >>"
    states = {
        "M1": f"<< /SMask << /Type /Mask /S /Luminosity /G {g_lum} 0 R >> >>",
        "M2": f"<< /SMask << /Type /Mask /S /Alpha /G {g_alpha} 0 R >> >>",
        "M3": f"<< /SMask << /Type /Mask /S /Luminosity /G {g_small} 0 R /BC [1] /TR {tr} >> >>",
        "M4": f"<< /SMask << /Type /Mask /S /Luminosity /G {g_small} 0 R /BC [0.5] >> >>",
        "N": "<< /SMask /None >>",
    }
    # A group drawn through a mask: two overlapping circles.
    circles = form(pdf, [0, 0, 120, 120], f"0.9 0.1 0.1 rg {circle(45, 60, 35)} f 0.1 0.3 0.9 rg {circle(75, 60, 35)} f", group="/I true")
    resources = f"<< /ExtGState << {' '.join(f'/{k} {v}' for k, v in states.items())} >> /XObject << /Circles {circles} 0 R >> >>"
    content = []
    for i in range(4):
        x, y = (i % 2) * 160 + 15, (1 - i // 2) * 160 + 15
        content.append(checker(x, y, 120, 120))
        if i == 0:
            content.append(f"q 1 0 0 1 {x} {y} cm /M1 gs 0.1 0.2 0.9 rg 0 0 120 120 re f Q ")
        elif i == 1:
            content.append(f"q 1 0 0 1 {x} {y} cm /M2 gs 0.9 0.1 0.1 rg 0 0 120 120 re f Q ")
        elif i == 2:
            content.append(f"q 1 0 0 1 {x} {y} cm /M3 gs 0.1 0.6 0.2 rg 0 0 120 120 re f Q ")
        else:
            content.append(f"q 1 0 0 1 {x} {y} cm /M4 gs /Circles Do Q ")
    pdf.page(320, 320, "".join(content), resources)
    save(pdf, "soft-masks.pdf")


# --- groups and knockout -----------------------------------------------------------------------------------

def groups():
    pdf = Pdf()
    cmy = (f"1 0 0 rg {circle(45, 65, 28)} f 0 1 0 rg {circle(75, 65, 28)} f 0 0 1 rg {circle(60, 40, 28)} f ")
    plain = form(pdf, [0, 0, 120, 100], cmy, group=None)
    group = form(pdf, [0, 0, 120, 100], cmy, group="/I true")
    blend_res = "<< /ExtGState << /M << /BM /Multiply >> >> >>"
    mult = "/M gs 0.2 0.8 0.9 rg 10 10 70 60 re f 0.95 0.6 0.1 rg 40 30 70 60 re f "
    iso = form(pdf, [0, 0, 120, 100], mult, blend_res, group="/I true")
    noniso = form(pdf, [0, 0, 120, 100], mult, blend_res, group="/I false")
    alpha_res = "<< /ExtGState << /A << /ca 0.5 >> >> >>"
    squares = "/A gs 0.9 0.1 0.1 rg 10 10 70 70 re f 0.1 0.2 0.9 rg 40 30 70 60 re f 0.1 0.7 0.2 rg 25 55 70 40 re f "
    knock = form(pdf, [0, 0, 120, 100], squares, alpha_res, group="/I true /K true")
    noknock = form(pdf, [0, 0, 120, 100], squares, alpha_res, group="/I true /K false")
    resources = (f"<< /ExtGState << /H << /ca 0.5 >> >> /XObject << /Plain {plain} 0 R /Group {group} 0 R /Iso {iso} 0 R /Non {noniso} 0 R "
                 f"/Knock {knock} 0 R /No {noknock} 0 R >> >>")
    cells = [("Plain", "H"), ("Group", "H"), ("Iso", None), ("Non", None), ("Knock", "H"), ("No", "H")]
    content = []
    for i, (name, gs) in enumerate(cells):
        x, y = (i % 2) * 160 + 15, (2 - i // 2) * 105 + 5
        # A backdrop with colour in it.
        content.append(f"0.95 0.95 0.6 rg {x} {y} 130 100 re f 0.6 0.8 0.95 rg {x} {y} 130 35 re f ")
        content.append(f"q 1 0 0 1 {x + 5} {y} cm {'/' + gs + ' gs ' if gs else ''}/{name} Do Q ")
    pdf.page(320, 330, "".join(content), resources)
    save(pdf, "groups.pdf")


# --- shadings -----------------------------------------------------------------------------------------------

def mesh_data(vertices, with_flag=True):
    """Type 4 (flag, x, y, r, g, b) or type 5 (x, y, r, g, b) vertices with coordinates in 0..1: bytes of 8 bits each."""
    data = bytearray()
    for v in vertices:
        flag, (x, y), (r, g, b) = v if with_flag else (None,) + tuple(v)
        if with_flag:
            data.append(flag)
        data += bytes([round(x * 255), round(y * 255), round(r * 255), round(g * 255), round(b * 255)])
    return bytes(data)


def patch_data(points, colours, flag=0, extra_points=()):
    """A patch: flag, the 12 boundary points (then the inside ones of a tensor patch), the corner colours; 8 bits each."""
    data = bytearray([flag])
    for x, y in list(points) + list(extra_points):
        data += bytes([round(x * 255), round(y * 255)])
    for r, g, b in colours:
        data += bytes([round(r * 255), round(g * 255), round(b * 255)])
    return bytes(data)


def shadings():
    pdf = Pdf()
    size = 90
    mesh = f"/ColorSpace /DeviceRGB /BitsPerCoordinate 8 /BitsPerComponent 8 /Decode [0 {size} 0 {size} 0 1 0 1 0 1]"
    red, green, blue, yellow, white = (1, 0, 0), (0, 0.7, 0), (0, 0, 1), (1, 0.9, 0), (1, 1, 1)
    xobjs = {}
    # Type 1: (x, y) to a colour, by a calculator function.
    f1 = pdf.stream("/FunctionType 4 /Domain [0 1 0 1] /Range [0 1 0 1 0 1]", "{ 2 copy mul 3 1 roll exch }")
    xobjs["S1"] = f"<< /ShadingType 1 /ColorSpace /DeviceRGB /Domain [0 1 0 1] /Matrix [{size} 0 0 {size} 0 0] /Function {f1} 0 R >>"
    # Type 2: three stops, extended at both ends, along a slanted axis that is shorter than the cell.
    xobjs["S2"] = (f"<< /ShadingType 2 /ColorSpace /DeviceRGB /Coords [20 20 70 60] /Function {stitch_fn([red, yellow, blue])} /Extend [true true] >>")
    # Type 3: circles of different size and place, extended at the large end only.
    xobjs["S3"] = f"<< /ShadingType 3 /ColorSpace /DeviceRGB /Coords [30 45 8 60 45 32] /Function {stitch_fn([white, green, blue])} /Extend [false true] >>"
    # Type 4: two triangles joined by an edge flag.
    tri = [(0, (0.05, 0.05), red), (0, (0.95, 0.1), green), (0, (0.5, 0.95), blue), (1, (1.0, 0.9), yellow)]
    xobjs["S4"] = pdf.stream(f"/ShadingType 4 {mesh} /BitsPerFlag 8", mesh_data(tri))
    # Type 5: a lattice of 3 by 3 vertices that is not flat.
    lattice = [((0.05, 0.05), red), ((0.5, 0.15), yellow), ((0.95, 0.05), green),
               ((0.1, 0.5), blue), ((0.5, 0.5), white), ((0.9, 0.55), red),
               ((0.05, 0.95), green), ((0.5, 0.85), blue), ((0.95, 0.95), yellow)]
    xobjs["S5"] = pdf.stream(f"/ShadingType 5 {mesh} /VerticesPerRow 3", mesh_data(lattice, with_flag=False))
    # Type 6: a Coons patch with bowed top and bottom, then one joined to its right edge (edge flag 2: the right edge of the first,
    # its points 7 to 10, are the left edge of the second, which brings 8 points and 2 colours of its own).
    boundary = [(0.05, 0.1), (0.0, 0.35), (0.05, 0.65), (0.05, 0.9), (0.2, 1.0), (0.35, 0.8), (0.5, 0.9),
                (0.52, 0.65), (0.48, 0.35), (0.5, 0.1), (0.35, 0.0), (0.2, 0.2)]
    second = [(0.65, 0.0), (0.8, 0.15), (0.95, 0.1), (0.97, 0.35), (0.93, 0.65), (0.95, 0.9), (0.8, 1.0), (0.65, 0.85)]
    coons = patch_data(boundary, [red, green, blue, yellow]) + patch_data(second, [red, green], flag=2)
    xobjs["S6"] = pdf.stream(f"/ShadingType 6 {mesh} /BitsPerFlag 8", coons)
    # Type 7: a tensor patch whose inside points are pulled to one corner.
    tensor_boundary = [(0.05, 0.05), (0.05, 0.35), (0.05, 0.65), (0.05, 0.95), (0.35, 0.95), (0.65, 0.95), (0.95, 0.95),
                       (0.95, 0.65), (0.95, 0.35), (0.95, 0.05), (0.65, 0.05), (0.35, 0.05)]
    xobjs["S7"] = pdf.stream(f"/ShadingType 7 {mesh} /BitsPerFlag 8", patch_data(tensor_boundary, [yellow, blue, red, green], extra_points=[(0.2, 0.2), (0.25, 0.8), (0.8, 0.75), (0.75, 0.3)]))
    # Not extended axial (a band across the cell) and radial with a focus off to one side.
    xobjs["S8"] = f"<< /ShadingType 2 /ColorSpace /DeviceRGB /Coords [25 0 65 0] /Function {exp_fn(red, blue)} >>"
    xobjs["S9"] = f"<< /ShadingType 3 /ColorSpace /DeviceRGB /Coords [55 35 4 45 45 40] /Function {stitch_fn([yellow, red, blue])} /Extend [true true] >>"
    names = sorted(xobjs)
    resources = f"<< /Shading << {' '.join(f'/{n} {xobjs[n]}' if not str(xobjs[n]).isdigit() else f'/{n} {xobjs[n]} 0 R' for n in names)} >> >>"
    content = []
    for i, name in enumerate(names):
        x, y = (i % 3) * 110 + 10, (2 - i // 3) * 110 + 10
        content.append(f"q {x} {y} {size} {size} re W n 1 0 0 1 {x} {y} cm /{name} sh Q 0 G {x} {y} {size} {size} re S ")
    pdf.page(340, 340, "".join(content), resources)
    save(pdf, "shadings.pdf")


# --- tiling patterns ----------------------------------------------------------------------------------------

def tiling(pdf, bbox, xstep, ystep, content, paint=1, matrix=None, resources="<< >>"):
    entries = (f"/Type /Pattern /PatternType 1 /PaintType {paint} /TilingType 1 /BBox [{nums(bbox)}] /XStep {xstep:g} /YStep {ystep:g} "
               f"/Resources {resources}")
    if matrix:
        entries += f" /Matrix [{nums(matrix)}]"
    return pdf.stream(entries, content)


def tiling_patterns():
    pdf = Pdf()
    pats = {}
    pats["P1"] = tiling(pdf, [0, 0, 10, 10], 10, 10, "0.8 0.1 0.1 rg 0 0 5 5 re f 0.1 0.2 0.8 rg 5 5 5 5 re f")
    pats["P2"] = tiling(pdf, [0, 0, 8, 8], 8, 8, "2 w 0 0 m 8 8 l S", paint=2)
    pats["P3"] = tiling(pdf, [0, 0, 12, 12], 12, 12, "0.1 0.5 0.2 rg 1 1 8 8 re f 1 g 3 3 4 4 re f", matrix=[0.866, 0.5, -0.5, 0.866, 0, 0])
    pats["P4"] = tiling(pdf, [0, 0, 8, 8], 14, 12, f"0.9 0.5 0.1 rg {circle(4, 4, 3.5)} f")
    pats["P5"] = tiling(pdf, [0, 0, 16, 6], 8, 6, "0.1 0.2 0.8 rg 0 0 4 6 re f 0.9 0.8 0.1 rg 12 0 3 6 re f")
    pats["P6"] = tiling(pdf, [0, 0, 30, 30], 30, 30, f"0.2 0.7 0.9 rg 0 0 30 30 re f 1 1 0.8 rg {circle(15, 15, 11)} f 0.8 0.1 0.3 rg {circle(15, 15, 5)} f")
    pats["P7"] = tiling(pdf, [0, 0, 6, 6], 6, 6, "0 g 0 0 3 3 re f 3 3 3 3 re f", matrix=[1.5, 0, 0, 1.5, 2, 3])
    inner = tiling(pdf, [0, 0, 4, 4], 4, 4, "0.9 0.2 0.2 rg 0 0 2 2 re f")
    pats["P8"] = tiling(pdf, [0, 0, 20, 20], 20, 20, "/Pattern cs /Inner scn 0 0 20 20 re f 0 0 1 RG 1 w 0.5 0.5 19 19 re S",
                        resources=f"<< /Pattern << /Inner {inner} 0 R >> >>")
    names = sorted(pats)
    resources = (f"<< /Pattern << {' '.join(f'/{n} {pats[n]} 0 R' for n in names)} >> "
                 f"/ColorSpace << /Cs [/Pattern /DeviceRGB] /Cmyk [/Pattern /DeviceCMYK] >> >>")
    content = []
    for i, name in enumerate(names):
        x, y = (i % 3) * 105 + 8, (2 - i // 3) * 105 + 8
        if name == "P2":
            content.append(f"/Cs cs 0.8 0.1 0.5 /{name} scn {x} {y} 95 95 re f 0 0.5 0.8 /{name} scn {x + 20} {y + 20} 55 55 re f ")
        else:
            content.append(f"/Pattern cs /{name} scn {x} {y} 95 95 re f ")
    # A ninth cell: a pattern used for a stroke and for a circle.
    x, y = 2 * 105 + 8, 8
    content.append(f"/Pattern CS /P1 SCN 14 w {x + 10} {y + 10} m {x + 85} {y + 85} l S /Pattern cs /P4 scn {circle(x + 60, y + 30, 25)} f ")
    pdf.page(330, 330, "".join(content), resources)
    save(pdf, "tiling-patterns.pdf")


# --- optional content ----------------------------------------------------------------------------------------

def optional_content():
    pdf = Pdf()
    on, off, other = pdf.obj("<< /Type /OCG /Name (visible) >>"), pdf.obj("<< /Type /OCG /Name (hidden) >>"), pdf.obj("<< /Type /OCG /Name (other) >>")
    all_on = pdf.obj(f"<< /Type /OCMD /OCGs [{on} 0 R {other} 0 R] /P /AllOn >>")
    mixed = pdf.obj(f"<< /Type /OCMD /OCGs [{on} 0 R {off} 0 R] /P /AllOn >>")
    any_off = pdf.obj(f"<< /Type /OCMD /OCGs [{on} 0 R {off} 0 R] /P /AnyOff >>")
    expr = pdf.obj(f"<< /Type /OCMD /VE [/And {on} 0 R [/Not {off} 0 R]] >>")
    layer_form = form(pdf, [0, 0, 60, 60], "0.1 0.6 0.2 rg 0 0 60 60 re f")
    hidden_form = pdf.stream(f"/Type /XObject /Subtype /Form /BBox [0 0 60 60] /OC {off} 0 R", "0.9 0.1 0.1 rg 0 0 60 60 re f")
    visible_form = pdf.stream(f"/Type /XObject /Subtype /Form /BBox [0 0 60 60] /OC {on} 0 R", "0.1 0.2 0.9 rg 0 0 60 60 re f")
    props = f"/On {on} 0 R /Off {off} 0 R /Other {other} 0 R /AllOn {all_on} 0 R /Mixed {mixed} 0 R /AnyOff {any_off} 0 R /Expr {expr} 0 R"
    resources = f"<< /Properties << {props} >> /XObject << /L {layer_form} 0 R /H {hidden_form} 0 R /V {visible_form} 0 R >> >>"
    pdf.catalog_extra = f"/OCProperties << /OCGs [{on} 0 R {off} 0 R {other} 0 R] /D << /Order [{on} 0 R {off} 0 R {other} 0 R] /OFF [{off} 0 R] >> >>"
    content = []
    marks = [("On", True), ("Off", False), ("AllOn", True), ("Mixed", False), ("AnyOff", True), ("Expr", True)]
    for i, (name, shown) in enumerate(marks):
        x, y = (i % 3) * 100 + 10, (1 - i // 3) * 100 + 110
        content.append(f"0.9 g {x} {y} 80 80 re f 0.2 g {x} {y} 80 80 re S ")
        content.append(f"/OC /{name} BDC 0.95 0.6 0.1 rg {x + 10} {y + 10} 60 60 re f 0 g {x + 25} {y + 25} 30 30 re f EMC ")
    content.append("q 1 0 0 1 10 10 cm /L Do Q q 1 0 0 1 80 10 cm /H Do Q q 1 0 0 1 150 10 cm /V Do Q ")
    pdf.page(320, 210, "".join(content), resources)
    save(pdf, "optional-content.pdf")


# --- CMYK ----------------------------------------------------------------------------------------------------

def cmyk_swatches():
    pdf = Pdf()
    colours = []
    ramp = [0, 0.25, 0.5, 0.75, 1]
    # Each ink alone, in steps; pairs and the three together at full strength and half; black with a colour.
    for ink in range(4):
        colours.append([tuple(v if i == ink else 0 for i in range(4)) for v in ramp])
    colours.append([(1, 1, 0, 0), (0, 1, 1, 0), (1, 0, 1, 0), (1, 1, 1, 0), (0.5, 0.5, 0.5, 0)])
    colours.append([(1, 0, 0, 0.5), (0, 1, 0, 0.5), (0, 0, 1, 0.5), (0.5, 0.5, 0.5, 0.5), (1, 1, 1, 1)])
    colours.append([(0.2, 0.6, 0.9, 0.1), (0.9, 0.3, 0.1, 0.2), (0.4, 0.4, 0.8, 0), (0.1, 0.8, 0.5, 0.3), (0.7, 0.2, 0.3, 0.6)])
    colours.append([(0, 0.5, 1, 0), (1, 0, 0.5, 0), (0.5, 1, 0, 0), (0.25, 0.1, 0.65, 0.05), (0.05, 0.4, 0.2, 0.7)])
    content = []
    for r, row in enumerate(colours):
        for c, (cy, m, y, k) in enumerate(row):
            content.append(f"{cy:g} {m:g} {y:g} {k:g} k {c * 40 + 10} {(len(colours) - 1 - r) * 36 + 10} 38 34 re f ")
    pdf.page(220, len(colours) * 36 + 20, "".join(content))
    save(pdf, "cmyk-swatches.pdf")


# --- a heavy page for the speed test ---------------------------------------------------------------------------

def heavy():
    """A page like a designer's flyer: groups at partial opacity, soft-masked gradients, shading fills, patterns, blends."""
    pdf = Pdf()
    shade = f"<< /ShadingType 2 /ColorSpace /DeviceRGB /Coords [0 0 612 0] /Function {stitch_fn([(0.1, 0.2, 0.6), (0.9, 0.5, 0.1), (0.2, 0.7, 0.4)])} /Extend [true true] >>"
    radial = f"<< /ShadingType 3 /ColorSpace /DeviceRGB /Coords [0 0 0 0 0 100] /Function {exp_fn((1, 1, 0.8), (0.9, 0.3, 0.2))} /Extend [false false] >>"
    mask_shade = f"<< /ShadingType 2 /ColorSpace /DeviceGray /Coords [0 0 0 160] /Function {exp_fn([1], [0])} /Extend [true true] >>"
    mask = form(pdf, [0, 0, 200, 160], "/Sh sh", f"<< /Shading << /Sh {mask_shade} >> >>", group="/CS /DeviceGray")
    hatch = tiling(pdf, [0, 0, 6, 6], 6, 6, "0.2 0.3 0.7 RG 0.7 w 0 0 m 6 6 l S")
    dots = tiling(pdf, [0, 0, 10, 10], 10, 10, f"0.8 0.2 0.3 rg {circle(5, 5, 3)} f")
    badge = form(pdf, [0, 0, 80, 80], f"0.95 0.8 0.2 rg {circle(40, 40, 36)} f 0.9 0.3 0.2 rg {circle(40, 40, 24)} f 1 g {circle(40, 40, 10)} f", group="/I true")
    tile_names = {"Hatch": hatch, "Dots": dots}
    resources = (f"<< /ExtGState << /H << /ca 0.5 >> /Q << /ca 0.25 >> /M << /BM /Multiply >> /S << /BM /Screen /ca 0.8 >> /O << /BM /Overlay >> "
                 f"/Mask << /SMask << /S /Luminosity /G {mask} 0 R >> >> /None << /SMask /None >> >> "
                 f"/Shading << /Wash {shade} /Glow {radial} >> /Pattern << {' '.join(f'/{n} {v} 0 R' for n, v in tile_names.items())} >> "
                 f"/XObject << /Badge {badge} 0 R >> >>")
    c = []
    c.append("q 0 0 612 792 re W n /Wash sh Q ")
    # A grid of badges at different opacities.
    for i in range(6):
        for j in range(8):
            gs = "H" if (i + j) % 2 else "Q"
            c.append(f"q /{gs} gs 1 0 0 1 {20 + i * 98} {20 + j * 96} cm 1.1 0 0 1.1 0 0 cm /Badge Do Q ")
    # Soft-masked gradients and glows.
    for j in range(4):
        c.append(f"q 1 0 0 1 {40 + (j % 2) * 280} {420 + (j // 2) * 170} cm /Mask gs 0.1 0.2 0.8 rg 0 0 200 160 re f Q ")
        c.append(f"q 1 0 0 1 {140 + (j % 2) * 280} {500 + (j // 2) * 170} cm 1.2 0 0 1.2 0 0 cm /Glow sh Q ")
    # Pattern fills, many of them with the same two patterns.
    for k in range(40):
        pat = "Hatch" if k % 2 else "Dots"
        c.append(f"/Pattern cs /{pat} scn {20 + (k % 8) * 72} {600 + (k // 8) * 30} 66 24 re f ")
    # Blended shapes.
    for k in range(40):
        gs = ["M", "S", "O"][k % 3]
        c.append(f"q /{gs} gs {0.2 + (k % 5) * 0.15:.2f} {0.8 - (k % 7) * 0.1:.2f} {0.3 + (k % 3) * 0.3:.2f} rg {circle(40 + (k % 10) * 55, 120 + (k // 10) * 140, 34)} f Q ")
    pdf.page(612, 792, "".join(c), resources)
    save(pdf, "transparency-heavy.pdf", out=PERF)


def main():
    if "--perf" in sys.argv:
        heavy()
        return
    blend_modes()
    soft_masks()
    groups()
    shadings()
    tiling_patterns()
    optional_content()
    cmyk_swatches()


if __name__ == "__main__":
    main()
