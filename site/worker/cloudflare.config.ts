import { defineConfig } from "cf/config";

// zeroflux.network: assets-only Worker serving site/dist (built by site/build.py).
export default defineConfig({
  worker: {
    "name": "zeroflux-network",
    "compatibilityDate": "2026-10-06",
    "observability": { "enabled": true },
    "assets": { "htmlHandling": "auto-trailing-slash", "notFoundHandling": "404-page" },
    "domains": ["zeroflux.network", "www.zeroflux.network"]
  }
});
