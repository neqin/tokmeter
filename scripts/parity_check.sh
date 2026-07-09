#!/usr/bin/env bash
# Compare tokmeter --dump-json totals against tok --once or a fixture.
# Exit 0 = match; 1 = mismatch; 2 = missing inputs.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

SCHEMA_ONLY="${TOK_PARITY_SCHEMA_ONLY:-0}"
if [[ "$SCHEMA_ONLY" == "1" ]]; then
  echo "schema-only mode (does not satisfy parity acceptance)" >&2
  dump="$(cargo run --quiet -- --dump-json 2>/dev/null)"
  echo "$dump" | rg -q 'total_tokens' || exit 1
  exit 0
fi

if [[ -n "${TOK_PARITY_FIXTURE:-}" ]]; then
  export HERDR_PLUGIN_STATE_DIR="$(dirname "$TOK_PARITY_FIXTURE")"
  expected_file="$(dirname "$TOK_PARITY_FIXTURE")/expected.json"
  if [[ ! -f "$expected_file" ]]; then
    echo "missing $expected_file" >&2
    exit 2
  fi
  dump="$(cargo run --quiet -- --dump-json 2>/dev/null)"
  python3 - "$dump" "$expected_file" <<'PY'
import json, sys
got = json.loads(sys.argv[1])["total_tokens"]
exp = json.load(open(sys.argv[2]))["total_tokens"]
err = 0.0 if exp == 0 else abs(got - exp) / exp
print(f"fixture parity: got={got} exp={exp} err={err:.4%}")
sys.exit(0 if err <= 0.01 else 1)
PY
  exit $?
fi

if ! command -v tok >/dev/null 2>&1; then
  echo "tok not on PATH and no TOK_PARITY_FIXTURE; exit 2" >&2
  exit 2
fi

dump="$(cargo run --quiet -- --dump-json 2>/dev/null)"
tok_out="$(tok --once 2>/dev/null || true)"

python3 - "$dump" "$tok_out" <<'PY'
import json, re, sys

dump = json.loads(sys.argv[1])
tok_out = sys.argv[2]
got_cost = float(dump.get("total_cost") or 0)
got_tokens = int(dump.get("total_tokens") or 0)
agents = ",".join(a["name"] for a in dump.get("agents", [])[:2])

# Prefer the Σ line cost: "Σ ... $3999.25"
tok_cost = 0.0
for line in tok_out.splitlines():
    if "Σ" in line or "\u03a3" in line:
        m = re.search(r"\$([0-9]+(?:\.[0-9]+)?)", line)
        if m:
            tok_cost = float(m.group(1))
            break
if tok_cost == 0.0:
    # fallback: first $ amount in output
    m = re.search(r"\$([0-9]+(?:\.[0-9]+)?)", tok_out)
    if m:
        tok_cost = float(m.group(1))

print(f"parity cost: tokmeter={got_cost:.2f} tok={tok_cost:.2f} tokens={got_tokens} agents={agents}")
if tok_cost == 0 and got_cost == 0:
    sys.exit(0)
if tok_cost == 0:
    print("tok cost parse failed", file=sys.stderr)
    sys.exit(1)
err = abs(got_cost - tok_cost) / tok_cost
sys.exit(0 if err <= 0.01 else 1)
PY
