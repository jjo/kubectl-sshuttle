"""
HOST_REQ smoke test: send CMD_HOST_REQ, verify CMD_HOST_LIST comes back
populated from /etc/hosts.
"""
import struct
import subprocess
import sys
import time

HDR = struct.Struct("!2sHHH")
MAGIC = b"SS"
CMD_HOST_REQ = 0x4208
CMD_HOST_LIST = 0x4209


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


def main():
    proc = subprocess.Popen(
        ["./target/release/rushtle", "-vv", "server"],
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )

    expected = b"\0\0SSHUTTLE0001"
    got = proc.stdout.read(len(expected))
    assert got == expected, f"sync header mismatch: {got!r}"
    proc.stdout.read(8 + 7)  # PING(chicken)
    proc.stdout.read(8)      # ROUTES

    proc.stdin.write(frame(0, CMD_HOST_REQ, b""))
    proc.stdin.flush()

    deadline = time.time() + 3
    response = None
    while time.time() < deadline:
        f = read_frame(proc.stdout)
        if f is None:
            break
        ch, cmd, data = f
        if cmd == CMD_HOST_LIST:
            response = data
            break

    proc.stdin.close()
    proc.terminate()
    try:
        proc.wait(timeout=2)
    except subprocess.TimeoutExpired:
        proc.kill()

    sys.stderr.write(proc.stderr.read().decode("utf-8", errors="replace"))
    if response is None:
        print("FAIL: no HOST_LIST")
        sys.exit(1)
    print(f"HOST_LIST {len(response)} bytes:")
    for line in response.split(b"\n")[:8]:
        print(f"  {line!r}")
    # /etc/hosts always has at least localhost
    assert b"localhost" in response, response[:200]
    print("OK: HOST_REQ -> HOST_LIST contains localhost")


if __name__ == "__main__":
    main()
