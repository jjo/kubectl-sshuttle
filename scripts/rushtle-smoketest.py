"""
Smoke test: pipe rushtle client (no iptables) to rushtle server in same
process group; manually send a CONNECT + DATA frame and verify echo.

We bypass the client binary because it requires iptables/root. Instead,
craft ssnet frames against the server directly and verify it forwards bytes
to a local TCP echo target.
"""
import socket
import struct
import subprocess
import sys
import threading
import time

HDR = struct.Struct("!2sHHH")  # SS chan cmd len
MAGIC = b"SS"

CMD_TCP_CONNECT = 0x4203
CMD_TCP_DATA = 0x4206
CMD_TCP_EOF = 0x4205
CMD_PING = 0x4201
CMD_PONG = 0x4202

SYNC_HEADER = b"\0\0SSHUTTLE0001"


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


def read_sync_header(rd):
    # mirror sshuttle/client.py: skip until two NULs, then read literal.
    expected = b"SSHUTTLE0001"
    nuls = 0
    while nuls < 2:
        b = rd.read(1)
        if not b:
            raise EOFError("eof during sync header")
        if b == b"\x00":
            nuls += 1
    got = rd.read(len(expected))
    assert got == expected, f"sync header mismatch: {got!r}"


def main():
    # 1) Start a local TCP echo server on 127.0.0.1:9999
    echo = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    echo.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    echo.bind(("127.0.0.1", 9999))
    echo.listen(1)

    def echo_loop():
        c, _ = echo.accept()
        while True:
            d = c.recv(4096)
            if not d:
                break
            c.sendall(d)
        c.close()

    threading.Thread(target=echo_loop, daemon=True).start()

    # 2) Spawn rushtle server.
    proc = subprocess.Popen(
        ["./target/release/rushtle", "-vv", "server"],
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )

    # 3) Read sync header.
    read_sync_header(proc.stdout)

    # 3b) Server emits PING(chicken) then empty ROUTES. Drain both.
    f = read_frame(proc.stdout)
    print("initial:", f, file=sys.stderr)
    assert f and f[1] == CMD_PING and f[2] == b"chicken", f

    f = read_frame(proc.stdout)
    print("routes:", f, file=sys.stderr)
    CMD_ROUTES = 0x4207
    assert f and f[1] == CMD_ROUTES, f

    proc.stdin.write(frame(0, CMD_PING, b"hello"))
    proc.stdin.flush()
    f = read_frame(proc.stdout)
    print("ping reply:", f, file=sys.stderr)
    assert f and f[1] == CMD_PONG and f[2] == b"hello", f

    # 4) Open ch=1 to 127.0.0.1:9999 with 3-part CONNECT, send "ABC",
    # expect echo back.
    proc.stdin.write(frame(1, CMD_TCP_CONNECT, b"2,127.0.0.1,9999"))
    proc.stdin.write(frame(1, CMD_TCP_DATA, b"ABC"))
    proc.stdin.flush()

    deadline = time.time() + 3
    received = b""
    while time.time() < deadline:
        f = read_frame(proc.stdout)
        if f is None:
            break
        ch, cmd, data = f
        print("  rx:", ch, hex(cmd), data, file=sys.stderr)
        if ch == 1 and cmd == CMD_TCP_DATA:
            received += data
            if received == b"ABC":
                break

    # Half-close the connection so the server can return EOF cleanly
    proc.stdin.write(frame(1, CMD_TCP_EOF, b""))
    proc.stdin.flush()
    proc.stdin.close()

    proc.terminate()
    try:
        proc.wait(timeout=2)
    except subprocess.TimeoutExpired:
        proc.kill()

    print("RX:", received, file=sys.stderr)
    print("STDERR LOG:", file=sys.stderr)
    sys.stderr.write(proc.stderr.read().decode("utf-8", errors="replace"))
    assert received == b"ABC", f"echo mismatch: {received!r}"
    print("\nOK: rushtle server PING+CONNECT+DATA roundtrip works.")


if __name__ == "__main__":
    main()
