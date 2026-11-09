#!/usr/bin/env python3
"""Read profiles back from a receiver after running examples/demo."""
import argparse
import json
import time
import urllib.request
from pathlib import Path

KINDS = {"cpu", "samples", "inuse_space", "inuse_objects", "alloc_space", "alloc_objects"}


def post(url, body, content_type="application/json"):
    data = body.encode() if isinstance(body, str) else json.dumps(body).encode()
    request = urllib.request.Request(url, data, {"Content-Type": content_type})
    with urllib.request.urlopen(request, timeout=30) as response:
        return json.load(response)


def collector(path):
    kinds, linked, frames = set(), 0, set()
    for line in Path(path).read_text().splitlines():
        request = json.loads(line)
        dictionary = request["dictionary"]
        strings = dictionary["stringTable"]
        for function in dictionary.get("functionTable", []):
            frames.add(strings[function.get("nameStrindex", 0)])
        for resource in request["resourceProfiles"]:
            for scope in resource["scopeProfiles"]:
                for profile in scope["profiles"]:
                    kinds.add(strings[profile["sampleType"]["typeStrindex"]])
                    linked += sum(bool(sample.get("linkIndex")) for sample in profile["samples"])
    assert kinds == KINDS, kinds
    assert linked > 0, "no span-linked samples"
    assert any("price_cart" in frame for frame in frames), "missing demo CPU frames"
    assert any("render_receipt" in frame for frame in frames), "missing demo heap frames"
    return {"receiver": "collector", "kinds": sorted(kinds), "linked_samples": linked}


def sigiro(base, service):
    def query(sql):
        return post(base + "/v1/query", sql, "text/plain")
    # This is a literal in SQL, not an identifier.
    service = service.replace("'", "''")
    rows = query("SELECT profile_type, count(*) AS rows, sum(value) AS total, "
                 "sum(CASE WHEN span_id != '' THEN 1 ELSE 0 END) AS linked "
                 f"FROM sigiro_profiles WHERE service_name = '{service}' GROUP BY 1")
    assert {row["profile_type"] for row in rows} == KINDS, rows
    assert all(row["total"] > 0 for row in rows), rows
    matched = query("SELECT count(*) AS n FROM sigiro_profiles p JOIN sigiro_spans s "
                    "ON p.trace_id = s.trace_id AND p.span_id = s.span_id "
                    f"WHERE p.service_name = '{service}'")[0]["n"]
    assert matched > 0, "sample links do not resolve to exported spans"
    return {"receiver": "sigiro", "profiles": rows, "samples_with_exported_span": matched}


def pyroscope(base, service):
    now = int(time.time() * 1000)
    window = {"start": now - 3600000, "end": now + 60000}
    types = post(base + "/querier.v1.QuerierService/ProfileTypes", window).get("profileTypes", [])
    results = []
    for kind in sorted(KINDS):
        name = "process_cpu" if kind in {"cpu", "samples"} else "space"
        candidates = [t for t in types if t.get("sampleType") == kind and t.get("name") == name]
        assert candidates, (kind, types)
        profile = candidates[0]
        body = dict(window, profileTypeID=profile["ID"],
                    labelSelector='{service_name=' + json.dumps(service) + '}', format=1)
        graph = post(base + "/querier.v1.QuerierService/SelectMergeStacktraces", body).get("flamegraph", {})
        if kind == "samples" and not int(graph.get("total", 0)):
            results.append({"kind": kind, "limitation": "type listed, no count flamegraph returned"})
            continue
        assert int(graph.get("total", 0)) > 0, (kind, graph)
        expected = "price_cart" if kind in {"cpu", "samples"} else "render_receipt"
        assert any(expected in frame for frame in graph.get("names", [])), (kind, graph.get("names"))
        results.append({"kind": kind, "total": graph["total"], "demo_frame": expected})
    return {"receiver": "pyroscope", "profiles": results}


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("receiver", choices=["collector", "sigiro", "pyroscope"])
    parser.add_argument("source", help="collector JSON file, or receiver base URL")
    parser.add_argument("--service", default="checkout")
    args = parser.parse_args()
    result = collector(args.source) if args.receiver == "collector" else globals()[args.receiver](args.source.rstrip("/"), args.service)
    print(json.dumps(result, indent=2))
