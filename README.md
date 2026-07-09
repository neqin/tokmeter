# tokmeter

GPUI prototype of the [`tok`](../tok) token-spend panel — same layout as the
terminal UI (tabs, period pills, bar chart, BY AGENT / BY MODEL), rendered in
a native window on Zed's GPUI engine.

Demo data is hardcoded from the reference screenshot. Tabs and period pills
are clickable; Projects/Rounds tabs are stubs.

## Run

```sh
cargo run
```

Requires a Wayland or X11 display (same stack as `zed`).

## Notes

- GPUI pin matches `zed`: Zed rev `fca4d60…`
- Bar chart is drawn with `canvas` + `paint_quad`
- Tables are flex layout + mono font
