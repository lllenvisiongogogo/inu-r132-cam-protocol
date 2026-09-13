#!/usr/bin/env python3
"""Dump the value distribution of an `inu-r132 serve --stream depth` stream.

Run it against a depth server to tell a data problem (holes, coarse/quantised
depth, wrong units) apart from a rendering problem (the [near, far] window and
the linear greyscale curve).

    ./build/inu-r132 serve --stream depth --port 5534 &
    scripts/depth_stats.py 127.0.0.1:5534

Values are little endian u16. 0 means "no measurement".
"""

import collections
import socket
import struct
import sys


def recv_msg(sock):
    header = b""
    while len(header) < 4:
        header += sock.recv(4 - len(header))
    (length,) = struct.unpack(">I", header)
    data = b""
    while len(data) < length:
        data += sock.recv(length - len(data))
    return data[0], data[1:]


def main():
    address = sys.argv[1] if len(sys.argv) > 1 else "127.0.0.1:5534"
    host, _, port = address.rpartition(":")
    sock = socket.create_connection((host or "127.0.0.1", int(port or 5534)), timeout=10)

    recv_msg(sock)  # hello
    sock.sendall(b"subscribe\n")
    recv_msg(sock)  # ok subscribed

    while True:
        kind, payload = recv_msg(sock)
        if kind != 1:
            continue
        width, height, codec, _ = struct.unpack(">IIBB", payload[:10])
        if codec != 2:
            print(f"not a depth stream (codec {codec}); run serve --stream depth")
            return 1
        values = struct.unpack("<%dH" % (width * height), payload[10 : 10 + width * height * 2])
        break

    total = len(values)
    zero = sum(1 for v in values if v == 0)
    valid = sorted(v for v in values if v != 0)
    print(f"frame {width}x{height}, {total} pixels")
    print(f"invalid (0)          : {zero} ({100 * zero / total:.1f}%)")
    if not valid:
        print("no valid depth at all")
        return 0

    def pct(p):
        return valid[min(len(valid) - 1, int(len(valid) * p / 100))]

    print(f"valid min/median/max : {valid[0]} / {valid[len(valid) // 2]} / {valid[-1]} mm")
    print("percentiles (mm)     : " + "  ".join(f"p{p}={pct(p)}" for p in (1, 5, 25, 50, 75, 95, 99)))

    lo, hi = pct(1), max(pct(99), pct(1) + 1)
    buckets = collections.Counter(
        min(7, (v - lo) * 8 // (hi - lo + 1)) for v in valid if lo <= v <= hi
    )
    inside = sum(buckets.values())
    print(f"p1..p99 = {lo}..{hi} mm, {100 * inside / total:.1f}% of pixels inside; histogram:")
    peak = max(buckets.values())
    for i in range(8):
        count = buckets.get(i, 0)
        start = lo + (hi - lo) * i // 8
        end = lo + (hi - lo) * (i + 1) // 8
        print(f"  {start:5d}-{end:5d} mm | {'#' * int(40 * count / peak)} {100 * count / total:.1f}%")
    print("top 8 exact values   :", collections.Counter(values).most_common(8))
    return 0


if __name__ == "__main__":
    sys.exit(main())
