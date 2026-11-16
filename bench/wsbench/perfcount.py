"""The instructions a call retires, from the CPU's own counter (Linux perf_event_open, user space, this thread).

Steadier than any time: a busy machine, a changing clock or another job beside it hardly move it. Available on
native Linux with a PMU (a desktop, a laptop, most servers); not on WSL2 or most cloud VMs, where `counter()` is
None and nothing is recorded. /proc/sys/kernel/perf_event_paranoid must be 2 or less (Ubuntu's default 4 forbids it
to users: `sudo sysctl kernel.perf_event_paranoid=2`).

    python bench/wsbench/perfcount.py          # does this machine count? (prints why not)
"""

from __future__ import annotations

import ctypes
import fcntl
import os
import platform
import struct

_SYSCALL = {"x86_64": 298, "aarch64": 241, "arm64": 241}
_RESET, _ENABLE, _DISABLE = 0x2403, 0x2400, 0x2401
_FLAGS = (1 << 0) | (1 << 5) | (1 << 6)      # disabled, exclude_kernel, exclude_hv


class Counter:
    def __init__(self, fd: int):
        self.fd = fd

    def start(self) -> None:
        fcntl.ioctl(self.fd, _RESET, 0)
        fcntl.ioctl(self.fd, _ENABLE, 0)

    def stop(self) -> int:
        fcntl.ioctl(self.fd, _DISABLE, 0)
        return struct.unpack("q", os.read(self.fd, 8))[0]


_COUNTER: Counter | None = None
_WHY = ""
_PID = None          # the process that opened it: a counter counts the thread that opened it, so a forked worker
                     # that kept its parent's would count the parent

def _open() -> tuple[Counter | None, str]:
    nr = _SYSCALL.get(platform.machine())
    if platform.system() != "Linux" or nr is None:
        return None, f"not Linux on x86_64 / aarch64 ({platform.system()} {platform.machine()})"
    attr = bytearray(128)                     # perf_event_attr; the kernel takes a larger one if the rest is zero
    struct.pack_into("IIQ", attr, 0, 0, 128, 1)   # PERF_TYPE_HARDWARE, size, PERF_COUNT_HW_INSTRUCTIONS
    struct.pack_into("Q", attr, 40, _FLAGS)
    libc = ctypes.CDLL(None, use_errno=True)
    buf = (ctypes.c_char * len(attr)).from_buffer(attr)
    fd = libc.syscall(nr, buf, 0, -1, -1, 0)  # this thread, any CPU, no group, no flags
    if fd < 0:
        err = ctypes.get_errno()
        why = os.strerror(err)
        if err == 2:
            why += " (no hardware counters here: a VM or WSL2)"
        elif err in (1, 13):
            why += " (kernel.perf_event_paranoid too high: sudo sysctl kernel.perf_event_paranoid=2)"
        return None, why
    return Counter(fd), ""


def counter() -> Counter | None:
    """This process's counter, opened once in it (None where there is none)."""
    global _COUNTER, _WHY, _PID
    if _PID != os.getpid():
        if _COUNTER is not None:
            os.close(_COUNTER.fd)          # the parent's, inherited through fork
        _COUNTER, _WHY = _open()
        _WHY = _WHY or "ok"
        _PID = os.getpid()
    return _COUNTER


def why() -> str:
    counter()
    return _WHY


if __name__ == "__main__":
    c = counter()
    if c is None:
        print("no instruction counter:", why())
        raise SystemExit(1)
    n = []
    for _ in range(3):
        c.start()
        sum(range(1_000_000))
        n.append(c.stop())
    print("instruction counter works: sum(range(1e6)) =", n, "instructions (should be nearly equal)")
    # the bench's workers are forked: each must count itself, not the parent it came from
    r, w = os.pipe()
    pid = os.fork()
    if pid == 0:
        c = counter()
        c.start()
        sum(range(1_000_000))
        os.write(w, str(c.stop()).encode())
        os._exit(0)
    os.waitpid(pid, 0)
    got = int(os.read(r, 64))
    ok = abs(got - n[0]) < 0.05 * n[0]
    print(f"in a forked worker: {got} instructions", "(counts itself: ok)" if ok else "(NOT its own count: a bug)")
    raise SystemExit(0 if ok else 1)
