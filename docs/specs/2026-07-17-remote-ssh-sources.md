# Remote SSH Sources for tokmeter

**Date:** 2026-07-17
**Status:** Draft awaiting final specification review

## Summary

Extend tokmeter so one desktop instance can collect Claude Code, Codex, and OMP
statistics from remote machines and LXC containers over SSH. A compatible
remote tokmeter performs the scan locally and exports a versioned JSON snapshot.
The local application keeps remote snapshots separate from the cache shared
with `tok`, combines them at presentation time, and preserves source identity
for filtering and diagnostics.

The default view sums local and remote statistics. Users can filter Usage,
Projects, Rounds, limits, and JSON output by source. A failed remote refresh
never blocks local collection or discards the last successful remote snapshot.

## Goals

- Collect Claude Code, Codex, and OMP sessions from one or more SSH hosts.
- Collect enabled account-limit snapshots from those hosts.
- Preserve the current local scanner and shared `tok` cache behavior.
- Prevent repeated refreshes and duplicate SSH aliases from double-counting a
  remote installation.
- Show combined totals by default while retaining source-level filtering.
- Keep the application useful when one or all remote hosts are unavailable.
- Introduce a tokmeter-owned configuration file for all current application
  settings while retaining environment-variable overrides.
- Preserve existing `--dump-json` consumers through additive schema changes.

## Non-goals for v1

- Reading raw remote JSONL files without a remote tokmeter installation.
- Installing or updating tokmeter on remote hosts.
- Password or passphrase entry in GPUI.
- Arbitrary SSH options or shell fragments in tokmeter configuration.
- A push daemon or network service.
- Global event-level deduplication of journals copied between distinct remote
  installations.
- Converting existing daily aggregates between different time zones.
- A GPUI editor for the configuration file.
- Changing the cache format shared with `tok`.
- Implementing the separate CLIProxy/Grok usage feature.

## Considered approaches

### 1. Pull a snapshot from remote tokmeter — selected

The local process runs a compatible tokmeter through OpenSSH. The remote binary
uses the existing local scanner and returns a compact snapshot.

Advantages:

- little network traffic;
- reuse of the existing Claude/Codex/OMP parsers;
- simple replacement semantics for refreshes;
- no remote filesystem protocol to maintain.

Cost:

- a compatible tokmeter binary must be installed on each host.

### 2. Read remote session files directly

This avoids installing tokmeter remotely, but requires remote file discovery,
metadata queries, tail reads, cursor persistence, shell quoting, and transfer of
potentially large JSONL data. It duplicates responsibilities already handled by
`Scanner` and is rejected for v1.

### 3. Push or daemon model

A remote agent could push snapshots or expose an API. This can provide lower
latency but introduces a service lifecycle, authentication, and a new network
surface. It is rejected for v1.

## Architecture

The implementation adds concrete components rather than a general collector
framework:

- `Config`: loads tokmeter-owned settings and applies environment overrides.
- `RemoteCollector`: invokes OpenSSH and validates one exported snapshot.
- `RemoteStore`: loads and atomically saves last-good snapshots and status.
- `Dataset`: presents the existing local `Cache` plus enabled remote snapshots
  to aggregation and rendering code.
- source-aware view models: retain `source_id` where identity matters.

The existing local `Scanner`, parsers, pricing, and shared `tok` cache remain the
source of local statistics.

### Paths

Configuration:

```text
$XDG_CONFIG_HOME/tokmeter/config.json
~/.config/tokmeter/config.json
```

Remote cache:

```text
$XDG_CACHE_HOME/tokmeter/remote.json
~/.cache/tokmeter/remote.json
```

Installation identity state:

```text
$XDG_STATE_HOME/tokmeter/instance-id
~/.local/state/tokmeter/instance-id
```

Files created by tokmeter are written atomically with mode `0600`. The remote
cache contains project paths and is treated as private even though it contains
no credentials.

The existing shared cache path resolution, including
`HERDR_PLUGIN_STATE_DIR`, is unchanged.

## Configuration

### Schema

The initial schema is:

```json
{
  "version": 1,
  "local_sources": {
    "claude": true,
    "codex": true,
    "omp": true
  },
  "limits": {
    "claude": true,
    "codex": true,
    "grok": true
  },
  "refresh": {
    "ui_secs": 3,
    "limits_ttl_secs": 300,
    "remote_secs": 60,
    "ssh_connect_timeout_secs": 5,
    "ssh_command_timeout_secs": 30
  },
  "retention": {
    "scan_days": 8,
    "history_days": 120,
    "hours_days": 8,
    "files_days": 14
  },
  "ssh_sources": [
    {
      "id": "lxc-agents",
      "label": "LXC agents",
      "host": "agents-lxc",
      "enabled": true,
      "binary": "tokmeter"
    }
  ]
}
```

### Semantics

- `id` is a unique, stable local identifier used by filters and cache entries.
  It must match `[a-z0-9][a-z0-9_-]{0,63}`; `all` and `local` are reserved.
- `label` is a nonempty user-visible source name of at most 64 characters.
- `host` is an OpenSSH alias or destination such as `user@host`. It must be
  nonempty, must not start with `-`, and may not contain whitespace or control
  characters.
- `binary` defaults to `tokmeter`. It may be an absolute path or a safe command
  token; whitespace, control characters, quotes, and shell metacharacters are
  rejected.
- SSH users, ports, keys, ProxyJump, and host-key policy belong in
  `~/.ssh/config`.
- Missing configuration uses the current local defaults and enables no SSH
  sources.
- Missing fields use the defaults shown above.
- Unknown fields are ignored for forward compatibility.
- An unsupported config version, duplicate source ID, or invalid source field is
  a configuration error. SSH collection is disabled until the error is
  corrected, while local collection continues.
- A malformed configuration never overwrites the file. The GUI and JSON source
  status expose the error, and local collection continues with safe defaults.
- Disabled sources are excluded from Dataset but their last-good snapshots are
  retained.

### Environment compatibility

Environment variables override file values:

- `TOK_REFRESH_SECS` over `refresh.ui_secs`;
- `TOK_LIMITS_TTL_SECS` over `refresh.limits_ttl_secs`;
- `TOK_WINDOW_DAYS` over `retention.scan_days`;
- `TOK_HISTORY_DAYS` over `retention.history_days`;
- `TOK_HOURS_DAYS` over `retention.hours_days`;
- `TOK_FILES_DAYS` over `retention.files_days`.

Invalid environment values fall back to the file value rather than replacing it
with a separate default.

The config file is manually edited in v1. No Settings editor is included.

### Relationship to the CLIProxy settings specification

`docs/specs/2026-07-15-cliproxy-grok-usage-settings.md` currently proposes
`$XDG_CONFIG_HOME/tok/config.json` and a full Settings tab. When that separate
feature is planned for implementation, its specification must be revised to use
the tokmeter-owned config defined here. This SSH feature does not implement the
CLIProxy client or Settings editor.

## Installation identity

Every tokmeter installation has one stable instance ID. The local source and
`--export-source-json` use the same ID.

On first use, tokmeter reads one UUID from `/proc/sys/kernel/random/uuid` and
persists it atomically in the instance-state file. If the UUID source is
unavailable, or the state file cannot be read or created, remote export fails
because deduplication cannot be guaranteed. The normal local GUI continues in
local-only mode until the state problem is fixed.

The ID identifies one tokmeter installation and its local session roots. It is
not a host credential and is transmitted only in the SSH response.

## Remote export protocol

### Invocation

The collector invokes the system OpenSSH client without a local shell:

```text
ssh -o BatchMode=yes -o ConnectTimeout=5 -- agents-lxc \
  tokmeter --export-source-json
```

The destination and remote binary are validated before invocation. No arbitrary
`ssh_args` field is supported.

OpenSSH still executes the remote command through the remote login environment,
so the binary token validation is mandatory. A non-default path is passed as one
validated token.

`BatchMode=yes` forbids password and passphrase prompts. tokmeter does not alter
OpenSSH host-key behavior. A new host must first be verified through a normal
interactive `ssh <alias>` session.

### Export behavior

`--export-source-json`:

- runs headless;
- scans only the remote machine's local Claude/Codex/OMP roots enabled by its
  config;
- refreshes enabled local account limits;
- never invokes configured SSH sources, preventing recursion;
- emits exactly one JSON document to stdout;
- emits diagnostics only to stderr;
- returns nonzero when it cannot produce a valid snapshot.

If the shared local cache lock on the remote machine is unavailable, export may
return the loaded last-good local cache with a structured `read_only` refresh
status. The caller marks the source with a warning rather than treating the JSON
as fresh scan output.

### Payload

Protocol v1 is conceptually:

```json
{
  "protocol": "tokmeter-source-snapshot",
  "protocol_version": 1,
  "app_version": "0.1.8",
  "cache_version": 5,
  "instance_id": "stable-generated-id",
  "generated_at": 1784300000,
  "utc_offset_secs": 10800,
  "refresh_status": "fresh",
  "retention": {
    "history_days": 120,
    "hours_days": 8
  },
  "data": {
    "agg": {},
    "hours": {},
    "rounds": [],
    "limits": {}
  }
}
```

The data collections use the existing compact cache representations. They do
not contain the local config source ID; the receiving configuration assigns it.

### Validation

Before replacing a stored snapshot, the collector validates:

- exact protocol name;
- supported protocol version;
- supported cache representation;
- nonempty instance ID;
- generated timestamp;
- expected object/array shapes and numeric ranges;
- output-size limit;
- effective UTC offset;
- retention metadata.

Unknown fields are ignored. `app_version` is diagnostic only; compatibility is
decided by `protocol_version` and the supported cache representation. Protocol
v1 does not promise compatibility with a higher protocol version; an unsupported
version is `incompatible`.

The stdout limit is 16 MiB. stderr is drained but only a short bounded and
sanitized excerpt is retained for status display. A bounded subprocess helper
must drain both streams while enforcing the command timeout, so a full pipe
cannot deadlock the process.

## Time-zone rule

Current cache aggregates are dated using the scanner's local UTC offset. V1
therefore requires a remote snapshot's effective `utc_offset_secs` to match the
local application's offset. A mismatch is `incompatible`, the new snapshot is
not activated, and the last compatible snapshot remains available as stale.

Cross-time-zone rebucketing is a separate feature because daily aggregates
cannot be converted exactly without finer-grained retained data.

## Remote store

`RemoteStore` is versioned independently of the shared `tok` cache. For each
configured source it retains:

- configured source ID and the last observed label;
- remote instance ID;
- last-good protocol snapshot;
- last attempt timestamp;
- last success timestamp;
- last duration;
- primary health state;
- warnings;
- bounded safe error text.

A successful validated refresh replaces the source snapshot atomically. Counts
are never incrementally added to an existing remote snapshot. The transient
`connecting` state is not persisted; after restart a previously in-flight source
is derived from its last success and last-good snapshot.

Removing or disabling a source excludes it from Dataset immediately. V1 retains
its cached snapshot until the same source ID is configured again or the user
removes the remote cache; no automatic orphan cleanup runs.

## Duplicate prevention

The rules are:

1. Repeated refresh of one source replaces its previous snapshot.
2. Source IDs must be unique in configuration.
3. One instance ID may contribute to Dataset only once.
4. The local installation's instance ID participates in the same check, so an
   SSH alias pointing back to the local installation is excluded.
5. If multiple enabled configured sources resolve to the same remote instance,
   the first source in config order contributes data. Later sources are marked
   `duplicate_instance` and excluded.
6. If an existing source ID returns a new instance ID after a valid SSH and
   protocol check, the new snapshot replaces the old installation's snapshot;
   data from both instances is never combined under one source ID.
7. Journals copied to distinct installations with different instance IDs are
   treated as distinct data in v1.

## Dataset and aggregation

`Dataset` exposes one local snapshot and zero or more enabled remote snapshots.
It does not rewrite or merge their underlying cache maps.

Each view build receives a source filter:

- `all` includes the local source and every active remote snapshot;
- `local` includes only the existing local cache;
- a configured source ID includes only that remote snapshot.

An enabled source's last compatible snapshot remains active while its health is
`healthy`, `stale`, or `incompatible`. Disabled and duplicate-instance sources
are excluded. An error source without a last-good snapshot contributes no data.

Aggregation rules:

- agent totals, model totals, chart buckets, rates, and headline totals sum the
  selected snapshots;
- project identity is `(source_id, project_path)`;
- round identity retains `source_id`;
- account limits retain `(source_id, agent)` and are never numerically combined;
- source status is available independently of the selected data filter.

The current local aggregate keys remain unchanged. Source identity exists at the
snapshot boundary and in source-aware view models, avoiding a shared cache
migration.

## Refresh flow

### Startup

1. Load config.
2. Load the local shared cache.
3. Load RemoteStore.
4. Apply enabled-source and duplicate-instance rules to cached snapshots.
5. Render immediately from available data.
6. Start local refresh and due remote refresh work in the background.

### Periodic work

- Local refresh retains the current UI cadence.
- Remote refresh uses `refresh.remote_secs`, default 60 seconds.
- Pressing `r` forces a local refresh and one attempt for every enabled remote
  source.
- One remote source does not delay presentation of another source's result.
- Sources run concurrently with a fixed maximum of four subprocesses.
- SSH work occurs outside the shared local cache lock.
- A completed source updates RemoteStore and notifies GPUI independently.

### Headless dump

A normal `--dump-json` invocation performs the local refresh and one bounded
attempt for each enabled SSH source before writing JSON. Remote failure does not
cancel the dump; last-good data and source status are returned.

`--export-source-json` is the exception: it never runs SSH sources.

## Source health

Primary health is one of:

- `disabled`;
- `connecting`;
- `healthy`;
- `stale`;
- `error`;
- `incompatible`;
- `duplicate_instance`.

Warnings may coexist with health:

- `partial_history` when remote retention is shorter than local retention;
- `read_only_refresh` when export could not update its local shared cache.

A source is stale when its last attempt failed while a last-good snapshot exists,
or when its last success is older than:

```text
max(3 * remote_secs, 300 seconds)
```

A failed source with no last-good snapshot is `error`.

An incompatible or invalid new response never replaces the last compatible
snapshot. If old data exists it remains available and is visibly marked stale or
incompatible.

## Error handling

- A config error disables SSH collection but not local collection.
- Failure of one remote source does not affect other sources.
- Connect and command timeouts terminate the subprocess.
- Nonzero SSH or remote exit status produces a bounded transport/remote error.
- Invalid JSON or protocol produces a protocol error.
- Remote-store write failure is reported separately from collection success.
- No error clears the last-good snapshot unless the user removes the cache file.
- Status text must not include credentials or unbounded remote output.

## GPUI design

### Source filter

A single filter row appears above the content shared by Usage, Projects, and
Rounds:

```text
Source: all sources | local | LXC agents | ...
```

- `all sources` is the default;
- mouse selection changes the source;
- `s` cycles forward and `Shift+s` cycles backward;
- period and agent keyboard behavior remain unchanged;
- filtering does not recolor surviving chart data.

The existing token chart remains one aggregate series. Source identity is a
filter and table dimension, not a categorical chart series.

### Health row

A compact row beneath the filters shows every configured source:

```text
Sources: ✓ local now   ✓ LXC agents 12s   ⚠ workstation stale 8m
```

Status uses an icon and text in addition to color. Long errors are not rendered
inline; a short detail is shown in the existing status/detail area.

### Usage

Headlines, agents, models, rate, rounds, and chart buckets reflect the selected
source filter. Agent and model labels do not embed source names.

### Projects

In `all sources`, each project row includes a source label. Equal paths on
different sources remain separate rows. With one source selected, the redundant
source label may be hidden.

### Rounds

Each round view contains source label, agent, model, project, tokens, and cost.
The current agent filter applies after the source filter.

### Limits

Limits are grouped by source, local first and remote sources in configuration
order. Percentages are not summed across sources. Stale snapshots retain their
bars with an explicit age/status marker.

### Accessibility and visual rules

- Health is never represented by color alone.
- Source labels use normal text or badges, not series colors.
- The existing chart remains single-series and needs no new categorical legend.
- Source controls stay in one filter row above the visualization.

## CLI and JSON

Existing modes remain:

- `--dump-json`;
- `--dump-json=projects`;
- `--dump-json=rounds`;
- `--dump-json=limits`.

New interfaces:

- `--dump-json=sources`;
- `--source=<id>` where `<id>` is `local` or a configured source ID;
- internal `--export-source-json`.

An unknown `--source` value is a usage error and exits nonzero without emitting a
misleading fallback dataset.

Existing JSON fields are not renamed or removed. Additive changes are:

- top-level `sources` status array in the global dump;
- `source_id` in project entries;
- `source_id` in round entries;
- `source_id` in limit entries;
- source metadata in `--dump-json=sources`.

Without `--source`, existing totals represent all active sources. The current
parity consumer continues to read its existing fields.

## Security and privacy

- tokmeter relies on OpenSSH configuration and ssh-agent; it stores no SSH
  private keys, passwords, or passphrases.
- `BatchMode=yes` prevents GUI subprocesses from waiting for input.
- Host keys are never accepted automatically.
- Destination and binary values are validated and passed as arguments.
- No arbitrary remote command is configurable.
- stdout and stderr are bounded.
- Config, remote cache, and instance state written by tokmeter use mode `0600`.
- Remote error excerpts are sanitized before display or persistence.
- Project paths and usage data remain local to the user's machines and SSH
  channel.

## Testing

### Config tests

- no-file defaults;
- complete and partial configuration;
- environment override precedence;
- invalid environment fallback to file value;
- duplicate source ID;
- invalid destination or binary;
- malformed JSON with local-mode fallback.

### Protocol tests

- valid protocol v1;
- unknown additional fields;
- unsupported protocol/cache version;
- missing instance ID;
- malformed collection shapes;
- time-zone mismatch;
- short retention warning;
- oversized stdout;
- malformed JSON;
- bounded stderr.

### RemoteStore and Dataset tests

- replacement rather than addition on refresh;
- repeated identical refresh leaves totals unchanged;
- two source IDs with one instance ID count once;
- local instance reached through SSH is excluded;
- failed refresh retains last-good data;
- invalid response does not replace valid data;
- disabled source remains cached but excluded;
- all-source totals equal the sum of active snapshots;
- one-source filtering;
- same project path remains separate by source;
- rounds retain source ID;
- limits remain separate by source and agent.

### Subprocess tests

Use a temporary fake `ssh` executable through a test-scoped `PATH`, protected by
an environment mutex as in existing engine tests:

- success;
- nonzero exit;
- connect/command timeout;
- large stdout and stderr;
- no stdin interaction;
- concurrent sources;
- maximum concurrency bound.

### UI and CLI tests

- source-filter cycling;
- source labels in Projects and Rounds;
- grouped limits;
- text/icon status independent of color;
- additive backward-compatible dump fields;
- `--dump-json=sources`;
- valid and invalid `--source`;
- export mode does not run SSH recursively.

### Repository checks

- formatting;
- clippy;
- unit tests;
- existing parity script;
- project end-to-end verification.

## Real LXC verification

Before completion:

1. Install compatible tokmeter builds locally and in the LXC container.
2. Verify the host key through an ordinary interactive SSH session.
3. Run remote `tokmeter --export-source-json` manually.
4. Compare its data with a local dump executed inside the container.
5. Add the source to the desktop application's config.
6. Confirm that a second refresh with no new sessions does not increase totals.
7. Confirm source filtering in Usage, Projects, Rounds, limits, and JSON.
8. Stop the container and confirm that local collection continues while remote
   data becomes stale.
9. Restart the container and confirm recovery to healthy.
10. Configure a second alias to the same container and confirm it is excluded as
    a duplicate instance.

## Acceptance criteria

- With no config file, tokmeter behaves as it does before this feature.
- A configured compatible LXC source contributes Claude/Codex/OMP sessions and
  enabled account limits to the default combined view.
- Repeated SSH refreshes do not double-count unchanged remote data.
- One installation reached through multiple aliases contributes once.
- Source filtering works consistently in all tabs and JSON modes.
- Equal project paths on different sources retain their source identity.
- Remote limits remain separate rather than being numerically combined.
- A failed remote source does not block GPUI, local scanning, another remote
  source, or the shared local cache lock.
- Last-good remote data remains available with explicit stale status.
- Existing environment settings and JSON consumers remain compatible.
- Automated checks and real-LXC verification pass.
