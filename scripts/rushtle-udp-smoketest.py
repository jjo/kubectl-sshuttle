"""
UDP smoke test: drive rushtle server, open a UDP channel, sendto a local
echo UDP server, verify reply comes back via UDP_DATA.
"""
import socket
import struct
import subprocess
import sys
import threading
import time

HDR = struct.Struct("!2sHHH")
MAGIC = b"SS"
CMD_UDP_OPEN = 0x420C
CMD_UDP_DATA = 0x420D
CMD_UDP_CLOSE = 0x420E


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
    # UDP echo server on 127.0.0.1:9998
    echo = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    echo.bind(("127.0.0.1", 9998))

    def echo_loop():
        while True:
            try:
                d, peer = echo.recvfrom(4096)
                echo.sendto(d, peer)
            except OSError:
                return

    threading.Thread(target=echo_loop, daemon=True).start()

    proc = subprocess.Popen(
        ["./target/release/rushtle", "-vv", "server"],
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )

    # consume sync header + initial PING
    expected = b"\0\0SSHUTTLE0001"
    got = proc.stdout.read(len(expected))
    assert got == expected, f"sync header mismatch: {got!r}"
    proc.stdout.read(8 + 7)  # PING(chicken)
    proc.stdout.read(8)      # ROUTES

    # UDP_OPEN family=2 (AF_INET) on channel 5
    proc.stdin.write(frame(5, CMD_UDP_OPEN, b"2"))
    # UDP_DATA: send "PING" to 127.0.0.1:9998
    proc.stdin.write(frame(5, CMD_UDP_DATA, b"127.0.0.1,9998,PING"))
    proc.stdin.flush()

    deadline = time.time() + 3
    response = None
    while time.time() < deadline:
        f = read_frame(proc.stdout)
        if f is None:
            break
        ch, cmd, data = f
        if cmd == CMD_UDP_DATA and ch == 5:
            response = data
            break

    proc.stdin.write(frame(5, CMD_UDP_CLOSE, b""))
    proc.stdin.close()
    proc.terminate()
    try:
        proc.wait(timeout=2)
    except subprocess.TimeoutExpired:
        proc.kill()

    sys.stderr.write(proc.stderr.read().decode("utf-8", errors="replace"))
    if not response:
        print("FAIL: no UDP_DATA reply")
        sys.exit(1)
    # expected payload: "127.0.0.1,9998,PING"
    parts = response.split(b",", 2)
    assert parts[0] == b"127.0.0.1", parts
    assert parts[1] == b"9998", parts
    assert parts[2] == b"PING", parts
    print("OK: rushtle server UDP_OPEN + UDP_DATA + reply works")


if __name__ == "__main__":
    main()
