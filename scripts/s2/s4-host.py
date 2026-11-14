#!/usr/bin/env python3
"""Spike S4's stand-in for the island socket: on the host, a Unix socket
placed in the Vessels' read-only root filesystem, as run h placed the
echo socket.  It answers the questions an agent cannot answer from inside
its Vessel, and keeps a journal outside every Vessel, so nothing a
rollback or a promotion does to /work can hide what happened.

    s4-host.py --socket PATH --journal FILE --uid-base N --prime-slot N

Protocol: one JSON object per line each way.

    {"op": "whoami"}                 -> {"uid", "slot", "role"}  role: prime | fork
    {"op": "log", "event": {...}}    -> {"ok": true}   appended to the journal
    {"op": "note", "text": "..."}    -> {"ok": true}   a fork's note to its parent
    {"op": "notes"}                  -> {"notes": [{"slot", "text"}, ...]}
    {"op": "put", "key", "value"}    -> {"ok": true}   kept outside the Vessel
    {"op": "get", "key"}             -> {"value": ... or null}

The answer to whoami comes from the connection's peer uid, as the real
island-socket's will: slot = (uid - uid_base) / 65536.  Every journal line
carries the slot of the Vessel that wrote it.
"""

import argparse
import json
import os
import socket
import struct
import sys
import threading

RANGE = 65536


class Host:
    def __init__(self, args):
        self.args = args
        self.lock = threading.Lock()
        self.notes = []
        self.kv = {}

    def slot_of(self, uid):
        return (uid - self.args.uid_base) // RANGE

    def journal(self, slot, event):
        line = json.dumps({"slot": slot, **event}, sort_keys=True)
        with self.lock, open(self.args.journal, "a") as out:
            out.write(line + "\n")

    def answer(self, slot, uid, req):
        op = req.get("op")
        if op == "whoami":
            role = "prime" if slot == self.args.prime_slot else "fork"
            return {"uid": uid, "slot": slot, "role": role}
        if op == "log":
            self.journal(slot, req.get("event", {}))
            return {"ok": True}
        if op == "note":
            with self.lock:
                self.notes.append({"slot": slot, "text": req.get("text", "")})
            self.journal(slot, {"step": "note-posted", "text": req.get("text", "")})
            return {"ok": True}
        if op == "notes":
            with self.lock:
                return {"notes": list(self.notes)}
        if op == "put":
            with self.lock:
                self.kv[req["key"]] = req.get("value")
            return {"ok": True}
        if op == "get":
            with self.lock:
                return {"value": self.kv.get(req["key"])}
        return {"error": f"unknown op {op!r}"}

    def serve(self, conn):
        try:
            creds = conn.getsockopt(socket.SOL_SOCKET, socket.SO_PEERCRED, struct.calcsize("3i"))
            _pid, uid, _gid = struct.unpack("3i", creds)
            slot = self.slot_of(uid)
            reader = conn.makefile("r")
            for line in reader:
                try:
                    reply = self.answer(slot, uid, json.loads(line))
                except (ValueError, KeyError) as e:
                    reply = {"error": f"{type(e).__name__}: {e}"}
                conn.sendall((json.dumps(reply) + "\n").encode())
        except OSError:
            pass
        finally:
            conn.close()


def main():
    p = argparse.ArgumentParser(description="spike S4: the island socket's stand-in (host)")
    p.add_argument("--socket", required=True)
    p.add_argument("--journal", required=True)
    p.add_argument("--uid-base", type=int, default=200000)
    p.add_argument("--prime-slot", type=int, required=True)
    args = p.parse_args()
    if os.path.exists(args.socket):
        os.unlink(args.socket)
    os.makedirs(os.path.dirname(args.socket), mode=0o755, exist_ok=True)
    host = Host(args)
    s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    s.bind(args.socket)
    os.chmod(args.socket, 0o666)  # any Vessel of the run; the real socket checks the uid
    s.listen(16)
    print(f"s4-host: listening on {args.socket}", file=sys.stderr, flush=True)
    while True:
        conn, _ = s.accept()
        threading.Thread(target=host.serve, args=(conn,), daemon=True).start()


if __name__ == "__main__":
    main()
