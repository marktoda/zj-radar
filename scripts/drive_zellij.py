#!/usr/bin/env python3
"""PTY driver for the release funnel test (scripts/funnel.sh).

Boots a real Zellij session on a sized pseudo-terminal (Zellij refuses 0x0),
lets the rail load, changes directory to exercise cwd-driven tab naming, and
writes the raw terminal stream to /tmp/typescript for the caller's assertions.
Exits after driving; the (detached) Zellij server stays up so the caller can
interrogate it with `zellij --session funnel action …`.
"""

import fcntl
import os
import pty
import select
import struct
import subprocess
import termios

BOOT_SECS = 12   # rail load + pre-seeded grant auto-resolve (CI runners are slow)
STEP_SECS = 8    # cwd poll is 1s and activity-gated; leave slack

master, slave = pty.openpty()
fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack('HHHH', 30, 110, 0, 0))
subprocess.Popen(
    ['zellij', '-s', 'funnel'],
    stdin=slave, stdout=slave, stderr=slave, close_fds=True, cwd='/opt',
)

buf = b''


def drain(seconds):
    global buf
    import time
    end = time.time() + seconds
    while time.time() < end:
        ready, _, _ = select.select([master], [], [], 0.2)
        if master in ready:
            try:
                buf += os.read(master, 65536)
            except OSError:
                break


drain(BOOT_SECS)
os.write(master, b'cd /srv\r')   # CwdChanged -> smart tab naming
drain(STEP_SECS)
with open('/tmp/typescript', 'wb') as f:
    f.write(buf)
