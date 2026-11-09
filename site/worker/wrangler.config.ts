import { defineWranglerConfig } from "wrangler/experimental-config";

// Static site built by ../build.py into ../dist.
export default defineWranglerConfig({
  "assetsDirectory": "../dist"
});
