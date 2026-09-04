#!/usr/bin/env python3
"""Give a command a terminal, and answer for it.

The suite runs the binaries through pipes, which never see the board: a row
drawn in place needs a terminal, and a terminal that answers. `script(1)`
gives a command a pty but is not a terminal emulator, so when the board asks
where the cursor is (ESC [ 6 n) nobody replies and the board falls back to
plain lines. This driver is the least terminal that can hold a board up: it
answers the cursor query, sets a window size, can change that size mid-run
with a SIGWINCH, can press keys, and reports whether the command gave the
terminal back in cooked mode.

Usage:
  pty.py [--cols N] [--rows N] [--resize AT:COLSxROWS]... [--key AT:TEXT]...
         --out FILE -- command args...

AT is seconds after start. Everything the command drew goes to FILE, raw, and
the exit status is the command's. To learn whether the command gave the
terminal back, run it through a shell that prints `stty -a` afterwards: the
slave is gone from this side once the session ends.

The cursor row it reports back is an estimate from the newlines it has seen.
That is enough for a board to anchor itself and redraw, and not enough to say
which screen row a given part landed on, so assert on what the board writes
rather than on where it went.
"""

import argparse
import fcntl
import os
import select
import signal
import struct
import subprocess
import sys
import termios
import time

CURSOR_QUERY = b"\x1b[6n"


def set_size(fd, cols, rows):
    fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack("HHHH", rows, cols, 0, 0))


def parse_at(spec, what):
    at, _, rest = spec.partition(":")
    if not rest:
        sys.exit(f"pty.py: {what} needs AT:VALUE, got {spec!r}")
    return float(at), rest


def main():
    ap = argparse.ArgumentParser(add_help=False)
    ap.add_argument("--cols", type=int, default=100)
    ap.add_argument("--rows", type=int, default=30)
    ap.add_argument("--resize", action="append", default=[])
    ap.add_argument("--key", action="append", default=[])
    ap.add_argument("--out", required=True)
    ap.add_argument("--timeout", type=float, default=60.0)
    ap.add_argument("command", nargs=argparse.REMAINDER)
    args = ap.parse_args()
    command = args.command[1:] if args.command[:1] == ["--"] else args.command
    if not command:
        sys.exit("pty.py: no command given")

    resizes = sorted(parse_at(r, "--resize") for r in args.resize)
    keys = sorted(parse_at(k, "--key") for k in args.key)
    rows, cols = args.rows, args.cols

    master, slave = os.openpty()
    set_size(slave, cols, rows)
    # Never block on the master: a read that select() said was ready can
    # still wait once the command has gone, and the loop below must always
    # get back to its exit checks.
    fcntl.fcntl(master, fcntl.F_SETFL, fcntl.fcntl(master, fcntl.F_GETFL) | os.O_NONBLOCK)

    def child_setup():
        # A session of its own with the pty as its controlling terminal, so
        # the command sees a terminal on all three fds and SIGWINCH reaches it.
        os.setsid()
        fcntl.ioctl(slave, termios.TIOCSCTTY, 0)

    proc = subprocess.Popen(
        command, stdin=slave, stdout=slave, stderr=slave, preexec_fn=child_setup, close_fds=True
    )

    started = time.monotonic()
    out = open(args.out, "wb")
    tail = b""
    newlines = 0
    exited_at = None
    while True:
        now = time.monotonic() - started
        while resizes and resizes[0][0] <= now:
            _, spec = resizes.pop(0)
            c, _, r = spec.partition("x")
            cols, rows = int(c), int(r)
            set_size(slave, cols, rows)
            os.kill(proc.pid, signal.SIGWINCH)
        while keys and keys[0][0] <= now:
            _, text = keys.pop(0)
            os.write(master, text.encode())
        ready, _, _ = select.select([master], [], [], 0.05)
        data = b""
        if ready:
            try:
                data = os.read(master, 65536)
            except BlockingIOError:
                data = b""
            except OSError:
                # EIO: the slave side is gone. Nothing more will arrive.
                break
            ready = bool(data)
            if data:
                out.write(data)
                out.flush()
                newlines += data.count(b"\n")
                # The query may straddle two reads; keep enough of the last
                # read to see it whole.
                window = tail + data
                if CURSOR_QUERY in window:
                    row = min(newlines, rows - 1) + 1
                    os.write(master, f"\x1b[{row};1R".encode())
                tail = window[-(len(CURSOR_QUERY) - 1):]
        if proc.poll() is not None:
            # Drain what the exit left behind, then stop.
            if exited_at is None:
                exited_at = time.monotonic()
            elif time.monotonic() - exited_at > 0.3 and not ready:
                break
        elif now > args.timeout:
            os.killpg(proc.pid, signal.SIGKILL)
            proc.wait()
            break
    out.close()
    # Whether the terminal came back cooked is asked from inside the session
    # (a `stty -a` after the command), not from here: macOS revokes the slave
    # the moment its session leader exits, and every ioctl after that fails.
    sys.exit(proc.returncode if proc.returncode is not None else 124)


if __name__ == "__main__":
    main()
