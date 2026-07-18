# Changelog

## [Unreleased]

## 0.1.9 — 2026-07-18
- feat: collect remote Claude, Codex, and OMP statistics over SSH
- feat: add Sources tab with per-source status, summary, and health dot
- change: show account limits for the local machine only
- fix: harden remote snapshot collection and refresh ordering
- fix: skip expired grok cached auth before billing rpc

## 0.1.8 — 2026-07-11
- fix: avoid counting inherited Codex subagent history

## 0.1.7 — 2026-07-10
- feat: show model column in Rounds view and --dump-json

## 0.1.6 — 2026-07-10
- fix: keep Grok SuperGrok weekly bar instead of TPM api 0% fallback
- feat: show req/in/out/cache columns in BY MODEL
- feat: scale cost colors by timeframe (yellow $50/day, red $200/day, ×4)

## 0.1.5 — 2026-07-10
- fix: color omp (and all agents) via shared agent_name_color in BY AGENT / Rounds

## 0.1.4 — 2026-07-10
- feat: scan oh-my-pi (`omp`) sessions for tokens, cost, and rounds
- feat: Gemini 3.x pricing rates (3.5 Flash, 3.1 Flash-Lite, 3.1 Pro, 3 Flash)

## 0.1.3 — 2026-07-09
- feat: compact limit bars with green-to-red fill
- feat: Grok weekly usage via `_x.ai/billing` ACP RPC
- fix: stretch limit bars across window; rename GLOBAL to Usage

## 0.1.2 — 2026-07-09
- fix: polish GLOBAL layout, metrics table, and agent columns
- fix: hide synthetic model rows and drop top projects from GLOBAL
- feat: add GPT-5.6 Sol/Terra/Luna pricing rates

## 0.1.1 — 2026-07-09
- feat: GPUI dashboard with live tok scan/cache/agg data
- feat: GLOBAL, Projects, Rounds tabs with limits and top projects
- feat: headless --dump-json and parity check vs tok
