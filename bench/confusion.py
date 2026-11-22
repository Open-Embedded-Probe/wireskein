"""Protocols taken for each other, and those that come close, from runs' e2e rows (one or more run directories or shard
files): which pairs the logic has to tell apart. Usage: confusion.py [--margin 0.15] [--json OUT] PATH...

- taken: a true bus of protocol A whose lines a wrong claim of protocol B took (detail[].taken_by), counted per pair
- close: a claim of A whose runner-up was another protocol B within the margin (claim_list's runner-up), counted per
  pair, right and wrong apart
"""
import argparse
import collections
import glob
import json
import os


def rows(paths):
    for p in paths:
        files = glob.glob(os.path.join(p, "*.jsonl")) if os.path.isdir(p) else [p]
        for f in files:
            for line in open(f):
                r = json.loads(line)
                for x in r.get("results", []):
                    if x.get("stage") == "e2e" and x.get("rows"):
                        yield r.get("dataset", ""), x["rows"][0]


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("paths", nargs="+")
    ap.add_argument("--margin", type=float, default=0.15)
    ap.add_argument("--json")
    a = ap.parse_args()
    taken, close_ok, close_bad = collections.Counter(), collections.Counter(), collections.Counter()
    for _, row in rows(a.paths):
        for d in row.get("detail", []):
            if d.get("taken_by") and d["taken_by"] != d["protocol"]:
                taken[(d["protocol"], d["taken_by"])] += 1
        for c in row.get("claim_list", []):
            if len(c) < 7 or not c[6] or c[6] == c[0] or c[3] > a.margin:
                continue
            pair = tuple(sorted((c[0], c[6])))
            (close_ok if c[4] else close_bad)[pair] += 1
    print("taken for another (true -> claimed):")
    for (t, b), n in taken.most_common(30):
        print(f"  {t:>12} -> {b:<12} {n}")
    print(f"close (runner-up another protocol within {a.margin}): pair, right, wrong")
    for pair in sorted(set(close_ok) | set(close_bad), key=lambda p: -(close_ok[p] + close_bad[p]))[:30]:
        print(f"  {pair[0]:>12} ~ {pair[1]:<12} {close_ok[pair]:5} {close_bad[pair]:5}")
    if a.json:
        json.dump({"taken": [[t, b, n] for (t, b), n in taken.most_common()],
                   "close": [[p[0], p[1], close_ok[p], close_bad[p]] for p in set(close_ok) | set(close_bad)]},
                  open(a.json, "w"), indent=1)


if __name__ == "__main__":
    main()
