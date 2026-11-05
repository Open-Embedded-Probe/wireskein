"""demo_link: a protocol of one's own, as an example of adding one to WireSkein's analysis (wireskein.protocols).

A made-up framed command protocol, the shape many device protocols have:

    0xA5 | len | seq | cmd | payload[len] | crc8

- 0xA5 starts a frame; a reader that starts mid-stream looks for the next 0xA5 whose frame checks.
- len is the payload's length (0..32), seq counts frames (0..255, wrapping), cmd the command; a reply has bit 7 of
  cmd set and the seq of its request.
- crc8 is CRC-8 (polynomial 0x07, initial 0, no reflection) of len, seq, cmd and the payload.

On a UART the frames follow one another in the byte stream; on SPI each window of CS carries one frame on MOSI
(the request) and one on MISO (the reply to the request before). The same decode reads both: it gets the bus as the
analysis exports it, finds frames in each byte stream and counts their CRCs - the evidence that the bus carries this
protocol.

Use it in a script with wireskein.protocols.register(DemoLink()), or install a package whose pyproject.toml says

    [project.entry-points."wireskein.protocols"]
    demo_link = "demo_link:PROTOCOL"
"""

SOF = 0xA5
MAX_LEN = 32


def crc8(data: bytes) -> int:
    crc = 0
    for b in data:
        crc ^= b
        for _ in range(8):
            crc = ((crc << 1) ^ 0x07) & 0xFF if crc & 0x80 else (crc << 1) & 0xFF
    return crc


def frame(seq: int, cmd: int, payload: bytes = b"") -> bytes:
    """One frame's bytes (for making test signals)."""
    body = bytes([len(payload), seq & 0xFF, cmd & 0xFF]) + payload
    return bytes([SOF]) + body + bytes([crc8(body)])


def frames_in(data: bytes, ticks: list[int] | None = None) -> tuple[list[dict], int, int]:
    """The frames in a byte stream: ([{"s", "seq", "cmd", "reply", "payload"}], CRCs passed, CRCs failed). A 0xA5
    whose frame fails its CRC is counted failed and the search goes on from the next byte (resynchronizing)."""
    out, ok, bad, i = [], 0, 0, 0
    while i < len(data):
        if data[i] != SOF or i + 5 > len(data):
            i += 1
            continue
        n = data[i + 1]
        end = i + 5 + n
        if n > MAX_LEN or end > len(data):
            i += 1
            continue
        body = data[i + 1:end - 1]
        if crc8(body) != data[end - 1]:
            bad += 1
            i += 1
            continue
        ok += 1
        cmd = body[2]
        out.append({"s": ticks[i] if ticks else i, "seq": body[1], "cmd": cmd & 0x7F, "reply": bool(cmd & 0x80),
                    "payload": body[3:].hex()})
        i = end
    return out, ok, bad


class DemoLink:
    name = "demo_link"
    over = ("uart", "spi")

    def decode(self, bus: dict) -> dict:
        msgs, ok, bad = [], 0, 0
        if "bytes" in bus:                                   # a UART: one byte stream, each byte's start tick
            data = bytes.fromhex(bus["bytes"]["hex"])
            msgs, ok, bad = frames_in(data, bus["bytes"]["s"])
        for t in bus.get("transfers", []):                   # SPI: a frame per CS window on each data line
            for pin, hx in t.items():
                if pin in ("s", "e") or not isinstance(hx, str):
                    continue
                got, o, b = frames_in(bytes.fromhex(hx))
                msgs += [{**m, "s": t["s"], "line": pin} for m in got]
                ok, bad = ok + o, bad + b
        return {"messages": msgs, "checks_passed": ok, "checks_failed": bad}


PROTOCOL = DemoLink()
