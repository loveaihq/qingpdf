#!/usr/bin/env python3
"""Make the encrypted test files in this folder with qpdf.

    python generate.py [path/to/qpdf.exe]

Every file is a small public corpus file (the sources are named in the table
below) encrypted by qpdf, which is only used here and in the tests as the
reference implementation. The files are ours: they carry the licence of their
source. `manifest.tsv` lists every file with the passwords that open it; the
tests read it, so edit the table, run the script and commit both.

The passwords are made up for the tests. They include Chinese ones (R6 and, to
exercise the UTF-8 fallback of the older revisions, R4), a long one, and one
written with full-width letters.
"""

import os
import subprocess
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
PUBLIC = os.path.normpath(os.path.join(HERE, "..", ".."))

QPDF = (sys.argv[1] if len(sys.argv) > 1 else os.environ.get("QINGPDF_QPDF")
        or r"C:\Dev\tools\qpdf\bin\qpdf.exe")

SOURCES = {
    "bookmarks": "outline-form-attach/bookmarks.pdf",          # classic xref, outlines
    "utf8": "outline-form-attach/utf-8.pdf",                   # /Info with a text string
    "objstm": "xref-stream-objstm/bug_757705.pdf",             # xref stream + object stream
    "pdf20": "pdf20/Simple PDF 2.0 file.pdf",                  # has a catalog /Metadata stream
    "pdf20utf8": "pdf20/pdf20-utf8-test.pdf",                  # Flate streams, outlines, /Info
    "vertical": "cjk/vertical.pdf",                            # CJK, /Info, Flate
    "two": "xref-classic/hello_world_2_pages.pdf",             # two pages
    "rects": "page-tree/rectangles_multi_pages.pdf",           # several pages
    "sig": "incremental/signature_reason.pdf",                 # a signature: /ByteRange and /Contents are not encrypted
    "forms": "outline-form-attach/multiple_form_types.pdf",    # form fields with string values
    "attach": "outline-form-attach/embedded_attachments_with_desc.pdf",  # an /EmbeddedFile stream
}

LONG = "correct horse battery staple, a very long password of more than thirty-two characters"

# name, source, bits, user, owner, qpdf options, other passwords that open it, what it is for
CASES = [
    ("r2-40-empty-print-none", "bookmarks", 40, "", "owner", ["--print=n"], [], "R2 RC4-40, empty user password, no printing"),
    ("r2-40-user", "bookmarks", 40, "user", "owner", [], [], "R2 RC4-40 with a user password"),
    ("r2-40-empty-modify-extract-none", "two", 40, "", "owner", ["--modify=n", "--extract=n"], [], "R2, no modifying or extracting"),
    ("r3-128-rc4-empty-print-none", "utf8", 128, "", "owner", ["--use-aes=n", "--print=none"], [], "R3 RC4-128, empty user password"),
    ("r3-128-rc4-user-modify-none", "bookmarks", 128, "user", "owner", ["--use-aes=n", "--modify=none", "--assemble=n"], [], "R3 RC4-128 with a user password"),
    ("r3-128-rc4-empty-extract-n", "rects", 128, "", "owner", ["--use-aes=n", "--extract=n"], [], "R3 RC4-128, no extracting"),
    ("r4-128-rc4-v4-user", "pdf20utf8", 128, "user", "owner", ["--use-aes=n", "--force-V4"], [], "R4, crypt filters with RC4"),
    ("r4-aes128-empty-print-none", "objstm", 128, "", "owner", ["--use-aes=y", "--print=none"], [], "R4 AES-128, object streams, empty user password"),
    ("r4-aes128-user-assemble-n", "bookmarks", 128, "user", "owner", ["--use-aes=y", "--assemble=n"], [], "R4 AES-128 with a user password"),
    ("r4-aes128-empty-cleartext-metadata", "pdf20", 128, "", "owner", ["--use-aes=y", "--cleartext-metadata"], [], "R4 AES-128, /EncryptMetadata false"),
    ("r4-aes128-user-chinese", "vertical", 128, "密码", "主人", ["--use-aes=y"], [], "R4 AES-128, Chinese passwords (UTF-8 bytes)"),
    ("r4-aes128-empty-modify-none", "pdf20utf8", 128, "", "owner", ["--use-aes=y", "--modify=none"], [], "R4 AES-128 with Flate streams"),
    ("r4-aes128-empty-owner-too", "bookmarks", 128, "", "", ["--use-aes=y", "--print=none"], [], "R4, the owner password is empty as well: whoever has the file is the owner"),
    ("r4-aes128-user-long", "bookmarks", 128, LONG, "owner", ["--use-aes=y"], [], "R4, a password longer than 32 bytes"),
    ("r5-aes256-empty-print-none", "bookmarks", 256, "", "owner", ["--force-R5", "--print=none"], [], "R5 AES-256 (Adobe extension level 3)"),
    ("r5-aes256-user", "objstm", 256, "user", "owner", ["--force-R5"], [], "R5 AES-256, object streams"),
    ("r6-aes256-empty-print-none", "bookmarks", 256, "", "owner", ["--print=none"], [], "R6 AES-256, empty user password"),
    ("r6-aes256-empty-extract-n", "objstm", 256, "", "owner", ["--extract=n"], [], "R6 AES-256, object streams"),
    ("r6-aes256-user-modify-none", "pdf20utf8", 256, "user", "owner", ["--modify=none"], [], "R6 AES-256 with a user password"),
    ("r6-aes256-empty-cleartext-metadata", "pdf20", 256, "", "owner", ["--cleartext-metadata"], [], "R6, metadata not encrypted"),
    ("r6-aes256-user-chinese", "vertical", 256, "密码", "主人", [], [], "R6, Chinese user and owner passwords"),
    ("r6-aes256-empty-user-chinese-owner", "utf8", 256, "", "管理员密码", ["--assemble=n"], [], "R6, empty user password, Chinese owner password"),
    ("r6-aes256-user-everything-denied", "two", 256, "user", "owner",
     ["--print=none", "--modify=none", "--extract=n", "--annotate=n", "--form=n", "--assemble=n", "--modify-other=n", "--accessibility=n"],
     [], "R6, every permission denied"),
    ("r6-aes256-user-long", "rects", 256, LONG, "owner", [], [], "R6, a password longer than 127 bytes is cut; this one is shorter"),
    ("r4-aes128-empty-signature", "sig", 128, "", "owner", ["--use-aes=y"], [], "R4, a signature dictionary (its /Contents stays as it is)"),
    ("r6-aes256-user-signature", "sig", 256, "user", "owner", [], [], "R6, a signature dictionary"),
    ("r6-aes256-empty-forms", "forms", 256, "", "owner", ["--form=n"], [], "R6, form fields with string values"),
    ("r4-aes128-user-attachments", "attach", 128, "user", "owner", ["--use-aes=y"], [], "R4, an embedded file stream"),
    ("r6-aes256-empty-attachments", "attach", 256, "", "owner", [], [], "R6, an embedded file stream"),
    ("r6-aes256-user-fullwidth", "utf8", 256, "ABC123", "owner", [], ["ＡＢＣ１２３"], "R6, opens with the full-width form of the password too"),
]


def main():
    manifest = ["# file\tuser password\towner password\tother passwords that open it\tsource\tpurpose"]
    for name, source, bits, user, owner, options, alt, purpose in CASES:
        src = os.path.join(PUBLIC, *SOURCES[source].split("/"))
        out = os.path.join(HERE, f"{source}.{name}.pdf")
        cmd = [QPDF, "--allow-weak-crypto", "--encrypt",
               f"--user-password={user}", f"--owner-password={owner}", f"--bits={bits}", *options, "--", src, out]
        run = subprocess.run(cmd, capture_output=True, text=True, encoding="utf-8", errors="replace")
        if run.returncode not in (0, 3):
            sys.exit(f"qpdf failed for {name}: {run.stderr or run.stdout}")
        manifest.append("\t".join([os.path.basename(out), user, owner, ",".join(alt), SOURCES[source], purpose]))
        print(f"{os.path.basename(out)}: {os.path.getsize(out)} bytes")
    with open(os.path.join(HERE, "manifest.tsv"), "w", encoding="utf-8", newline="\n") as f:
        f.write("\n".join(manifest) + "\n")


if __name__ == "__main__":
    main()
