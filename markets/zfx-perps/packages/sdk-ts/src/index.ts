export * from "./wire.js";
export * from "./signer.js";
export * from "./types.js";
export * from "./client.js";
export * from "./feed.js";
export * from "./near.js";

// v1 clearing API (isolated-margin perps settled on the NEAR clearing contract). Several names
// (ZfxPerpsClient, orderMessage, Side, ...) clash with the spot API above, so the clearing API is
// a namespace here and a subpath export: `import { ZfxPerpsClient } from "@zeroflux/zfx-perps-sdk/clearing"`.
export * as clearing from "./clearing/index.js";
export { ZfxPerpsClient as ZfxPerpsClearingClient } from "./clearing/client.js";
