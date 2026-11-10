import { useEffect, useState } from 'react';
import {
  del,
  get,
  post,
  type ModelTestResult,
  type ModelsStatus,
  type NearModel,
  type NearVerification,
  type ProviderConfig,
  type ProviderEntry,
  type ProviderTest,
  type SettingsResponse,
  type SpanInstallResult,
  type SpanModelName,
  type SpanSize,
  type SpanStatus,
} from '../api';
import { useEntityLabels } from '../entities';
import { categoryLabel, dateTime, megabytes } from '../format';
import {
  Badge,
  Button,
  Card,
  Check,
  ConfirmDialog,
  Dialog,
  Empty,
  ErrorText,
  Field,
  KV,
  Loading,
  Notice,
  PageHeader,
  SecretInput,
  TierBadge,
  type Loaded,
  useAction,
  useLoad,
} from '../ui';

export const DEFAULT_NEAR_CANDIDATE = 'deepseek-ai/DeepSeek-V4.1-Flash-1017';

/** Per-size facts from docs/evaluation.md (270-case synthetic corpus, CPU, 2026-11-08). */
export const SPAN_SIZES: { size: SpanSize; model: SpanModelName; name: string; blurb: string }[] = [
  { size: 'edge', model: 'gliner-pii-edge', name: 'Edge', blurb: 'Smallest and fastest. 0.922 decision accuracy, 11 ms median per prompt.' },
  { size: 'small', model: 'gliner-pii-small', name: 'Small', blurb: '0.922 decision accuracy, 20 ms median. Base is about as fast and more accurate.' },
  { size: 'base', model: 'gliner-pii-base', name: 'Base', blurb: 'Most accurate. 0.944 decision accuracy, 21 ms median. Names found 89% of the time vs 26% with rules alone.' },
];

export const spanSizeOf = (model: string | null | undefined) => SPAN_SIZES.find((x) => x.model === model);
export const spanName = (model: string | null | undefined) => {
  const m = spanSizeOf(model);
  return m ? `GLiNER PII ${m.name}` : model ?? 'None';
};

export function spanState(s: SpanStatus): { label: string; tone: 'green' | 'outline' | 'red' } {
  if (s.installing) return { label: 'installing', tone: 'outline' };
  if (s.loaded) return { label: 'loaded', tone: 'green' };
  if (s.error) return { label: 'error', tone: 'red' };
  if (s.provisioned) return { label: 'installed, not loaded', tone: 'outline' };
  return { label: 'not installed', tone: 'outline' };
}

/** TEE-capable NEAR candidates (before attestation). */
export const isTeeCandidate = (m: NearModel) => m.attestation_supported && (m.privacy_tier === 'PRIVATE_TEE' || m.privacy_tier === 'PRIVATE_3P_TEE');

// ------------------------------------------------------------------ attestation

export function AttestationDetails({ v }: { v: NearVerification }) {
  return (
    <KV
      rows={[
        ['Result', v.eligible ? <Badge tone="dark">Eligible: {v.tier}</Badge> : <Badge tone="red">Not eligible</Badge>],
        ['Model', <span className="mono">{v.model}</span>],
        ['Error', v.error],
        ['Gateway signer', v.gateway_signer ? <span className="mono small">{v.gateway_signer}</span> : null],
        ['Gateway TCB', v.gateway_tcb],
        ['Gateway TLS key (SPKI)', v.gateway_tls_spki ? <span className="mono small">{v.gateway_tls_spki}</span> : null],
        ['Model signers', v.model_signers?.length ? <span className="mono small">{v.model_signers.join(', ')}</span> : null],
        ['Model TCB', v.model_tcb?.join(', ')],
        ['GPU attested', v.gpu_verified ? 'Yes' : 'No'],
        ['End-to-end encryption key', v.e2ee_key ? 'Present' : 'Missing'],
        ['Verifier', <span className="small">{v.verifier}</span>],
        ['Checked', dateTime(v.checked_at)],
      ]}
    />
  );
}

// ------------------------------------------------------------------ page

export default function ModelsPage() {
  const models = useLoad(() => get<ModelsStatus>('/models'), []);
  const providers = useLoad(() => get<{ providers: ProviderEntry[]; verifications: NearVerification[]; attestations?: NearVerification[] }>('/providers'), []);
  const settings = useLoad(() => get<SettingsResponse>('/settings'), []);

  return (
    <div className="content">
      <PageHeader title="Models & destinations" subtitle="The local detection model that inspects prompts, and the destinations that generate replies. Every destination carries a privacy tier that policy enforces." />
      <DetectionModelCard m={models} />
      <DestinationsCard providers={providers} settings={settings.data} />
      <NearCard verifications={[...(providers.data?.attestations ?? []), ...(providers.data?.verifications ?? [])]} onChanged={() => providers.reload()} hasNear={!!providers.data?.providers.some((p) => p.config.kind === 'NEAR_AI')} />
      <div className="grid-2">
        <OllamaGenCard m={models.data} onSaved={() => providers.reload()} />
        <CustomEndpointCard onSaved={() => providers.reload()} />
      </div>
    </div>
  );
}

function DetectionModelCard({ m }: { m: Loaded<ModelsStatus> }) {
  const [installing, setInstalling] = useState<SpanSize | null>(null);
  const [result, setResult] = useState<string | null>(null);
  const act = useAction();
  const d = m.data;
  const serverInstalling = !!d?.span.installing;

  // Another tab or the CLI may be installing: refresh until it finishes.
  useEffect(() => {
    if (!serverInstalling || installing) return;
    const t = window.setInterval(() => void m.reload(), 3000);
    return () => window.clearInterval(t);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [serverInstalling, installing]);

  if (!d) {
    return (
      <Card title="Detection model">
        <ErrorText error={m.error} />
        <Loading />
      </Card>
    );
  }
  const active = d.span.model ?? d.selection.model;
  const st = spanState(d.span);
  const lock = d.lock.span?.[active];
  const busy = !!installing || serverInstalling || act.busy;

  const install = (size: SpanSize) =>
    act.run(async () => {
      setInstalling(size);
      setResult(null);
      try {
        const r = await post<SpanInstallResult>('/models/install', { model: size });
        setResult(`${spanName(r.model)} installed, verified and ${r.loaded ? 'loaded' : 'selected, but it did not load'}.`);
      } finally {
        setInstalling(null);
        await m.reload();
      }
    });
  const select = (size: SpanSize) =>
    act.run(async () => {
      setResult(null);
      const r = await post<SpanInstallResult>('/models/select', { model: size });
      setResult(`Now using ${spanName(r.model)}${r.loaded ? '' : ', but it did not load'}.`);
      await m.reload();
    });

  return (
    <Card title="Detection model" actions={<Badge tone={st.tone}>{st.label}</Badge>}>
      <div className="grid-2">
        <div className="stack">
          <p className="muted small">
            A GLiNER span model running in an embedded ONNX runtime on this computer. It finds names, health details, addresses and your custom entity types in free text. Rules and exact values work without it.
          </p>
          <KV
            rows={[
              ['Active model', d.span.provisioned || d.span.loaded ? <span>{spanName(active)} <span className="muted mono small">{active}</span></span> : null],
              ['State', d.span.loaded ? 'Loaded and in use' : d.span.provisioned ? 'Installed, not loaded' : 'Not installed (requests that need it are held for review)'],
              ['Download size', megabytes(d.span.size_bytes)],
              ['Revision', d.span.revision ? <span className="mono small">{d.span.revision.slice(0, 12)}</span> : null],
              ['License', lock?.license],
              ['Encoder', lock?.encoder ? <span className="mono small">{lock.encoder}</span> : null],
              ['Runtime compiled in', d.span.compiled ? 'Yes' : <span style={{ color: 'var(--red)' }}>No, this build cannot run the model</span>],
              ['Enabled in Settings', d.detector.span_enabled ? (d.detector.span_required ? 'Yes, fail closed if unavailable' : 'Yes') : 'No'],
              ['Error', d.span.error],
            ]}
          />
        </div>
        <div className="stack-s">
          <span className="field-label">Sizes</span>
          {SPAN_SIZES.map((x) => {
            const opt = d.options.find((o) => o.model === x.model);
            const isActive = x.model === active && (d.span.loaded || d.span.provisioned);
            return (
              <div key={x.size} className={`radio-card${isActive ? ' selected' : ''}`}>
                <div className="row between gap-s" style={{ alignItems: 'flex-start' }}>
                  <div className="stack-s" style={{ gap: 4 }}>
                    <div className="row gap-s">
                      <strong>{x.name}</strong>
                      <span className="muted small">{megabytes(opt?.size_bytes)}</span>
                      {opt?.default ? <Badge tone="dark">recommended</Badge> : null}
                    </div>
                    <div className="muted small">{x.blurb}</div>
                    <div className="row gap-s">
                      {isActive ? <Badge tone="green">active</Badge> : null}
                      {opt?.installed ? <Badge>installed</Badge> : <Badge tone="outline">not installed</Badge>}
                    </div>
                  </div>
                  {isActive ? null : opt?.installed ? (
                    <Button small disabled={busy || !d.span.compiled} onClick={() => select(x.size)}>
                      Use this size
                    </Button>
                  ) : (
                    <Button small variant={opt?.default ? 'primary' : 'secondary'} busy={installing === x.size} disabled={busy || !d.span.compiled} onClick={() => install(x.size)}>
                      {installing === x.size ? 'Downloading' : 'Install'}
                    </Button>
                  )}
                </div>
              </div>
            );
          })}
          {installing || serverInstalling ? (
            <Notice tone="info">
              <span className="row gap-s">
                <span className="spinner" aria-hidden /> Downloading{installing ? ` ${spanName(SPAN_SIZES.find((x) => x.size === installing)?.model)}` : ''} and checking its sha256 against the pinned lock file. This can take a minute.
              </span>
            </Notice>
          ) : null}
          {result ? <Notice tone="ok">{result}</Notice> : null}
          <ErrorText error={act.error} />
        </div>
      </div>
      <TestDetection disabled={busy} />
    </Card>
  );
}

const SAMPLE_TEXT = 'Synthetic test: remind maria gonzalez (test.user@example.com) about her insulin refill. Her insurance policy number is HX-2291-0042; SSN 123-45-6789.';

function TestDetection({ disabled }: { disabled: boolean }) {
  const [text, setText] = useState(SAMPLE_TEXT);
  const [res, setRes] = useState<ModelTestResult | null>(null);
  const act = useAction();
  useEntityLabels();
  return (
    <div className="stack" style={{ marginTop: 20, paddingTop: 16, borderTop: '1px solid var(--line)' }}>
      <div className="row between">
        <span className="section-title" style={{ margin: 0 }}>Test detection</span>
        <span className="muted small">Runs rules, exact values and the detection model on this text. Nothing is sent or logged.</span>
      </div>
      <textarea className="input" rows={3} value={text} onChange={(e) => setText(e.target.value)} aria-label="Detection test text" />
      <div className="row gap-s" style={{ justifyContent: 'flex-end' }}>
        <Button variant="primary" busy={act.busy} disabled={disabled || !text.trim()} onClick={() => act.run(async () => setRes(await post<ModelTestResult>('/models/test', { text })))}>
          Test detection
        </Button>
      </div>
      <ErrorText error={act.error} />
      {res ? (
        <div className="stack-s">
          <div className="small muted">
            {res.findings.length} {res.findings.length === 1 ? 'span' : 'spans'} found in {res.latency_ms} ms
            {res.span_latency_ms != null ? ` (detection model ${res.span_latency_ms} ms)` : ' (detection model did not run)'}
          </div>
          {res.findings.length ? (
            <table className="table">
              <thead>
                <tr>
                  <th>Category</th>
                  <th>Found text</th>
                  <th>Confidence</th>
                  <th>Recognizer</th>
                </tr>
              </thead>
              <tbody>
                {res.findings.map((f, i) => (
                  <tr key={i}>
                    <td>{categoryLabel(f.category)}</td>
                    <td className="mono small">{f.text ?? <span className="muted">(whole field)</span>}</td>
                    <td className="small">{f.confidence.toFixed(2)}</td>
                    <td className="small mono">{f.recognizer}</td>
                  </tr>
                ))}
              </tbody>
            </table>
          ) : (
            <Empty>Nothing detected.</Empty>
          )}
          {res.failures.length ? <Notice tone="warn">Failures: {res.failures.join('; ')}</Notice> : null}
        </div>
      ) : null}
    </div>
  );
}

function DestinationsCard({ providers, settings }: { providers: Loaded<{ providers: ProviderEntry[]; verifications: NearVerification[] }>; settings: SettingsResponse | null }) {
  const [tests, setTests] = useState<Record<string, ProviderTest | string>>({});
  const [busy, setBusy] = useState<string | null>(null);
  const [deleting, setDeleting] = useState<ProviderConfig | null>(null);
  const list = providers.data?.providers ?? [];
  const def = settings?.engine.default_destination;
  const priv = settings?.engine.private_destination;
  const failover = settings?.engine.failover ?? [];
  return (
    <Card title="Destinations">
      <ErrorText error={providers.error} />
      {providers.loading && !providers.data ? (
        <Loading />
      ) : !list.length ? (
        <Empty>No destinations. Add NEAR AI, a local Ollama model, or a custom endpoint below.</Empty>
      ) : (
        <div className="list">
          {list.map(({ config: c, has_credential }) => {
            const t = tests[c.id];
            return (
              <div key={c.id} className="list-item" style={{ alignItems: 'flex-start', flexDirection: 'column', gap: 10 }}>
                <div className="row gap" style={{ width: '100%' }}>
                  <div className="li-main">
                    <div className="li-title row gap-s">
                      {c.display_name}
                      {c.id === def ? <Badge tone="dark">default</Badge> : null}
                      {c.id === priv ? <Badge tone="dark">private route</Badge> : null}
                      {failover.includes(c.id) ? <Badge tone="outline">failover</Badge> : null}
                      {c.enabled === false ? <Badge tone="outline">disabled</Badge> : null}
                    </div>
                    <div className="li-sub">
                      <span className="mono">{c.id}</span> · {c.kind} · <span className="mono">{c.base_url}</span> · model <span className="mono">{c.default_model}</span>
                      {c.kind !== 'OLLAMA' ? ` · ${has_credential ? 'credential stored' : 'no credential'}` : ''}
                    </div>
                  </div>
                  <TierBadge tier={c.declared_tier} />
                  <Button
                    small
                    busy={busy === c.id}
                    onClick={async () => {
                      setBusy(c.id);
                      try {
                        const r = await post<ProviderTest>(`/providers/${encodeURIComponent(c.id)}/test`);
                        setTests((s) => ({ ...s, [c.id]: r }));
                      } catch (e) {
                        setTests((s) => ({ ...s, [c.id]: e instanceof Error ? e.message : String(e) }));
                      } finally {
                        setBusy(null);
                      }
                    }}
                  >
                    Test
                  </Button>
                  <Button small variant="danger" onClick={() => setDeleting(c)}>
                    Remove
                  </Button>
                </div>
                {t ? (
                  typeof t === 'string' ? (
                    <Notice tone="error">{t}</Notice>
                  ) : (
                    <Notice tone={t.ok ? 'ok' : 'error'}>
                      {t.ok ? 'Reachable' : 'Failed'} · tier {t.tier} · model {t.model} · {t.latency_ms} ms
                      {t.detail.reply ? ` · reply "${String(t.detail.reply).slice(0, 80)}"` : ''}
                      {t.detail.error ? ` · ${t.detail.error}` : ''}
                    </Notice>
                  )
                ) : null}
              </div>
            );
          })}
        </div>
      )}
      <p className="muted small" style={{ marginTop: 12 }}>
        LOCAL stays on this computer. ATTESTED_TEE is a NEAR model whose hardware attestation verified. EXTERNAL_UNVERIFIED is any other remote endpoint; it never replaces a TEE for prompts that require one. Change the default and failover order in Settings.
      </p>
      {deleting ? (
        <ConfirmDialog
          title={`Remove ${deleting.display_name}?`}
          danger
          confirmLabel="Remove destination"
          onClose={() => setDeleting(null)}
          onConfirm={async () => {
            await del('/providers/' + encodeURIComponent(deleting.id));
            await providers.reload();
          }}
        >
          <p>Its stored credential is deleted from the OS credential store. Apps routed to it fall back per your failover settings or get an error.</p>
        </ConfirmDialog>
      ) : null}
    </Card>
  );
}

/** NEAR model list with per-model attestation verification. */
function NearCard({ verifications, onChanged, hasNear }: { verifications: NearVerification[]; onChanged: () => void; hasNear: boolean }) {
  const near = useLoad(() => get<{ models: NearModel[]; spec_preferred: string }>('/near/models'), []);
  const [showAll, setShowAll] = useState(false);
  const [results, setResults] = useState<Record<string, NearVerification>>({});
  const [busy, setBusy] = useState<string | null>(null);
  const [err, setErr] = useState<string | null>(null);
  const [detail, setDetail] = useState<NearVerification | null>(null);
  const [setup, setSetup] = useState<string | null>(null);
  const cached = Object.fromEntries(verifications.map((v) => [v.model, v]));
  const all = near.data?.models ?? [];
  const shown = showAll ? all : all.filter(isTeeCandidate);
  const verify = async (model: string) => {
    setBusy(model);
    setErr(null);
    try {
      const v = await post<NearVerification>('/near/verify', { model });
      setResults((r) => ({ ...r, [model]: v }));
      setDetail(v);
    } catch (e) {
      setErr(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(null);
    }
  };
  const aliasOf = near.data ? all.find((m) => m.aliases.includes(near.data!.spec_preferred)) : undefined;
  return (
    <Card
      title="NEAR AI private inference"
      actions={
        <Check checked={showAll} onChange={setShowAll}>
          Show non-TEE models
        </Check>
      }
    >
      <div className="stack">
        <p className="muted small">
          NEAR AI runs models inside hardware TEEs and publishes attestation. Prompt VPN only treats a model as ATTESTED_TEE after verifying its attestation report here. {hasNear ? '' : 'NEAR is not configured yet; verify a model, then use it.'}
        </p>
        {aliasOf ? (
          <Notice tone="info">
            The spec's preferred <span className="mono">{near.data!.spec_preferred}</span> is currently an alias of <span className="mono">{aliasOf.id}</span>, so it is not offered. The current DeepSeek TEE model is{' '}
            <span className="mono">{DEFAULT_NEAR_CANDIDATE}</span>.
          </Notice>
        ) : null}
        <ErrorText error={near.error || err} />
        {near.loading && !near.data ? (
          <Loading what="Discovering NEAR models" />
        ) : (
          <div className="table-wrap">
            <table className="table">
              <thead>
                <tr>
                  <th>Model</th>
                  <th>Privacy tier</th>
                  <th>Attestation</th>
                  <th />
                </tr>
              </thead>
              <tbody>
                {shown.map((m) => {
                  const v = results[m.id] ?? cached[m.id];
                  return (
                    <tr key={m.id}>
                      <td>
                        <div className="mono small" style={{ fontWeight: 600 }}>
                          {m.id}
                        </div>
                        <div className="muted small">
                          {m.name}
                          {m.id === DEFAULT_NEAR_CANDIDATE ? ' · recommended' : ''}
                        </div>
                      </td>
                      <td>
                        <TierBadge tier={m.privacy_tier} />
                      </td>
                      <td className="small">
                        {v ? (
                          <Button variant="link" onClick={() => setDetail(v)}>
                            {v.eligible ? 'Verified eligible' : 'Not eligible'}
                          </Button>
                        ) : m.attestation_supported ? (
                          <span className="muted">not checked</span>
                        ) : (
                          <span className="muted">not supported</span>
                        )}
                      </td>
                      <td className="right nowrap">
                        <div className="row gap-s" style={{ justifyContent: 'flex-end' }}>
                          {m.attestation_supported ? (
                            <Button small busy={busy === m.id} onClick={() => verify(m.id)}>
                              Verify
                            </Button>
                          ) : null}
                          {v?.eligible ? (
                            <Button small variant="primary" onClick={() => setSetup(m.id)}>
                              Use
                            </Button>
                          ) : null}
                        </div>
                      </td>
                    </tr>
                  );
                })}
              </tbody>
            </table>
          </div>
        )}
      </div>
      {detail ? (
        <Dialog wide title="Attestation" onClose={() => setDetail(null)} footer={<Button onClick={() => setDetail(null)}>Close</Button>}>
          <AttestationDetails v={detail} />
        </Dialog>
      ) : null}
      {setup ? <NearSetupDialog model={setup} onClose={() => setSetup(null)} onSaved={onChanged} /> : null}
    </Card>
  );
}

function NearSetupDialog({ model, onClose, onSaved }: { model: string; onClose: () => void; onSaved: () => void }) {
  const [key, setKey] = useState('');
  const act = useAction();
  return (
    <Dialog
      title="Use NEAR AI"
      onClose={onClose}
      footer={
        <>
          <Button onClick={onClose}>Cancel</Button>
          <Button
            variant="primary"
            busy={act.busy}
            onClick={() =>
              act.run(async () => {
                const cfg: ProviderConfig & { api_key?: string } = {
                  schema_version: 1,
                  id: 'near',
                  kind: 'NEAR_AI',
                  display_name: 'NEAR AI',
                  base_url: 'https://cloud-api.near.ai/v1',
                  default_model: model,
                  declared_tier: 'ATTESTED_TEE',
                  credential_ref: 'provider:near',
                  enabled: true,
                  allow_failover: true,
                  supports_responses: true,
                  timeout_secs: 180,
                };
                if (key) cfg.api_key = key;
                await post('/providers', cfg);
                setKey('');
                onSaved();
                onClose();
              })
            }
          >
            Save destination
          </Button>
        </>
      }
    >
      <p>
        Saves NEAR AI as destination <span className="mono">near</span> with model <span className="mono">{model}</span>. Attestation is re-verified before private routing. Set it as the default or private route in Settings.
      </p>
      <Field label="NEAR AI API key" hint="Stored in the OS credential store. Leave blank to keep an existing key.">
        <SecretInput value={key} onChange={setKey} />
      </Field>
      <ErrorText error={act.error} />
    </Dialog>
  );
}

function OllamaGenCard({ m, onSaved }: { m: ModelsStatus | null; onSaved: () => void }) {
  const [model, setModel] = useState('');
  const [url, setUrl] = useState('http://127.0.0.1:11434/v1');
  const [done, setDone] = useState(false);
  const act = useAction();
  const tags = (m?.ollama.models ?? []).filter((t) => !/guard/i.test(t));
  return (
    <Card title="Ollama (optional local generation)" actions={m ? m.ollama.reachable ? <Badge tone="neutral">running{m.ollama.version ? ` v${m.ollama.version}` : ''}</Badge> : <Badge tone="outline">not running</Badge> : null}>
      <div className="stack">
        <p className="muted small">
          Optional. Ollama can generate replies on this computer (tier LOCAL), as a fallback when NEAR is unavailable or for private handling if you allow it in Settings. Prompt VPN does not use Ollama for detection.
        </p>
        {m && !m.ollama.reachable ? <p className="small muted">{m.ollama.install ?? m.ollama.error ?? 'Install Ollama from https://ollama.com/download'}</p> : null}
        <Field label="Model" hint={m?.ollama.reachable ? 'Pick an installed model or type a name' : 'Ollama is not reachable'}>
          <input className="input mono" list="ollama-tags" value={model} onChange={(e) => setModel(e.target.value)} placeholder="qwen3.5:4b" />
          <datalist id="ollama-tags">
            {tags.map((t) => (
              <option key={t} value={t} />
            ))}
          </datalist>
        </Field>
        <Field label="Ollama URL" hint="Must be loopback">
          <input className="input mono" value={url} onChange={(e) => setUrl(e.target.value)} />
        </Field>
        {done ? <Notice tone="ok">Saved destination ollama.</Notice> : null}
        <ErrorText error={act.error} />
        <div className="row" style={{ justifyContent: 'flex-end' }}>
          <Button
            variant="primary"
            busy={act.busy}
            disabled={!model.trim()}
            onClick={() =>
              act.run(async () => {
                const cfg: ProviderConfig = {
                  schema_version: 1,
                  id: 'ollama',
                  kind: 'OLLAMA',
                  display_name: `Ollama (${model.trim()})`,
                  base_url: url.trim(),
                  default_model: model.trim(),
                  declared_tier: 'LOCAL',
                  credential_ref: null,
                  enabled: true,
                  allow_failover: true,
                  supports_responses: false,
                  timeout_secs: 300,
                };
                await post('/providers', cfg);
                setDone(true);
                onSaved();
              })
            }
          >
            Save Ollama destination
          </Button>
        </div>
      </div>
    </Card>
  );
}

function CustomEndpointCard({ onSaved }: { onSaved: () => void }) {
  const [id, setId] = useState('custom');
  const [name, setName] = useState('');
  const [url, setUrl] = useState('');
  const [model, setModel] = useState('');
  const [key, setKey] = useState('');
  const [failover, setFailover] = useState(false);
  const [ack, setAck] = useState(false);
  const [done, setDone] = useState(false);
  const act = useAction();
  return (
    <Card title="Custom OpenAI-compatible endpoint">
      <div className="stack">
        <div className="form-grid">
          <Field label="ID">
            <input className="input mono" value={id} onChange={(e) => setId(e.target.value)} />
          </Field>
          <Field label="Display name">
            <input className="input" value={name} onChange={(e) => setName(e.target.value)} placeholder="Company gateway" />
          </Field>
          <div className="full">
            <Field label="Base URL" hint="https, or http on loopback only">
              <input className="input mono" value={url} onChange={(e) => setUrl(e.target.value)} placeholder="https://llm.example.com/v1" />
            </Field>
          </div>
          <Field label="Model">
            <input className="input mono" value={model} onChange={(e) => setModel(e.target.value)} />
          </Field>
          <Field label="API key" hint="Optional. Stored in the OS credential store.">
            <SecretInput value={key} onChange={setKey} />
          </Field>
        </div>
        <Check checked={failover} onChange={setFailover}>
          Allow as failover when the default is unavailable
        </Check>
        <div className="alert alert-warn">
          <Check checked={ack} onChange={setAck}>
            I understand this endpoint is <strong>EXTERNAL_UNVERIFIED</strong>: Prompt VPN cannot verify where it runs or who can read the prompts. It receives only content your policy already allows for external destinations, and it never replaces a verified TEE.
          </Check>
        </div>
        {done ? <Notice tone="ok">Saved destination {id}.</Notice> : null}
        <ErrorText error={act.error} />
        <div className="row" style={{ justifyContent: 'flex-end' }}>
          <Button
            variant="primary"
            busy={act.busy}
            disabled={!ack || !url.trim() || !model.trim() || !id.trim()}
            onClick={() =>
              act.run(async () => {
                const cfg: ProviderConfig & { api_key?: string } = {
                  schema_version: 1,
                  id: id.trim(),
                  kind: 'OPENAI_COMPATIBLE',
                  display_name: name.trim() || 'Custom endpoint',
                  base_url: url.trim(),
                  default_model: model.trim(),
                  declared_tier: 'EXTERNAL_UNVERIFIED',
                  credential_ref: null,
                  enabled: true,
                  allow_failover: failover,
                  supports_responses: false,
                  timeout_secs: 180,
                };
                if (key) cfg.api_key = key;
                await post('/providers', cfg);
                setKey('');
                setDone(true);
                onSaved();
              })
            }
          >
            Save endpoint
          </Button>
        </div>
      </div>
    </Card>
  );
}
