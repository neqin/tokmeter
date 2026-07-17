# tokmeter

GPUI desktop token-spend panel for Claude Code, Codex, and OMP. It scans the
local machine and can collect compact snapshots from compatible tokmeter
installations over OpenSSH.

The local data layer and on-disk cache remain compatible with `tok`. Remote
snapshots are stored separately and are combined only when building dashboard
or JSON views.

## Run

```sh
cargo run
```

## Remote SSH sources

tokmeter reads its optional configuration from:

```text
$XDG_CONFIG_HOME/tokmeter/config.json
~/.config/tokmeter/config.json          # when XDG_CONFIG_HOME is unset
```

A complete configuration with one SSH source looks like this:

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
      "id": "lxc",
      "label": "Development LXC",
      "host": "user@dev-lxc",
      "enabled": true,
      "binary": "/usr/local/bin/tokmeter"
    }
  ]
}
```

Missing fields use the defaults shown above. Source IDs must start with a
lowercase letter or digit and may contain lowercase letters, digits, `_`, and
`-`; `all` and `local` are reserved. `host` may be an OpenSSH alias or a
`user@host` value. `binary` is passed as one argument and therefore cannot
contain spaces or shell syntax.

### SSH prerequisites

1. Install a protocol- and cache-compatible tokmeter binary on the remote host.
2. Configure the host in OpenSSH as needed, including `ProxyJump` or identity
   files in `~/.ssh/config`.
3. Connect with `ssh <host>` once and verify the host key interactively.
4. Configure non-interactive public-key or agent authentication.
5. Verify the remote export:

   ```sh
   ssh -o BatchMode=yes -- user@dev-lxc /usr/local/bin/tokmeter --export-source-json
   ```

tokmeter invokes OpenSSH without a local shell, closes stdin, enables
`BatchMode=yes`, and applies connect and command timeouts. It does not accept
host keys, store SSH passwords or passphrases, or expose arbitrary SSH argument
configuration.

The remote export contains aggregate usage, hourly totals, recent rounds, and
limits. It never contains file cursors or raw session files. Remote snapshots
must use the same UTC offset as the local installation at generation time;
timezone-incompatible snapshots are rejected while the last compatible
snapshot is retained.

A successful refresh replaces the previous snapshot for that source. Stable
installation IDs prevent double counting when two SSH aliases point to the same
tokmeter installation or when an alias points back to the local installation.
The first source in configuration order wins. A last-good snapshot remains
available after an SSH failure and becomes stale; the automatic stale threshold
is `max(3 * remote_secs, 300)` seconds.

### Storage

| Data | XDG path | HOME fallback |
|------|----------|---------------|
| Local cache shared with `tok` | `$XDG_CACHE_HOME/tok/cache.json` | `~/.cache/tok/cache.json` |
| Remote snapshots and health | `$XDG_CACHE_HOME/tokmeter/remote.json` | `~/.cache/tokmeter/remote.json` |
| Stable installation ID | `$XDG_STATE_HOME/tokmeter/instance-id` | `~/.local/state/tokmeter/instance-id` |

`HERDR_PLUGIN_STATE_DIR/cache.json` still overrides the shared local cache path.
Remote snapshots and the installation ID are written atomically with mode
`0600`; remote data never enters the cache shared with `tok`.

## Headless JSON

```sh
cargo run -- --dump-json
cargo run -- --dump-json=global
cargo run -- --dump-json=projects
cargo run -- --dump-json=rounds
cargo run -- --dump-json=limits
cargo run -- --dump-json=sources
```

The default source filter is `all`. Select local data or one configured SSH
source with:

```sh
cargo run -- --dump-json --source=local
cargo run -- --dump-json=projects --source=lxc
```

A normal dump performs one bounded remote refresh batch before emitting JSON.
`--dump-json=sources` reports source health, warnings, timing, and errors.
Configuration, identity, and remote-cache problems are exposed through the
additive `diagnostics` field.

`--export-source-json` is an internal local-only protocol endpoint for remote
collection. It never starts another SSH collection.

## Dashboard

- **Top:** shared source filter, source health, and Claude/Codex subscription
  limits. Grok collection can remain enabled for export, but Grok is not shown
  in dashboard limit rows.
- **Tabs:** GLOBAL (default), Projects, and Rounds.
- **GLOBAL:** period pills, one aggregate bar chart, totals/rate/rounds, BY
  AGENT, BY MODEL, and top projects.
- **Projects/Rounds:** source identity is shown when the combined `all` filter
  is selected.
- **Keys:** Tab / Shift-Tab, ←/→ period or rounds agent, `s` / Shift-`s` source,
  and `r` to reload configuration and refresh local and remote data.

Source health is shown with both an icon and text: `disabled`, `connecting`,
`healthy`, `stale`, `error`, `incompatible`, or `duplicate instance`.

## Environment overrides

Environment variables override matching configuration values. Invalid values
fall back to the configured value.

| Variable | Default |
|----------|---------|
| `TOK_REFRESH_SECS` | 3 |
| `TOK_LIMITS_TTL_SECS` | 300 |
| `TOK_WINDOW_DAYS` | 8 |
| `TOK_HISTORY_DAYS` | 120 |
| `TOK_HOURS_DAYS` | 8 |
| `TOK_FILES_DAYS` | 14 |
| `XDG_CONFIG_HOME` | `~/.config` |
| `XDG_CACHE_HOME` | `~/.cache` |
| `XDG_STATE_HOME` | `~/.local/state` |
| `HERDR_PLUGIN_STATE_DIR` | shared local cache override |

If the configuration is malformed or unsupported, tokmeter reports a bounded
diagnostic and continues with safe local-only defaults.

## Parity with tok

The parity check always selects the local source so configured remote snapshots
do not affect the comparison:

```sh
bash scripts/parity_check.sh
# optional: TOK_PARITY_FIXTURE=/path/to/cache.json + expected.json
```
