"""
Bootstrap-eater smoke test: prefix stdin with a fake sshuttle assembler-protocol
module list, run `rushtle server --compat-bootstrap`, verify it consumes the
prefix and then emits the sync header + initial PING.
"""
import struct
import subprocess
import sys

HDR = struct.Struct("!2sHHH")
MAGIC = b"SS"


def main():
    # Fake bootstrap: 2 modules (gibberish payloads — server discards)
    bootstrap = b""
    for name, payload in [(b"sshuttle.helpers", b"x" * 17), (b"sshuttle.cmdline_options", b"y" * 5)]:
        bootstrap += name + b"\n"
        bootstrap += str(len(payload)).encode() + b"\n"
        bootstrap += payload
    bootstrap += b"\n"  # empty name terminator

    proc = subprocess.Popen(
        ["./target/release/rushtle", "-vv", "server", "--compat-bootstrap"],
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )

    proc.stdin.write(bootstrap)
    proc.stdin.flush()

    expected = b"\0\0SSHUTTLE0001"
    got = proc.stdout.read(len(expected))
    if got != expected:
        proc.kill()
        sys.stderr.write(proc.stderr.read().decode("utf-8", errors="replace"))
        print(f"FAIL: bad sync header after bootstrap: {got!r}")
        sys.exit(1)

    # Initial PING(chicken)
    hdr = proc.stdout.read(8)
    magic, ch, cmd, ln = HDR.unpack(hdr)
    assert magic == MAGIC and ln == 7
    body = proc.stdout.read(ln)
    assert body == b"chicken", body

    # ROUTES (empty)
    hdr = proc.stdout.read(8)
    magic, ch, cmd, ln = HDR.unpack(hdr)
    assert magic == MAGIC and cmd == 0x4207 and ln == 0

    proc.stdin.close()
    proc.terminate()
    try:
        proc.wait(timeout=2)
    except subprocess.TimeoutExpired:
        proc.kill()

    sys.stderr.write(proc.stderr.read().decode("utf-8", errors="replace"))
    print("OK: --compat-bootstrap consumed bootstrap and entered ssnet mode")


if __name__ == "__main__":
    main()
