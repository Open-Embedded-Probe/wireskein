"""demo_cmd: a text command protocol of WireSkein's examples, on two UART lines (a command line and its answers).

The host sends a command, the device answers it; both are lines of text ending in "\\n":

    host     #<seq> <VERB>[ <arg>...]          seq: two hex digits, one more each command ("#1f LED 1")
    device   #<seq> OK[ <value>...]            the command done ("#1f OK")
             #<seq> ERR <code>                 the command refused ("#20 ERR 3")
             !<EVENT>[ <value>...]             something the device tells unasked ("!BUTTON 1")

Evidence: an answer whose seq is the command's just before it is a check passed (by chance one in 256); an answer
to no command, a seq that does not match, a command left unanswered when the next one comes, are checks failed.
Text without this shape is neither: on other text the protocol finds nothing and shows nothing.

It rides on "uart_pair": the analysis pairs two UART lines at one rate whose messages follow one another, and gives
both lines' messages in time order (wireskein.protocols). Which line is the host is read from the lines themselves.
"""

from __future__ import annotations

import re

COMMAND = re.compile(r"#([0-9a-f]{2}) ([A-Z][A-Z0-9_]*)((?: \S+)*)$")
ANSWER = re.compile(r"#([0-9a-f]{2}) (OK|ERR)((?: \S+)*)$")
EVENT = re.compile(r"!([A-Z][A-Z0-9_]*)((?: \S+)*)$")


def _lines(msg: dict):
    """A message's lines, each with the tick it starts at (spread over the message by its characters)."""
    text, s, e = msg["text"], msg["s"], msg["e"]
    per = (e - s) / max(1, len(text))
    pos = 0
    for line in text.split("\n"):
        if line.strip("\r"):
            yield line.strip("\r"), int(s + pos * per), int(s + (pos + len(line) + 1) * per)
        pos += len(line) + 1


class DemoCmd:
    name = "demo_cmd"
    over = ("uart_pair",)

    def decode(self, bus: dict) -> dict:
        lines = sorted(((t, a, b, m["pin"]) for m in bus["messages"] for t, a, b in _lines(m)), key=lambda x: x[1])
        # the host is the line that sends commands
        pins = {p for *_, p in lines}
        host = max(pins, key=lambda p: sum(1 for t, *_, q in lines if q == p and COMMAND.match(t)
                                           and not ANSWER.match(t)), default=None)
        out, passed, failed = [], 0, 0
        pending = None                                # the last command not yet answered
        for text, a, b, pin in lines:
            if pin == host:
                c = COMMAND.match(text)
                if not c or ANSWER.match(text):
                    continue
                if pending is not None:                # unanswered before the next one
                    failed += 1
                    pending["level"] = "warn"
                    pending["text"] += " -> (no answer)"
                pending = {"s": a, "e": b, "seq": c[1], "verb": c[2], "args": c[3].split(), "status": None,
                           "text": text}
                out.append(pending)
                continue
            ev = EVENT.match(text)
            if ev:
                out.append({"s": a, "e": b, "event": ev[1], "values": ev[2].split(), "text": text, "level": "ok"})
                continue
            r = ANSWER.match(text)
            if not r:
                continue
            if pending is None or r[1] != pending["seq"]:
                failed += 1
                out.append({"s": a, "e": b, "seq": r[1], "status": r[2], "text": text + " (answers no command)",
                            "level": "error"})
                continue
            passed += 1
            pending.update(e=b, status=r[2], values=r[3].split(), text=pending["text"] + " -> " + text[4:],
                           level="ok" if r[2] == "OK" else "warn")
            pending = None
        return {"messages": out, "checks_passed": passed, "checks_failed": failed}


PROTOCOL = DemoCmd()
