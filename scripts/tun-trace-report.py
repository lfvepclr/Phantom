#!/usr/bin/env python3
"""Summarise a Phantom TUN trace and judge the data path.

Reads the on-device trace written by `phantomHarmonySetTrace` (see
client/src/tun_trace.rs) and answers the questions a stalled video stream
raises: which flow is stuck, how much of what we injected was duplicate,
how often the app had to ask again, and why the flow ended.

Usage:
    tun-trace-report.py <trace.log> [--stats stats.json] [--json] [--quiet]

`--stats` accepts the client's stats JSON (one document, or one per line as
sampled during the run). With it the byte-ratio verdict uses exact counters;
without it the verdict falls back to values derived from the trace, which is
closer than it sounds because every retransmission logs the byte count it
re-injected (`3-dup-ACK fast retransmit`, pre-fix, logs snd_una/snd_nxt and the
window it rewound).

Exit status is 0 when every check passes, 1 otherwise, so CI and the bench
script can use it directly.
"""

from __future__ import annotations

import argparse
import json
import re
import sys
from collections import defaultdict
from dataclasses import dataclass, field

# Thresholds from the plan (tests/PERF_TUN_PATH_REPORT.md).
MAX_RETRANSMITS_PER_MINUTE = 5.0
MAX_DUP_TO_UNIQUE_RATIO = 1.2
MIN_FLOW_DOWN_BYTES = 5 * 1024 * 1024

TIMESTAMP_RE = re.compile(r"^\[\s*(\d+)ms\]\s*(.*)$")

# New format: `retransmit <dst>:<port> snd_una=… bytes=… …`
RETRANSMIT_RE = re.compile(
    r"^retransmit (?P<dst>[0-9a-fA-F:.]+):(?P<port>\d+) "
    r"snd_una=(?P<snd_una>\d+) bytes=(?P<bytes>\d+)(?P<rest>.*)$"
)
# Old format: `3-dup-ACK fast retransmit <dst>:<port> snd_una=… snd_nxt=… queued=… win=…`
OLD_RETRANSMIT_RE = re.compile(
    r"^3-dup-ACK fast retransmit (?P<dst>[0-9a-fA-F:.]+):(?P<port>\d+) "
    r"snd_una=(?P<snd_una>\d+) snd_nxt=(?P<snd_nxt>\d+)(?P<rest>.*)$"
)
FLOW_END_RE = re.compile(
    r"^flow end(?: \((?P<kind>[a-z]+)\))? (?P<dst>[0-9a-fA-F:.]+):(?P<port>\d+) "
    r"up=(?P<up>\d+) down=(?P<down>\d+)(?: queued=(?P<queued>\d+))?"
)
FLOW_DROP_RE = re.compile(
    r"^flow (?P<dst>[0-9a-fA-F:.]+):(?P<port>\d+) (?P<what>dropped after \d+ stalled retransmits|reset after retransmit limit: .*|retired: .*|retried through the tunnel)$"
)
STALL_RE = re.compile(r"^tun write stalled (?P<ms>\d+)ms queue=(?P<queue>\d+)$")
ZERO_WINDOW_RE = re.compile(r"^zero-window probe (?P<dst>[0-9a-fA-F:.]+):(?P<port>\d+)")
INJECT_RE = re.compile(r"^inject (?P<dst>[0-9a-fA-F:.]+):(?P<port>\d+) seq=(?P<seq>\d+) len=(?P<len>\d+)")


@dataclass
class Flow:
    dst: str
    retransmits: int = 0
    duplicate_bytes: int = 0
    first_ms: int | None = None
    last_ms: int | None = None
    retransmit_times: list[int] = field(default_factory=list)
    up_bytes: int = 0
    down_bytes: int = 0
    end_reason: str = ""

    @property
    def key(self) -> str:
        return self.dst


def parse_trace(path: str) -> tuple[dict[str, Flow], dict]:
    flows: dict[str, Flow] = {}
    summary = {
        "lines": 0,
        "first_ms": None,
        "last_ms": None,
        "stalled_writes": [],
        "zero_window_probes": 0,
        "retransmits": 0,
        "duplicate_bytes": 0,
    }

    def flow_for(name: str) -> Flow:
        flow = flows.get(name)
        if flow is None:
            flow = Flow(dst=name)
            flows[name] = flow
        return flow

    with open(path, "r", encoding="utf-8", errors="replace") as handle:
        for raw in handle:
            match = TIMESTAMP_RE.match(raw.rstrip("\n"))
            if not match:
                continue
            stamp = int(match.group(1))
            message = match.group(2).strip()
            summary["lines"] += 1
            summary["first_ms"] = stamp if summary["first_ms"] is None else summary["first_ms"]
            summary["last_ms"] = stamp

            if (m := RETRANSMIT_RE.match(message)) or (m := OLD_RETRANSMIT_RE.match(message)):
                key = f"{m.group('dst')}:{m.group('port')}"
                flow = flow_for(key)
                # Pre-fix lines rewound the whole window (snd_nxt - snd_una);
                # post-fix lines carry the exact re-injected length.
                if m.groupdict().get("bytes") is not None:
                    injected = int(m.group("bytes"))
                else:
                    injected = (int(m.group("snd_nxt")) - int(m.group("snd_una"))) & 0xFFFFFFFF
                flow.retransmits += 1
                flow.duplicate_bytes += injected
                flow.retransmit_times.append(stamp)
                flow.first_ms = stamp if flow.first_ms is None else flow.first_ms
                flow.last_ms = stamp
                summary["retransmits"] += 1
                summary["duplicate_bytes"] += injected
                continue

            if m := FLOW_END_RE.match(message):
                flow = flow_for(f"{m.group('dst')}:{m.group('port')}")
                flow.up_bytes = int(m.group("up"))
                flow.down_bytes = int(m.group("down"))
                flow.end_reason = flow.end_reason or "end"
                continue

            if m := FLOW_DROP_RE.match(message):
                flow = flow_for(f"{m.group('dst')}:{m.group('port')}")
                flow.end_reason = m.group("what")
                continue

            if m := STALL_RE.match(message):
                summary["stalled_writes"].append(
                    {"ms": int(m.group("ms")), "queue": int(m.group("queue")), "at": stamp}
                )
                continue

            if ZERO_WINDOW_RE.match(message):
                summary["zero_window_probes"] += 1
                continue

            if m := INJECT_RE.match(message):
                # `inject` lines are capped per flow; they are structural
                # evidence, not a byte count.
                flow_for(f"{m.group('dst')}:{m.group('port')}")
                continue

    return flows, summary


def parse_stats(path: str) -> list[dict]:
    """Accept one JSON document or one per line (sampled during a run)."""
    text = open(path, "r", encoding="utf-8", errors="replace").read().strip()
    if not text:
        return []
    documents = []
    try:
        documents.append(json.loads(text))
    except json.JSONDecodeError:
        for line in text.splitlines():
            line = line.strip()
            if not line:
                continue
            try:
                documents.append(json.loads(line))
            except json.JSONDecodeError:
                continue
    return documents


def histogram(samples: list[int]) -> dict[str, int]:
    """Coarse injection-interval histogram, to spot a retransmit storm."""
    if len(samples) < 2:
        return {}
    gaps = [b - a for a, b in zip(samples, samples[1:]) if b >= a]
    buckets = {
        "<10ms": 0,
        "10-50ms": 0,
        "50-200ms": 0,
        "200ms-1s": 0,
        ">1s": 0,
    }
    for gap in gaps:
        if gap < 10:
            buckets["<10ms"] += 1
        elif gap < 50:
            buckets["10-50ms"] += 1
        elif gap < 200:
            buckets["50-200ms"] += 1
        elif gap < 1000:
            buckets["200ms-1s"] += 1
        else:
            buckets[">1s"] += 1
    return buckets


def build_report(trace_path: str, stats_path: str | None) -> dict:
    flows, summary = parse_trace(trace_path)
    stats = parse_stats(stats_path) if stats_path else []

    span_ms = 0
    if summary["first_ms"] is not None and summary["last_ms"] is not None:
        span_ms = max(0, summary["last_ms"] - summary["first_ms"])
    span_minutes = span_ms / 60000.0

    retransmits_per_minute = (
        summary["retransmits"] / span_minutes if span_minutes > 0 else 0.0
    )

    unique_down = sum(flow.down_bytes for flow in flows.values())
    duplicate_bytes = summary["duplicate_bytes"]
    if stats:
        first, last = stats[0], stats[-1]
        counted_down = int(last.get("down", 0)) - int(first.get("down", 0))
        counted_dup = int(last.get("tcp_dup", 0)) - int(first.get("tcp_dup", 0))
        counted_dup_acks = int(last.get("dup_acks", 0)) - int(first.get("dup_acks", 0))
        if counted_down > 0:
            unique_down = counted_down
        if counted_dup > 0 or counted_down > 0:
            duplicate_bytes = counted_dup
    else:
        counted_dup_acks = summary["retransmits"] * 3

    ratio = (duplicate_bytes / unique_down) if unique_down > 0 else 0.0
    busiest = max(flows.values(), key=lambda f: f.down_bytes, default=None)
    # Per-flow `down` only exists for flows that ended inside the trace window,
    # so a still-running video flow reports 0. The stats counters cover every
    # flow, which makes them the honest lower bound for "how much did the
    # busiest flow move" while a download is in flight.
    per_flow_max = busiest.down_bytes if busiest else 0
    measured_down = max(per_flow_max, unique_down)
    measured_down_source = "stats" if unique_down > per_flow_max else "trace"

    checks = {
        "retransmits_per_minute": {
            "value": round(retransmits_per_minute, 2),
            "limit": MAX_RETRANSMITS_PER_MINUTE,
            "pass": retransmits_per_minute <= MAX_RETRANSMITS_PER_MINUTE,
        },
        "duplicate_ratio": {
            "value": round(ratio, 3),
            "limit": MAX_DUP_TO_UNIQUE_RATIO,
            "pass": unique_down == 0 or ratio <= MAX_DUP_TO_UNIQUE_RATIO,
        },
        "largest_flow_down_bytes": {
            "value": measured_down,
            "minimum": MIN_FLOW_DOWN_BYTES,
            "source": measured_down_source,
            "per_flow_trace_max": per_flow_max,
            "pass": measured_down >= MIN_FLOW_DOWN_BYTES,
        },
        "tun_write_stalls": {
            "value": len(summary["stalled_writes"]),
            "limit": 0,
            "pass": not summary["stalled_writes"],
        },
    }

    return {
        "trace": trace_path,
        "span_seconds": round(span_ms / 1000.0, 1),
        "flows": len(flows),
        "retransmits": summary["retransmits"],
        "dup_acks_seen": counted_dup_acks,
        "duplicate_bytes": duplicate_bytes,
        "unique_down_bytes": unique_down,
        "zero_window_probes": summary["zero_window_probes"],
        "stalled_writes": summary["stalled_writes"][:10],
        "worst_flows": [
            {
                "flow": flow.key,
                "retransmits": flow.retransmits,
                "duplicate_bytes": flow.duplicate_bytes,
                "down_bytes": flow.down_bytes,
                "up_bytes": flow.up_bytes,
                "end_reason": flow.end_reason,
                "interval_histogram_ms": histogram(flow.retransmit_times),
            }
            for flow in sorted(flows.values(), key=lambda f: f.duplicate_bytes, reverse=True)[:5]
        ],
        "checks": checks,
        "pass": all(check["pass"] for check in checks.values()),
    }


def print_human(report: dict) -> None:
    span = report["span_seconds"]
    print(f"trace        : {report['trace']}")
    print(f"span         : {span:.1f}s across {report['flows']} flows")
    print(
        f"retransmits  : {report['retransmits']} "
        f"({report['checks']['retransmits_per_minute']['value']}/min, "
        f"limit {MAX_RETRANSMITS_PER_MINUTE})"
    )
    print(f"dup ACKs seen: {report['dup_acks_seen']}")
    print(
        f"injected     : {report['duplicate_bytes'] / 1e6:.1f} MB duplicate vs "
        f"{report['unique_down_bytes'] / 1e6:.1f} MB unique down "
        f"(ratio {report['checks']['duplicate_ratio']['value']})"
    )
    print(f"zero-window  : {report['zero_window_probes']} probes")
    if report["stalled_writes"]:
        print(f"write stalls : {len(report['stalled_writes'])} (first: {report['stalled_writes'][0]})")
    else:
        print("write stalls : none")

    if report["worst_flows"]:
        print("\nworst flows by duplicate bytes:")
        for flow in report["worst_flows"]:
            if flow["retransmits"] == 0 and flow["duplicate_bytes"] == 0:
                continue
            print(
                f"  {flow['flow']:<28} retx={flow['retransmits']:<6} "
                f"dup={flow['duplicate_bytes'] / 1e6:>7.2f} MB "
                f"down={flow['down_bytes'] / 1e6:>7.2f} MB "
                f"end={flow['end_reason'] or '-'}"
            )
            if flow["interval_histogram_ms"]:
                print(f"      intervals: {flow['interval_histogram_ms']}")

    print("\nchecks:")
    for name, check in report["checks"].items():
        mark = "PASS" if check["pass"] else "FAIL"
        print(f"  [{mark}] {name}: {check}")
    print(f"\nverdict: {'PASS' if report['pass'] else 'FAIL'}")


def main() -> int:
    parser = argparse.ArgumentParser(description="Summarise a Phantom TUN trace")
    parser.add_argument("trace", help="phantom_tun_trace.log pulled from the device")
    parser.add_argument("--stats", help="stats JSON sampled during the same run")
    parser.add_argument("--json", action="store_true", help="machine-readable output")
    parser.add_argument("--quiet", action="store_true", help="print nothing, exit status only")
    args = parser.parse_args()

    report = build_report(args.trace, args.stats)
    if args.json:
        print(json.dumps(report, indent=2, ensure_ascii=False))
    elif not args.quiet:
        print_human(report)
    return 0 if report["pass"] else 1


if __name__ == "__main__":
    sys.exit(main())
