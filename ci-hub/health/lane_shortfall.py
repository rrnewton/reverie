#!/usr/bin/env python3
"""Which open heads are ONE CODEX LANE SHORT -- computed from MARKERS, not labels.

WHY THIS EXISTS. The obvious implementation reads `approval_binding`'s rows[].
That row builder starts from `passed-review-*` LABELS, so a lane carrying no label
produces NO ROW AT ALL and its markers are invisible. Measured 2026-08-25, that
defect fired in BOTH directions from the same cause:
  * false work      -- hermit#2686 advertised as "needs codex" while a LIVE codex
                       refusal named its exact head;
  * false emptiness -- the dispatcher reporting NO codex-short heads while several
                       were dispatchable.
A LABEL IS A SUMMARY OF A VERDICT; THE MARKER IS THE VERDICT.

⚠️ AND A RAW markers() SCAN IS ALSO WRONG: markers() alone counts a QUOTED marker as
a verdict, because citation suppression lives above it. So this uses the two
citation-aware, label-independent instruments:
    positive -> approval_binding.lane_shas(comments, lane, ...)
    negative -> changes-requested-gate's evaluate(bodies, head, ids)
and it reads BOTH comment surfaces: issues/N/comments AND pulls/N/comments.

THE CHANGE AUTHOR MUST NOT SUPPLY EITHER REVIEW LANE. GitHub's account identity
cannot establish that here because agents share one account. The disclosure tag
in the pull-request body names the change author, and ``lane_shas`` returns each
marker's recorded issuer, falling back to its disclosure tag. A current-head
marker counts only when both identities are known and differ.
"""
import json, subprocess, sys, time, importlib.util, importlib.machinery, pathlib

ROOT = pathlib.Path(__file__).resolve().parent.parent.parent  # repo root
def _load(name, rel):
    ld = importlib.machinery.SourceFileLoader(name, str(ROOT / rel))
    m = importlib.util.module_from_spec(importlib.util.spec_from_loader(name, ld))
    ld.exec_module(m); return m
ab  = _load("ab",  "ci-hub/health/approval_binding.py")
crg = _load("crg", "ci-hub/bin/changes-requested-gate")

def gh(args):
    p = subprocess.run(["with-proxy", str(ROOT / "ci-hub/bin/gh")] + args,
                       capture_output=True, text=True)
    if p.returncode != 0:
        raise RuntimeError(p.stderr.strip()[:160] or f"gh exited {p.returncode}")
    return json.loads(p.stdout)

def lane_status(comments, lane, head, change_author):
    """Return one lane's state, excluding markers issued by the change author."""
    refs = []
    issuers = []
    shas = ab.lane_shas(
        comments,
        lane,
        malformed=[],
        citations=[],
        refusals=refs,
        issuers=issuers,
    ) or []
    if len(shas) != len(issuers):
        raise RuntimeError(
            f"{lane} marker and issuer counts differ: {len(shas)} != {len(issuers)}"
        )

    current_issuers = []
    for sha, (issuer_sha, issuer) in zip(shas, issuers):
        if sha != issuer_sha:
            raise RuntimeError(
                f"{lane} marker and issuer SHAs differ: {sha} != {issuer_sha}"
            )
        if sha == head:
            current_issuers.append(issuer)

    independent = [
        issuer
        for issuer in current_issuers
        if change_author is not None
        and issuer is not None
        and issuer != change_author
    ]
    return {
        "at_head": bool(independent),
        "refusals_seen": len(refs),
        "issuers_at_head": sorted({issuer for issuer in current_issuers if issuer}),
        "self_authored_at_head": sum(
            issuer is not None and issuer == change_author
            for issuer in current_issuers
        ),
        "unattributed_at_head": sum(
            issuer is None or change_author is None
            for issuer in current_issuers
        ),
    }

def scan_result(pr, d, ic, rc):
    """Evaluate one pull request from its API response and two comment streams."""
    head, state, draft = d["head"]["sha"], d["state"], d["draft"]
    comments = [
        {
            "id": c["id"],
            "body": c.get("body") or "",
            "html_url": c.get("html_url") or c.get("url") or "?",
        }
        for c in ic + rc
    ]
    if d.get("body"):
        comments.insert(0, {"id": "body", "body": d["body"],
                            "html_url": d.get("html_url") or "?"})
    bodies = [c["body"] for c in comments]
    ids = [c["id"] for c in comments]
    change_author = ab.comment_author(d.get("body") or "")
    lanes = {
        lane: lane_status(comments, lane, head, change_author)
        for lane in ("claude", "codex")
    }
    gate_rc, gate_msg = crg.evaluate(bodies, head, ids)
    return {"pr": pr, "head": head, "state": state, "draft": draft,
            "gate_rc": gate_rc, "gate": gate_msg.split("--")[0].strip(),
            "mergeable": d.get("mergeable"), "change_author": change_author,
            "lanes": lanes}

def scan(repo, pr, pace=2.0):
    d = gh(["api", f"repos/{repo}/pulls/{pr}"])
    ic = gh(["api", f"repos/{repo}/issues/{pr}/comments?per_page=100"])
    rc = gh(["api", f"repos/{repo}/pulls/{pr}/comments?per_page=100"])
    result = scan_result(pr, d, ic, rc)
    time.sleep(pace)
    return result

def classify(r):
    """⚠️ ORDER-INDEPENDENT ON PURPOSE. The first version tested codex before claude
    and had no claude-short bucket at all, so a head with codex ALREADY BOUND and no
    claude lane was filed as `codex-already-bound` -- hiding the one head that was
    actually dispatchable. That is the same shape as the label-vs-marker defect this
    file exists to fix: a classifier that cannot represent a state reports the
    neighbouring one. Both lanes are now named in every verdict."""
    if r["state"] != "open" or r["draft"]:  return "not-open"
    if r["gate_rc"] == 2:                    return "INDETERMINATE"
    if r["gate_rc"] != 0:                    return "live-refusal"
    c, x = r["lanes"]["claude"], r["lanes"]["codex"]
    have = [n for n, v in (("claude", c), ("codex", x)) if v["at_head"]]
    if len(have) == 2:                       return "BOTH-LANES-BOUND"
    if not have:                             return "ZERO-LANES (needs both)"
    missing = "codex" if have == ["claude"] else "claude"
    if r["mergeable"] is False:              return f"{missing.upper()}-SHORT-DIRTY"
    if r["lanes"][missing]["refusals_seen"]: return f"{missing.upper()}-SHORT-WITHDRAWAL-FIRST"
    return f"{missing.upper()}-SHORT-DISPATCHABLE"


def self_test() -> int:
    head = "6457ba0acfb4eda22cf5fd617412c6b734b92801"

    def C(agent, lane="codex"):
        return {
            "body": (
                f"[hermit2, {agent}, unresolved, devbig014, role=reviewer]\n"
                f"APPROVED-AT: {lane} {head}"
            ),
            "id": agent,
            "html_url": f"c/{agent}",
        }

    def P(agent):
        return {
            "body": f"[hermit2, {agent}, unresolved, devbig014, role=impl]",
            "draft": False,
            "head": {"sha": head},
            "html_url": "p/1",
            "mergeable": True,
            "state": "open",
        }

    def R(gate=0, claude=False, codex=False, cref=0, xref=0, merge=True,
          state="open", draft=False):
        return {"state": state, "draft": draft, "gate_rc": gate, "mergeable": merge,
                "lanes": {"claude": {"at_head": claude, "refusals_seen": cref},
                          "codex":  {"at_head": codex,  "refusals_seen": xref}}}
    bad = []
    def want(label, r, expect):
        got = classify(r)
        if got != expect:
            bad.append(f"{label}: expected {expect!r}, got {got!r}")

    author_result = scan_result(1, P("change-author"), [C("change-author")], [])
    author_marker = author_result["lanes"]["codex"]
    if author_marker["at_head"] or author_marker["self_authored_at_head"] != 1:
        bad.append(f"change author's marker satisfied the lane: {author_marker!r}")

    unattributed_marker = scan_result(
        1,
        P("change-author"),
        [{"body": f"APPROVED-AT: codex {head}", "id": "unknown"}],
        [],
    )["lanes"]["codex"]
    if (
        unattributed_marker["at_head"]
        or unattributed_marker["unattributed_at_head"] != 1
    ):
        bad.append(f"unattributed marker satisfied the lane: {unattributed_marker!r}")

    independent_result = scan_result(
        1,
        P("change-author"),
        [C("change-author"), C("independent-reviewer")],
        [],
    )
    independent_marker = independent_result["lanes"]["codex"]
    if not independent_marker["at_head"]:
        bad.append(
            "independent reviewer's marker did not satisfy the lane: "
            f"{independent_marker!r}"
        )

    # ⚠️ THE REGRESSION THIS FILE WAS WRITTEN AFTER. The first classifier tested
    # codex before claude and had NO claude-short bucket, so a head with codex
    # already bound and no claude lane came back "codex-already-bound" -- the one
    # dispatchable head in the queue, filed as needing nothing.
    want("codex bound, claude missing", R(codex=True), "CLAUDE-SHORT-DISPATCHABLE")
    want("claude bound, codex missing", R(claude=True), "CODEX-SHORT-DISPATCHABLE")
    want("both bound", R(claude=True, codex=True), "BOTH-LANES-BOUND")
    want("neither bound", R(), "ZERO-LANES (needs both)")
    # A missing lane that already carries a refusal needs a WITHDRAWAL, not a review.
    want("codex missing but refused before",
         R(claude=True, xref=2), "CODEX-SHORT-WITHDRAWAL-FIRST")
    want("claude missing but refused before",
         R(codex=True, cref=1), "CLAUDE-SHORT-WITHDRAWAL-FIRST")
    # A conflicting head cannot absorb a lane yet.
    want("codex missing, tree dirty", R(claude=True, merge=False), "CODEX-SHORT-DIRTY")
    # ⚠️ THE THIRD STATE MUST SURVIVE. rc=2 is not CLEAR and not a refusal.
    want("gate indeterminate", R(gate=2, claude=True), "INDETERMINATE")
    want("gate blocked", R(gate=1, claude=True), "live-refusal")
    want("closed", R(state="closed", claude=True), "not-open")
    want("draft", R(draft=True, claude=True), "not-open")

    if bad:
        for b in bad:
            print(f"lane_shortfall self-test: FAIL -- {b}")
        return 1
    print("PASS: lane_shortfall rejects markers from the change author, accepts "
          "markers from another author, names the MISSING lane in both directions, "
          "keeps "
          "INDETERMINATE distinct from CLEAR, and routes a previously-refused lane "
          "to a withdrawal rather than a review")
    return 0


if __name__ == "__main__":
    if "--self-test" in sys.argv[1:]:
        sys.exit(self_test())
    repo = sys.argv[1] if len(sys.argv) > 1 else "rrnewton/hermit"
    prs = [int(a) for a in sys.argv[2:]]
    if not prs:
        prs = sorted(p["number"] for p in gh(["api", f"repos/{repo}/pulls?state=open&per_page=100"])
                     if not p["draft"])
    out = []
    for pr in prs:
        try:
            r = scan(repo, pr); r["class"] = classify(r); out.append(r)
        except Exception as e:
            out.append({"pr": pr, "class": "INDETERMINATE", "gate": f"scan failed: {e}"})
        r = out[-1]
        print(f"  #{r['pr']:<6}{(r.get('head') or '')[:12]:<14}{r['class']:<30}{r.get('gate','')[:44]}")
    buckets = {}
    for r in out: buckets.setdefault(r["class"], []).append(r["pr"])
    print("\nSUMMARY")
    for k in sorted(buckets): print(f"  {k:<30} {len(buckets[k]):>2}  {buckets[k]}")
    json.dump(out, open("/tmp/codex-short.json", "w"), indent=1)
