"""Taso's Field protocol, version 2: the part spike S2 uses.

The codec follows `taso-field/src/protocol.rs` (Taso c031ee5) and Taso's
own reference relay, `scripts/p3-relay.py`, which the no-trace gate runs.
Shared by the S2 agent (inside a Vessel) and the S2 sender (on the host).

  Frame = u32 BE length + body.
  Body  = u8 tag + fields; ints LE, bytes and strings as u64 LE length +
          data, enums as a u8 tag, options as 0/1 + value.
"""

import socket
import struct

VERSION = 2
MAX_FRAME = 16 * 1024 * 1024

# Consequence tags.
IRREVERSIBLE = "Irreversible"
NO_EXTERNAL_MUTATION = "NoExternalMutation"


# ── writing ──────────────────────────────────────────────────────────

def _u8(v):
    return struct.pack("<B", v)


def _u16(v):
    return struct.pack("<H", v)


def _u64(v):
    return struct.pack("<Q", v)


def _bytes(b):
    return _u64(len(b)) + b


def _str(s):
    return _bytes(s.encode("utf-8"))


def _bool(v):
    return _u8(1 if v else 0)


def hello(version=VERSION):
    return _u8(0) + _u16(version)


def effect(call, provider, request, key, consequence, authority=b"s2",
           footprint=b""):
    """Request::Effect (tag 1), retry ProviderDeduplicated with `key`."""
    body = _u8(1) + _u64(call) + _str(provider) + _bytes(request) + _bytes(footprint)
    body += _u8(0) + _bytes(key)
    if consequence == IRREVERSIBLE:
        body += _u8(0)
    elif consequence == NO_EXTERNAL_MUTATION:
        body += _u8(2)
    else:
        raise ValueError(f"unknown consequence {consequence!r}")
    return body + _bytes(authority)


def transcript(entry):
    return _u8(2) + _bytes(entry)


def turn_end(turn, quiescent=True):
    return _u8(3) + _u64(turn) + _bool(quiescent)


def outcome_applied(op, receipt):
    return _u8(4) + _u64(op) + _u8(0) + _bytes(receipt)


def outcome_not_applied(op, reason):
    return _u8(4) + _u64(op) + _u8(1) + _str(reason)


def open_():
    return _u8(5)


# ── reading ──────────────────────────────────────────────────────────

class Reader:
    """Strict reader: every read is bounds-checked, trailing bytes are an error."""

    def __init__(self, data):
        self.data = data
        self.pos = 0

    def _take(self, n, what):
        if self.pos + n > len(self.data):
            raise ValueError(f"truncated at {what}")
        v = self.data[self.pos:self.pos + n]
        self.pos += n
        return v

    def u8(self):
        return self._take(1, "u8")[0]

    def u16(self):
        return struct.unpack("<H", self._take(2, "u16"))[0]

    def u64(self):
        return struct.unpack("<Q", self._take(8, "u64"))[0]

    def raw(self):
        return self._take(self.u64(), "bytes")

    def string(self):
        return self.raw().decode("utf-8")

    def opt_u64(self):
        t = self.u8()
        if t == 0:
            return None
        if t == 1:
            return self.u64()
        raise ValueError(f"bad option tag {t}")

    def boolean(self):
        v = self.u8()
        if v > 1:
            raise ValueError(f"bad bool {v}")
        return v == 1

    def retry(self):
        t = self.u8()
        if t == 0:
            return ["ProviderDeduplicated", self.raw().hex()]
        if t == 1:
            return ["Queryable", self.raw().hex()]
        if t == 2:
            return ["UnsafeToRetry", None]
        raise ValueError(f"bad retry tag {t}")

    def consequence(self):
        t = self.u8()
        if t == 0:
            return IRREVERSIBLE
        if t == 1:
            return f"Compensatable({self.u64()})"
        if t == 2:
            return NO_EXTERNAL_MUTATION
        raise ValueError(f"bad consequence tag {t}")

    def finish(self):
        if self.pos != len(self.data):
            raise ValueError(f"{len(self.data) - self.pos} trailing bytes")


ERROR_CODES = {0: "BadHello", 1: "Malformed", 2: "Closed", 3: "Fenced", 4: "Internal"}


def decode_reply(body):
    """A reply body as a dict with "type" and its fields (JSON-ready)."""
    r = Reader(body)
    tag = r.u8()
    if tag == 0:
        reply = {"type": "Hello", "version": r.u16(), "last_sealed_turn": r.opt_u64()}
    elif tag == 1:
        call = r.u64()
        t = r.u8()
        if t == 0:
            res = {"result": "Released", "op": r.u64()}
        elif t == 1:
            res = {"result": "Held", "op": r.u64()}
        elif t == 2:
            res = {"result": "Refused", "reason": r.string()}
        else:
            raise ValueError(f"bad effect result tag {t}")
        reply = {"type": "Effect", "call": call, **res}
    elif tag == 2:
        reply = {"type": "Recorded", "lsn": r.u64()}
    elif tag == 3:
        reply = {"type": "TurnEnd", "turn": r.u64(), "sealed": r.opt_u64()}
    elif tag == 4:
        code = r.u8()
        reply = {"type": "Error", "code": ERROR_CODES.get(code, code), "message": r.string()}
    elif tag == 5:
        op = r.u64()
        t = r.u8()
        if t == 0:
            refused = None
        elif t == 1:
            refused = r.string()
        else:
            raise ValueError(f"bad outcome option tag {t}")
        reply = {"type": "Outcome", "op": op, "refused": refused}
    elif tag == 6:
        effects = []
        for _ in range(r.u64()):
            effects.append({
                "op": r.u64(),
                "provider": r.string(),
                "request": r.raw().decode("utf-8", "replace"),
                "retry": r.retry(),
                "consequence": r.consequence(),
                "authority": r.raw().decode("utf-8", "replace"),
                "unknown": r.boolean(),
            })
        reply = {"type": "Open", "effects": effects}
    else:
        raise ValueError(f"reply tag {tag} is not one S2 uses")
    r.finish()
    return reply


# ── the connection ───────────────────────────────────────────────────

def _recv_exact(sock, n):
    buf = bytearray()
    while len(buf) < n:
        chunk = sock.recv(n - len(buf))
        if not chunk:
            if not buf:
                return None
            raise ConnectionError("connection closed inside a frame")
        buf.extend(chunk)
    return bytes(buf)


def send_frame(sock, body):
    sock.sendall(struct.pack(">I", len(body)) + body)


def recv_frame(sock):
    hdr = _recv_exact(sock, 4)
    if hdr is None:
        return None
    (n,) = struct.unpack(">I", hdr)
    if n > MAX_FRAME:
        raise ValueError(f"frame of {n} bytes is over the limit")
    return _recv_exact(sock, n)


class Field:
    """One conversation on a Field or sender socket."""

    def __init__(self, path):
        self.sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        self.sock.connect(path)
        self.hello = self.ask(hello())
        if self.hello["type"] != "Hello":
            raise ConnectionError(f"Hello refused: {self.hello}")
        if self.hello["version"] != VERSION:
            raise ConnectionError(f"protocol version {self.hello['version']}, want {VERSION}")

    def ask(self, body):
        send_frame(self.sock, body)
        reply = recv_frame(self.sock)
        if reply is None:
            raise ConnectionError("the Warden closed the connection")
        return decode_reply(reply)

    def close(self):
        self.sock.close()
