"""G5 (bench/notes/2026-11-21-test-plan.ja.md): the analysis' time grows with the capture about in proportion, on the
shapes that make it heavy (tests/complexity_shapes.py). A capture of each shape at n and 4n changes: the time's ratio
8 at most (linear is 4, quadratic 16), and the larger one's time a change under a cap (a few times what this version
takes). 907bb3d's Modbus RTU split, quadratic, took 251 s on uart_gappy at 20k changes (12.5 ms a change), and fails
both."""
import signal
import time

import pytest

from wireskein import decode

import complexity_shapes as C

N = 5000
RATIO_MAX = 8.0
PER_CHANGE_MAX_S = 300e-6
RATIO_MIN_BASE_S = 0.2        # under this the small one's time is mostly fixed costs: its ratio says little
STOP_S = 30                   # an analysis this long has blown up already: stopped, not waited for


class _TooLong(Exception):
    pass


def _cpu(cap):
    def stop(*_):
        raise _TooLong()
    old = signal.signal(signal.SIGALRM, stop)
    signal.alarm(STOP_S)
    t = time.process_time()
    try:
        decode.analyze(cap)
    except _TooLong:
        pytest.fail(f"analysis over {STOP_S} s (the time blew up)")
    finally:
        signal.alarm(0)
        signal.signal(signal.SIGALRM, old)
    return time.process_time() - t


@pytest.mark.parametrize("shape", sorted(C.SHAPES))
def test_time_grows_in_proportion(shape):
    small, large = C.capture(shape, N), C.capture(shape, 4 * N)
    t1, t4 = _cpu(small), _cpu(large)
    changes = sum(len(ch.edges) for ch in large.channels)
    assert t4 / changes < PER_CHANGE_MAX_S, f"{shape}: {t4:.2f} s for {changes} changes"
    if t1 >= RATIO_MIN_BASE_S:
        assert t4 / t1 < RATIO_MAX, f"{shape}: {t1:.2f} s -> {t4:.2f} s for 4 times the changes"
