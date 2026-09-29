#!/usr/bin/env python3
"""Replace internal host names (FQDNs) in the TFA write-up with their first DNS label.

Policy (hermit .llms/skills/benchmark/SKILL.md, "Metadata And Privacy"): persist only
the first DNS label of an internal host name. Applied 2026-09-29 to every tracked file
under experiments/tfa-writeup_20260928/:

  <machine>.<site>.<internal domain>   -> <machine>   (for example devbig030)
  www.<internal domain>               -> www         (so an internal URL keeps its path)
  <short-link domain>                 -> its first label
  <task>.<tier>.<site>.<internal domain> -> <task>

SUFFIXES below lists domain suffixes (not host names); the check skips this file and
REDACTIONS.json, which also list them.

In the top-level Markdown documents (not evidence copies), a Markdown link whose
target is an internal host is first replaced by its link text, so
`[D121244476](https://<internal host>/diff/D121244476)` becomes `D121244476`.

Usage: redact_internal_fqdns.py ROOT [--check]
Writes ROOT/REDACTIONS.json (per-file counts) unless --check, which only reports
remaining hits and exits 1 if any remain.
"""
import json
import os
import re
import sys

SUFFIXES = [
    "facebook.com", "fb.com", "thefacebook.com", "fbinfra.net", "facebook.net",
    "internalfb.com", "internalmeta.com", "workplace.com", "fburl.com", "tfbnw.net", "fbsv.net",
]
SUF = "|".join(re.escape(s) for s in sorted(SUFFIXES, key=len, reverse=True))
# label(.label)*.suffix, or a bare suffix; not preceded/followed by host characters
FQDN = re.compile(r"(?<![A-Za-z0-9.-])((?:[A-Za-z0-9-]+\.)*?)(" + SUF + r")(?![A-Za-z0-9-])", re.IGNORECASE)
MDLINK = re.compile(r"\[([^\]]+)\]\(https?://(?:[A-Za-z0-9-]+\.)*(?:" + SUF + r")(?:/[^)]*)?\)", re.IGNORECASE)


def first_label(m):
    labels = m.group(1)
    return labels.split(".")[0] if labels else m.group(2).split(".")[0]


def process(path, top_level_md):
    data = open(path, "rb").read()
    try:
        text = data.decode("utf-8")
    except UnicodeDecodeError:
        text = data.decode("latin-1")
    n_links = 0
    if top_level_md:
        text, n_links = MDLINK.subn(lambda m: m.group(1), text)
    new, n = FQDN.subn(first_label, text)
    return new, n_links, n, data


def main():
    root = sys.argv[1]
    check = "--check" in sys.argv
    counts = {}
    hits = 0
    for dp, dn, fn in os.walk(root):
        for f in sorted(fn):
            p = os.path.join(dp, f)
            rel = os.path.relpath(p, root)
            if rel == "REDACTIONS.json" or rel.startswith("tools/"):
                continue
            top = rel.endswith(".md") and "/" not in rel
            new, nl, n, old = process(p, top)
            if nl or n:
                if check:
                    hits += nl + n
                    print(f"HIT {rel}: {nl} links, {n} host names")
                    continue
                enc = "utf-8"
                try:
                    old.decode("utf-8")
                except UnicodeDecodeError:
                    enc = "latin-1"
                open(p, "wb").write(new.encode(enc))
                counts[rel] = {"markdown_links_unlinked": nl, "host_names": n}
    if check:
        print(f"remaining internal host names: {hits}")
        sys.exit(1 if hits else 0)
    json.dump({"policy": "internal FQDN -> first DNS label", "suffixes": SUFFIXES, "files": counts},
              open(os.path.join(root, "REDACTIONS.json"), "w"), indent=1, sort_keys=True)
    print(f"files changed: {len(counts)}; host names: {sum(c['host_names'] for c in counts.values())}; "
          f"links unlinked: {sum(c['markdown_links_unlinked'] for c in counts.values())}")


if __name__ == "__main__":
    main()
