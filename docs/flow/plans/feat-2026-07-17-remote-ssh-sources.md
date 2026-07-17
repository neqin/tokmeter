+++
slug = "remote-ssh-sources"
type = "feat"
created = "2026-07-17"
status = "approved"
mode = "worktree"
tdd = false
review_after_group = false
checkpoints = []
auto_commit = false
goal = "Collect Claude Code, Codex, and OMP statistics and account limits from compatible remote tokmeter installations over SSH, combine them with local data without double counting, and expose source filtering and health in GPUI and JSON while preserving the shared tok cache."
non_goals = [
    "Read remote JSONL files without remote tokmeter.",
    "Install or update tokmeter on remote hosts.",
    "Add password or passphrase UI, arbitrary SSH arguments, a push daemon, or a network service.",
    "Deduplicate copied journals across distinct installation IDs or rebucket aggregates across time zones.",
    "Add a GPUI config editor, change the shared tok cache format, or implement CLIProxy usage.",
]
max_review_iterations = 2
worktree_include = [
    "src/data/cache.rs",
    "src/data/engine.rs",
    "src/data/limits.rs",
    "src/data/scan.rs",
    "docs/specs/2026-07-17-remote-ssh-sources.md",
    "docs/flow/plans/feat-2026-07-17-remote-ssh-sources.md",
]

[[task]]
id = "T1"
title = "Protect the imported baseline and add private XDG file helpers"
status = "pending"
stage = "foundation"
depends_on = []
files_create = ["src/data/private_io.rs"]
files_modify = ["src/data/mod.rs"]
verify = [
    "cargo test data::private_io::tests",
    "cargo test data::scan::tests::skips_inherited_codex_subagent_history",
    "cargo test data::scan::tests::scans_legacy_codex_subagent_without_replay_marker",
    "cargo test data::limits::tests::rows_hide_additional_codex_windows",
    "git diff --check",
]

[[task]]
id = "T2"
title = "Add the versioned tokmeter config with validation and environment overrides"
status = "pending"
stage = "foundation"
depends_on = ["T1"]
files_create = ["src/data/config.rs"]
files_modify = ["src/data/mod.rs", "Cargo.toml", "Cargo.lock"]
verify = ["cargo test data::config::tests"]

[[task]]
id = "T3"
title = "Add stable installation identity state"
status = "pending"
stage = "foundation"
depends_on = ["T1"]
files_create = ["src/data/identity.rs"]
files_modify = ["src/data/mod.rs"]
verify = ["cargo test data::identity::tests"]

[[task]]
id = "T4"
title = "Extract compact cache data and implement remote snapshot protocol v1"
status = "pending"
stage = "data"
depends_on = ["T2", "T3"]
files_create = ["src/data/protocol.rs"]
files_modify = ["src/data/mod.rs", "src/data/cache.rs"]
verify = [
    "cargo test data::cache::tests",
    "cargo test data::protocol::tests",
    "cargo test data::scan::tests::skips_inherited_codex_subagent_history",
    "cargo test data::scan::tests::scans_legacy_codex_subagent_without_replay_marker",
]

[[task]]
id = "T5"
title = "Make local refresh config-driven and add local-only source export"
status = "pending"
stage = "data"
depends_on = ["T4"]
files_create = []
files_modify = ["src/data/scan.rs", "src/data/engine.rs", "src/main.rs"]
verify = [
    "cargo test data::engine::tests",
    "cargo test data::scan::tests",
    "cargo test export_source_tests",
]

[[task]]
id = "T6"
title = "Persist versioned last-good remote snapshots and source status"
status = "pending"
stage = "remote"
depends_on = ["T4"]
files_create = ["src/data/remote_store.rs"]
files_modify = ["src/data/mod.rs"]
verify = ["cargo test data::remote_store::tests"]

[[task]]
id = "T7"
title = "Add bounded OpenSSH collection and fixed-size refresh coordination"
status = "pending"
stage = "remote"
depends_on = ["T4", "T6"]
files_create = ["src/data/remote.rs"]
files_modify = ["src/data/mod.rs"]
verify = ["cargo test data::remote::tests -- --test-threads=1"]

[[task]]
id = "T8"
title = "Add source activation, instance deduplication, filters, and aggregate composition"
status = "pending"
stage = "data"
depends_on = ["T6"]
files_create = ["src/data/dataset.rs"]
files_modify = ["src/data/mod.rs", "src/data/agg.rs"]
verify = [
    "cargo test data::dataset::tests",
    "cargo test data::agg::tests",
]

[[task]]
id = "T9"
title = "Make view snapshots, CLI, and JSON source-aware"
status = "pending"
stage = "integration"
depends_on = ["T5", "T7", "T8"]
files_create = []
files_modify = ["src/main.rs"]
verify = [
    "cargo test cli_tests",
    "cargo test json_schema_tests",
]

[[task]]
id = "T10"
title = "Integrate independent local and remote refresh lifecycles into GPUI"
status = "pending"
stage = "integration"
depends_on = ["T9"]
files_create = []
files_modify = ["src/main.rs"]
verify = ["cargo test dashboard_refresh_tests"]

[[task]]
id = "T11"
title = "Render source filters, health, projects, rounds, and grouped limits"
status = "pending"
stage = "ui"
depends_on = ["T10"]
files_create = []
files_modify = ["src/main.rs"]
verify = [
    "cargo test source_filter_tests",
    "cargo test source_status_tests",
    "cargo test source_aware_view_tests",
    "cargo test data::limits::tests::rows_hide_additional_codex_windows",
]

[[task]]
id = "T12"
title = "Document remote sources, preserve local parity, and run the release gate"
status = "pending"
stage = "verify"
depends_on = ["T11"]
files_create = []
files_modify = ["README.md", "scripts/parity_check.sh"]
verify = [
    "cargo fmt -- --check",
    "cargo clippy --all-targets -- -D warnings",
    "cargo test",
    "git diff --check",
    "bash scripts/parity_check.sh",
]
+++

# Implementation Plan: remote-ssh-sources

## Goal

Implement the approved design from
`docs/specs/2026-07-17-remote-ssh-sources.md`: collect compact local snapshots
from compatible remote tokmeter installations through OpenSSH, retain last-good
remote data separately from the shared `tok` cache, combine active sources in a
source-aware Dataset, and expose combined totals, filtering, health, CLI, JSON,
and GPUI behavior.

## Non-goals

- No raw JSONL transport, remote installation/update, password UI, arbitrary SSH
  arguments, push daemon, or network service.
- No cross-installation event deduplication or cross-time-zone rebucketing.
- No GPUI config editor, CLIProxy implementation, shared `tok` cache migration,
  or unrelated dashboard refactor.

## Protected baseline

The worktree imports existing uncommitted user changes. They are requirements,
not cleanup targets:

- `src/data/cache.rs` stays at cache v5 and retains its current recent-history
  rebuild semantics.
- `src/data/engine.rs` keeps the history-window upgrade rebuild.
- `src/data/scan.rs` keeps both Codex subagent replay fixes and tests.
- `src/data/limits.rs` continues to render only Claude and Codex limits; Grok
  may remain configurable for fetch/export but is not restored to GPUI rows.

Do not restore these files from `HEAD`, replace them wholesale, or include
unrelated refactors.

## Architecture

### Configuration and identity

Add a tokmeter-owned JSON config under XDG config with typed defaults,
validation, environment overrides, and local-only fallback diagnostics. Add a
stable installation UUID under XDG state. Config is reloaded at startup, on
manual `r`, and in every headless invocation; v1 has no periodic config hot
reload.

### Local and remote storage boundaries

The cache shared with `tok` remains local and schema-compatible. Extract only
its compact `agg`, `hours`, `rounds`, and `limits` representation for export.
Remote snapshots and source status live in a separate versioned, private,
atomically-written cache. Remote file cursors and source IDs never enter the
shared cache.

### SSH protocol and process handling

`--export-source-json` performs a local-only configurable refresh and emits one
versioned JSON snapshot. The collector uses `std::process::Command` with
OpenSSH, `BatchMode=yes`, bounded connect/command timeouts, closed stdin,
parallel stdout/stderr draining, a 16 MiB stdout cap, bounded sanitized stderr,
kill/reap on timeout, and at most four concurrent sources.

### Dataset and deduplication

Dataset presents local cache plus active last-good remote snapshots without
physically merging maps. It applies source filtering, disabled/error rules,
staleness, config-order instance deduplication, and local-loop exclusion before
aggregation. A refresh replaces one source snapshot rather than incrementing
it. Projects use `(source_id, path)` identity; rounds and limits retain source.

### CLI, JSON, and GPUI

Keep existing dump fields and add source metadata, `--dump-json=sources`, and
`--source=<id>`. Normal dump performs one bounded remote batch; export never
recurses. GPUI renders cached data immediately, schedules local and remote work
independently, reloads config on `r`, and adds one shared source filter plus an
icon-and-text health row. The token chart remains one aggregate series.

## Affected files

Expected create:

- `src/data/private_io.rs`
- `src/data/config.rs`
- `src/data/identity.rs`
- `src/data/protocol.rs`
- `src/data/remote_store.rs`
- `src/data/remote.rs`
- `src/data/dataset.rs`

Expected modify:

- `Cargo.toml`, `Cargo.lock`, `src/data/mod.rs`
- `src/data/cache.rs`, `src/data/scan.rs`, `src/data/engine.rs`
- `src/data/agg.rs`, `src/main.rs`
- `README.md`, `scripts/parity_check.sh`

Do not modify `.env*`, `pricing.json`, project instruction files, the approved
specification, or CHANGELOG again.

## Decisions

- Use one compatible remote tokmeter instead of transferring raw JSONL files.
- Use concrete config/protocol/store/dataset modules rather than a generic
  collector framework.
- Keep `auto_commit = false` because the worktree contains pre-existing user WIP;
  ask once before the final commit.
- Preserve Grok as hidden in current GPUI limits rows.
- Compare timezone offsets for the snapshot's `generated_at`; a mismatched new
  snapshot is incompatible and does not replace last-compatible data.
- Expose config/identity/store problems through additive JSON diagnostics rather
  than synthetic source entries.

## Risks

- Existing user WIP could be lost or committed accidentally; use `worktree_include`,
  never restore protected files, keep `auto_commit = false`, and rerun protected
  tests before completion.
- Remote fields could leak into the shared cache; isolate them in compact protocol,
  RemoteStore, and Dataset and test that exported data excludes file cursors.
- SSH subprocesses could hang or deadlock on full pipes; drain both streams,
  enforce memory/time bounds, kill/reap, and cap concurrency at four.
- Concurrent completions could lose updates; apply ordered attempt results through
  one store writer and ignore stale outcomes.
- Duplicate aliases could inflate totals; deduplicate local and remote installation
  IDs before aggregation.
- Default all-source output could break parity; run parity with `--source=local`.
- Large changes in `main.rs` could regress existing UI; build and test the data
  layer first and avoid unrelated rendering refactors.

## Acceptance criteria

- [ ] No config preserves current local-only behavior.
- [ ] Compatible SSH sources contribute Claude/Codex/OMP data and enabled limits
      to the combined view.
- [ ] Repeated refresh and duplicate SSH aliases do not increase totals.
- [ ] Remote data never enters the shared `tok` cache.
- [ ] Source filtering is consistent across Usage, Projects, Rounds, limits, and
      JSON.
- [ ] Equal project paths on different sources remain distinguishable.
- [ ] Remote failures do not block local scan, GPUI, another source, or the
      shared cache lock; last-good data becomes stale.
- [ ] Existing JSON fields, environment variables, Codex replay behavior, cache
      v5 behavior, and Claude/Codex-only limit rows remain compatible.
- [ ] Automated tests, formatting, clippy, parity, and project verification pass.
- [ ] Real LXC verification is completed when an SSH alias and compatible remote
      binary are available; otherwise it is reported as skipped, never passed.

## Real LXC verification

1. Verify the host key interactively outside tokmeter.
2. Run remote `tokmeter --export-source-json` and compare it with a dump inside
   the container.
3. Confirm two refreshes without new sessions leave totals unchanged.
4. Confirm source filters in GPUI and JSON.
5. Stop and restart the container and observe stale then healthy.
6. Configure a second alias to the same container and confirm duplicate-instance
   exclusion.
7. Confirm an alias back to the local installation is excluded.
8. Confirm a timezone mismatch retains the old compatible snapshot.

## Progress Log

- 2026-07-17: specification approved; worktree mode selected; Grok remains
  hidden in GPUI limits; implementation plan approved.
- 2026-07-17: T1 completed — protected baseline tests pass; private XDG path and
  atomic mode-0600 write helpers added.
- 2026-07-17: T2 completed — versioned tokmeter config, validation, defaults,
  source settings, and environment override precedence added.
- 2026-07-17: T3 completed — stable race-safe installation identity state and
  failure behavior added.
- 2026-07-17: T4 completed — shared cache compact codec and validated remote
  snapshot protocol v1 added without changing cache v5 or file cursors.
- 2026-07-17: T5 completed — local scans and limit fetches honor config, lock
  contention reports read-only, and local-only source export passes smoke tests.
- 2026-07-17: T6 completed — separate versioned private RemoteStore retains
  last-good snapshots across failures and rejects corrupt persistence safely.
- 2026-07-18: T7 completed — bounded OpenSSH collection drains both pipes,
  validates snapshots, kills timed-out process groups, and coordinates at most
  four completion-ordered workers with stale-generation protection.
- 2026-07-18: T8 completed — Dataset applies config-order activation, age-based
  staleness, local/remote instance deduplication, validated source filters, and
  source-aware totals, projects, rounds, and limits without merging cache maps.
- 2026-07-18: T9 completed — typed dump arguments, one-shot remote refresh,
  source-aware owned views, additive source JSON and diagnostics, and cached
  remote data in the initial GPUI snapshot are implemented.
- 2026-07-18: T10 completed — GPUI local and remote refreshes run on independent
  dynamic cadences, apply each SSH completion sequentially, persist snapshots,
  preserve forced reruns, and reload config on manual refresh.
- 2026-07-18: T11 completed — the dashboard provides clickable and keyboard
  source filters, icon-and-text health status, and source-aware project, round,
  and Claude/Codex limit rows while keeping Usage as one aggregate series.
- 2026-07-18: T12 completed — README documents configuration, OpenSSH security,
  source behavior, storage, JSON, and dashboard controls; parity is explicitly
  local-only and all formatting, clippy, test, diff, and parity gates pass.
- 2026-07-18: Real LXC verification skipped because no SSH alias and compatible
  remote binary were provided in this session. Project `/verify` was also
  unavailable because this checkout has no verify skill; the explicit release
  gate commands were run instead.
