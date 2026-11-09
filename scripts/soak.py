#!/usr/bin/env python3
"""Run one interleaved round of the release-mode overhead experiment."""
import argparse
import json
import os
import shutil
import subprocess
import time
from pathlib import Path

CASES = [
    ("jemalloc", "off"), ("jemalloc", "otel-cpu"), ("jemalloc", "pyroscope-cpu"),
    ("jemalloc", "otel-all"), ("jemalloc", "pyroscope-all"),
    ("system", "off"), ("sampled", "off"), ("sampled", "otel-all"),
]


def run(args):
    args.output.mkdir(parents=True, exist_ok=True)
    cases = CASES if args.round % 2 else list(reversed(CASES))
    for allocator, mode in cases:
        assert shutil.disk_usage(args.output).free > 10 * 1024**3, "disk floor"
        stem = args.output / f"r{args.round}-{allocator}-{mode}"
        assert not stem.with_suffix(".json").exists(), f"refusing to overwrite {stem}"
        env = dict(os.environ, SOAK_OTLP=args.otlp, SOAK_PYROSCOPE=args.pyroscope)
        # pyroscope's pprof_util parser panics on zero-inuse, accumulated-only stacks.
        if mode == "pyroscope-all":
            env["_RJEM_MALLOC_CONF"] = "prof:true,prof_active:true,prof_accum:false,lg_prof_sample:19"
        start = time.time()
        with stem.with_suffix(".log").open("w") as log:
            result = subprocess.run([str(args.binaries / f"soak-{allocator}"), mode, str(args.seconds)],
                                    env=env, text=True, stdout=subprocess.PIPE, stderr=log, timeout=120)
        assert result.returncode == 0, (stem, result.returncode)
        text = stem.with_suffix(".log").read_text()
        assert not any(word in text.lower() for word in ("panicked", "error", "failed", "warn")), text
        value = json.loads(result.stdout)
        assert value["ops"] > 0 and value["checksum"] != 0, value
        assert value["wall_s"] >= args.seconds, value
        value.update(round=args.round, started_epoch=start, log_errors=0)
        stem.with_suffix(".json").write_text(json.dumps(value, indent=2) + "\n")
        print(json.dumps(value), flush=True)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binaries", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--round", type=int, required=True)
    parser.add_argument("--seconds", type=int, default=20)
    parser.add_argument("--otlp", default="http://localhost:14338")
    parser.add_argument("--pyroscope", default="http://localhost:14440")
    run(parser.parse_args())
