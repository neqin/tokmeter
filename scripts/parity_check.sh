#!/usr/bin/env bash
# Compare tokmeter --dump-json totals against tok --once or a fixture.
# Exit 0 = match; 1 = mismatch; 2 = missing inputs.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

SCHEMA_ONLY="${TOK_PARITY_SCHEMA_ONLY:-0}"
if [[ "$SCHEMA_ONLY" == "1" ]]; then
  echo "schema-only mode (does not satisfy parity acceptance)" >&2
  dump="$(cargo run --quiet -- --dump-json --source=local 2>/dev/null)"
  echo "$dump" | rg -q 'total_tokens' || exit 1
  exit 0
fi

if [[ -n "${TOK_PARITY_FIXTURE:-}" ]]; then
  fixture_dir="$(cd "$(dirname "$TOK_PARITY_FIXTURE")" && pwd)"
  expected_file="${fixture_dir}/expected.json"
  if [[ ! -f "$expected_file" ]]; then
    echo "missing $expected_file" >&2
    exit 2
  fi
  # Isolate HOME so Scanner does not walk the developer's live sessions.
  empty_home="$(mktemp -d /tmp/tokmeter-parity-home.XXXXXX)"
  trap 'rm -rf "$empty_home"' EXIT
  export HOME="$empty_home"
  export HERDR_PLUGIN_STATE_DIR="$fixture_dir"
  unset XDG_CACHE_HOME || true
  dump="$(cargo run --quiet -- --dump-json --source=local 2>/dev/null)"
  python3 - "$dump" "$expected_file" <<'PY'
import json, sys
got = int(json.loads(sys.argv[1])["total_tokens"])
exp = int(json.load(open(sys.argv[2]))["total_tokens"])
if exp == 0:
    print(f"fixture parity: got={got} exp=0 (exact)")
    sys.exit(0 if got == 0 else 1)
err = abs(got - exp) / exp
print(f"fixture parity: got={got} exp={exp} err={err:.4%}")
sys.exit(0 if err <= 0.01 else 1)
PY
  exit $?
fi

if ! command -v tok >/dev/null 2>&1; then
  echo "tok not on PATH and no TOK_PARITY_FIXTURE; exit 2" >&2
  exit 2
fi

dump="$(cargo run --quiet -- --dump-json --source=local 2>/dev/null)"
tok_out="$(tok --once 2>/dev/null || true)"

python3 - "$dump" "$tok_out" <<'PY'
import json, re, sys

dump = json.loads(sys.argv[1])
tok_out = sys.argv[2]
got_tokens = int(dump.get("total_tokens") or 0)
agents = ",".join(a["name"] for a in dump.get("agents", [])[:2])

scales = {"": 1, "K": 1_000, "M": 1_000_000, "B": 1_000_000_000, "T": 1_000_000_000_000}
tok_tokens = None
tolerance = 0
for line in tok_out.splitlines():
    if "Σ" not in line:
        continue
    match = re.search(r"Σ\s+([0-9]+(?:\.([0-9]+))?)([KMBT]?)", line)
    if match:
        scale = scales[match.group(3)]
        decimals = len(match.group(2) or "")
        tok_tokens = float(match.group(1)) * scale
        tolerance = max(tok_tokens * 0.01, 0.5 * scale / (10 ** decimals))
        break

if tok_tokens is None:
    print("tok token total parse failed", file=sys.stderr)
    sys.exit(1)

delta = abs(got_tokens - tok_tokens)
print(
    f"parity tokens: tokmeter={got_tokens} tok={tok_tokens:.0f} "
    f"delta={delta:.0f} tolerance={tolerance:.0f} agents={agents}"
)
sys.exit(0 if delta <= tolerance else 1)
PY
