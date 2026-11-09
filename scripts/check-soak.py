#!/usr/bin/env python3
"""Require receiver-side evidence for every profiled soak run."""
import argparse
import json
import time
from pathlib import Path
from urllib.request import Request, urlopen


def post(url, body, content_type="application/json"):
    data = body.encode() if isinstance(body, str) else json.dumps(body).encode()
    with urlopen(Request(url, data, {"Content-Type": content_type}), timeout=30) as response:
        return json.load(response)


def check(args):
    output = []
    for path in sorted(args.results.glob("r*.json")):
        result = json.loads(path.read_text())
        mode, service = result["mode"], result["service"]
        if mode == "off":
            continue
        if mode.startswith("otel-"):
            rows = post(args.otlp + "/v1/query", "SELECT profile_type, sum(value) AS total FROM sigiro_profiles "
                        f"WHERE service_name='{service}' GROUP BY 1", "text/plain")
            values = {r["profile_type"]: r["total"] for r in rows}
            kinds = {"cpu", "samples"}
            if mode == "otel-all":
                kinds |= {"inuse_space", "inuse_objects", "alloc_space", "alloc_objects"}
            assert kinds <= values.keys() and all(values[k] > 0 for k in kinds), (path, values)
            frames = post(args.otlp + "/v1/query", "SELECT count(*) AS n FROM (SELECT unnest(stack_trace) AS frame "
                          f"FROM sigiro_profiles WHERE service_name='{service}') t WHERE frame LIKE '%checkout_work%'", "text/plain")
            assert frames[0]["n"] > 0, (path, "missing workload frames")
            output.append({"run": path.name, "service": service, "totals": values, "workload_frames": frames[0]["n"]})
        else:
            window = {"start": int(result["started_epoch"] * 1000) - 60000,
                      "end": int(time.time() * 1000) + 60000}
            types = ["process_cpu:cpu:nanoseconds:cpu:nanoseconds"]
            if mode == "pyroscope-all":
                types.append("memory:inuse_space:bytes:space:bytes")
            totals = {}
            for kind in types:
                graph = post(args.pyroscope + "/querier.v1.QuerierService/SelectMergeStacktraces",
                             dict(window, profileTypeID=kind, labelSelector='{service_name=' + json.dumps(service) + '}', format=1)).get("flamegraph", {})
                assert int(graph.get("total", 0)) > 0, (path, kind, graph)
                assert any("checkout_work" in frame for frame in graph.get("names", [])), (path, kind, graph.get("names"))
                totals[kind] = int(graph["total"])
            output.append({"run": path.name, "service": service, "totals": totals})
    print(json.dumps(output, indent=2))


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("results", type=Path)
    parser.add_argument("--otlp", default="http://localhost:19998", help="sigiro query base URL")
    parser.add_argument("--pyroscope", default="http://localhost:14440")
    check(parser.parse_args())
