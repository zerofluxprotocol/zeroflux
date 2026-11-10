# Policy semantics

The engine (`crates/pv-core/src/policy.rs`) is a pure function of findings, the active rule set, the request context and owner settings.

## Rules

A rule has an `id`, an immutable `version`, a `scope` (`user`, `agent`, `application`, `request`; `organization` and `team` are reserved and rejected in v1), a `selector`, an `action`, a `severity` and an `override` mode. Saving a rule always creates a new version; old versions stay in history. Every decision records the effective policy hash: SHA-256 over all active rules (sorted by id) and the fallback settings.

Selector:
- `categories`: built-in IDs (`EMAIL`, `GOVERNMENT_ID`, ...), `EXACT_SECRET`, `EXACT_SECRET:<id>`, `CUSTOM:<id>`, `CUSTOM:*`, `UNCERTAIN`, `CLASSIFIER_FAILURE`, `UNSUPPORTED_CONTENT`, or `*`.
- `destinations`: `any`, `external` (any remote tier, TEEs included), `local`, `attested_tee`, `external_unverified`, `dest:<id>`.
- `direction`: `inbound` (to the model), `outbound` (model output), `both`.
- `min_confidence`.

Actions: `ALLOW`, `TOKENIZE`, `REDACT`, `GENERALIZE`, `PRIVATE_ROUTE`, `REQUIRE_APPROVAL`, `BLOCK`, and `FALLBACK` (resolves through the owner's fallback setting, see below).

## Choosing the action for a finding

1. Collect enabled rules whose scope applies, direction and destination match, and selector matches the finding.
2. The most specific scope wins (request > application > agent > user). Within a scope, the most specific selector wins (`EXACT_SECRET:<id>` > category > `CUSTOM:*` > `*`), then the strictest action.
3. A less specific rule whose override mode is not `ALLOWED` clamps the action upward: a lower scope cannot weaken it. Strictness: BLOCK > REQUIRE_APPROVAL > REDACT > TOKENIZE > GENERALIZE > PRIVATE_ROUTE > ALLOW.
4. No matching rule: critical findings bound for a remote tier are blocked; everything else is allowed.
5. A noncritical finding below the uncertain threshold (0.5) that would otherwise be transformed becomes `FALLBACK`.
6. `FALLBACK` resolves to `PRIVATE_ROUTE` when the mode is TEE routing and either TEE consent or local private handling is on; otherwise to `REQUIRE_APPROVAL` (block-and-review).
7. An owner approval presented with the retried request turns `REQUIRE_APPROVAL` into `ALLOW` unless the chain contains a `FORBIDDEN` rule; `BLOCK` only yields to approval when its rule says `APPROVAL_ONCE`.

Override modes: `ALLOWED` (lower scopes may replace it), `APPROVAL_ONCE` (not weakened by scopes; a one-time owner approval may pass it), `POLICY_OWNER_ONLY` (only the owner, by editing the rule), `FORBIDDEN` (nothing weakens it, approvals included).

## Request disposition

The request takes the strictest outcome across its findings: BLOCK > REQUIRE_APPROVAL > PRIVATE_ROUTE > TRANSFORM > ALLOW. A `PRIVATE_ROUTE` request whose destination is already a verified TEE (or an allowed local model) becomes TRANSFORM/ALLOW there.

Transforms over overlapping findings replace the union of their spans with the strictest action and the most sensitive category, so a secret is never left exposed because a weaker finding overlapped it. A finding that covers a whole field (classifier failure, unsupported media) cannot be partially transformed; it follows its own rule (review or block).

## Defaults

- Critical categories (government IDs, passports, driver's licenses, payment cards, API keys, private keys, auth secrets): `BLOCK`, `FORBIDDEN`, for all remote destinations including TEEs. The owner can relax one category through Categories by typing `DISABLE <ID>`; this writes a new rule version flagged `critical_exception` and an audit event. There is no global off switch.
- Email, phone, address, date of birth, health information: `TOKENIZE`. Bank accounts: `REDACT`. Names, public information: `ALLOW`. Each is editable in onboarding and Settings.
- Registered exact values: `BLOCK` (per-value action possible). Custom entity types: `TOKENIZE` unless the entity sets its own action.
- `UNCERTAIN`, `CLASSIFIER_FAILURE`: `FALLBACK`. `UNSUPPORTED_CONTENT`: `BLOCK` for remote tiers.
- Outbound: critical categories and registered values in model output are redacted.
