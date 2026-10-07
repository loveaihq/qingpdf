"""A tiny writer of encrypted PDFs for hostile tests (ISO 32000-1 7.6, and
pypdf's AlgV5 for R6). Objects are text templates; '<<S:text>>' becomes the
encrypted hex string of `text` for that object (or the plain hex string inside
object streams)."""
import hashlib, os, re, struct, zlib
from cryptography.hazmat.primitives.ciphers import Cipher, algorithms, modes
from cryptography.hazmat.decrepit.ciphers.algorithms import ARC4

PAD = bytes([0x28, 0xBF, 0x4E, 0x5E, 0x4E, 0x75, 0x8A, 0x41, 0x64, 0x00, 0x4E, 0x56, 0xFF, 0xFA, 0x01, 0x08,
             0x2E, 0x2E, 0x00, 0xB6, 0xD0, 0x68, 0x3E, 0x80, 0x2F, 0x0C, 0xA9, 0xFE, 0x64, 0x53, 0x69, 0x7A])


def rc4(key, data):
    if len(key) * 8 in (40, 56, 64, 80, 128, 160, 192, 256):
        e = Cipher(ARC4(key), mode=None).encryptor()
        return e.update(data) + e.finalize()
    S = list(range(256)); j = 0
    for i in range(256):
        j = (j + S[i] + key[i % len(key)]) & 255; S[i], S[j] = S[j], S[i]
    i = j = 0; out = bytearray()
    for b in data:
        i = (i + 1) & 255; j = (j + S[i]) & 255; S[i], S[j] = S[j], S[i]
        out.append(b ^ S[(S[i] + S[j]) & 255])
    return bytes(out)


def aes_cbc(key, iv, data, pad=True):
    if pad:
        n = 16 - len(data) % 16
        data = data + bytes([n]) * n
    e = Cipher(algorithms.AES(key), modes.CBC(iv)).encryptor()
    return iv + e.update(data) + e.finalize()


class Enc:
    def __init__(self, kind, id0=b"\x01" * 16, user=b"", owner=b"o", P=-4, encrypt_metadata=True, length=128):
        self.kind, self.id0, self.P, self.em = kind, id0, P, encrypt_metadata
        if kind == "aes-r6":
            from pypdf._encryption import AlgV5
            self.key = os.urandom(32)
            v = AlgV5.generate_values(6, user, owner, self.key, P & 0xFFFFFFFF, encrypt_metadata)
            self.vals = v
            self.method = "aesv3"
            return
        self.R = 2 if kind == "rc4-r2" else (4 if kind in ("aes-r4", "rc4-v4") else 3)
        self.V = 1 if kind == "rc4-r2" else (4 if kind in ("aes-r4", "rc4-v4") else 2)
        self.n = 5 if self.R == 2 else (16 if kind == "aes-r4" else length // 8)
        self.length = length
        self.method = "aesv2" if kind == "aes-r4" else "rc4"
        # Algorithm 3: O
        h = hashlib.md5((owner + PAD)[:32]).digest()
        if self.R >= 3:
            for _ in range(50):
                h = hashlib.md5(h).digest()
        k = h[:self.n]
        o = rc4(k, (user + PAD)[:32])
        if self.R >= 3:
            for i in range(1, 20):
                o = rc4(bytes(b ^ i for b in k), o)
        self.O = o
        # Algorithm 2: key
        m = (user + PAD)[:32] + self.O + struct.pack("<i", P) + id0
        if self.R >= 4 and not encrypt_metadata:
            m += b"\xff" * 4
        h = hashlib.md5(m).digest()
        if self.R >= 3:
            for _ in range(50):
                h = hashlib.md5(h[:self.n]).digest()
        self.key = h[:self.n]
        # Algorithms 4/5: U
        if self.R == 2:
            self.U = rc4(self.key, PAD)
        else:
            x = rc4(self.key, hashlib.md5(PAD + id0).digest())
            for i in range(1, 20):
                x = rc4(bytes(b ^ i for b in self.key), x)
            self.U = x + b"\x00" * 16

    def objkey(self, num, gen, method):
        if method == "aesv3":
            return self.key
        k = self.key
        if self.kind == "rc4-v4" and len(k) < 16:
            k = k + b"\0" * (16 - len(k))
        m = k + struct.pack("<I", num)[:3] + struct.pack("<H", gen)
        if method == "aesv2":
            m += b"sAlT"
        return hashlib.md5(m).digest()[:min(len(k) + 5, 16)]

    def encrypt(self, num, gen, data, method=None, pad=True):
        method = method or self.method
        if method == "none":
            return data
        key = self.objkey(num, gen, method)
        if method == "rc4":
            return rc4(key, data)
        return aes_cbc(key, os.urandom(16), data, pad)

    def dict_text(self, extra=b""):
        h = lambda b: b"<" + b.hex().encode() + b">"
        if self.kind == "aes-r6":
            v = self.vals
            return (b"<< /Filter /Standard /V 5 /R 6 /Length 256 /P %d /O %s /U %s /OE %s /UE %s /Perms %s "
                    b"/CF << /StdCF << /CFM /AESV3 /AuthEvent /DocOpen /Length 32 >> >> /StmF /StdCF /StrF /StdCF %s%s >>") % (
                self.P, h(v["/O"]), h(v["/U"]), h(v["/OE"]), h(v["/UE"]), h(v["/Perms"]),
                b"" if self.em else b"/EncryptMetadata false ", extra)
        if self.V == 4:
            cfm = b"/AESV2" if self.kind == "aes-r4" else b"/V2"
            return (b"<< /Filter /Standard /V 4 /R 4 /Length %d /P %d /O %s /U %s "
                    b"/CF << /StdCF << /CFM %s /AuthEvent /DocOpen /Length %d >> >> /StmF /StdCF /StrF /StdCF %s%s >>") % (
                self.length, self.P, h(self.O), h(self.U), cfm, self.n, b"" if self.em else b"/EncryptMetadata false ", extra)
        return b"<< /Filter /Standard /V %d /R %d /Length %d /P %d /O %s /U %s %s>>" % (
            self.V, self.R, self.n * 8, self.P, h(self.O), h(self.U), extra)


def subst(body, enc, num, gen=0, plain=False):
    def rep(m):
        text = m.group(1)
        data = text if plain or enc is None else enc.encrypt(num, gen, text, method=enc.method)
        return b"<" + data.hex().encode() + b">"
    return re.sub(rb"<<S:(.*?)>>", rep, body, flags=re.S)


def build(objs, objstms=None, enc=None, root=1, trailer_extra=b"", xref_stream=True, encrypt_num=None, flate_objstm=True,
          id0=b"\x01" * 16):
    """objs: {num: bytes (dict/other) | (dict_bytes_without_length, data_bytes, method or None)}.
    objstms: {num: [(objnum, body_bytes)]}."""
    objstms = objstms or {}
    buf = bytearray(b"%PDF-1.7\n%\xe2\xe3\xcf\xd3\n")
    entries = {}
    for num, body in objs.items():
        entries[num] = (1, len(buf), 0)
        if isinstance(body, tuple):
            d, data, method = body
            d = subst(d, enc if num != encrypt_num else None, num)
            if enc is not None and method != "none" and num != encrypt_num:
                data = enc.encrypt(num, 0, data, method)
            buf += b"%d 0 obj\n" % num + d[:-2] + b" /Length %d >>\nstream\n" % len(data) + data + b"\nendstream\nendobj\n"
        else:
            b = subst(body, enc if num != encrypt_num else None, num)
            buf += b"%d 0 obj\n" % num + b + b"\nendobj\n"
    for snum, items in objstms.items():
        header = b""; payload = b""
        for idx, (num, body) in enumerate(items):
            header += b"%d %d " % (num, len(payload))
            payload += subst(body, enc, num, plain=True) + b" "
            entries[num] = (2, snum, idx)
        data = header + payload
        filt = b""
        if flate_objstm:
            data = zlib.compress(data); filt = b"/Filter /FlateDecode "
        if enc is not None:
            data = enc.encrypt(snum, 0, data)
        entries[snum] = (1, len(buf), 0)
        buf += b"%d 0 obj\n<< /Type /ObjStm /N %d /First %d %s/Length %d >>\nstream\n" % (
            snum, len(items), len(header), filt, len(data)) + data + b"\nendstream\nendobj\n"
    idtext = b"/ID [<" + id0.hex().encode() + b"> <" + id0.hex().encode() + b">]"
    enc_ref = b"/Encrypt %d 0 R" % encrypt_num if encrypt_num else b""
    if xref_stream:
        xnum = max(entries) + 1
        entries[xnum] = (1, len(buf), 0)
        size = xnum + 1
        rows = b"".join(struct.pack(">BIH", *entries.get(n, (0, 0, 0xFFFF if n == 0 else 0))) for n in range(size))
        packed = zlib.compress(rows)
        buf += (b"%d 0 obj\n<< /Type /XRef /Size %d /W [1 4 2] /Root %d 0 R %s %s %s /Filter /FlateDecode /Length %d >>\nstream\n"
                % (xnum, size, root, idtext, enc_ref, trailer_extra, len(packed))) + packed + b"\nendstream\nendobj\n"
        buf += b"startxref\n%d\n%%%%EOF\n" % entries[xnum][1]
    else:
        size = max(entries) + 1
        at = len(buf)
        buf += b"xref\n0 %d\n" % size
        for n in range(size):
            e = entries.get(n)
            if e and e[0] == 1:
                buf += b"%010d 00000 n\r\n" % e[1]
            else:
                buf += b"0000000000 65535 f\r\n"
        buf += b"trailer\n<< /Size %d /Root %d 0 R %s %s %s >>\nstartxref\n%d\n%%%%EOF\n" % (size, root, idtext, enc_ref, trailer_extra, at)
    return bytes(buf)
