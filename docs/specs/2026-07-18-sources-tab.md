# Sources Tab for tokmeter

**Date:** 2026-07-18
**Status:** Draft awaiting review
**Extends:** `docs/specs/2026-07-17-remote-ssh-sources.md`

## Summary

Add a fourth top-level tab, `Sources`, that presents per-source update status
and a compact data summary for the local source and every configured remote SSH
source. It is a diagnostics view: "which machines are we collecting from, are
they healthy, when did they last update, and how much data did each contribute."

The former per-source health row in the header is removed; overall source health
is now a single colored dot on the `Sources` tab (green when every source is
fine, red when any source has a problem). The header keeps only the conditional
config/identity/remote-store diagnostics line. The tab introduces no new
persisted state and no protocol or `remote.json` schema change.

Account limits are shown for the local machine only, regardless of the selected
source filter.

## Goals

- One place that lists every source (local first, then configured order) with
  health, freshness, SSH duration, warnings, and bounded error text.
- A per-source data summary — projects, rounds, tokens — computed from the whole
  current snapshot.
- An aggregate footer summing only active sources.
- A single health dot on the `Sources` tab replacing the header status row.
- Limits scoped to the local machine only.

## Non-goals

- A refresh event log / history. Only current state is shown; no per-attempt
  timeline is stored or rendered.
- New fields in the snapshot protocol or `remote.json`.
- Period- or agent-scoped numbers on this tab (the summary is whole-snapshot).
- A settings editor or any mutation of sources from the tab.
- Per-source account limits (limits are local-only).

## Navigation

- Add `Tab::Sources` to the `Tab` enum (`src/main.rs:590`).
- Tab bar order: `Usage · Projects · Rounds · Sources`.
- `Tab` / `Shift+Tab` cycling and tab clicks include it (extend `Tab::ALL`,
  `next`, `prev`, and the label mapping).
- The tab is always present, including local-only configurations. With no remote
  sources it shows the local source row and the footer.

## Filters on this tab

The `Sources` tab shows all sources in full, and its summary does not depend on
the period. Therefore, while `Sources` is the active tab:

- the `source` filter row (all / local / remote ids) is hidden;
- the `period` selector row is hidden;
- their key bindings are inert: `s` / `Shift+s` (source cycle) and `←` / `→`
  (period) do nothing on this tab. `Tab` / `Shift+Tab` and `r` (force refresh)
  keep working.

Leaving the tab restores the filter rows and their keys unchanged. The stored
`source_filter` and period values are preserved, not reset.

## Per-source row

One block per source, local first, then configured order (the order already
produced by `dataset.sources()`). Each block is stacked over two lines so it
never overflows a narrow window (minimum width 420 px):

```text
● <label>  <health text ……………>  <ssh>
  <projects> proj · <rounds> rnd · <tokens> tok
```

- line 1: health icon (colored via `source_health_color`), label, then the
  `source_health_text` (health name, stale/incompatible age, warnings, bounded
  error text). The health text is the flexible, truncating element; `<ssh>` is
  pinned to the right (`duration_ms` for remote, `scan` for local).
- line 2: the data summary `projects / rounds / tokens` for that source, dimmed.
  A source with no active snapshot (`disabled`, `error` without last-good,
  `duplicate_instance`) shows `—`.

Because line 1's health text truncates and the summary sits on its own line,
columns do not clip or collide at the minimum window width.

## Aggregate footer

A single line beneath the list:

```text
Σ 2 active · 6 proj · 14 rnd · 2.0M tok
```

- `active` counts sources contributing to Dataset (`source.active == true`).
- projects / rounds / tokens sum the same active sources.
- Disabled, duplicate-instance, and error-without-snapshot sources remain visible
  as rows but are excluded from these totals.

## Summary computation (approach A)

Per-source summary numbers are computed on the fly in `build_snapshot`, reusing
the existing aggregation path rather than adding stored fields:

- For each active source, call `agg::projects_view_dataset` with that source's
  filter at `Timeframe::All` and uncapped `n`. It returns the per-project rows
  and a `Tot` total.
- `tokens` = `total.tokens()` from that call. `projects` = number of rows.
- `agg::build_dataset` at `Timeframe::All` is intentionally NOT used: its chart
  path (`chart_buckets_maps`) slices raw aggregate keys (`date[..7]`) without a
  UTF-8 boundary check, so a corrupted local cache key could panic on every
  snapshot build. `projects_view_dataset` filters through `ymd_to_days` and is
  safe.
- `rounds` = number of round records physically present in the source's snapshot
  (`DataView::rounds.len()`). `Summary::rounds_total` is intentionally not used:
  it is only computed for windowed timeframes (`Timeframe::rounds_known()` is
  false for `All`), whereas the summary is a whole-snapshot "how much was loaded"
  count.
- The values are attached to the source view model (`SourceOwned`) as an optional
  `summary` (present only for active sources). The footer aggregation
  (`sources_totals`) uses saturating addition.

No new field is added to `SourceSnapshot`, `StoredSource`, or `remote.json`.
Numbers therefore always match the data actually present in the snapshot.

## JSON

`--dump-json=sources` already exists (remote-ssh-sources spec). Additively add a
per-source `summary` object; existing fields are unchanged:

```json
{
  "id": "dev",
  "label": "dev",
  "local": false,
  "enabled": true,
  "active": true,
  "health": "healthy",
  "warnings": [],
  "last_attempt": 1784363100,
  "last_success": 1784363100,
  "duration_ms": 10,
  "error": "",
  "summary": { "projects": 4, "rounds": 7, "tokens": 812000 }
}
```

`summary` is present for active sources and omitted (or null) for sources without
an active snapshot. No existing `--dump-json` field is renamed or removed.

## Error handling

- The tab renders whatever `dataset.sources()` and `build_snapshot` already
  produce; it introduces no new failure modes.
- A config error still surfaces through the existing diagnostics line, which the
  tab may reuse beneath the footer (same source as `render_source_status`).
- A source without an active snapshot never crashes the summary path; it renders
  `—` and is excluded from the footer totals.

## Testing

- View-model builder: per-source `summary` equals the `Dataset` totals for that
  source's filter (tokens, rounds, projects).
- Footer totals equal the sum over active sources only.
- Disabled / error / duplicate-instance rows render and are excluded from the
  footer.
- Local-only configuration: the tab shows the local row and a footer with local
  totals.
- Additive JSON: `--dump-json=sources` includes `summary` for active sources and
  omits it for inactive ones; existing fields are byte-for-byte compatible for
  the current parity consumer.

## Acceptance criteria

- A fourth tab `Sources` is reachable by keyboard and mouse and lists every
  source with health, freshness, SSH duration, and warnings/errors.
- Each active source shows a projects / rounds / tokens summary matching the
  Usage tab's totals for that source with period = all.
- The footer sums only active sources.
- The `source` and `period` filter rows and their keys are inert on this tab and
  unchanged elsewhere.
- The `Sources` tab carries a health dot (green when all sources are fine, red
  otherwise); the former per-source header status row is gone.
- Account limits are shown for the local source only, under any source filter.
- `--dump-json=sources` gains a `summary` object without breaking existing
  fields.
- No new persisted state or protocol/`remote.json` schema change is introduced.
