"""Check that `qingpdf compress` keeps to the memory it says it uses on files that lie about their sizes.

It makes three small files whose pictures do not tell the truth, runs the command line on each while it reads the peak working set of the
process (Windows `GetProcessMemoryInfo`, through ctypes), and fails when the peak is over the bound set for the case:

  liar JPEG   eight pages, each a JPEG of 9000 by 9000 pixels that its dictionary says is 100 by 100 (a decoder that goes by the JPEG
              allocates 243 MB for each; the dictionary's size is the one the memory is counted by, so nothing is decoded);
  big mask    eight pages, each a 400 by 400 picture with a soft mask of 9500 by 9500 (the mask is decoded at its own size, which the
              memory of a picture counts);
  bombs       three streams of 250 MiB of zeros in 250 KB (Flate), which are not pictures: they are deflated again only when they inflate to
              less than half the memory lent to the pictures.

    python tests/tools/compress_memory_check.py [--ours target/release/qingpdf.exe] [--out folder]

It exits with 1 when a check fails. A development check, not part of `cargo test` (no test can measure the peak of the process without an
allocator hook, and the workspace has no `unsafe` outside `win32.rs`): it needs Pillow and Windows.
"""

import argparse
import ctypes
import io
import os
import subprocess
import sys
import time
import zlib
from ctypes import wintypes

from PIL import Image

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.normpath(os.path.join(HERE, "..", ".."))

ap = argparse.ArgumentParser()
ap.add_argument("--ours", default=os.path.join(ROOT, "target", "release", "qingpdf.exe"))
ap.add_argument("--out", default=os.path.join(ROOT, "tests", "out", "compress-memory"))
args = ap.parse_args()
os.makedirs(args.out, exist_ok=True)
bad = False


class Counters(ctypes.Structure):
    _fields_ = [
        ("cb", wintypes.DWORD),
        ("PageFaultCount", wintypes.DWORD),
        ("PeakWorkingSetSize", ctypes.c_size_t),
        ("WorkingSetSize", ctypes.c_size_t),
        ("QuotaPeakPagedPoolUsage", ctypes.c_size_t),
        ("QuotaPagedPoolUsage", ctypes.c_size_t),
        ("QuotaPeakNonPagedPoolUsage", ctypes.c_size_t),
        ("QuotaNonPagedPoolUsage", ctypes.c_size_t),
        ("PagefileUsage", ctypes.c_size_t),
        ("PeakPagefileUsage", ctypes.c_size_t),
    ]


kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)
psapi = ctypes.WinDLL("psapi", use_last_error=True)
kernel32.OpenProcess.restype = wintypes.HANDLE
kernel32.OpenProcess.argtypes = [wintypes.DWORD, wintypes.BOOL, wintypes.DWORD]
kernel32.CloseHandle.argtypes = [wintypes.HANDLE]
psapi.GetProcessMemoryInfo.argtypes = [wintypes.HANDLE, ctypes.POINTER(Counters), wintypes.DWORD]
PROCESS_QUERY_INFORMATION, PROCESS_VM_READ = 0x0400, 0x0010


def run_peak(cmd):
    """Run `cmd`; return (exit code, stderr text, peak working set in bytes). The peak only grows, so the last reading is the one."""
    p = subprocess.Popen(cmd, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True, encoding="utf-8", errors="replace")
    handle = kernel32.OpenProcess(PROCESS_QUERY_INFORMATION | PROCESS_VM_READ, False, p.pid)
    peak = 0
    c = Counters()
    c.cb = ctypes.sizeof(c)
    while p.poll() is None:
        if handle and psapi.GetProcessMemoryInfo(handle, ctypes.byref(c), c.cb):
            peak = max(peak, c.PeakWorkingSetSize)
        time.sleep(0.01)
    if handle:
        kernel32.CloseHandle(handle)
    out, err = p.communicate()
    return p.returncode, err, peak


def write_pdf(path, objs):
    out = bytearray(b"%PDF-1.7\n")
    offs = {}
    for k in sorted(objs):
        offs[k] = len(out)
        out += b"%d 0 obj\n" % k + objs[k] + b"\nendobj\n"
    x = len(out)
    n = max(objs) + 1
    out += b"xref\n0 %d\n0000000000 65535 f \n" % n
    for k in range(1, n):
        out += b"%010d 00000 n \n" % offs[k]
    out += b"trailer\n<< /Size %d /Root 1 0 R >>\nstartxref\n%d\n%%%%EOF\n" % (n, x)
    with open(path, "wb") as f:
        f.write(out)


def stream(dict_text, data):
    return b"<< " + dict_text + b" /Length %d >>\nstream\n" % len(data) + data + b"\nendstream"


def page_objs(pages, per_page):
    """The catalog, the page tree, and for each page its object, its content and the objects `per_page(i, first_free)` makes."""
    objs = {1: b"<< /Type /Catalog /Pages 2 0 R >>"}
    kids = []
    n = 3
    for i in range(pages):
        page, content, first = n, n + 1, n + 2
        extra, count = per_page(i, first)
        kids.append(page)
        objs[page] = b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Contents %d 0 R /Resources << /XObject << /Im1 %d 0 R >> >> >>" % (content, first)
        objs[content] = stream(b"", b"q 100 0 0 100 50 50 cm /Im1 Do Q")
        objs.update(extra)
        n = first + count
    objs[2] = b"<< /Type /Pages /Kids [%s] /Count %d >>" % (b" ".join(b"%d 0 R" % k for k in kids), pages)
    return objs


def liar_jpeg(path):
    side = 9000

    def per_page(i, first):
        img = Image.new("RGB", (side, side), (i * 20, 100, 200))
        for k in range(50):
            img.putpixel((k * 7 % side, k * 11 % side), (255, i, k))
        b = io.BytesIO()
        img.save(b, "JPEG", quality=75)
        return {first: stream(b"/Type /XObject /Subtype /Image /Width 100 /Height 100 /ColorSpace /DeviceRGB /BitsPerComponent 8 /Filter /DCTDecode", b.getvalue())}, 1

    write_pdf(path, page_objs(8, per_page))


def big_mask(path):
    side = 9500

    def per_page(i, first):
        w = 400
        raw = bytes(((x * 3 + y + i * 5 + (x * y) % 24) & 255) for y in range(w) for x in range(w) for _ in range(3))
        mask = b"".join(bytes([(y // 40 + i) & 255]) * side for y in range(side))
        return {
            first: stream(b"/Type /XObject /Subtype /Image /Width 400 /Height 400 /ColorSpace /DeviceRGB /BitsPerComponent 8 /Filter /FlateDecode /SMask %d 0 R" % (first + 1), zlib.compress(raw, 6)),
            first + 1: stream(b"/Type /XObject /Subtype /Image /Width %d /Height %d /ColorSpace /DeviceGray /BitsPerComponent 8 /Filter /FlateDecode" % (side, side), zlib.compress(mask, 1)),
        }, 2

    write_pdf(path, page_objs(8, per_page))


def bombs(path, count=3, size=250 * 1024 * 1024):
    objs = {
        1: b"<< /Type /Catalog /Pages 2 0 R /Bomb [%s] >>" % b" ".join(b"%d 0 R" % (4 + i) for i in range(count)),
        2: b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
        3: b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] >>",
    }
    for i in range(count):
        c = zlib.compressobj(1)
        z = c.compress(bytes([i + 1]) + bytes(size - 1)) + c.flush()
        objs[4 + i] = stream(b"/Filter /FlateDecode", z)
    write_pdf(path, objs)


def check(name, make, bound_mb, expect_smaller=None):
    global bad
    path = os.path.join(args.out, name + ".pdf")
    out = os.path.join(args.out, name + ".out.pdf")
    make(path)
    code, err, peak = run_peak([args.ours, "compress", path, "-o", out, "--force"])
    mb = peak / (1024 * 1024)
    ok = code == 0 and mb <= bound_mb
    print(f"{name}: input {os.path.getsize(path):,} bytes, peak {mb:,.0f} MB (bound {bound_mb} MB), exit {code} {'ok' if ok else 'FAILED'}", flush=True)
    if code != 0:
        print("  stderr:", err.strip()[:300])
    if not ok:
        bad = True
    for f in (path, out):
        if os.path.exists(f):
            os.remove(f)


# The bounds: the memory lent to the pictures (256 MiB at the command line) plus the file and its copies and a margin; the figures before
# the fix were 1,880 MB, 708 MB and 290 MB.
check("liar-jpeg", liar_jpeg, 100)
check("big-mask", big_mask, 350)
check("bombs", bombs, 220)
print("\nFAILED" if bad else "\nall checks passed")
sys.exit(1 if bad else 0)
