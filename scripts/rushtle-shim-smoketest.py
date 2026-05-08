"""
Shim-mode smoke test: invoke `rushtle -c <pyscript>` (sshuttle's python -c
shape) with a fake assembler.py source + module records, verify rushtle
parses N from pyscript, eats those bytes, then consumes module records and
emits sync header.
"""
import struct
import subprocess
import sys

HDR = struct.Struct("!2sHHH")
MAGIC = b"SS"


def main():
    # Fake assembler.py source — content is meaningless; rushtle just
    # discards N bytes.
    assembler = b"# assembler.py source goes here\nprint('hi')\n"
    n = len(assembler)
    pyscript = (
        f"import sys, os; verbosity=0; "
        f"stdin = os.fdopen(0, 'rb'); "
        f"exec(compile(stdin.read({n}), 'assembler.py', 'exec')); "
        f"sys.exit(98);"
    )

    # Fake bootstrap module records (after assembler.py source).
    bootstrap = b""
    for name, payload in [(b"sshuttle.helpers", b"x" * 17), (b"sshuttle.cmdline_options", b"y" * 5)]:
        bootstrap += name + b"\n"
        bootstrap += str(len(payload)).encode() + b"\n"
        bootstrap += payload
    bootstrap += b"\n"

    proc = subprocess.Popen(
        ["./target/release/rushtle", "-c", pyscript],
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )

    proc.stdin.write(assembler + bootstrap)
    proc.stdin.flush()

    expected = b"\0\0SSHUTTLE0001"
    got = proc.stdout.read(len(expected))
    if got != expected:
        proc.kill()
        sys.stderr.write(proc.stderr.read().decode("utf-8", errors="replace"))
        print(f"FAIL: bad sync header: {got!r}")
        sys.exit(1)

    hdr = proc.stdout.read(8)
    magic, ch, cmd, ln = HDR.unpack(hdr)
    body = proc.stdout.read(ln)
    assert body == b"chicken", body
    # ROUTES
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
    print(f"OK: rushtle -c <pyscript> parsed N={n}, consumed bootstrap, sent sync+PING")


if __name__ == "__main__":
    main()
