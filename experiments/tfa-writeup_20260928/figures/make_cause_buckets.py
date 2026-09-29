#!/usr/bin/env python3
"""Build the TFA cause-bucket figure and its data table from ../targets.csv.

Deterministic: no timestamps, no randomness, stdlib only. Run from anywhere:
    python3 experiments/tfa-writeup_20260928/figures/make_cause_buckets.py
Writes cause_buckets.csv and cause_buckets.svg next to this script.

Outcome of each of the 30 registry targets in the kernel-7.1 campaign
(experiments/tfa-kernel71-*_20260921), derived only from targets.csv columns:
  both_seeds      main_k71_outcome starts with "SUCCESS" (a repeatable passing
                  seed and a repeatable failing seed, strict verify).
  loader_bug_fix_pending  attempted, and stopped by the Hermit loader bug (injected
                  fstat turns a successful openat into EFAULT; why_not_both_seeds
                  names EFAULT). Fix approved, not landed as of 2026-09-29:
                  https://github.com/rrnewton/hermit/pull/3374.
  blocked_other   attempted and stopped by anything else (a harder Hermit defect
                  or gap, ptrace speed, or no failing seed found).
  not_attempted   main_k71_attempted == "N".
"""
import csv
import os

HERE = os.path.dirname(os.path.abspath(__file__))
SRC = os.path.join(HERE, "..", "targets.csv")

CATS = [  # key, legend label, light fill, dark fill, label ink light, label ink dark
    ("both_seeds", "Both seeds found", "#2a78d6", "#3987e5", "#ffffff", "#ffffff"),
    ("loader_bug_fix_pending", "Stopped by Hermit loader bug (fix approved, not landed)", "#eb6834", "#d95926", "#0b0b0b", "#ffffff"),
    ("blocked_other", "Stopped otherwise", "#1baf7a", "#199e70", "#0b0b0b", "#ffffff"),
    ("not_attempted", "Not attempted", "#c3c2b7", "#52514e", "#0b0b0b", "#ffffff"),
]
BUCKET_LABELS = {
    "Timer, timeout or wall-clock": "Timer, timeout or wall-clock",
    "Thread-interleaving or async-ordering race": "Thread or async-ordering race",
    "Unknown or harness-only evidence": "Unknown or harness-only",
    "External service, environment or resource": "External service or environment",
    "Kernel feature": "Kernel feature (io_uring)",
}


def outcome(row):
    if row["main_k71_attempted"].strip() != "Y":
        return "not_attempted"
    if row["main_k71_outcome"].startswith("SUCCESS"):
        return "both_seeds"
    if "EFAULT" in row["why_not_both_seeds"]:
        return "loader_bug_fix_pending"
    return "blocked_other"


def load():
    rows = list(csv.DictReader(open(SRC, newline="")))
    assert len(rows) == 30, len(rows)
    counts = {}
    members = {}
    for r in rows:
        b = r["cause_bucket"].split(".")[0].strip()
        b = next(k for k in BUCKET_LABELS if b.startswith(k))
        o = outcome(r)
        counts.setdefault(b, {c[0]: 0 for c in CATS})[o] += 1
        pkg, name = r["canonical_target"].split("/")[-1].split(":")
        members.setdefault((b, o), []).append(f"{pkg}:{name}" if name == "tests" else name)
    order = sorted(counts, key=lambda b: (-sum(counts[b].values()), b))
    return order, counts, members


def write_csv(order, counts, members):
    path = os.path.join(HERE, "cause_buckets.csv")
    with open(path, "w", newline="") as f:
        w = csv.writer(f, lineterminator="\n")
        w.writerow(["cause_bucket", "targets", "attempted"] + [c[0] for c in CATS] + ["targets_by_outcome"])
        for b in order:
            c = counts[b]
            tot = sum(c.values())
            att = tot - c["not_attempted"]
            who = "; ".join(f"{k}: {' '.join(members[(b, k)])}" for k, *_ in CATS if (b, k) in members and k != "not_attempted")
            w.writerow([b, tot, att] + [c[k] for k, *_ in CATS] + [who])
        tc = {k: sum(counts[b][k] for b in order) for k, *_ in CATS}
        w.writerow(["TOTAL", sum(tc.values()), sum(tc.values()) - tc["not_attempted"]] + [tc[k] for k, *_ in CATS] + [""])


def esc(s):
    return s.replace("&", "&amp;").replace("<", "&lt;").replace(">", "&gt;")


def write_svg(order, counts):
    W, left, right = 860, 250, 150
    top, row_h, bar_h = 92, 44, 22
    xmax = 11
    plot_w = W - left - right
    H = top + row_h * len(order) + 46
    sx = lambda v: left + v * plot_w / xmax
    out = []
    a = out.append
    a(f'<svg xmlns="http://www.w3.org/2000/svg" width="{W}" height="{H}" viewBox="0 0 {W} {H}" role="img" aria-labelledby="t d">')
    a('<title id="t">30 flaky test targets by likely flakiness cause, and what Hermit did with each</title>')
    a('<desc id="d">Stacked horizontal bars, one per cause bucket, counting targets whose kernel-7.1 campaign outcome was: both seeds found, stopped by the Hermit loader bug (fix approved in PR 3374, not landed), stopped otherwise, or not attempted. Data: cause_buckets.csv.</desc>')
    css = ["<style>",
           ".bg{fill:#fcfcfb}.ink{fill:#0b0b0b}.ink2{fill:#52514e}.mute{fill:#898781}.grid{stroke:#e1e0d9;stroke-width:1}.base{stroke:#c3c2b7;stroke-width:1}"]
    for i, (k, _, lf, df, li, di) in enumerate(CATS):
        css.append(f".s{i}{{fill:{lf}}}.l{i}{{fill:{li}}}")
    css.append("text{font-family:-apple-system,BlinkMacSystemFont,'Segoe UI',Helvetica,Arial,sans-serif}")
    css.append("@media (prefers-color-scheme: dark){.bg{fill:#1a1a19}.ink{fill:#ffffff}.ink2{fill:#c3c2b7}.grid{stroke:#2c2c2a}.base{stroke:#383835}")
    for i, (k, _, lf, df, li, di) in enumerate(CATS):
        css.append(f".s{i}{{fill:{df}}}.l{i}{{fill:{di}}}")
    css.append("}</style>")
    a("".join(css))
    a(f'<rect class="bg" x="0" y="0" width="{W}" height="{H}"/>')
    a(f'<text class="ink" x="16" y="26" font-size="16" font-weight="600">What Hermit did with 30 flaky test targets, by likely cause of flakiness</text>')
    a(f'<text class="ink2" x="16" y="46" font-size="12">Kernel-7.1 campaign, 2026-09-21; cause is inferred from CI failure text and test source. Counts are targets.</text>')
    # legend, one row
    lx = 16
    for i, (k, lab, *_) in enumerate(CATS):
        a(f'<rect class="s{i}" x="{lx}" y="60" width="12" height="12" rx="2"/>')
        a(f'<text class="ink2" x="{lx + 17}" y="70.5" font-size="12">{esc(lab)}</text>')
        lx += 17 + 6.3 * len(lab) + 26
    # grid + ticks
    y0, y1 = top - 6, top + row_h * len(order) - 10
    for v in range(0, xmax + 1, 2):
        x = sx(v)
        a(f'<line class="{"base" if v == 0 else "grid"}" x1="{x:.1f}" y1="{y0}" x2="{x:.1f}" y2="{y1}"/>')
        a(f'<text class="mute" x="{x:.1f}" y="{y1 + 16}" font-size="11" text-anchor="middle">{v}</text>')
    a(f'<text class="mute" x="{sx(xmax / 2):.1f}" y="{y1 + 32}" font-size="11" text-anchor="middle">Targets</text>')
    for j, b in enumerate(order):
        c = counts[b]
        yc = top + j * row_h + bar_h / 2
        tot = sum(c.values())
        att = tot - c["not_attempted"]
        a(f'<text class="ink" x="{left - 12}" y="{yc + 4:.1f}" font-size="13" text-anchor="end">{esc(BUCKET_LABELS[b])}</text>')
        x = sx(0)
        segs = [(i, k, c[k]) for i, (k, *_) in enumerate(CATS) if c[k] > 0]
        for n, (i, k, v) in enumerate(segs):
            w = v * plot_w / xmax
            last = n == len(segs) - 1
            draw_w = w - (0 if last else 2)  # 2px surface gap between segments
            y = yc - bar_h / 2
            label = f"{BUCKET_LABELS[b]}: {v} {CATS[i][1].lower()}"
            if last:  # 4px rounded data end, square at the baseline side
                r = 4
                d = (f"M{x:.1f},{y:.1f} H{x + draw_w - r:.1f} Q{x + draw_w:.1f},{y:.1f} {x + draw_w:.1f},{y + r:.1f} "
                     f"V{y + bar_h - r:.1f} Q{x + draw_w:.1f},{y + bar_h:.1f} {x + draw_w - r:.1f},{y + bar_h:.1f} H{x:.1f} Z")
                a(f'<path class="s{i}" d="{d}"><title>{esc(label)}</title></path>')
            else:
                a(f'<rect class="s{i}" x="{x:.1f}" y="{y:.1f}" width="{draw_w:.1f}" height="{bar_h}"><title>{esc(label)}</title></rect>')
            if draw_w >= 18:
                a(f'<text class="l{i}" x="{x + draw_w / 2:.1f}" y="{yc + 4.5:.1f}" font-size="12" font-weight="600" text-anchor="middle">{v}</text>')
            x += w
        a(f'<text class="ink2" x="{x + 8:.1f}" y="{yc + 4:.1f}" font-size="12">{att} of {tot} attempted</text>')
    a("</svg>")
    with open(os.path.join(HERE, "cause_buckets.svg"), "w") as f:
        f.write("\n".join(out) + "\n")


if __name__ == "__main__":
    order, counts, members = load()
    write_csv(order, counts, members)
    write_svg(order, counts)
    for b in order:
        print(b, counts[b])
