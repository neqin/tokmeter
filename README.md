# tokmeter

GPUI desktop token-spend panel.

Same data layer (scan / cache / agg / pricing / limits), shared on-disk cache
with `tok` (`$XDG_CACHE_HOME/tok/cache.json` or `HERDR_PLUGIN_STATE_DIR`),
rendered in a native window on Zed's GPUI.

## Run

```sh
cargo run
```

### Headless dump

```sh
cargo run -- --dump-json           # GLOBAL week snapshot (JSON)
cargo run -- --dump-json=projects
cargo run -- --dump-json=rounds
cargo run -- --dump-json=limits
```

### Parity vs tok

```sh
bash scripts/parity_check.sh
# optional: TOK_PARITY_FIXTURE=/path/to/cache.json + expected.json
```

## Layout

- **Top:** subscription limits (claude / codex)
- **Tabs:** GLOBAL (default) · Projects · Rounds (no cwd Project tab)
- **GLOBAL:** period pills, bar chart, Σ/rate/rounds, BY AGENT, BY MODEL,
  top-10 projects
- **Keys:** Tab / Shift-Tab, ←/→ period or rounds agent, `r` refresh

## Env (same as tok)

| Var | Default |
|-----|---------|
| `TOK_REFRESH_SECS` | 3 |
| `TOK_LIMITS_TTL_SECS` | 300 |
| `TOK_WINDOW_DAYS` | 8 |
| `TOK_HISTORY_DAYS` | 120 |
| `XDG_CACHE_HOME` / `HERDR_PLUGIN_STATE_DIR` | cache location |
