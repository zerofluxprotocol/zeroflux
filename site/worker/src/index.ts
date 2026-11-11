// zeroflux.network: static site (site/dist) plus one endpoint, POST /api/beta, which stores a
// private-beta request in the BETA KV namespace. Read requests with:
//   cf kv keys list --namespace-id <BETA id> ; cf kv values get <key> --namespace-id <BETA id>
interface Env {
  ASSETS: Fetcher;
  BETA: KVNamespace;
}

const PRODUCTS = new Set(["hosted-vpn", "sandboxes", "trading", "zpu"]);
const json = (status: number, body: unknown) =>
  new Response(JSON.stringify(body), { status, headers: { "content-type": "application/json", "cache-control": "no-store" } });

async function beta(req: Request, env: Env): Promise<Response> {
  if (req.method !== "POST") return json(405, { error: "method" });
  if (Number(req.headers.get("content-length") || "0") > 4096) return json(413, { error: "too large" });
  let b: Record<string, unknown>;
  try { b = await req.json(); } catch { return json(400, { error: "invalid json" }); }
  if (typeof b.website === "string" && b.website.length) return json(200, { ok: true }); // honeypot: bots fill hidden fields
  const email = String(b.email || "").trim().toLowerCase();
  if (!/^[^\s@]{1,64}@[^\s@]{1,190}\.[a-z]{2,24}$/.test(email)) return json(400, { error: "Enter a valid email address." });
  const products = (Array.isArray(b.products) ? b.products : []).map(String).filter((p) => PRODUCTS.has(p));
  const note = String(b.note || "").slice(0, 500);
  const ip = req.headers.get("cf-connecting-ip") || "";
  // one request per address per day is plenty; repeats overwrite
  const key = `beta:${email}`;
  const prev = await env.BETA.get(key, "json") as { at?: string } | null;
  if (prev?.at && Date.now() - Date.parse(prev.at) < 30_000) return json(429, { error: "Already received. Thanks." });
  await env.BETA.put(key, JSON.stringify({ email, products, note, at: new Date().toISOString(), country: req.headers.get("cf-ipcountry") || "", ip_prefix: ip.split(/[.:]/).slice(0, 2).join(".") }));
  return json(200, { ok: true });
}

export default {
  async fetch(req: Request, env: Env): Promise<Response> {
    const url = new URL(req.url);
    if (url.pathname === "/api/beta") return beta(req, env);
    return env.ASSETS.fetch(req);
  },
} satisfies ExportedHandler<Env>;
