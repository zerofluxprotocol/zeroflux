#!/bin/sh
# Full local verification for ZFX VPN (no hosted CI).
#   sh scripts/check.sh            # tests, schemas, dashboard, skill, deterministic privacy gate
#   sh scripts/check.sh --models   # also benchmark GLiNER edge/small/base (downloads ~340 MB once)
set -eu
cd "$(dirname "$0")/.."
. "$HOME/.cargo/env" 2>/dev/null || true
echo "== rust tests"; cargo test --workspace --locked -q
echo "== schemas current"; cargo run -q -p pv-cli --bin zfx-vpn -- schema --out schemas >/dev/null && git diff --exit-code schemas
echo "== dashboard"; (cd apps/dashboard && npm ci --no-audit --no-fund -s && npm run -s gen:types && git diff --exit-code src/generated && npx tsc --noEmit && npm run -s build)
echo "== skill adapters current"; python3 -I scripts/gen_skill_adapters.py . >/dev/null && git diff --exit-code skills
echo "== skill script tests"; cargo build -q -p pv-cli && sh skills/zfx-vpn-setup/tests/run.sh </dev/null
echo "== upgrade from Prompt VPN 0.1 and rollback (builds 0.1 from the import commit)"; sh tests/migration/upgrade_rollback.sh
echo "== demos"; sh demos/run_all.sh
echo "== sdk"; python3 -I sdk/python/test_zfx_vpn.py
echo "== privacy gate"
python3 -I scripts/gen_eval_corpus.py > tests/fixtures/eval_corpus.jsonl
cargo run -q -p pv-cli --bin zfx-vpn -- eval --configs deterministic --out /tmp/zfx-eval.json >/dev/null
python3 -I -c "import json; r=json.load(open('/tmp/zfx-eval.json'))[0]; print('critical escapes', r['critical_escapes'], 'false blocks', r['false_block_rate']); assert r['critical_escapes'] <= 1 and r['false_block_rate'] == 0"
if [ "${1:-}" = "--models" ]; then
  cargo run -q -p pv-cli --bin zfx-vpn -- eval --configs all --out /tmp/zfx-eval-all.json
  python3 -I -c "import json; rs=json.load(open('/tmp/zfx-eval-all.json')); assert all(r['critical_escapes'] <= 1 and r['false_block_rate'] <= 0.02 for r in rs)"
fi
echo "all checks passed"
