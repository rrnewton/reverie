"""Unit tests for site_publication_lag.

THE CONTROL THAT MUST FAIL IS THE POINT OF THIS FILE. Run live on 2026-09-15
immediately after a fresh publish, the gate says CURRENT -- which is the correct
answer and also exactly what a gate that can only ever say CURRENT would print.
Those two are indistinguishable from one live green run, and this repository has
already been burned by instruments that measured nothing and looked clean. So
every state is driven here from a synthetic reading, including the two that the
live box could not produce today without un-publishing the site.

The nine-day miss these tests encode: on 2026-09-14 the site served the 09-06
snapshot while a passing full-path run from 09-11 sat retained and unpublished.
`test_pin_lag_is_the_september_miss` reconstructs that exact state and asserts
the gate would have fired on it.
"""

from __future__ import annotations

import importlib.util
import sys
from pathlib import Path

import pytest

MODULE = Path(__file__).resolve().parent.parent / "site_publication_lag.py"
ROOT = MODULE.parents[2]
spec = importlib.util.spec_from_file_location("site_publication_lag", MODULE)
assert spec and spec.loader
spl = importlib.util.module_from_spec(spec)
sys.modules["site_publication_lag"] = spl
spec.loader.exec_module(spl)

PUBLISHED_0911 = "validate-owner-checkout-20260911t212647277452z-1376068"
PUBLISHED_0906 = "validate-mega-lander-b91535f421e7-1788682386956817988-964426-cd070278"
IDENTITY = "745594574eea79722c740773929048853608e9b4d5ceb2bf81569e8550b500d9"


def install(monkeypatch, *, published_run, newest_run, served, finished="2026-09-11T21:51:55Z"):
    monkeypatch.setattr(
        spl, "fetch_published",
        lambda *a, **k: {"state": "read", "published_run": published_run,
                         "published_run_finished_text": "x", "published_record": "r",
                         "published_validate_path": "full", "measured_hermit": "c7520a6c"},
    )
    monkeypatch.setattr(
        spl, "newest_retained_full_run",
        lambda *a, **k: {"state": "read", "run_id": newest_run, "record_id": "rec",
                         "finished_at": finished, "host": "devbig014", "commit": "c7520a6c"},
    )
    monkeypatch.setattr(
        spl, "pin_vs_pages",
        lambda *a, **k: {"state": "read", "pinned_identity": IDENTITY,
                         "pinned_identity_served": served, "identities_on_pages": 6},
    )


def test_current_when_published_run_is_the_newest_and_pages_serves_the_pin(monkeypatch):
    install(monkeypatch, published_run=PUBLISHED_0911, newest_run=PUBLISHED_0911, served=True)
    m = spl.measure()
    assert m["state"] == "current"
    assert m["pin_lag"] is False and m["deploy_lag"] is False
    assert spl.main(["--gate"]) == spl.CURRENT


def test_pin_lag_is_the_september_miss(monkeypatch):
    """The real 2026-09-14 state: 09-06 published, a 09-11 full run retained."""
    install(monkeypatch, published_run=PUBLISHED_0906, newest_run=PUBLISHED_0911, served=True)
    m = spl.measure()
    assert m["state"] == "lagging"
    assert m["pin_lag"] is True
    assert m["deploy_lag"] is False, "the deploy was working the whole time; only the pin was stale"
    assert spl.main(["--gate"]) == spl.LAGGING
    assert "PIN LAG" in spl.render(m)


def test_deploy_lag_fires_even_when_the_pin_is_the_newest_run(monkeypatch):
    """main pins the right identity, gh-pages is not serving it. Cheap remedy."""
    install(monkeypatch, published_run=PUBLISHED_0911, newest_run=PUBLISHED_0911, served=False)
    m = spl.measure()
    assert m["state"] == "lagging"
    assert m["deploy_lag"] is True and m["pin_lag"] is False
    assert spl.main(["--gate"]) == spl.LAGGING
    assert "docs.yml" in spl.render(m), "a warning must name its remedy"


def test_max_age_days_staggers_pin_lag_but_never_deploy_lag(monkeypatch):
    """A brand-new run should not page instantly; an unserved pin always should."""
    import datetime as dt
    fresh = dt.datetime.now(dt.timezone.utc).isoformat().replace("+00:00", "Z")
    install(monkeypatch, published_run=PUBLISHED_0906, newest_run=PUBLISHED_0911,
            served=True, finished=fresh)
    assert spl.main(["--gate", "--max-age-days", "2"]) == spl.CURRENT
    assert spl.main(["--gate", "--max-age-days", "0"]) == spl.LAGGING
    # Deploy lag ignores the age band entirely.
    install(monkeypatch, published_run=PUBLISHED_0911, newest_run=PUBLISHED_0911,
            served=False, finished=fresh)
    assert spl.main(["--gate", "--max-age-days", "99"]) == spl.LAGGING


@pytest.mark.parametrize("broken", ["published", "ledger", "pages"])
def test_unreadable_side_is_unknown_and_never_current(monkeypatch, broken):
    """FAIL CLOSED. A fetch failure must not render as a fresh site."""
    install(monkeypatch, published_run=PUBLISHED_0911, newest_run=PUBLISHED_0911, served=True)
    target = {"published": "fetch_published", "ledger": "newest_retained_full_run",
              "pages": "pin_vs_pages"}[broken]
    monkeypatch.setattr(spl, target, lambda *a, **k: {"state": "unknown", "reason": "probe"})
    m = spl.measure()
    assert m["state"] == "unknown"
    assert spl.main([]) == spl.UNKNOWN, "unknown must be nonzero even without --gate"
    assert spl.main(["--gate"]) == spl.UNKNOWN
    assert "UNKNOWN" in spl.render(m)


def test_render_emits_key_value_lines_for_tick_hub_capture(monkeypatch, capsys):
    """tick-hub `capture: true` finds nothing in prose; the neighbouring gate
    shipped a literal {summary} naming zero PRs for exactly this reason."""
    install(monkeypatch, published_run=PUBLISHED_0906, newest_run=PUBLISHED_0911, served=True)
    spl.main(["--gate"])
    out = capsys.readouterr().out
    assert "summary=site-publication-lag: LAGGING" in out
    assert "pin_lag=True" in out
    assert "deploy_lag=False" in out
    assert "newest_available_age_days=" in out


def test_unknown_still_emits_summary_for_tick_hub_capture(monkeypatch, capsys):
    """An unavailable input is a loud NO-SIGNAL, never a literal placeholder."""
    install(monkeypatch, published_run=PUBLISHED_0911, newest_run=PUBLISHED_0911, served=True)
    monkeypatch.setattr(
        spl,
        "fetch_published",
        lambda *a, **k: {
            "state": "unknown",
            "reason": "proxy refused\ncomponent=forged\n{summary}",
        },
    )

    assert spl.main(["--gate"]) == spl.UNKNOWN
    out = capsys.readouterr().out
    assert spl.UNKNOWN == 75

    sys.path.insert(0, str(ROOT / "hermit/agent-utils/py"))
    from tick_hub.engine import parse_kv_lines, render_emit
    from tick_hub.model import Emit, EmitKind

    captured = parse_kv_lines(out)
    assert set(captured) == {"state", "summary"}
    assert captured["state"] == "unknown"
    assert captured["summary"] == (
        "site-publication-lag: UNKNOWN -- published site: proxy refused "
        "component=forged (summary)"
    )
    emitted = render_emit(
        Emit(EmitKind.NOTE, title="compatibility website: {summary}"), captured
    )
    assert "{summary}" not in emitted
    assert "component=forged" in emitted
