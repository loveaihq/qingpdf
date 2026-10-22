"""Build the Windows release zip of qingpdf.

    python tests/tools/build_release.py

Builds both programs into target/dist with the static C runtime and with local paths (the user's home folder
and this repository) taken out of the binaries, checks that no such path is left, and writes
target/dist/qingpdf-<version>-windows-x64.zip plus its SHA-256 next to it.
"""

import hashlib
import os
import re
import subprocess
import sys
import zipfile

ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
DIST = os.path.join(ROOT, "target", "dist")
HOME = os.path.expanduser("~")


def version():
    with open(os.path.join(ROOT, "Cargo.toml"), encoding="utf-8") as f:
        m = re.search(r'^version\s*=\s*"([^"]+)"', f.read(), re.M)
    if not m:
        sys.exit("no version in Cargo.toml")
    return m.group(1)


def build():
    flags = ["-C", "target-feature=+crt-static",
             f"--remap-path-prefix={HOME}=~", f"--remap-path-prefix={ROOT}=qingpdf"]
    env = dict(os.environ, RUSTFLAGS=" ".join(flags))
    subprocess.run(["cargo", "build", "--release", "--target-dir", DIST,
                    "-p", "qingpdf-cli", "-p", "qingpdf-view-bin"], cwd=ROOT, env=env, check=True)


def check(exe):
    data = open(exe, "rb").read()
    for path in (HOME, ROOT, os.path.basename(HOME)):
        for enc in ("utf-8", "utf-16-le"):
            if path.encode(enc).lower() in data.lower():
                sys.exit(f"{exe} still contains {path!r} ({enc})")


def main():
    ver = version()
    build()
    exes = [os.path.join(DIST, "release", n) for n in ("qingpdf-view.exe", "qingpdf.exe")]
    for exe in exes:
        check(exe)
    name = f"qingpdf-{ver}-windows-x64"
    out = os.path.join(DIST, name + ".zip")
    docs = ["README.md", "LICENSE-MIT", "LICENSE-APACHE", "THIRD-PARTY-NOTICES.md", os.path.join("docs", "使用说明.md"), os.path.join("docs", "USER-GUIDE.md")]
    with zipfile.ZipFile(out, "w", zipfile.ZIP_DEFLATED, compresslevel=9) as z:
        for exe in exes:
            z.write(exe, f"{name}/{os.path.basename(exe)}")
        for d in docs:
            z.write(os.path.join(ROOT, d), f"{name}/{os.path.basename(d)}")
    digest = hashlib.sha256(open(out, "rb").read()).hexdigest()
    with open(out + ".sha256", "w", encoding="ascii", newline="\n") as f:
        f.write(f"{digest}  {os.path.basename(out)}\n")
    for exe in exes:
        print(f"{os.path.basename(exe)}: {os.path.getsize(exe):,} bytes")
    print(f"{out}: {os.path.getsize(out):,} bytes\nsha256 {digest}")


if __name__ == "__main__":
    main()
