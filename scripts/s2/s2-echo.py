#!/usr/bin/env python3
"""The S2 spike's stand-in for an inference socket: on the host, a Unix
socket that echoes what it is sent.  Run h places it inside the Vessels'
root filesystem and checks that an agent, prime or fork child, can reach
it through the read-only root.

    s2-echo.py PATH

Protocol: u64 LE length + that many bytes; the bytes come back.  Each
connection is logged to stderr with the peer's uid, which shows the
Vessel's mapped user id on the host.
"""

import os
import socket
import struct
import sys
import threading


def serve(conn):
    try:
        creds = conn.getsockopt(socket.SOL_SOCKET, socket.SO_PEERCRED, struct.calcsize("3i"))
        pid, uid, gid = struct.unpack("3i", creds)
        hdr = b""
        while len(hdr) < 8:
            chunk = conn.recv(8 - len(hdr))
            if not chunk:
                return
            hdr += chunk
        (n,) = struct.unpack("<Q", hdr)
        left = n
        while left:
            chunk = conn.recv(min(1 << 16, left))
            if not chunk:
                break
            conn.sendall(chunk)
            left -= len(chunk)
        print(f"s2-echo: {n - left} of {n} bytes echoed for pid {pid} uid {uid} gid {gid}",
              file=sys.stderr, flush=True)
    finally:
        conn.close()


def main():
    path = sys.argv[1]
    if os.path.exists(path):
        os.unlink(path)
    os.makedirs(os.path.dirname(path), mode=0o755, exist_ok=True)
    s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    s.bind(path)
    # Any Vessel's mapped uid may connect: the spike tests reachability,
    # not who may reach it.  The real proxy decides that per Vessel.
    os.chmod(path, 0o666)
    s.listen(16)
    print(f"s2-echo: listening on {path}", file=sys.stderr, flush=True)
    while True:
        conn, _ = s.accept()
        threading.Thread(target=serve, args=(conn,), daemon=True).start()


if __name__ == "__main__":
    main()
