"""Check qingpdf's encrypted output against two other PDF engines: PDFium (Chrome)
and MuPDF.

For every encrypted file whose password is known (the generated set in
tests/corpus/public/encrypted/qpdf-generated, and the other encrypted files of
the corpus), run qingpdf's commands on it with the password and open each
result with both engines, with the same password. The result must

  * open with the password in both engines and not with a wrong one,
  * show the same encryption and the same permissions as the input does in
    that engine (so the author's restrictions came through), and
  * look the same as the input (copy, delete, merge) in that engine.

`qingpdf decrypt` outputs must open without a password and look the same.

This is a development check, not part of `cargo test`: it needs Python with
pypdfium2, pymupdf, numpy and Pillow.

    python tests/tools/encrypted_compare.py [path/to/qingpdf.exe]
"""

import glob
import os
import subprocess
import sys
import tempfile

import numpy as np
import pymupdf
import pypdfium2 as pdfium
import pypdfium2.raw as pdfium_c

HERE = os.path.dirname(os.path.abspath(__file__))
CORPUS = os.path.normpath(os.path.join(HERE, "..", "corpus"))
GENERATED = os.path.join(CORPUS, "public", "encrypted", "qpdf-generated")
QINGPDF = sys.argv[1] if len(sys.argv) > 1 else os.path.normpath(
    os.path.join(HERE, "..", "..", "target", "release", "qingpdf.exe"))
SCALE = 0.5
THRESHOLD = 0.5  # mean absolute difference per pixel, 0-255 grey levels
MAX_PAGES = 6

# The other encrypted files of the corpus and their user passwords (PDFium's
# tests, pdf.js's manifest; see crates/qingpdf-core/tests/common/mod.rs).
KNOWN = {
    "public/encrypted/bug_644.pdf": "a",
    "public/encrypted/encrypted_hello_world_r2.pdf": "hôtel",
    "public/encrypted/encrypted_hello_world_r3.pdf": "hôtel",
    "public/encrypted/encrypted_hello_world_r5.pdf": "hôtel",
    "public/encrypted/encrypted_hello_world_r6.pdf": "hôtel",
    "local/encrypted/bug900822.pdf": "",
    "local/encrypted/empty_protected.pdf": "",
    "local/encrypted/issue17215.pdf": "",
    "local/encrypted/issue19484_1.pdf": "",
    "local/encrypted/issue3371.pdf": "ELXRTQWS",
    "local/encrypted/pr6531_1.pdf": "asdfasdf",
}


def files():
    out = []
    with open(os.path.join(GENERATED, "manifest.tsv"), encoding="utf-8") as f:
        for line in f:
            if line.startswith("#") or not line.strip():
                continue
            cols = line.rstrip("\n").split("\t")
            out.append((os.path.join(GENERATED, cols[0]), cols[1], cols[2]))
    for rel, user in KNOWN.items():
        path = os.path.join(CORPUS, *rel.split("/"))
        if os.path.isfile(path):
            out.append((path, user, None))
    return out


# --- the two engines ------------------------------------------------------------------------

class Pdfium:
    name = "PDFium"

    def open(self, path, password):
        """(document, None) or (None, why not)."""
        try:
            return pdfium.PdfDocument(path, password=password or None), None
        except Exception as e:  # noqa: BLE001 - any refusal counts
            return None, str(e)

    def facts(self, doc):
        return {
            "permissions": pdfium_c.FPDF_GetDocPermissions(doc.raw) & 0xFFFFFFFF,
            "revision": pdfium_c.FPDF_GetSecurityHandlerRevision(doc.raw),
        }

    def pages(self, doc):
        return len(doc)

    def render(self, doc, i):
        return np.asarray(doc[i].render(scale=SCALE).to_pil().convert("L"), dtype=np.int16)

    def close(self, doc):
        doc.close()


class Mupdf:
    name = "MuPDF"

    def open(self, path, password):
        try:
            doc = pymupdf.open(path)
        except Exception as e:  # noqa: BLE001
            return None, str(e)
        if doc.needs_pass:
            if not doc.authenticate(password or ""):
                doc.close()
                return None, "password refused"
        return doc, None

    def facts(self, doc):
        return {"permissions": doc.permissions & 0xFFFFFFFF, "encryption": doc.metadata.get("encryption")}

    def pages(self, doc):
        return doc.page_count

    def render(self, doc, i):
        pix = doc[i].get_pixmap(matrix=pymupdf.Matrix(SCALE, SCALE), colorspace=pymupdf.csGRAY, alpha=False)
        return np.frombuffer(pix.samples, dtype=np.uint8).reshape(pix.height, pix.width).astype(np.int16)

    def close(self, doc):
        doc.close()


ENGINES = [Pdfium(), Mupdf()]


def mean_difference(a, b):
    if a.shape != b.shape:
        return 999.0
    return float(np.abs(a - b).mean())


# --- running qingpdf --------------------------------------------------------------------------

def qingpdf(*args):
    run = subprocess.run([QINGPDF, *args], capture_output=True, text=True, encoding="utf-8", errors="replace")
    return run.returncode, (run.stderr or run.stdout).strip()


def main():
    problems = []
    notes = []
    checked = 0
    with tempfile.TemporaryDirectory() as tmp:
        for path, user, owner in files():
            name = os.path.relpath(path, CORPUS)
            # What the engines make of the input.
            reference = {}
            for engine in ENGINES:
                doc, why = engine.open(path, user)
                if doc is None:
                    notes.append(f"{name}: {engine.name} does not open the input with the password ({why})")
                    continue
                count = engine.pages(doc)
                if count == 0:
                    # An engine that reads no page of the input (MuPDF and the
                    # RC4 file with the 5-byte key of pdf.js's issue19484_1) has
                    # nothing to compare the output with.
                    notes.append(f"{name}: {engine.name} finds no page in the input")
                    engine.close(doc)
                    continue
                reference[engine.name] = {
                    "facts": engine.facts(doc),
                    "count": count,
                    "images": [engine.render(doc, i) for i in range(min(count, MAX_PAGES))],
                }
                engine.close(doc)
            if not reference:
                continue
            count_of = {k: v["count"] for k, v in reference.items()}
            pages = max(count_of.values())

            outputs = [("copy", ["split", path, "--pages", "1-"], "same", 0)]
            if pages >= 2:
                outputs.append(("delete page 1", ["delete", path, "--pages", "1"], "tail", 1))
            outputs.append(("merge with itself", ["merge", path, path], "twice", 0))
            for label, args, shape, skip in outputs:
                out = os.path.join(tmp, "out.pdf")
                code, message = qingpdf(*args, "-o", out, "--force", "--password", user)
                if code != 0:
                    problems.append(f"{name}: qingpdf {label} failed: {message[:150]}")
                    continue
                for engine in ENGINES:
                    ref = reference.get(engine.name)
                    if ref is None:
                        continue
                    doc, why = engine.open(out, user)
                    if doc is None:
                        problems.append(f"{name} [{label}]: {engine.name} does not open the output with the password: {why}")
                        continue
                    facts = engine.facts(doc)
                    for key, value in ref["facts"].items():
                        if facts.get(key) != value:
                            problems.append(f"{name} [{label}]: {engine.name} says {key} {facts.get(key)!r} for the output, {value!r} for the input")
                    count = engine.pages(doc)
                    expected = {"same": ref["count"], "tail": ref["count"] - skip, "twice": ref["count"] * 2}[shape]
                    if count != expected:
                        problems.append(f"{name} [{label}]: {engine.name} counts {count} pages, expected {expected}")
                    else:
                        # Of a merge with itself only the first copy is compared: the
                        # second has no layer settings (/OCProperties) of its own, and
                        # shows layers the first one hides (merge, layer 1).
                        for i in range(min(count, MAX_PAGES)):
                            j = i + skip if shape == "tail" else i
                            if j >= len(ref["images"]) or (shape == "twice" and i >= ref["count"]):
                                continue
                            d = mean_difference(ref["images"][j], engine.render(doc, i))
                            if d >= THRESHOLD:
                                problems.append(f"{name} [{label}]: {engine.name} page {i + 1} differs from the input's page {j + 1} (mean difference {d:.2f})")
                    engine.close(doc)
                    # A wrong password does not open it (if the file has a user password).
                    if user:
                        wrong, _ = engine.open(out, "this is not the password")
                        if wrong is not None:
                            problems.append(f"{name} [{label}]: {engine.name} opens the output with a wrong password")
                            engine.close(wrong)
                checked += 1

            # decrypt (with the owner password if we know it; else only if the file allows everything)
            for password in ([owner] if owner is not None else [user]):
                out = os.path.join(tmp, "plain.pdf")
                code, message = qingpdf("decrypt", path, "-o", out, "--force", "--password", password)
                if code != 0:
                    if owner is None and "not allow" in message:
                        continue  # refused, as it should be
                    problems.append(f"{name}: qingpdf decrypt failed: {message[:150]}")
                    continue
                for engine in ENGINES:
                    ref = reference.get(engine.name)
                    if ref is None:
                        continue
                    doc, why = engine.open(out, "")
                    if doc is None:
                        problems.append(f"{name} [decrypt]: {engine.name} does not open the plain copy: {why}")
                        continue
                    if engine.pages(doc) != ref["count"]:
                        problems.append(f"{name} [decrypt]: {engine.name} counts {engine.pages(doc)} pages, the input {ref['count']}")
                    else:
                        for i in range(min(ref["count"], MAX_PAGES)):
                            d = mean_difference(ref["images"][i], engine.render(doc, i))
                            if d >= THRESHOLD:
                                problems.append(f"{name} [decrypt]: {engine.name} page {i + 1} differs (mean difference {d:.2f})")
                    engine.close(doc)
                checked += 1

    print(f"{checked} outputs of {len(files())} encrypted files checked in PDFium and MuPDF")
    for n in notes:
        print("  note:", n)
    for p in problems:
        print("  PROBLEM:", p)
    print("PASS" if not problems else f"FAIL: {len(problems)} problem(s)")
    return 1 if problems else 0


if __name__ == "__main__":
    sys.exit(main())
