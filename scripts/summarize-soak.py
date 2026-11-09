#!/usr/bin/env python3
"""Print a Markdown table from soak.py JSON records or a combined results.json."""
import argparse
import collections
import json
import statistics
from pathlib import Path

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("results", type=Path)
args = parser.parse_args()
records = (json.loads(args.results.read_text()) if args.results.is_file() else
           [json.loads(p.read_text()) for p in sorted(args.results.glob("r*.json"))])
groups = collections.defaultdict(list)
for record in records:
    groups[(record["allocator"], record["mode"])].append(record)
print("| Allocator | Mode | n | Ops/s median [min, max] | CPU µs/op median | Peak RSS MiB median | Stop ms median |")
print("|---|---|---:|---:|---:|---:|---:|")
for (allocator, mode), rows in sorted(groups.items()):
    rate = [r["ops_per_s"] for r in rows]
    cpu = statistics.median(r["cpu_s"] * 1e6 / r["ops"] for r in rows)
    rss = statistics.median(r["peak_rss_mib"] for r in rows)
    stop = statistics.median(r["shutdown_s"] * 1000 for r in rows)
    print(f"| {allocator} | {mode} | {len(rows)} | {statistics.median(rate):,.0f} [{min(rate):,.0f}, {max(rate):,.0f}] | {cpu:.3f} | {rss:.2f} | {stop:.2f} |")
