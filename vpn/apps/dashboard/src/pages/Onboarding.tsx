import { useEffect, useMemo, useState } from 'react';
import { registerBuiltinLabels } from '../entities';
import {
  get,
  post,
  put,
  type Action,
  type CategoryInfo,
  type CategoryState,
  type EntitiesResponse,
  type FallbackMode,
  type NearModel,
  type NearVerification,
  type OnboardingComplete,
  type OnboardingInfo,
  type ProviderEntry,
  type SettingsResponse,
  type SpanInstallResult,
  type SpanSize,
} from '../api';
import { megabytes } from '../format';
import { href, navigate } from '../router';
import { Badge, Button, Check, CopyButton, ErrorText, Field, Loading, Notice, Radio, SecretInput, Segmented, Toggle, TierBadge, errMessage, useAction } from '../ui';
import { CriticalWarning } from './Categories';
import { EntityForm, type EntityDraft } from './Entities';
import { KeyOnce } from './Apps';
import { AttestationDetails, DEFAULT_NEAR_CANDIDATE, SPAN_SIZES, isTeeCandidate, spanName, spanSizeOf } from './Models';
import { ActionSelect } from './ApprovalDetail';

const STEPS = 6;
type Choice = 'ALLOW' | 'TOKENIZE' | 'REDACT' | 'BLOCK';
const CHOICES: { value: Choice; label: string }[] = [
  { value: 'ALLOW', label: 'Allow' },
  { value: 'TOKENIZE', label: 'Tokenize' },
  { value: 'REDACT', label: 'Redact' },
  { value: 'BLOCK', label: 'Block' },
];

interface Loaded {
  info: OnboardingInfo;
  cats: CategoryState[];
  providers: ProviderEntry[];
  settings: SettingsResponse;
  /** Labels of entity types that already exist (on a re-run), to catch duplicates. */
  entityLabels: string[];
}

interface Draft {
  critOff: Record<string, { action: Action; confirm: string }>;
  critOn: Record<string, boolean>;
  categories: Record<string, Action>;
  secrets: { label: string; value: string; action: Action }[];
  entities: EntityDraft[];
  fallbackMode: FallbackMode;
  teeConsent: boolean;
  localPrivate: boolean;
  /** Detection model size to install, or 'none' for deterministic rules only. */
  spanModel: SpanSize | 'none';
  nearSkip: boolean;
  nearKey: string;
  nearModel: string;
  ollamaOn: boolean;
  ollamaModel: string;
  customOn: boolean;
  customUrl: string;
  customModel: string;
  customKey: string;
  customFailover: boolean;
  customAck: boolean;
  defaultDestination: string;
  notifications: boolean;
}

/** The installed, selected size; else the recommended size on a first run; else none. */
function initialSpan(info: OnboardingInfo): SpanSize | 'none' {
  const selected = info.span_options.find((o) => o.model === info.span_model.model);
  if (selected?.installed) return spanSizeOf(selected.model)?.size ?? 'none';
  if (info.state.completed && info.span_options.every((o) => !o.installed)) return 'none';
  const def = info.span_options.find((o) => o.default);
  return spanSizeOf(def?.model)?.size ?? 'base';
}

export default function OnboardingPage({ onDone }: { onDone: () => void }) {
  const [data, setData] = useState<Loaded | null>(null);
  const [loadErr, setLoadErr] = useState<string | null>(null);
  const [step, setStep] = useState(1);
  const [draft, setDraft] = useState<Draft | null>(null);
  const [stepErr, setStepErr] = useState<string | null>(null);
  const [result, setResult] = useState<OnboardingComplete | null>(null);
  const submit = useAction();

  useEffect(() => {
    Promise.all([
      get<OnboardingInfo>('/onboarding'),
      get<{ categories: CategoryState[] }>('/categories'),
      get<{ providers: ProviderEntry[] }>('/providers'),
      get<SettingsResponse>('/settings'),
      get<EntitiesResponse>('/entities').then((r) => (registerBuiltinLabels(r.builtin_labels), r)),
    ])
      .then(([info, cats, providers, settings, entities]) => {
        setData({ info, cats: cats.categories, providers: providers.providers, settings, entityLabels: entities.entities.map((e) => e.entity.label) });
        const categories: Record<string, Action> = {};
        for (const c of cats.categories.filter((c) => !c.critical)) categories[c.id] = (c.action ?? info.defaults[c.id] ?? 'TOKENIZE') as Action;
        const critOn: Record<string, boolean> = {};
        for (const c of cats.categories.filter((c) => c.critical)) critOn[c.id] = c.action === 'BLOCK';
        const near = providers.providers.find((p) => p.config.kind === 'NEAR_AI');
        const ollama = providers.providers.find((p) => p.config.kind === 'OLLAMA');
        setDraft({
          critOff: {},
          critOn,
          categories,
          secrets: [],
          entities: [],
          fallbackMode: settings.engine.fallback.mode === 'TEE_PRIVATE_ROUTE' ? 'TEE_PRIVATE_ROUTE' : 'BLOCK_AND_REVIEW',
          teeConsent: false, // never pre-checked, even on re-run
          localPrivate: settings.engine.fallback.local_private_handling,
          spanModel: initialSpan(info),
          nearSkip: false,
          nearKey: '',
          nearModel: near?.config.default_model ?? '',
          ollamaOn: false,
          ollamaModel: ollama?.config.default_model ?? '',
          customOn: false,
          customUrl: '',
          customModel: '',
          customKey: '',
          customFailover: false,
          customAck: false,
          defaultDestination: settings.engine.default_destination ?? '',
          notifications: settings.notifications.enabled,
        });
      })
      .catch((e) => setLoadErr(errMessage(e)));
  }, []);

  const set = (p: Partial<Draft>) => setDraft((d) => (d ? { ...d, ...p } : d));

  if (!data || !draft) {
    return (
      <WizardFrame>
        <div className="wizard-body" style={{ paddingBottom: 28 }}>
          {loadErr ? <ErrorText error={loadErr} /> : <Loading what="Loading setup" />}
        </div>
      </WizardFrame>
    );
  }

  if (result) return <Finished result={result} draft={draft} onDone={onDone} />;

  const existingNear = data.providers.find((p) => p.config.kind === 'NEAR_AI');
  const needsDownload = draft.spanModel !== 'none' && !data.info.span_options.find((o) => spanSizeOf(o.model)?.size === draft.spanModel)?.installed;

  const validate = (): string | null => {
    if (step === 4 && draft.fallbackMode === 'TEE_PRIVATE_ROUTE' && !draft.teeConsent) return 'Verified NEAR TEE auto-route sends data off this device, so it needs your consent. Tick the box or choose block-and-review.';
    if (step === 5) {
      if (!draft.nearSkip) {
        if (!draft.nearModel) return 'Choose a verified NEAR model, or use Skip for now.';
        if (!draft.nearKey && !existingNear?.has_credential) return 'Enter your NEAR AI API key, or use Skip for now.';
      }
      if (draft.ollamaOn && !draft.ollamaModel.trim()) return 'Enter the Ollama model name or untick the Ollama option.';
      if (draft.customOn) {
        if (!draft.customUrl.trim() || !draft.customModel.trim()) return 'The custom endpoint needs a base URL and model.';
        if (!draft.customAck) return 'Acknowledge that the custom endpoint is EXTERNAL_UNVERIFIED before adding it.';
      }
    }
    return null;
  };

  const next = () => {
    const err = validate();
    setStepErr(err);
    if (!err) setStep((s) => Math.min(STEPS, s + 1));
  };
  const back = () => {
    setStepErr(null);
    setStep((s) => Math.max(1, s - 1));
  };

  const finish = () =>
    submit.run(async () => {
      const body = {
        categories: draft.categories,
        critical_exceptions: Object.entries(draft.critOff).map(([category, v]) => ({ category, action: v.action, confirm: v.confirm })),
        fallback_mode: draft.fallbackMode,
        tee_consent: draft.fallbackMode === 'TEE_PRIVATE_ROUTE' && draft.teeConsent,
        local_private_handling: draft.localPrivate,
        span_model: draft.spanModel === 'none' ? null : draft.spanModel,
        near: draft.nearSkip ? { skip: true } : { skip: false, model: draft.nearModel, ...(draft.nearKey ? { api_key: draft.nearKey } : {}) },
        ...(draft.ollamaOn ? { ollama_generation: { model: draft.ollamaModel.trim() } } : {}),
        ...(draft.customOn
          ? {
              custom: {
                base_url: draft.customUrl.trim(),
                model: draft.customModel.trim(),
                ...(draft.customKey ? { api_key: draft.customKey } : {}),
                allow_failover: draft.customFailover,
                acknowledged_privacy_tier: draft.customAck,
              },
            }
          : {}),
        ...(draft.defaultDestination ? { default_destination: draft.defaultDestination } : {}),
        notifications: draft.notifications,
        secrets: draft.secrets,
        entities: draft.entities.map((e) => ({ label: e.label, action: e.action, ...(e.description ? { description: e.description } : {}) })),
      };
      const r = await post<OnboardingComplete>('/onboarding/complete', body);
      // Critical categories the owner switched back on during a re-run.
      for (const c of data.cats.filter((c) => c.critical && c.action !== 'BLOCK' && draft.critOn[c.id])) {
        await put('/categories/' + encodeURIComponent(c.id), { action: 'BLOCK' });
      }
      // Drop secret values from memory as soon as the daemon has them.
      set({ secrets: draft.secrets.map((s) => ({ ...s, value: '' })), nearKey: '', customKey: '' });
      setResult(r);
    });

  const titles = [
    'Prompt VPN runs on this computer',
    'Critical data stays blocked',
    'Choose what stays private',
    'When Prompt VPN is unsure',
    'Detection model and destinations',
    'Notifications and connect',
  ];

  return (
    <WizardFrame right={data.info.state.completed ? <a href={href('/settings')} className="small muted">Exit setup</a> : <span className="small muted">Get started</span>}>
      <div className="wizard-body">
        <div className="row between">
          <span className="wizard-step">
            {step} of {STEPS}
          </span>
          <div className="wizard-progress" aria-hidden>
            {Array.from({ length: STEPS }, (_, i) => (
              <span key={i} className={i < step ? 'done' : ''} />
            ))}
          </div>
        </div>
        <h1>{titles[step - 1]}</h1>
        {step === 1 ? <Step1 info={data.info} /> : null}
        {step === 2 ? <Step2 cats={data.cats} draft={draft} set={set} /> : null}
        {step === 3 ? <Step3 cats={data.cats} draft={draft} set={set} existingEntities={data.entityLabels} /> : null}
        {step === 4 ? <Step4 draft={draft} set={set} /> : null}
        {step === 5 ? <Step5 info={data.info} draft={draft} set={set} existingNear={existingNear} providers={data.providers} /> : null}
        {step === 6 ? <Step6 draft={draft} set={set} info={data.info} /> : null}
        {step === STEPS && submit.busy && needsDownload ? (
          <Notice tone="info">Saving your policy, then downloading and verifying the detection model. This can take a minute.</Notice>
        ) : null}
        <ErrorText error={stepErr || submit.error} />
      </div>
      <div className="wizard-foot">
        <div className="row gap">
          {step > 1 ? (
            <Button variant="link" onClick={back}>
              Back
            </Button>
          ) : null}
          {step === 5 ? (
            <Button
              variant="link"
              onClick={() => {
                set({ nearSkip: true, nearKey: '' });
                setStepErr(null);
                setStep(6);
              }}
            >
              Skip for now
            </Button>
          ) : null}
        </div>
        {step < STEPS ? (
          <Button variant="primary" onClick={next}>
            Continue
          </Button>
        ) : (
          <Button variant="primary" busy={submit.busy} onClick={finish}>
            {submit.busy && needsDownload ? 'Downloading the detection model' : 'Finish setup'}
          </Button>
        )}
      </div>
    </WizardFrame>
  );
}

function WizardFrame({ children, right }: { children: React.ReactNode; right?: React.ReactNode }) {
  return (
    <div className="wizard-page">
      <div className="wizard">
        <div className="wizard-top">
          <span className="brand">prompt vpn</span>
          {right}
        </div>
        {children}
      </div>
    </div>
  );
}

// ------------------------------------------------------------------ step 1

function Step1({ info }: { info: OnboardingInfo }) {
  return (
    <>
      <p className="lede">
        Prompt VPN sits between your AI apps and the model. It checks every prompt on this computer, removes or blocks what you choose, and only then forwards it.
      </p>
      <div className="stack">
        <div>
          <div className="section-title">What happens locally</div>
          <p className="muted">Detection (pattern rules, your exact values and a small GLiNER detection model), your policy, the encrypted vault and the audit log all run here. Audit entries never contain prompt text.</p>
        </div>
        <div>
          <div className="section-title">Your local API</div>
          <p className="muted" style={{ marginBottom: 8 }}>
            Point any OpenAI-compatible client at this base URL instead of the provider. You get an API key at the end of setup.
          </p>
          <div className="keybox">
            <code>{info.base_url}</code>
            <CopyButton text={info.base_url} />
          </div>
        </div>
        <Notice tone="warn">
          <strong>Coverage limit.</strong> Prompt VPN inspects messages sent through this API, including tool-call arguments and tool results inside them. It does not intercept tools your agent runs on its own, such as shell commands, HTTP requests or a browser.
        </Notice>
      </div>
    </>
  );
}

// ------------------------------------------------------------------ step 2

function Step2({ cats, draft, set }: { cats: CategoryState[]; draft: Draft; set: (p: Partial<Draft>) => void }) {
  const [warn, setWarn] = useState<CategoryInfo | null>(null);
  const critical = cats.filter((c) => c.critical);
  return (
    <>
      <p className="lede">These are blocked before any remote destination is considered. They start on. Turning one off needs a typed confirmation and is recorded.</p>
      <div className="list">
        {critical.map((c) => {
          const on = draft.critOn[c.id] && !draft.critOff[c.id];
          return (
            <div className="option-row" key={c.id}>
              <div className="li-main">
                <div className="li-title row gap-s">
                  {c.label}
                  {!on ? <Badge tone="red">off</Badge> : null}
                </div>
                <div className="li-sub">{c.description}</div>
                <div className="examples">{c.examples.join('  ·  ')}</div>
              </div>
              <Toggle
                label={`${c.label} protection`}
                checked={!!on}
                onChange={(v) => {
                  if (!v) setWarn(c);
                  else {
                    const { [c.id]: _drop, ...rest } = draft.critOff;
                    set({ critOff: rest, critOn: { ...draft.critOn, [c.id]: true } });
                  }
                }}
              />
            </div>
          );
        })}
      </div>
      {warn ? (
        <CriticalWarning
          cat={warn}
          onClose={() => setWarn(null)}
          onConfirm={(action, phrase) => {
            set({ critOff: { ...draft.critOff, [warn.id]: { action, confirm: phrase } }, critOn: { ...draft.critOn, [warn.id]: false } });
          }}
        />
      ) : null}
    </>
  );
}

// ------------------------------------------------------------------ step 3

function Step3({ cats, draft, set, existingEntities }: { cats: CategoryState[]; draft: Draft; set: (p: Partial<Draft>) => void; existingEntities: string[] }) {
  const other = cats.filter((c) => !c.critical);
  const [label, setLabel] = useState('');
  const [value, setValue] = useState('');
  const [vAction, setVAction] = useState<Action>('BLOCK');
  const [showValue, setShowValue] = useState(false);
  const [showEntity, setShowEntity] = useState(false);

  return (
    <>
      <p className="lede">Prompt VPN scans your prompts and keeps the information you choose private before it ever leaves your device.</p>
      <p className="muted small">Tokenize swaps a value for a placeholder and restores it in the reply. Redact removes it. Block stops the prompt.</p>
      <div className="list">
        {other.map((c) => {
          const cur = draft.categories[c.id] ?? 'ALLOW';
          const isChoice = CHOICES.some((x) => x.value === cur);
          return (
            <div className="option-row" key={c.id}>
              <div className="li-main">
                <div className="li-title">{c.label}</div>
                <div className="li-sub">{c.description}</div>
              </div>
              {isChoice ? (
                <Segmented<Choice> label={c.label} value={cur as Choice} options={CHOICES} onChange={(v) => set({ categories: { ...draft.categories, [c.id]: v } })} />
              ) : (
                <div className="row gap-s">
                  <Badge tone="outline">{cur}</Badge>
                  <Button small variant="ghost" onClick={() => set({ categories: { ...draft.categories, [c.id]: 'TOKENIZE' } })}>
                    Change
                  </Button>
                </div>
              )}
            </div>
          );
        })}
      </div>

      <div className="stack">
        <div className="option-row" style={{ borderTop: '1px solid var(--line)' }}>
          <div className="li-main">
            <div className="li-title">Exact values</div>
            <div className="li-sub">Customer IDs, internal hostnames, codenames. Optional.</div>
          </div>
          <Toggle label="Add exact values" checked={showValue} onChange={setShowValue} />
        </div>
        {showValue ? (
          <div className="stack">
            <div className="form-grid">
              <Field label="Label">
                <input className="input" value={label} onChange={(e) => setLabel(e.target.value)} placeholder="Acme customer ID" />
              </Field>
              <Field label="Action">
                <ActionSelect value={vAction} onChange={setVAction} options={['BLOCK', 'REDACT', 'TOKENIZE', 'REQUIRE_APPROVAL']} />
              </Field>
              <div className="full">
                <Field label="Value" hint="Masked. Held in this tab until you finish, then stored encrypted by the daemon.">
                  <SecretInput value={value} onChange={setValue} />
                </Field>
              </div>
            </div>
            <div className="row" style={{ justifyContent: 'flex-end' }}>
              <Button
                small
                disabled={!label.trim() || !value}
                onClick={() => {
                  set({ secrets: [...draft.secrets, { label: label.trim(), value, action: vAction }] });
                  setLabel('');
                  setValue('');
                }}
              >
                Add value
              </Button>
            </div>
          </div>
        ) : null}
        {draft.secrets.length ? (
          <div className="list">
            {draft.secrets.map((s, i) => (
              <div className="list-item" key={i}>
                <div className="li-main">
                  <div className="li-title">{s.label}</div>
                  <div className="li-sub mono">{'•'.repeat(Math.min(12, s.value.length))}</div>
                </div>
                <Badge tone="outline">{s.action}</Badge>
                <Button small variant="ghost" onClick={() => set({ secrets: draft.secrets.filter((_, j) => j !== i) })}>
                  Remove
                </Button>
              </div>
            ))}
          </div>
        ) : null}

        <div className="option-row" style={{ borderTop: '1px solid var(--line)' }}>
          <div className="li-main">
            <div className="li-title">Custom entity types</div>
            <div className="li-sub">Kinds of information with no fixed format, like an insurance policy number or a project codename. The detection model finds them in free text. Optional.</div>
          </div>
          <Toggle label="Add custom entity types" checked={showEntity} onChange={setShowEntity} />
        </div>
        {showEntity ? (
          <>
            {draft.spanModel === 'none' ? <Notice tone="warn">Custom entity types need a detection model. Pick a size in step 5, or prompts that need one are held for review.</Notice> : null}
            <EntityForm
              submitLabel="Add entity type"
              existing={[...existingEntities, ...draft.entities.map((x) => x.label)]}
              onSubmit={async (e) => {
                set({ entities: [...draft.entities, e] });
              }}
            />
          </>
        ) : null}
        {draft.entities.length ? (
          <div className="list">
            {draft.entities.map((e, i) => (
              <div className="list-item" key={i}>
                <div className="li-main">
                  <div className="li-title">{e.label}</div>
                  {e.description ? <div className="li-sub ellipsis">{e.description}</div> : null}
                </div>
                <Badge tone="outline">{e.action}</Badge>
                <Button small variant="ghost" onClick={() => set({ entities: draft.entities.filter((_, j) => j !== i) })}>
                  Remove
                </Button>
              </div>
            ))}
          </div>
        ) : null}
      </div>
    </>
  );
}

// ------------------------------------------------------------------ step 4

function Step4({ draft, set }: { draft: Draft; set: (p: Partial<Draft>) => void }) {
  return (
    <>
      <p className="lede">Sometimes a finding is uncertain, or the local detection model is unavailable. Choose what happens then. Explicit block rules always win.</p>
      <div className="stack">
        <div className={`radio-card${draft.fallbackMode === 'TEE_PRIVATE_ROUTE' ? ' selected' : ''}`}>
          <Radio name="fb" checked={draft.fallbackMode === 'TEE_PRIVATE_ROUTE'} onChange={() => set({ fallbackMode: 'TEE_PRIVATE_ROUTE' })}>
            <strong>Verified NEAR TEE auto-route</strong>
            <div className="muted small">
              Sends the prompt off this device, unredacted, to a NEAR AI model running in a hardware TEE whose attestation Prompt VPN verified. Requires your consent and a configured NEAR model. Critical data is still blocked.
            </div>
          </Radio>
          {draft.fallbackMode === 'TEE_PRIVATE_ROUTE' ? (
            <div style={{ marginTop: 12, paddingLeft: 26 }}>
              <Check checked={draft.teeConsent} onChange={(v) => set({ teeConsent: v })}>
                I understand the prompt leaves this device and consent to sending uncertain prompts to a verified NEAR TEE.
              </Check>
            </div>
          ) : null}
        </div>
        <div className={`radio-card${draft.fallbackMode === 'BLOCK_AND_REVIEW' ? ' selected' : ''}`}>
          <Radio name="fb" checked={draft.fallbackMode === 'BLOCK_AND_REVIEW'} onChange={() => set({ fallbackMode: 'BLOCK_AND_REVIEW', teeConsent: false })}>
            <strong>Block locally and review</strong>
            <div className="muted small">The request stops on this computer and waits in Approvals for 15 minutes. Nothing is sent until you approve and the client retries.</div>
          </Radio>
        </div>
        <Check checked={draft.localPrivate} onChange={(v) => set({ localPrivate: v })}>
          Let a local Ollama generation model handle prompts that need private handling (stays on this computer)
        </Check>
      </div>
    </>
  );
}

// ------------------------------------------------------------------ step 5

function Step5({
  info,
  draft,
  set,
  existingNear,
  providers,
}: {
  info: OnboardingInfo;
  draft: Draft;
  set: (p: Partial<Draft>) => void;
  existingNear?: ProviderEntry;
  providers: ProviderEntry[];
}) {
  const [models, setModels] = useState<NearModel[] | null>(null);
  const [modelsErr, setModelsErr] = useState<string | null>(null);
  const [checks, setChecks] = useState<Record<string, NearVerification>>({});
  const [checking, setChecking] = useState<string | null>(null);
  const [other, setOther] = useState('');
  const [showDetail, setShowDetail] = useState<NearVerification | null>(null);

  const verify = async (model: string) => {
    setChecking(model);
    try {
      const v = await post<NearVerification>('/near/verify', { model });
      setChecks((c) => ({ ...c, [model]: v }));
      if (v.eligible && (!draft.nearModel || model === DEFAULT_NEAR_CANDIDATE)) set({ nearModel: model, nearSkip: false });
      if (!v.eligible && draft.nearModel === model) set({ nearModel: '' });
      return v;
    } catch (e) {
      setModelsErr(errMessage(e));
    } finally {
      setChecking(null);
    }
  };

  useEffect(() => {
    get<{ models: NearModel[] }>('/near/models')
      .then((r) => {
        setModels(r.models);
        const want = draft.nearModel || DEFAULT_NEAR_CANDIDATE;
        const first = r.models.find((m) => m.id === want) ?? r.models.find((m) => m.id === DEFAULT_NEAR_CANDIDATE);
        if (first) void verify(first.id);
      })
      .catch((e) => setModelsErr(errMessage(e)));
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  const eligible = Object.values(checks).filter((v) => v.eligible);
  const candidates = (models ?? []).filter(isTeeCandidate).filter((m) => !checks[m.id]);
  const ollamaTags = (info.ollama.models ?? []).filter((t) => !/guard/i.test(t));
  const destOptions = useMemo(() => {
    const list = providers.map((p) => ({ id: p.config.id, label: `${p.config.display_name} (${p.config.declared_tier})` }));
    const add = (id: string, label: string) => !list.some((x) => x.id === id) && list.push({ id, label });
    if (!draft.nearSkip && draft.nearModel) add('near', 'NEAR AI (ATTESTED_TEE)');
    if (draft.ollamaOn) add('ollama', 'Ollama (LOCAL)');
    if (draft.customOn) add('custom', 'Custom endpoint (EXTERNAL_UNVERIFIED)');
    return list;
  }, [providers, draft.nearSkip, draft.nearModel, draft.ollamaOn, draft.customOn]);

  return (
    <>
      <p className="lede">A small detection model on this computer finds names, health details and your custom entity types in free text. Replies come from the destination you pick here.</p>

      <div className="section-title">Detection model</div>
      <div className="stack-s">
        {SPAN_SIZES.map((x) => {
          const opt = info.span_options.find((o) => o.model === x.model);
          return (
            <div key={x.size} className={`radio-card${draft.spanModel === x.size ? ' selected' : ''}`}>
              <Radio name="span" checked={draft.spanModel === x.size} onChange={() => set({ spanModel: x.size })}>
                <span className="row gap-s">
                  <strong>{x.name}</strong>
                  <span className="muted small">{megabytes(opt?.size_bytes)}</span>
                  {opt?.default ? <Badge tone="dark">recommended</Badge> : null}
                  {opt?.installed ? <Badge>installed</Badge> : <Badge tone="outline">downloads at the end</Badge>}
                </span>
                <div className="muted small">{x.blurb}</div>
              </Radio>
            </div>
          );
        })}
        <div className={`radio-card${draft.spanModel === 'none' ? ' selected' : ''}`}>
          <Radio name="span" checked={draft.spanModel === 'none'} onChange={() => set({ spanModel: 'none' })}>
            <strong>None, rules only</strong>
            <div className="muted small">Critical data, structured PII and exact values are still caught. Free-text names, health details and custom entity types are not; prompts that need the model are held for review.</div>
          </Radio>
        </div>
      </div>
      <p className="muted small">The model downloads from Hugging Face at a pinned revision and is checked against sha256 hashes before it loads. You can change the size later under Models & destinations.</p>

      <div className="section-title">Private inference with NEAR AI (recommended)</div>
      <div className={`radio-card${!draft.nearSkip ? ' selected' : ''}`} style={{ display: 'flex', flexDirection: 'column', gap: 12 }}>
        <p className="small">
          NEAR AI runs open models inside hardware TEEs and publishes attestation reports. Prompt VPN verifies the report before sending anything and lists only models that pass.
        </p>
        <Field label="Verified model">
          <select className="input" value={draft.nearModel} onChange={(e) => set({ nearModel: e.target.value, nearSkip: false })} disabled={!eligible.length}>
            {!eligible.length ? <option value="">{checking ? 'Verifying attestation...' : 'No verified models yet'}</option> : null}
            {eligible.length && !draft.nearModel ? <option value="">Choose a model</option> : null}
            {eligible.map((v) => (
              <option key={v.model} value={v.model}>
                {v.model}
                {v.model === DEFAULT_NEAR_CANDIDATE ? ' (recommended)' : ''}
              </option>
            ))}
          </select>
        </Field>
        {draft.nearModel && checks[draft.nearModel] ? (
          <div className="row gap-s small">
            <TierBadge tier={checks[draft.nearModel].tier} />
            <span className="muted">
              GPU {checks[draft.nearModel].gpu_verified ? 'attested' : 'not attested'} · TCB {checks[draft.nearModel].gateway_tcb ?? 'unknown'}
            </span>
            <Button variant="link" onClick={() => setShowDetail(checks[draft.nearModel])}>
              Details
            </Button>
          </div>
        ) : null}
        {candidates.length ? (
          <div className="row gap-s" style={{ alignItems: 'flex-end' }}>
            <Field label="Check another TEE model">
              <select className="input" value={other} onChange={(e) => setOther(e.target.value)}>
                <option value="">Select a model to verify</option>
                {candidates.map((m) => (
                  <option key={m.id} value={m.id}>
                    {m.id} ({m.privacy_tier})
                  </option>
                ))}
              </select>
            </Field>
            <Button
              busy={!!checking && checking === other}
              disabled={!other}
              onClick={async () => {
                const v = await verify(other);
                if (v?.eligible) set({ nearModel: other, nearSkip: false });
                setOther('');
              }}
            >
              Verify
            </Button>
          </div>
        ) : null}
        {Object.values(checks).filter((v) => !v.eligible).map((v) => (
          <p key={v.model} className="small" style={{ color: 'var(--red)' }}>
            {v.model} is not eligible: {v.error}
          </p>
        ))}
        {modelsErr ? <ErrorText error={modelsErr} /> : null}
        {!models && !modelsErr ? <Loading what="Discovering NEAR models" /> : null}
        <Field label="NEAR AI API key" hint={existingNear?.has_credential ? 'A key is already stored. Leave blank to keep it.' : 'Stored in the OS credential store, never in the browser.'}>
          <SecretInput value={draft.nearKey} onChange={(v) => set({ nearKey: v, nearSkip: false })} />
        </Field>
        {draft.nearSkip ? <Notice tone="info">NEAR is skipped. You can finish local-only setup and add it later under Models & destinations.</Notice> : null}
      </div>

      <div className="section-title">Other destinations</div>
      <div className="stack">
        <Check checked={draft.ollamaOn} onChange={(v) => set({ ollamaOn: v })}>
          Use a local Ollama model for replies (LOCAL, stays on this computer)
        </Check>
        {draft.ollamaOn && !info.ollama.reachable ? (
          <Notice tone="warn">
            Ollama is not running. {info.ollama.install ?? ''}{' '}
            <a href="https://ollama.com/download" target="_blank" rel="noreferrer noopener">
              Download Ollama
            </a>
            . You can save the destination now and start Ollama later.
          </Notice>
        ) : null}
        {draft.ollamaOn ? (
          <Field label="Ollama model">
            <input className="input mono" list="ob-ollama" value={draft.ollamaModel} onChange={(e) => set({ ollamaModel: e.target.value })} placeholder="qwen3.5:4b" />
            <datalist id="ob-ollama">
              {ollamaTags.map((t) => (
                <option key={t} value={t} />
              ))}
            </datalist>
          </Field>
        ) : null}
        <Check checked={draft.customOn} onChange={(v) => set({ customOn: v, customAck: v ? draft.customAck : false })}>
          Add a custom OpenAI-compatible endpoint
        </Check>
        {draft.customOn ? (
          <div className="stack">
            <div className="form-grid">
              <div className="full">
                <Field label="Base URL">
                  <input className="input mono" value={draft.customUrl} onChange={(e) => set({ customUrl: e.target.value })} placeholder="https://llm.example.com/v1" />
                </Field>
              </div>
              <Field label="Model">
                <input className="input mono" value={draft.customModel} onChange={(e) => set({ customModel: e.target.value })} />
              </Field>
              <Field label="API key (optional)">
                <SecretInput value={draft.customKey} onChange={(v) => set({ customKey: v })} />
              </Field>
            </div>
            <Check checked={draft.customFailover} onChange={(v) => set({ customFailover: v })}>
              Allow as failover
            </Check>
            <div className="alert alert-warn">
              <Check checked={draft.customAck} onChange={(v) => set({ customAck: v })}>
                I understand this endpoint is <strong>EXTERNAL_UNVERIFIED</strong>. Prompt VPN cannot verify where it runs. It receives only what my policy allows for external destinations and never stands in for a verified TEE.
              </Check>
            </div>
          </div>
        ) : null}
        <Field label="Default destination for replies" hint="Leave on automatic to use the first destination configured here.">
          <select className="input" value={draft.defaultDestination} onChange={(e) => set({ defaultDestination: e.target.value })}>
            <option value="">Automatic</option>
            {destOptions.map((d) => (
              <option key={d.id} value={d.id}>
                {d.label}
              </option>
            ))}
          </select>
        </Field>
      </div>
      {showDetail ? (
        <div className="overlay" onMouseDown={(e) => e.target === e.currentTarget && setShowDetail(null)}>
          <div className="dialog wide" role="dialog" aria-modal="true" aria-label="Attestation details">
            <h2>Attestation</h2>
            <AttestationDetails v={showDetail} />
            <div className="dialog-foot">
              <Button onClick={() => setShowDetail(null)}>Close</Button>
            </div>
          </div>
        </div>
      ) : null}
    </>
  );
}

// ------------------------------------------------------------------ step 6

function Step6({ draft, set, info }: { draft: Draft; set: (p: Partial<Draft>) => void; info: OnboardingInfo }) {
  const off = Object.keys(draft.critOff);
  return (
    <>
      <p className="lede">Get a desktop notification when a prompt is blocked, redacted, held for review or privately routed. Banners never show the sensitive values.</p>
      <div className="option-row">
        <div className="li-main">
          <div className="li-title">Desktop notifications</div>
          <div className="li-sub">Rate limited. You can pick which kinds in Settings.</div>
        </div>
        <Toggle label="Desktop notifications" checked={draft.notifications} onChange={(v) => set({ notifications: v })} />
      </div>
      <div className="section-title">Review</div>
      <dl className="kv">
        <div className="kv-row">
          <dt>Critical protection</dt>
          <dd>{off.length ? <span style={{ color: 'var(--red)' }}>On, except {off.join(', ')}</span> : 'All on'}</dd>
        </div>
        <div className="kv-row">
          <dt>Uncertain prompts</dt>
          <dd>{draft.fallbackMode === 'TEE_PRIVATE_ROUTE' ? 'Verified NEAR TEE auto-route (consented)' : 'Block locally and review'}</dd>
        </div>
        <div className="kv-row">
          <dt>Detection model</dt>
          <dd>{draft.spanModel === 'none' ? 'None, rules only' : spanName(SPAN_SIZES.find((x) => x.size === draft.spanModel)?.model)}</dd>
        </div>
        <div className="kv-row">
          <dt>NEAR AI</dt>
          <dd>{draft.nearSkip ? 'Skipped for now' : <span className="mono">{draft.nearModel}</span>}</dd>
        </div>
        <div className="kv-row">
          <dt>Other destinations</dt>
          <dd>{[draft.ollamaOn ? `Ollama ${draft.ollamaModel}` : '', draft.customOn ? `Custom ${draft.customUrl}` : ''].filter(Boolean).join(', ') || 'None added'}</dd>
        </div>
        <div className="kv-row">
          <dt>Exact values / custom entities</dt>
          <dd>
            {draft.secrets.length} / {draft.entities.length}
          </dd>
        </div>
        <div className="kv-row">
          <dt>Base URL</dt>
          <dd className="mono">{info.base_url}</dd>
        </div>
      </dl>
      <p className="muted small">Finishing saves a new policy version and a consent record. You can change all of this later in Settings.</p>
    </>
  );
}

// ------------------------------------------------------------------ done

function Finished({ result, draft, onDone }: { result: OnboardingComplete; draft: Draft; onDone: () => void }) {
  const [span, setSpan] = useState(result.span_model ?? null);
  const retry = useAction();
  useEffect(() => {
    onDone();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);
  const spanErr = span && 'error' in span ? span.error : null;
  const spanOk = span && !('error' in span) ? span : null;
  return (
    <WizardFrame right={<span className="small muted">Setup saved</span>}>
      <div className="wizard-body">
        <span className="wizard-step">Setup complete</span>
        <h1>You are set up</h1>
        <p className="lede">Your policy is saved{result.near_status === 'NEAR_NOT_CONFIGURED' ? ' and Prompt VPN is running local-only. Add NEAR AI any time from Models & destinations' : ''}.</p>
        {result.api_key ? (
          <KeyOnce apiKey={result.api_key} baseUrl={result.base_url} />
        ) : (
          <div className="stack-s">
            <div className="keybox">
              <code>{result.base_url}</code>
              <CopyButton text={result.base_url} />
            </div>
            <p className="muted small">You already have an app key, so no new key was created. Create more under Apps.</p>
          </div>
        )}
        <div className="stack-s">
          <div className="section-title">Detection model</div>
          {draft.spanModel === 'none' ? (
            <p className="muted small">
              Running on rules and exact values only.{draft.entities.length ? ' Your custom entity types need a model, so prompts that need one are held for review.' : ''} <a href={href('/models')}>Install a detection model</a> any time.
            </p>
          ) : spanOk ? (
            spanOk.loaded ? (
              <Notice tone="ok">{spanName(spanOk.model)} is installed, verified and loaded.</Notice>
            ) : (
              <Notice tone="warn">{spanName(spanOk.model)} is installed but did not load{spanOk.status?.error ? `: ${spanOk.status.error}` : ''}. Check Models & destinations.</Notice>
            )
          ) : (
            <>
              <Notice tone="warn">The detection model was not installed{spanErr ? `: ${spanErr}` : ''}. Your policy is saved; prompts that need the model are held for review until it is installed.</Notice>
              <div>
                <Button
                  busy={retry.busy}
                  onClick={() =>
                    retry.run(async () => {
                      setSpan(await post<SpanInstallResult>('/models/install', { model: draft.spanModel }));
                    })
                  }
                >
                  {retry.busy ? 'Downloading' : 'Try the download again'}
                </Button>
              </div>
              <ErrorText error={retry.error} />
            </>
          )}
        </div>
        <div className="stack-s">
          <div className="section-title">Connect your agent</div>
          <p className="muted small">
            To have a coding agent point your clients here with reversible edits, give it the setup skill at <code>skills/promptvpn-setup</code> in the Prompt VPN repository.
          </p>
        </div>
        <p className="muted small mono">policy {result.policy_hash.slice(0, 23)}</p>
      </div>
      <div className="wizard-foot">
        <Button variant="link" onClick={() => navigate('/activity')}>
          Preview activity
        </Button>
        <Button variant="primary" onClick={() => navigate('/overview')}>
          Open dashboard
        </Button>
      </div>
    </WizardFrame>
  );
}
