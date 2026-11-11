import { defineConfig } from "cf/config";

// zeroflux.network: site/dist (built by site/build.py) plus POST /api/beta (src/index.ts), which
// stores private-beta requests in the BETA KV namespace.
export default defineConfig({
  worker: {
    "name": "zeroflux-network",
    "compatibilityDate": "2026-10-06",
    "observability": { "enabled": true },
    "entrypoint": "src/index.ts",
    "assets": { "htmlHandling": "auto-trailing-slash", "notFoundHandling": "404-page", "runWorkerFirst": ["/api/*"] },
    "env": { "ASSETS": { "type": "assets" }, "BETA": { "type": "kv", "id": "831667e7dda14ba6bb17dd3b522ce786" } },
    "domains": ["zeroflux.network", "www.zeroflux.network"]
  }
});
