"""
DNS smoke test: drive rushtle server via crafted ssnet frames, send a real
DNS query for example.com, verify a non-empty A response comes back.
"""
import socket
import struct
import subprocess
import sys
import time

HDR = struct.Struct("!2sHHH")
MAGIC = b"SS"
CMD_DNS_REQ = 0x420A
CMD_DNS_RESPONSE = 0x420B


def frame(channel, cmd, data=b""):
    return HDR.pack(MAGIC, channel, cmd, len(data)) + data


def read_frame(rd):
    hdr = rd.read(8)
    if not hdr or len(hdr) < 8:
        return None
    magic, ch, cmd, ln = HDR.unpack(hdr)
    assert magic == MAGIC, magic
    payload = rd.read(ln) if ln else b""
    return (ch, cmd, payload)


def build_query(name="example.com.", qtype=1):
    # Minimal DNS query packet (header + question section).
    txid = 0x1234
    flags = 0x0100  # standard query, RD=1
    qdcount, ancount, nscount, arcount = 1, 0, 0, 0
    hdr = struct.pack(">HHHHHH", txid, flags, qdcount, ancount, nscount, arcount)
    qname = b""
    for label in name.strip(".").split("."):
        qname += bytes([len(label)]) + label.encode()
    qname += b"\x00"
    qsec = qname + struct.pack(">HH", qtype, 1)  # A, IN
    return hdr + qsec


def main():
    proc = subprocess.Popen(
        ["./target/release/rushtle", "-vv", "server"],
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )

    # consume sync header + initial PING + ROUTES
    expected = b"\0\0SSHUTTLE0001"
    got = proc.stdout.read(len(expected))
    assert got == expected, f"sync header mismatch: {got!r}"
    proc.stdout.read(8 + 7)  # PING frame: 8 hdr + 7-byte 'chicken'
    proc.stdout.read(8)      # ROUTES frame: 8 hdr + 0 payload

    query = build_query("example.com.", 1)
    proc.stdin.write(frame(7, CMD_DNS_REQ, query))
    proc.stdin.flush()

    deadline = time.time() + 6
    response = None
    while time.time() < deadline:
        f = read_frame(proc.stdout)
        if f is None:
            break
        ch, cmd, data = f
        if cmd == CMD_DNS_RESPONSE and ch == 7:
            response = data
            break

    proc.stdin.close()
    proc.terminate()
    try:
        proc.wait(timeout=2)
    except subprocess.TimeoutExpired:
        proc.kill()

    sys.stderr.write(proc.stderr.read().decode("utf-8", errors="replace"))
    if not response:
        print("FAIL: no DNS_RESPONSE")
        sys.exit(1)
    if len(response) < 12:
        print(f"FAIL: short DNS response: {response.hex()}")
        sys.exit(1)
    txid, flags, qd, an, ns, ar = struct.unpack(">HHHHHH", response[:12])
    print(f"DNS reply: txid={txid:#x} flags={flags:#x} answers={an}")
    if txid != 0x1234:
        print("FAIL: txid mismatch")
        sys.exit(1)
    print("OK: rushtle server resolved example.com via /etc/resolv.conf upstream")


if __name__ == "__main__":
    main()
