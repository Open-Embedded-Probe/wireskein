"""demo_telemetry: a protocol of WireSkein's examples on the frames of CAN and LIN (and of a protocol one defines).

Each node sends its readings in frames of its own identifier; the data of a frame is

    seq | reading... | check

- seq counts the node's frames (one more each, 0 after 255),
- check is (identifier + the bytes before it) & 0xFF, xor 0x5A: one in 256 by chance on other data.

The check is the evidence: a frame of 2 bytes or more whose check holds is a check passed, one whose check does
not a check failed; a frame shorter than 2 bytes is not this protocol's. A seq that does not follow the node's last
one is shown (level "warn": a frame lost or sent again), not a failed check.

It rides on "can" and "lin": the analysis gives it the frames it decoded ({"s", "id", "data": hex, ...}), so one
decode reads both (wireskein.protocols).
"""

from __future__ import annotations


def check_of(ident: int, body: bytes) -> int:
    return ((ident + sum(body)) & 0xFF) ^ 0x5A


class DemoTelemetry:
    name = "demo_telemetry"
    over = ("can", "lin")

    def decode(self, bus: dict) -> dict:
        out, passed, failed, last = [], 0, 0, {}
        for f in bus.get("frames", []):
            if not isinstance(f, dict) or f.get("ok") is False:
                continue                                  # a frame the bus itself got wrong
            data = bytes.fromhex(f.get("data") or "")
            if len(data) < 2:
                continue
            ident = int(f["id"])
            if data[-1] != check_of(ident, data[:-1]):
                failed += 1
                continue
            passed += 1
            seq, readings = data[0], data[1:-1]
            gap = ident in last and seq != (last[ident] + 1) & 0xFF
            last[ident] = seq
            out.append({"s": f["s"], "id": ident, "seq": seq, "readings": readings.hex(),
                        "text": f"{ident:#x} #{seq} {readings.hex() or '-'}" + (" (seq skipped)" if gap else ""),
                        "level": "warn" if gap else "ok"})
        return {"messages": out, "checks_passed": passed, "checks_failed": failed}


PROTOCOL = DemoTelemetry()


def frame(ident: int, seq: int, readings: bytes) -> bytes:
    """A frame's data, for the tests and for a sender."""
    body = bytes([seq & 0xFF]) + readings
    return body + bytes([check_of(ident, body)])
