#!/usr/bin/env python3
"""Startup Plugin — Idea validation & strategy for indie founders.

Protocol: Bidirectional JSON-RPC 2.0 over stdin/stdout (newline-delimited).
All logging goes to stderr to avoid corrupting the JSON-RPC stream on stdout.

Data: SurrealDB embedded at ~/.aman/startup/data/ (surrealkv://)
"""

import json
import sys
import os
import re
import traceback
from pathlib import Path
from string import Template
from typing import Any, Callable, Dict, Optional

# ---------------------------------------------------------------------------
# JSON-RPC Bridge
# ---------------------------------------------------------------------------

_PENDING: Dict[int, "PendingRequest"] = {}


class _PendingRequest:
    __slots__ = ("method", "resolve")

    def __init__(self, method: str, resolve: Callable[[Any], None]):
        self.method = method
        self.resolve = resolve


_next_id = 1


def _make_id() -> int:
    global _next_id
    rid = _next_id
    _next_id += 1
    return rid


def _log(msg: str) -> None:
    print(f"[startup-plugin] {msg}", file=sys.stderr, flush=True)


# ── Sending (Plugin → Server) ──────────────────────────────────────────


def send_response(req_id: int, result: Any) -> None:
    payload = json.dumps({"jsonrpc": "2.0", "id": req_id, "result": result})
    _write_line(payload)


def send_error(req_id: int, code: int, message: str) -> None:
    payload = json.dumps(
        {"jsonrpc": "2.0", "id": req_id, "error": {"code": code, "message": message}}
    )
    _write_line(payload)


def send_request(method: str, params: Any) -> Any:
    rid = _make_id()
    payload = json.dumps({"jsonrpc": "2.0", "id": rid, "method": method, "params": params})
    result_holder: list = []

    def resolve(val: Any) -> None:
        result_holder.append(val)

    _PENDING[rid] = _PendingRequest(method, resolve)
    _write_line(payload)
    _process_until_response(rid)

    if not result_holder:
        raise RuntimeError(f"No response for {method}")
    return result_holder[0]


def send_notification(method: str, params: Any) -> None:
    payload = json.dumps({"jsonrpc": "2.0", "method": method, "params": params})
    _write_line(payload)


def _write_line(data: str) -> None:
    try:
        sys.stdout.write(data + "\n")
        sys.stdout.flush()
    except BrokenPipeError:
        pass


# ── Receiving (Server → Plugin handling) ───────────────────────────────


def _process_until_response(rid: int) -> None:
    while rid in _PENDING:
        line = sys.stdin.readline()
        if not line:
            break
        _dispatch(line.rstrip("\n"))


def _dispatch(line: str) -> None:
    if not line.strip():
        return
    try:
        msg = json.loads(line)
    except json.JSONDecodeError as e:
        _log(f"Invalid JSON from server: {e}")
        return

    msg_id = msg.get("id")

    if msg_id is not None and "method" not in msg:
        # Response to one of our requests
        rid = int(msg_id)
        pending = _PENDING.pop(rid, None)
        if pending:
            result = msg.get("result")
            error = msg.get("error")
            if error:
                _log(f"Server error for {pending.method}: {error}")
                pending.resolve({"__error__": error})
            else:
                pending.resolve(result)
        return

    method = msg.get("method")
    if method is None:
        return

    params = msg.get("params")
    req_id = int(msg_id) if msg_id is not None else None
    _handle_incoming_request(method, params, req_id)


def _handle_incoming_request(method: str, params: Any, req_id: Optional[int]) -> None:
    handler = _HANDLERS.get(method)
    if handler is None:
        _log(f"Unknown method: {method}")
        if req_id is not None:
            send_error(req_id, -32601, f"Method not found: {method}")
        return

    try:
        result = handler(params)
        if req_id is not None:
            send_response(req_id, result)
    except Exception as e:
        _log(f"Handler error for {method}: {traceback.format_exc()}")
        if req_id is not None:
            send_error(req_id, -32000, str(e))


_HANDLERS: Dict[str, Callable[[Any], Any]] = {}


def on(method: str):
    """Decorator to register a handler for a server→plugin method."""
    def decorator(fn):
        _HANDLERS[method] = fn
        return fn
    return decorator


# ---------------------------------------------------------------------------
# Store & Scoring imports
# ---------------------------------------------------------------------------

from store import StartupStore, IDEA_STATES, VALID_TRANSITIONS
from scoring import score_idea, Verdict, Confidence, ScoreResult

# ---------------------------------------------------------------------------
# Global state
# ---------------------------------------------------------------------------

_store: Optional[StartupStore] = None

TEMPLATE_DIR = os.path.join(os.path.dirname(os.path.abspath(__file__)), "templates")
STATIC_DIR = os.path.join(os.path.dirname(os.path.abspath(__file__)), "static")

_MIME = {
    ".js": "application/javascript; charset=utf-8",
    ".css": "text/css; charset=utf-8",
    ".svg": "image/svg+xml",
    ".png": "image/png",
    ".html": "text/html; charset=utf-8",
}


# ---------------------------------------------------------------------------
# HTML Helpers
# ---------------------------------------------------------------------------


def _load_template(name: str) -> Template:
    with open(os.path.join(TEMPLATE_DIR, name), "r") as f:
        return Template(f.read())


def _esc(s: str) -> str:
    return (s or "").replace("&", "&amp;").replace("<", "&lt;").replace(">", "&gt;").replace('"', "&quot;")


def _html_response(html: str) -> dict:
    return {"status": 200, "headers": {"content-type": "text/html; charset=utf-8"}, "body": html}


def _json_response(data: Any, status: int = 200) -> dict:
    return {"status": status, "headers": {"content-type": "application/json"}, "body": json.dumps(data)}


def _parse_body(body: Any) -> dict:
    if body is None:
        return {}
    if isinstance(body, dict):
        return body
    if isinstance(body, str):
        try:
            return json.loads(body)
        except json.JSONDecodeError:
            return {}
    return {}


# ---------------------------------------------------------------------------
# Page Renderers
# ---------------------------------------------------------------------------


def _render_startup_index() -> dict:
    """Render the main startup page with left nav + right content area."""
    ideas = _store.list_ideas() if _store else []

    # Build idea cards HTML
    idea_cards = []
    for idea in ideas:
        slug = _esc(idea.get("slug", ""))
        status = _esc(idea.get("status", "candidate"))
        verdict = _esc(idea.get("verdict", "")) if idea.get("verdict") else ""
        score = idea.get("final_score")
        score_str = f"{int(score)}/100" if score is not None else "—"
        idea_cards.append(
            f'<div class="idea-card status-{status}" onclick="window.location.href=\'/startup/ideas/{slug}\'">'
            f'<span class="idea-status">{status}</span>'
            f'<h3>{slug}</h3>'
            f'<span class="idea-score">{score_str}</span>'
            f'<span class="idea-verdict">{verdict}</span>'
            f'</div>'
        )

    cards_html = "\n".join(idea_cards) if idea_cards else '<div class="empty">No ideas yet. Start by evaluating one.</div>'

    tmpl = _load_template("startup-index.html")
    html = tmpl.substitute(
        idea_cards=cards_html,
        idea_count=str(len(ideas)),
    )
    return _html_response(html)


def _render_idea_detail(slug: str) -> dict:
    """Render a single idea's detail page with full analysis data."""
    idea = _store.get_idea(slug) if _store else None
    if not idea:
        return _html_response(f"<h1>Idea '{_esc(slug)}' not found</h1>")

    competitor = _store.get_competitor_analysis(slug) if _store else None
    score_history = _store.get_score_history(slug) if _store else []
    competing_ideas = _store.find_competing_ideas(
        competitor.get("direct_competitors", [{}])[0].get("name", "")
    ) if competitor and competitor.get("direct_competitors") else []

    # Build score history rows
    score_rows = []
    for snap in score_history:
        score_rows.append(
            f'<tr><td>{_esc(_short_date(snap.get("snapshot_at", "")))}</td>'
            f'<td class="score-value-td">{snap.get("final_score", 0)}</td>'
            f'<td><span class="verdict-badge verdict-{_esc(snap.get("verdict", ""))}">'
            f'{_esc(snap.get("verdict", ""))}</span></td></tr>'
        )

    # Competitor list
    comp_list = ""
    if competitor:
        items = []
        for c in competitor.get("direct_competitors", []):
            items.append(
                f'<div class="comp-item">'
                f'<strong>{_esc(c.get("name", "?"))}</strong> '
                f'<span class="hint">{_esc(c.get("platform", ""))} · {_esc(c.get("pricing_model", ""))}</span>'
                f'</div>'
            )
        comp_list = "\n".join(items) if items else '<p class="hint">No direct competitors found</p>'

    # Positioning gaps
    gaps_html = ""
    if competitor:
        gap_items = []
        for g in competitor.get("positioning_gaps", [])[:5]:
            gap_items.append(
                f'<li><strong>{_esc(g.get("gap_type", ""))}</strong> '
                f'({_esc(g.get("defensibility", ""))}): {_esc(g.get("description", ""))}</li>'
            )
        gaps_html = "\n".join(gap_items) if gap_items else '<p class="hint">No significant gaps identified</p>'

    tmpl = _load_template("idea-detail.html")
    html = tmpl.substitute(
        slug=_esc(slug),
        status=_esc(idea.get("status", "")),
        verdict=_esc(idea.get("verdict", "")) if idea.get("verdict") else "—",
        final_score=str(idea.get("final_score", "—")),
        description=_esc(idea.get("description", "")),
        competitor_count=str(len(competitor.get("direct_competitors", [])) if competitor else 0),
        saturation=_esc(competitor.get("market_saturation", "—")) if competitor else "—",
        saturation_score=str(competitor.get("saturation_score", {}).get("total", "—")) if competitor else "—",
        comp_list=comp_list,
        gaps_html=gaps_html,
        score_rows="\n".join(score_rows) if score_rows else '<tr><td colspan="3">No scores yet</td></tr>',
        related_ideas_count=str(len(competing_ideas)),
    )
    return _html_response(html)


def _short_date(iso_str: str) -> str:
    """Shorten an ISO datetime to date only."""
    return iso_str[:10] if iso_str else ""


# API endpoint: get full analysis
def _handle_get_analysis(slug: str) -> dict:
    """Return full analysis data for an idea as JSON."""
    idea = _store.get_idea(slug) if _store else None
    if not idea:
        return _json_response({"error": f"idea '{slug}' not found"}, 404)

    competitor = _store.get_competitor_analysis(slug) if _store else None
    score_history = _store.get_score_history(slug) if _store else []

    return _json_response({
        "idea": idea,
        "competitor_analysis": competitor,
        "score_history": score_history,
    })


# ---------------------------------------------------------------------------
# API Handlers
# ---------------------------------------------------------------------------


def _handle_validate_idea(body: dict) -> dict:
    """Run the full idea validation pipeline with parallel execution.

    Phase 1 (sequential — dependencies exist):
      1. Create idea, 2. Desire, 3. Competitors, 4. Pricing
    Phase 2 (parallel — all independent):
      5. Market size, CAC, Distribution, Retention, Complexity
    Phase 3 (sequential — depends on all above):
      6. Weakness detection, 7. Scoring, 8. Decision memo
    Phase 4 (conditional):
      9. Pivot engine (if verdict == pivot)
    """
    slug = body.get("idea_slug", "").strip().lower()
    description = body.get("description", "").strip()
    keywords = body.get("keywords", [])
    niche = body.get("niche", "")

    if not slug or not description:
        return _json_response({"error": "idea_slug and description are required"}, 400)
    if not re.match(r"^[a-z0-9]([a-z0-9-]*[a-z0-9])?$", slug):
        return _json_response({"error": "slug must be lowercase alphanumeric with hyphens"}, 400)
    if not _store:
        return _json_response({"error": "store not initialized"}, 500)

    from llm import LlmClient
    from skills import (
        analyze_competitors, evaluate_desire, analyze_pricing, generate_decision_memo,
        estimate_market_size, model_cac, analyze_distribution, predict_retention,
        assess_complexity, detect_weaknesses, generate_pivots,
    )
    from scoring import score_idea, DEFAULT_WEIGHTS
    import concurrent.futures

    llm = LlmClient()
    errors = []

    # ── Step 1: Create idea ──────────────────────────────────────────
    try:
        idea = _store.create_idea(slug, {"description": description, "keywords": keywords, "niche": niche})
    except Exception as e:
        return _json_response({"error": f"Failed to create idea: {e}"}, 500)

    _store.update_idea_status(slug, "in_validation")
    _log(f"[{slug}] Validation started (Phase 1+2)")

    # ── Phase 1: Sequential core analysis ────────────────────────────
    desire = _run_or_fallback(f"[{slug}] desire", lambda: evaluate_desire(description, llm=llm),
                               {"desire_scores": {}, "desire_label": "unknown", "desire_strength": 0,
                                "primary_driver": "unknown", "virality_potential": "unknown"}, errors)

    competitors = _run_or_fallback(f"[{slug}] competitors",
        lambda: analyze_competitors(description, keywords=keywords, niche=niche, llm=llm),
        {"direct_competitors": [], "market_saturation": "unknown", "saturation_score": {"total": 0}}, errors)
    if competitors.get("direct_competitors"):
        _store.store_competitor_analysis(slug, competitors)

    comp_pricing = _build_competitor_pricing_context(competitors)
    pricing = _run_or_fallback(f"[{slug}] pricing",
        lambda: analyze_pricing(description, desire_scores=desire, competitor_pricing=comp_pricing, llm=llm),
        {"recommended_price_monthly": 0, "pricing_model": "unknown"}, errors)

    # ── Phase 2: Parallel analysis (all independent) ─────────────────
    def _run_parallel():
        with concurrent.futures.ThreadPoolExecutor(max_workers=5) as pool:
            f_market = pool.submit(lambda: _run_or_fallback(f"[{slug}] market_size",
                lambda: estimate_market_size(description, competitors=competitors,
                    trend_velocity="stable", llm=llm),
                {"market_size_verdict": "unknown"}, errors))
            f_cac = pool.submit(lambda: _run_or_fallback(f"[{slug}] cac",
                lambda: model_cac(description, pricing=pricing, llm=llm),
                {"blended_cac": 0}, errors))
            f_dist = pool.submit(lambda: _run_or_fallback(f"[{slug}] distribution",
                lambda: analyze_distribution(description, competitors=competitors, llm=llm),
                {"composite_k_factor": 0, "distribution_confidence": "low"}, errors))
            f_ret = pool.submit(lambda: _run_or_fallback(f"[{slug}] retention",
                lambda: predict_retention(description, desire=desire, llm=llm),
                {"retention_tier": "unknown", "predicted_retention": {}}, errors))
            f_comp = pool.submit(lambda: _run_or_fallback(f"[{slug}] complexity",
                lambda: assess_complexity(description, llm=llm),
                {"total_complexity": 0, "build_time_estimate_months": 0}, errors))
            return f_market.result(), f_cac.result(), f_dist.result(), f_ret.result(), f_comp.result()

    market_size, cac, distribution, retention, complexity = _run_parallel()
    _log(f"[{slug}] Phase 2 complete: market={market_size.get('market_size_verdict')}, "
         f"cac=${cac.get('blended_cac', 0)}, dist_k={distribution.get('composite_k_factor', 0)}, "
         f"ret={retention.get('retention_tier')}, build={complexity.get('build_time_estimate_months', 0)}mo")

    # ── Phase 3: Synthesis ───────────────────────────────────────────
    weaknesses = _run_or_fallback(f"[{slug}] weaknesses",
        lambda: detect_weaknesses(
            dimension_scores=_build_dimension_scores(desire, competitors, pricing, market_size,
                                                      cac, distribution, retention, complexity),
            desire=desire, competitors=competitors, pricing=pricing,
            distribution=distribution, retention=retention, complexity=complexity, llm=llm),
        {"weaknesses": [], "overall_weakness_severity": "unknown"}, errors)

    dimension_scores = _build_dimension_scores(desire, competitors, pricing, market_size,
                                                cac, distribution, retention, complexity)
    score_result = score_idea(dimension_scores, weights=DEFAULT_WEIGHTS)
    _log(f"[{slug}] Scored: {score_result.final_score}/100 → {score_result.verdict.value}")

    scores_dict = {
        "dimension_scores": score_result.dimension_scores, "weights_applied": score_result.weights_applied,
        "base_score": score_result.base_score, "floor_penalty": score_result.floor_penalty,
        "missing_discount": score_result.missing_discount, "final_score": score_result.final_score,
        "verdict": score_result.verdict.value, "confidence": score_result.confidence.value,
        "killer_dimensions": score_result.killer_dimensions,
    }
    _store.store_score_snapshot(slug, scores_dict)

    memo = _run_or_fallback(f"[{slug}] memo",
        lambda: generate_decision_memo(idea_slug=slug, idea_description=description,
            scores=scores_dict, desire=desire, competitors=competitors, pricing=pricing, llm=llm),
        f"# Decision Memo: {slug}\n\nError generating memo.", errors)

    # ── Phase 4: Conditional pivot ───────────────────────────────────
    pivot_result = None
    if score_result.verdict.value == "pivot":
        pivot_result = _run_or_fallback(f"[{slug}] pivot",
            lambda: generate_pivots(idea_description=description, scores=scores_dict,
                                     weaknesses=weaknesses.get("weaknesses", []),
                                     competitors=competitors, llm=llm),
            None, errors)

    # ── Team integration: auto-create work item ─────────────────────
    from team_bridge import create_team_work_item, sync_to_longterm_memory
    if score_result.verdict.value in ("test", "pursue"):
        try:
            rat = scores_dict.get("rat_experiment") if score_result.verdict.value == "test" else None
            create_team_work_item(
                send_request, slug, score_result.verdict.value,
                score_result.final_score, rat_experiment=rat,
                description=description,
            )
        except Exception as e:
            _log(f"[{slug}] Team work item creation failed: {e}")

    # ── YantrikDB sync ──────────────────────────────────────────────
    try:
        sync_to_longterm_memory(send_request, slug, llm.agent_id, {
            "competitors": {"direct_count": len(competitors.get("direct_competitors", [])),
                             "market_saturation": competitors.get("market_saturation"),
                             "saturation_score": competitors.get("saturation_score", {})},
            "scores": scores_dict,
        })
    except Exception as e:
        _log(f"[{slug}] Memory sync failed: {e}")

    # ── Emit event ───────────────────────────────────────────────────
    send_notification("aman.emit_event", {
        "event_type": "startup:decided",
        "payload": {"idea_slug": slug, "verdict": score_result.verdict.value,
                     "final_score": score_result.final_score, "confidence": score_result.confidence.value},
    })
    _log(f"[{slug}] Done: {score_result.final_score}/100 → {score_result.verdict.value}"
         + (f" ({len(errors)} errors)" if errors else ""))

    return _json_response({
        "ok": True,
        "idea": idea,
        "desire": desire,
        "competitors": {"direct_count": len(competitors.get("direct_competitors", [])),
                         "market_saturation": competitors.get("market_saturation"),
                         "saturation_score": competitors.get("saturation_score", {})},
        "pricing": {"recommended_monthly": pricing.get("recommended_price_monthly"),
                     "model": pricing.get("pricing_model")},
        "market_size": {"verdict": market_size.get("market_size_verdict"),
                         "som_year_1": market_size.get("som", {}).get("year_1", 0)},
        "cac": {"blended": cac.get("blended_cac", 0)},
        "distribution": {"k_factor": distribution.get("composite_k_factor", 0)},
        "retention": {"tier": retention.get("retention_tier")},
        "complexity": {"months": complexity.get("build_time_estimate_months", 0)},
        "weaknesses": weaknesses,
        "scores": scores_dict,
        "decision_memo": memo,
        "pivot": pivot_result,
        "errors": errors if errors else None,
    }, 201)


def _run_or_fallback(label: str, fn, fallback, errors: list) -> dict:
    """Run a skill and return its result, or fallback on error."""
    try:
        _log(f"{label} running...")
        result = fn()
        return result
    except Exception as e:
        _log(f"{label} failed: {e}")
        errors.append(f"{label}: {e}")
        return fallback


def _build_dimension_scores(desire: dict, competitors: dict, pricing: dict,
                             market_size: dict | None = None, cac: dict | None = None,
                             distribution: dict | None = None, retention: dict | None = None,
                             complexity: dict | None = None) -> dict[str, float]:
    """Map all analysis results to 0-100 dimension scores."""
    scores = {}
    scores["demand"] = _desire_to_demand_score(desire)
    scores["competition"] = _saturation_to_competition_score(competitors)

    # Monetization: from pricing + CAC + market size
    ms = market_size or {}
    cac_d = cac or {}
    ltv = pricing.get("recommended_price_monthly", 0) * 12  # annual LTV
    blended_cac = cac_d.get("blended_cac", ltv * 0.5) or ltv * 0.5
    ltv_cac_ratio = ltv / max(blended_cac, 0.01)
    if ltv_cac_ratio >= 5:    base_mon = 90
    elif ltv_cac_ratio >= 3:  base_mon = 75
    elif ltv_cac_ratio >= 1:  base_mon = 45
    else:                     base_mon = 15
    if ms.get("market_size_verdict") in ("large", "medium"):
        base_mon += 10
    elif ms.get("market_size_verdict") == "micro-niche":
        base_mon -= 15
    scores["monetization"] = max(0.0, min(100.0, base_mon))

    # Distribution: use actual analysis data
    dist = distribution or {}
    k = dist.get("composite_k_factor", 0)
    if k >= 0.5:      dist_score = 85.0
    elif k >= 0.3:    dist_score = 65.0
    elif k >= 0.1:    dist_score = 40.0
    else:             dist_score = 15.0
    if dist.get("distribution_confidence") == "high":
        dist_score += 10
    scores["distribution"] = max(0.0, min(100.0, dist_score))

    # Retention: use actual predicted retention
    ret = retention or {}
    d30 = ret.get("predicted_retention", {}).get("day_30_pct", 0) if ret else 0
    tier = ret.get("retention_tier", "average") if ret else "average"
    tier_map = {"excellent": 90, "good": 70, "average": 45, "poor": 20, "unknown": 30}
    scores["retention"] = float(max(0.0, min(100.0, tier_map.get(tier, 30) + (d30 * 0.2))))

    # Founder fit: placeholder (requires user_profile)
    scores["founder_fit"] = 50.0

    return scores


def _desire_to_demand_score(desire: dict) -> float:
    strength = desire.get("desire_strength", 2.0)
    viral = desire.get("virality_potential", "medium")
    base = strength * 20.0
    if viral == "high": base += 10
    elif viral == "low": base -= 10
    return max(0.0, min(100.0, base))


def _saturation_to_competition_score(competitors: dict) -> float:
    total = competitors.get("saturation_score", {}).get("total", 8)
    return max(0.0, min(100.0, 120.0 - total * 6.667))


def _build_competitor_pricing_context(competitors: dict) -> str:
    """Build a text summary of competitor pricing for the pricing LLM call."""
    parts = []
    for comp in competitors.get("direct_competitors", []):
        name = comp.get("name", "")
        model = comp.get("pricing_model", "")
        if name and model:
            parts.append(f"- {name}: {model}")
    return "\n".join(parts) if parts else "No competitor pricing data available"


def _handle_list_ideas(params: dict) -> dict:
    status = params.get("status") if isinstance(params, dict) else None
    ideas = _store.list_ideas(status) if _store else []
    return _json_response({"ideas": ideas})


def _handle_get_idea(slug: str) -> dict:
    idea = _store.get_idea(slug) if _store else None
    if not idea:
        return _json_response({"error": f"idea '{slug}' not found"}, 404)
    return _json_response(idea)


def _handle_update_idea_status(slug: str, body: dict) -> dict:
    new_status = body.get("status", "").strip()
    if new_status not in IDEA_STATES:
        return _json_response({"error": f"invalid status '{new_status}'. valid: {IDEA_STATES}"}, 400)
    try:
        result = _store.update_idea_status(slug, new_status)
        return _json_response({"ok": True, "idea": result})
    except ValueError as e:
        return _json_response({"error": str(e)}, 400)


# ---------------------------------------------------------------------------
# Route Handler
# ---------------------------------------------------------------------------


def handle_route(method: str, path: str, query: Optional[str],
                 headers: dict, body: Optional[str]) -> dict:
    """Route HTTP requests to the appropriate handler."""
    clean = path.removeprefix("/api/v1")

    # ── Static files ──────────────────────────────────────────────
    m_static = re.match(r"/startup/static/(.+)", clean)
    if m_static and method == "GET":
        filename = m_static.group(1)
        if ".." in filename or "/" in filename or "\\" in filename:
            return {"status": 400, "body": "bad filename"}
        filepath = os.path.join(STATIC_DIR, filename)
        if not os.path.isfile(filepath):
            return {"status": 404, "body": "not found"}
        ext = os.path.splitext(filename)[1].lower()
        mime = _MIME.get(ext, "application/octet-stream")
        try:
            with open(filepath, "r") as f:
                content = f.read()
            return {"status": 200, "headers": {"content-type": mime}, "body": content}
        except Exception:
            return {"status": 500, "body": "failed to read file"}

    # ── Main page ──────────────────────────────────────────────────
    if method == "GET" and clean in ("/startup", "/startup/"):
        return _render_startup_index()

    # ── Idea detail page ───────────────────────────────────────────
    m_detail = re.match(r"/startup/ideas/([^/]+)$", clean)
    if m_detail and method == "GET":
        return _render_idea_detail(m_detail.group(1))

    # ── API: List ideas ────────────────────────────────────────────
    if method == "GET" and clean == "/startup/api/ideas":
        return _handle_list_ideas({})

    # ── API: Get idea ──────────────────────────────────────────────
    m_get = re.match(r"/startup/api/ideas/([^/]+)$", clean)
    if m_get and method == "GET":
        return _handle_get_idea(m_get.group(1))

    # ── API: Validate idea ─────────────────────────────────────────
    if method == "POST" and clean == "/startup/api/validate":
        body_json = _parse_body(body)
        return _handle_validate_idea(body_json)

    # ── API: Get full analysis ─────────────────────────────────────
    m_analysis = re.match(r"/startup/api/ideas/([^/]+)/analysis$", clean)
    if m_analysis and method == "GET":
        return _handle_get_analysis(m_analysis.group(1))

    # ── API: Generate ideas from niche ───────────────────────────────
    if method == "POST" and clean == "/startup/api/generate":
        body_json = _parse_body(body)
        return _handle_generate_ideas(body_json)

    # ── API: Market deep dive ────────────────────────────────────────
    if method == "POST" and clean == "/startup/api/market-deepdive":
        body_json = _parse_body(body)
        return _handle_market_deepdive(body_json)

    # ── Strategy layer ─────────────────────────────────────────────
    m_landing = re.match(r"/startup/api/ideas/([^/]+)/landing-page$", clean)
    if m_landing and method == "POST":
        return _handle_strategy_skill(m_landing.group(1), "landing_page")
    m_gtm = re.match(r"/startup/api/ideas/([^/]+)/gtm$", clean)
    if m_gtm and method == "POST":
        return _handle_strategy_skill(m_gtm.group(1), "gtm")
    m_pricepage = re.match(r"/startup/api/ideas/([^/]+)/pricing-page$", clean)
    if m_pricepage and method == "POST":
        return _handle_strategy_skill(m_pricepage.group(1), "pricing_page")
    m_outreach = re.match(r"/startup/api/ideas/([^/]+)/outreach$", clean)
    if m_outreach and method == "POST":
        return _handle_strategy_skill(m_outreach.group(1), "outreach")

    # ── Execution layer ─────────────────────────────────────────────
    m_mvp = re.match(r"/startup/api/ideas/([^/]+)/mvp-scope$", clean)
    if m_mvp and method == "POST":
        return _handle_execution_skill(m_mvp.group(1), "mvp_scope")

    m_feedback = re.match(r"/startup/api/ideas/([^/]+)/feedback$", clean)
    if m_feedback and method == "POST":
        return _handle_execution_skill(m_feedback.group(1), "feedback", _parse_body(body))

    # ── Reflection layer ────────────────────────────────────────────
    if method == "POST" and clean == "/startup/api/reflection/journal":
        return _handle_reflection_skill("journal")
    if method == "POST" and clean == "/startup/api/reflection/ikigai":
        return _handle_reflection_skill("ikigai")

    # ── AI-Native ──────────────────────────────────────────────────
    m_whatif = re.match(r"/startup/api/ideas/([^/]+)/what-if$", clean)
    if m_whatif and method == "POST":
        body_json = _parse_body(body)
        return _handle_what_if(m_whatif.group(1), body_json)

    # ── Incubation bridge (for Gateway IncubationRunner) ──────────────
    if method == "GET" and clean == "/startup/api/incubation-data":
        from team_bridge import build_incubation_context
        return _json_response(build_incubation_context(_store))

    # ── API: Run pivot engine ────────────────────────────────────────
    m_pivot = re.match(r"/startup/api/ideas/([^/]+)/pivot$", clean)
    if m_pivot and method == "POST":
        return _handle_run_pivot(m_pivot.group(1))

    # ── API: Update idea status ────────────────────────────────────
    m_status = re.match(r"/startup/api/ideas/([^/]+)/status$", clean)
    if m_status and method == "POST":
        body_json = _parse_body(body)
        return _handle_update_idea_status(m_status.group(1), body_json)

    return {"status": 404, "body": json.dumps({"error": "not found"})}

# ---------------------------------------------------------------------------
# Idea Generation workflow
# ---------------------------------------------------------------------------


def _handle_generate_ideas(body: dict) -> dict:
    """Generate 5-10 app ideas from a niche + user context."""
    niche = body.get("niche", "").strip()
    user_context = body.get("user_context", "").strip()

    if not niche:
        return _json_response({"error": "niche is required"}, 400)

    from llm import LlmClient
    from skills import analyze_trends

    llm = LlmClient()
    _log(f"Generating ideas for niche: {niche}")

    # Step 1: Trend analysis
    trends = _run_or_fallback(f"[generate] trends",
        lambda: analyze_trends(niche, llm=llm),
        {"trend_velocity": "stable", "top_signals": []}, [])

    # Step 2: Generate ideas based on trends
    prompt = (
        f"Generate 7-10 app ideas for the niche '{niche}' based on these trends:\n\n"
        f"Trend velocity: {trends.get('trend_velocity', 'stable')}\n"
        f"Top signals: {json.dumps(trends.get('top_signals', []))}\n"
        f"User context: {user_context or 'indie developer, B2C focus'}\n\n"
        f"For each idea, provide: name (kebab-case slug), one-line description, "
        f"primary desire driver, estimated complexity (low/medium/high), "
        f"and a novelty score (1-10). Return as JSON array."
    )
    try:
        ideas = llm.chat_json(
            "You are a startup idea generator. Generate creative, viable app ideas "
            "based on real market trends. Be specific and original.",
            prompt, temperature=0.8, max_tokens=4000,
        )
    except Exception as e:
        return _json_response({"error": f"Idea generation failed: {e}"}, 500)

    # Store trends in SurrealDB
    if _store:
        for platform in ["tiktok", "reddit", "app_store", "google_trends"]:
            try:
                _store.store_market_insight(niche, platform, "current", trends)
            except Exception:
                pass

    # Create idea stubs
    created = []
    if isinstance(ideas, list):
        for item in ideas[:10]:
            if isinstance(item, dict):
                slug = item.get("name", "").lower().replace(" ", "-")
                if slug and _store:
                    try:
                        _store.create_idea(slug, {
                            "description": item.get("description", ""),
                            "keywords": [niche],
                            "niche": niche,
                        })
                        created.append(slug)
                    except Exception:
                        pass

    return _json_response({
        "ok": True,
        "niche": niche,
        "trends": trends,
        "ideas": ideas,
        "created_count": len(created),
    })


# ---------------------------------------------------------------------------
# Market Deep Dive workflow
# ---------------------------------------------------------------------------


def _handle_market_deepdive(body: dict) -> dict:
    """Deep-dive into a market niche: trends + TAM + competitor landscape."""
    niche = body.get("niche", "").strip()

    if not niche:
        return _json_response({"error": "niche is required"}, 400)

    from llm import LlmClient
    from skills import analyze_trends, analyze_competitors, estimate_market_size

    llm = LlmClient()
    _log(f"Market deep dive for: {niche}")

    trends = _run_or_fallback(f"[deepdive] trends",
        lambda: analyze_trends(niche, llm=llm),
        {"trend_velocity": "stable", "top_signals": []}, [])

    competitors = _run_or_fallback(f"[deepdive] competitors",
        lambda: analyze_competitors(f"Market analysis for {niche}", keywords=[niche], niche=niche, llm=llm),
        {"direct_competitors": [], "market_saturation": "unknown", "saturation_score": {"total": 0}}, [])

    market_size = _run_or_fallback(f"[deepdive] market_size",
        lambda: estimate_market_size(f"Market for {niche} apps", competitors=competitors,
                                      trend_velocity=trends.get("trend_velocity", "stable"), llm=llm),
        {"market_size_verdict": "unknown"}, [])

    # Store market insight
    if _store:
        _store.store_market_insight(niche, "combined", "current", {
            "trend_velocity": trends.get("trend_velocity", "stable"),
            "top_signals": trends.get("top_signals", []),
            "monetization_evidence": trends.get("monetization_evidence", False),
            "narrative": json.dumps(trends),
        })

    return _json_response({
        "ok": True,
        "niche": niche,
        "trends": trends,
        "competitors": {
            "direct_count": len(competitors.get("direct_competitors", [])),
            "market_saturation": competitors.get("market_saturation"),
            "saturation_score": competitors.get("saturation_score", {}),
        },
        "market_size": market_size,
    })


# ---------------------------------------------------------------------------
# Pivot Engine handler
# ---------------------------------------------------------------------------


def _handle_run_pivot(slug: str) -> dict:
    """Run the pivot engine for an existing idea."""
    idea = _store.get_idea(slug) if _store else None
    if not idea:
        return _json_response({"error": f"idea '{slug}' not found"}, 404)

    # Get latest scores
    history = _store.get_score_history(slug) if _store else []
    if not history:
        return _json_response({"error": "no scores available for pivot analysis"}, 400)

    latest = history[-1]
    if latest.get("verdict") != "pivot":
        return _json_response({"error": f"idea verdict is '{latest.get('verdict')}', not 'pivot'"}, 400)

    from llm import LlmClient
    from skills import generate_pivots

    llm = LlmClient()
    competitor = _store.get_competitor_analysis(slug) if _store else None

    result = _run_or_fallback(f"[pivot] {slug}",
        lambda: generate_pivots(
            idea_description=idea.get("description", ""),
            scores=latest,
            competitors=competitor,
            llm=llm,
        ), None, [])

    if result is None:
        return _json_response({"error": "Pivot generation failed"}, 500)

    return _json_response({"ok": True, "slug": slug, "pivot": result})


def _handle_strategy_skill(slug: str, skill_type: str) -> dict:
    """Run a strategy-layer skill for an existing idea.

    skill_type: landing_page, gtm, pricing_page, outreach
    """
    idea = _store.get_idea(slug) if _store else None
    if not idea:
        return _json_response({"error": f"idea '{slug}' not found"}, 404)

    from llm import LlmClient
    from skills import (build_landing_page, build_gtm_narrative,
                         optimize_pricing_page, design_outreach)

    llm = LlmClient()
    description = idea.get("description", "")
    keywords = idea.get("keywords", [])
    competitor = _store.get_competitor_analysis(slug) if _store else None
    desire_scores = {}  # Could load from last analysis

    skill_map = {
        "landing_page": lambda: build_landing_page(description, desire=desire_scores,
            competitors=competitor, keywords=keywords, llm=llm),
        "gtm": lambda: build_gtm_narrative(description, competitors=competitor,
            distribution=None, desire=desire_scores, llm=llm),
        "pricing_page": lambda: optimize_pricing_page(description, pricing=None,
            competitors=competitor, desire=desire_scores, llm=llm),
        "outreach": lambda: design_outreach(description, user_profile="",
            competitors=competitor, llm=llm),
    }

    fn = skill_map.get(skill_type)
    if fn is None:
        return _json_response({"error": f"unknown strategy skill: {skill_type}"}, 400)

    result = _run_or_fallback(f"[strategy] {slug}/{skill_type}", fn, None, [])
    if result is None:
        return _json_response({"error": f"{skill_type} generation failed"}, 500)

    # Store in SurrealDB
    store_methods = {
        "landing_page": lambda: _store.store_landing_page(slug, result) if _store else None,
        "gtm": lambda: _store.store_gtm_narrative(slug, result) if _store else None,
    }
    store_fn = store_methods.get(skill_type)
    if store_fn:
        try:
            store_fn()
        except Exception as e:
            _log(f"Failed to store {skill_type} for {slug}: {e}")

    return _json_response({"ok": True, "slug": slug, "skill": skill_type, "result": result})


def _handle_execution_skill(slug: str, skill_type: str, req_body: dict | None = None) -> dict:
    """Run an execution-layer skill for an existing idea."""
    idea = _store.get_idea(slug) if _store else None
    if not idea:
        return _json_response({"error": f"idea '{slug}' not found"}, 404)

    from llm import LlmClient
    from skills import negotiate_mvp, synthesize_feedback

    llm = LlmClient()
    description = idea.get("description", "")
    competitor = _store.get_competitor_analysis(slug) if _store else None

    if skill_type == "mvp_scope":
        result = _run_or_fallback(f"[exec] {slug}/mvp",
            lambda: negotiate_mvp(description, competitors=competitor, llm=llm), None, [])
    elif skill_type == "feedback":
        feedback_text = (req_body or {}).get("feedback_text", description)
        result = _run_or_fallback(f"[exec] {slug}/feedback",
            lambda: synthesize_feedback(feedback_text, competitor_analysis=competitor, llm=llm), None, [])
    else:
        return _json_response({"error": f"unknown execution skill: {skill_type}"}, 400)

    return _json_response({"ok": True, "slug": slug, "skill": skill_type, "result": result}) if result else \
           _json_response({"error": f"{skill_type} failed"}, 500)


def _handle_reflection_skill(skill_type: str) -> dict:
    """Run a reflection-layer skill across all ideas."""
    if not _store:
        return _json_response({"error": "store not initialized"}, 500)

    from llm import LlmClient
    from skills import audit_decisions, check_ikigai

    llm = LlmClient()
    ideas = _store.list_ideas()

    if skill_type == "journal":
        entries = _store.list_decision_entries() if _store else []
        result = _run_or_fallback(f"[reflect] journal",
            lambda: audit_decisions([e for e in entries], llm=llm), None, [])
    elif skill_type == "ikigai":
        scores = {}
        for idea in ideas:
            slug = idea.get("slug", "")
            if slug:
                scores[slug] = _store.get_score_history(slug) if _store else []
        result = _run_or_fallback(f"[reflect] ikigai",
            lambda: check_ikigai(ideas, scores_history=scores, llm=llm), None, [])
        if result and _store:
            _store.store_ikigai_check(result)
    else:
        return _json_response({"error": f"unknown reflection skill: {skill_type}"}, 400)

    return _json_response({"ok": True, "skill": skill_type, "result": result}) if result else \
           _json_response({"error": f"{skill_type} failed"}, 500)


def _handle_what_if(slug: str, body: dict) -> dict:
    """Run what-if simulation for an idea."""
    idea = _store.get_idea(slug) if _store else None
    if not idea:
        return _json_response({"error": f"idea '{slug}' not found"}, 404)

    question = body.get("question", "").strip()
    if not question:
        return _json_response({"error": "question is required"}, 400)

    from llm import LlmClient
    from skills import simulate_what_if

    llm = LlmClient()
    competitor = _store.get_competitor_analysis(slug) if _store else None
    score_history = _store.get_score_history(slug) if _store else []
    latest_scores = score_history[-1] if score_history else None

    result = _run_or_fallback(f"[whatif] {slug}",
        lambda: simulate_what_if(
            idea_description=idea.get("description", ""),
            question=question,
            scores=latest_scores,
            competitors=competitor,
            history=score_history,
            llm=llm,
        ), None, [])

    return _json_response({"ok": True, "slug": slug, "question": question, "result": result}) if result else \
           _json_response({"error": "what-if simulation failed"}, 500)


# ---------------------------------------------------------------------------
# Lifecycle Handlers (Server → Plugin requests)
# ---------------------------------------------------------------------------


_scheduler = None  # type: ignore
@on("aman.on_load")


def handle_on_load(params: Any) -> dict:
    """Initialize: open SurrealDB, register routes, subscribe to events, start scheduler."""
    global _store, _scheduler
    plugin_name = params.get("plugin_name", "startup") if isinstance(params, dict) else "startup"
    _log(f"on_load: {plugin_name}")

    # Open SurrealDB
    try:
        _store = StartupStore()
        _store.connect()
        _log(f"SurrealDB connected at {_store._path}")
    except Exception as e:
        _log(f"SurrealDB connection failed: {e}")
        return {"ok": False, "error": str(e)}

    # Register routes
    route_specs = [
        {"method": "GET", "path": "/startup"},
        {"method": "GET", "path": "/startup/ideas"},
        {"method": "GET", "path": "/startup/api/ideas"},
        {"method": "POST", "path": "/startup/api/validate"},
        {"method": "POST", "path": "/startup/api/generate"},
        {"method": "POST", "path": "/startup/api/market-deepdive"},
        {"method": "GET", "path": "/startup/api/incubation-data"},
    ]
    try:
        result = send_request("aman.register_routes", route_specs)
        _log(f"Registered {len(route_specs)} route(s): {result}")
    except Exception as e:
        _log(f"Route registration failed: {e}")

    # Subscribe to events
    try:
        send_request("aman.subscribe_events", {
            "events": ["startup:analyzed", "startup:scored", "startup:decided", "startup:pivot"],
        })
        _log("Subscribed to startup events")
    except Exception as e:
        _log(f"Event subscription failed: {e}")

    # Start autonomous scheduler
    try:
        from scheduler import StartupScheduler
        agent_id = _store.db.url.split("/")[-1] if _store else "default"
        _scheduler = StartupScheduler(_store)
        _scheduler.start()
        _log("Autonomous scheduler started (trend_watcher, rat_reminder, market_monitor)")
    except Exception as e:
        _log(f"Scheduler start failed: {e}")

    # Register cron jobs for reliability (gateway-side scheduling as backup)
    try:
        send_request("aman.add_cron_job", {
            "id": "startup-trend-watch",
            "expression": "0 9 * * 1",  # Monday 9am
        })
        send_request("aman.add_cron_job", {
            "id": "startup-rat-reminder",
            "expression": "0 10 * * *",  # Daily 10am
        })
        _log("Cron jobs registered")
    except Exception as e:
        _log(f"Cron registration failed (non-fatal): {e}")

    return {"ok": True, "store_path": _store._path}


@on("aman.on_unload")
def handle_on_unload(params: Any) -> dict:
    """Shutdown: close SurrealDB, stop scheduler."""
    global _store, _scheduler
    _log("on_unload")
    if _scheduler:
        _scheduler.stop()
        _scheduler = None
    if _store:
        _store.close()
        _store = None
    return {"ok": True}


@on("aman.handle_route")
def handle_http_route(params: Any) -> dict:
    """Handle an HTTP request forwarded from the server."""
    if not isinstance(params, dict):
        return {"status": 400, "body": json.dumps({"error": "invalid params"})}

    method = params.get("method", "GET")
    path = params.get("path", "")
    query = params.get("query")
    headers = params.get("headers", {})
    body = params.get("body")

    return handle_route(method, path, query, headers, body)


@on("aman.on_event")
def handle_on_event(params: Any) -> None:
    """Handle an event notification from the server."""
    if not isinstance(params, dict):
        return

    event_type = params.get("event_type", "")
    payload = params.get("payload", {})
    _log(f"Event: {event_type} — {json.dumps(payload, default=str)[:200]}")


# ---------------------------------------------------------------------------
# Main loop
# ---------------------------------------------------------------------------


def main() -> None:
    """Read JSON-RPC lines from stdin forever."""
    _log("Startup plugin started, waiting for JSON-RPC...")
    try:
        for line in sys.stdin:
            _dispatch(line.rstrip("\n"))
    except KeyboardInterrupt:
        pass
    except BrokenPipeError:
        pass
    _log("Startup plugin stopped")


if __name__ == "__main__":
    main()
