# Legacy Prompt VPN 0.1.0 install (test fixture)

Created on 2026-11-10 by the original `promptvpn` binary built from promptvpn
commit d2ac753 (imported here as commit 499f9f6), using `generate.sh`. Every
value is synthetic, including the data key in `home/keys.insecure.json` and the
local app keys in `home/apps/`. They protect nothing outside this fixture.

Contents written by the old code:

- `home/promptvpn.db`: schema version 1; policy with PHONE=REDACT and EMAIL=TOKENIZE;
  two registered secrets (TOKENIZE "Nightjar-Cedar-77", BLOCK "zz-legacy-blocked-value-991");
  one custom entity type (sealed); two placeholder mappings in scope
  `app_16d7a9e750cf:fixture-conv-1` (`[[PV_EMAIL_bb25f99e]]` = alice.fixture@example.test,
  `[[PV_EXACT_SECRET_caf08ead]]` = Nightjar-Cedar-77); 9 MAC-chained audit events.
- `home/keys.insecure.json`: the file key backend (PV_KEY_BACKEND=file).
- `home/admin.token`, `home/apps/*.key`: admin token and two app keys.
- `policy-export.json`, `secrets-list.json`: what the old CLI reported, used to
  compare state after upgrade and after rollback.
