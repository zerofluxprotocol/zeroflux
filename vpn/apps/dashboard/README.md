# Prompt VPN dashboard

React + TypeScript + Vite. `npm run build` writes static files to `dist/`, which `pv-daemon` embeds (`crates/pv-daemon/src/assets.rs`) and serves at `http://127.0.0.1:<port>/dashboard/`. Routing uses the URL hash (`#/approvals/apr_...`), so every server path can fall back to `index.html`.

Runtime dependencies are `react` and `react-dom` only. No router, UI kit, CSS framework, web fonts or external requests, so the bundle runs under the daemon's CSP (`default-src 'self'; connect-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' data:`).

## Commands

```sh
npm install
npm run gen:types    # schemas/*.json -> src/generated/types.ts (commit the output)
npm run typecheck    # app + vite config
npm run build        # typecheck, then emit dist/
```

Rebuild the daemon after `npm run build` so the new `dist/` is embedded.

## Development against a running daemon

The daemon accepts only its own loopback `Host` and `Origin`. The Vite dev and preview servers proxy `/api` and `/dashboard/login` to the daemon and rewrite both headers. Point them at the daemon port with `PV_DAEMON_URL` (default `http://127.0.0.1:8787`).

```sh
PV_DAEMON_URL=http://127.0.0.1:8799 npm run dev        # http://127.0.0.1:5173/dashboard/
```

Log in through the dev server so the session cookie is set for it:

```sh
T=$(cat ~/.promptvpn/admin.token)   # or your data dir's admin.token
TICKET=$(curl -s -X POST -H "Authorization: Bearer $T" http://127.0.0.1:8799/api/admin/v1/session/ticket | jq -r .ticket)
open "http://127.0.0.1:5173/dashboard/login?ticket=$TICKET"
```

Tickets work once and expire after two minutes.

To test the production bundle under the daemon's exact CSP before rebuilding Rust:

```sh
npm run build && PV_DAEMON_URL=http://127.0.0.1:8799 npm run preview   # http://127.0.0.1:4180/dashboard/
```

## Screenshot and console check

`npm run screenshots` logs in with a fresh ticket, visits every page, opens (and cancels) the reveal, save-as-rule, new-rule and critical-disable dialogs, runs the policy playground, and walks onboarding steps 1 to 6 without pressing Finish. It writes PNGs to `screenshots/` (gitignored) and exits non-zero on any console error, page error or failed `/api` response.

```sh
PV_DAEMON_URL=http://127.0.0.1:8799 PV_ADMIN_TOKEN_FILE=/tmp/pv-e2e-home/admin.token \
PV_DASH_URL=http://127.0.0.1:4180 npm run screenshots
```

It uses `playwright-core` and an existing Chromium from the Playwright cache (`CHROMIUM_PATH` overrides). It does call `POST /near/verify` on the onboarding NEAR step, which records an attestation audit event.

## Security notes

- The session cookie is HttpOnly; the CSRF token lives in memory only and is sent as `X-PV-CSRF` on every non-GET request.
- Nothing is written to `localStorage` or `sessionStorage`. Secret values and API keys use masked inputs and are cleared from component state after submission.
- Held prompts are never shown by default. "Reveal original" requires a confirmation and the daemon audits it; revealed text stays in component state and disappears when hidden or when you leave the page.
- New app keys are shown once with a copy button.

## Layout

- `src/api.ts` fetch wrapper, session/CSRF, admin response types
- `src/generated/types.ts` generated from `/schemas`
- `src/ui.tsx` primitives (buttons, toggles, dialogs, typed confirmation, masked input)
- `src/components.tsx` event list, action badges, redacted preview renderer
- `src/pages/*` one file per page; `Onboarding.tsx` is the six-step wizard
- `scripts/gen-types.mjs`, `scripts/screenshots.mjs`
