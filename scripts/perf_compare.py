#!/usr/bin/env python3
"""Compare two performance result files (JSON lines from examples/perf.rs).

    python scripts/perf_compare.py base.jsonl head.jsonl [--sigma 3] [--min-drop 0.05]
                                   [--summary out.md] [--warn-only]

For every scenario it compares the mean speed of `head` with `base`. A scenario
counts as a regression when head is slower by more than

    max(sigma * standard error of the difference, min_drop * base mean)

i.e. the drop must be both statistically clear (default 3 standard errors) and
big enough to matter (default 5%). Exits with status 1 if any scenario
regressed, unless --warn-only is given. Standard library only.
"""

import argparse
import json
import math
import sys
from collections import defaultdict


def load(path):
    runs = defaultdict(list)
    with open(path, encoding="utf-8") as f:
        for line in f:
            line = line.strip()
            if line.startswith("{"):
                r = json.loads(line)
                runs[r["scenario"]].append(float(r["mbps"]))
    return runs


def stats(xs):
    n = len(xs)
    mean = sum(xs) / n
    var = sum((x - mean) ** 2 for x in xs) / (n - 1) if n > 1 else 0.0
    return n, mean, math.sqrt(var)


def main():
    if hasattr(sys.stdout, "reconfigure"):
        sys.stdout.reconfigure(encoding="utf-8")
    p = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    p.add_argument("base")
    p.add_argument("head")
    p.add_argument("--sigma", type=float, default=3.0)
    p.add_argument("--min-drop", type=float, default=0.05)
    p.add_argument("--summary", help="also write the markdown table to this file")
    p.add_argument("--title", default="Performance: this change vs base")
    p.add_argument("--warn-only", action="store_true")
    a = p.parse_args()

    base, head = load(a.base), load(a.head)
    rows, regressions = [], []
    for name in list(base) + [n for n in head if n not in base]:
        if name not in head:
            rows.append(f"| {name} | {stats(base[name])[1]:.2f} | – | – | removed |")
            continue
        if name not in base:
            rows.append(f"| {name} | – | {stats(head[name])[1]:.2f} | – | new |")
            continue
        nb, mb, sb = stats(base[name])
        nh, mh, sh = stats(head[name])
        se = math.sqrt(sb**2 / nb + sh**2 / nh)
        allowed = max(a.sigma * se, a.min_drop * mb)
        change = (mh - mb) / mb * 100
        if mb - mh > allowed:
            verdict = f"**slower** (more than {allowed / mb * 100:.1f}% allowed)"
            regressions.append(name)
        elif mh - mb > allowed:
            verdict = "faster"
        else:
            verdict = "same"
        rows.append(
            f"| {name} | {mb:.2f} ± {sb:.2f} | {mh:.2f} ± {sh:.2f} | {change:+.1f}% | {verdict} |"
        )

    table = "\n".join(
        [
            f"### {a.title}",
            "",
            "Mean MB/s ± standard deviation over the runs; local test server, not real-world speeds.",
            "",
            "| Scenario | Base | This change | Change | Result |",
            "|---|---:|---:|---:|---|",
            *rows,
            "",
            (
                f"Regression threshold: {a.sigma:g} standard errors and at least {a.min_drop * 100:g}%."
            ),
        ]
    )
    print(table)
    if a.summary:
        with open(a.summary, "a", encoding="utf-8") as f:
            f.write(table + "\n\n")
    if regressions:
        print(f"\nSlower: {', '.join(regressions)}", file=sys.stderr)
        if not a.warn_only:
            sys.exit(1)


if __name__ == "__main__":
    main()
