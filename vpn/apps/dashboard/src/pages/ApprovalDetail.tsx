import { useState } from 'react';
import { registerBuiltinLabels } from '../entities';
import { ACTIONS, ApiError, get, post, put, type Action, type ApprovalDetail, type CategoryState, type EntitiesResponse } from '../api';
import { invalidateEntityLabels, useEntityLabels } from '../entities';
import { useShell } from '../App';
import { RedactedText, Severity } from '../components';
import { withEntityLabels, categoryLabel, dateTime, shortHash, timeAgo } from '../format';
import { href } from '../router';
import {
  Badge,
  Button,
  Card,
  Code,
  ConfirmDialog,
  Dialog,
  ErrorText,
  Field,
  KV,
  Loading,
  Notice,
  PageHeader,
  SecretInput,
  Segmented,
  TypedConfirmDialog,
  useAction,
  useLoad,
} from '../ui';
import { statusTone } from './Approvals';
import { ENTITY_ACTIONS, SpanModelMissing, entityLabelProblem, normalizeEntityLabel } from './Entities';

type RuleKind = 'exact' | 'category' | 'entity';

export default function ApprovalDetailPage({ id }: { id: string }) {
  useEntityLabels();
  const shell = useShell();
  const d = useLoad(() => get<ApprovalDetail>('/approvals/' + encodeURIComponent(id)), [id]);
  const act = useAction();
  const [result, setResult] = useState<{ tone: 'ok' | 'info' | 'warn' | 'error'; text: string } | null>(null);
  const [revealOpen, setRevealOpen] = useState(false);
  const [revealed, setRevealed] = useState<unknown>(null);
  const [editing, setEditing] = useState(false);
  const [confirm, setConfirm] = useState<null | 'deny' | 'blacklist' | 'whitelist' | 'cancel'>(null);
  const [ruleOpen, setRuleOpen] = useState(false);

  if (!d.data) {
    return (
      <div className="content narrow">
        <PageHeader back={{ href: href('/approvals'), label: 'Approvals' }} title="Review required" />
        <ErrorText error={d.error} />
        {d.loading ? <Loading /> : null}
      </div>
    );
  }

  const { approval: a, event: ev, payload_available } = d.data;
  const expired = new Date(a.expires_at).getTime() < Date.now();
  const open = (a.status === 'PENDING' || a.status === 'APPROVED') && !expired;
  const pending = a.status === 'PENDING' && !expired;
  const unapprovable = a.approved_payload_hash === 'unapprovable';

  const done = async (text: string, tone: 'ok' | 'info' = 'ok') => {
    setResult({ tone, text });
    await d.reload();
    await shell.refresh();
  };

  const decide = (action: 'approve' | 'deny' | 'cancel' | 'whitelist' | 'blacklist') =>
    act.run(async () => {
      const r = await post<Record<string, unknown>>(`/approvals/${encodeURIComponent(id)}/${action}`);
      const msg: Record<string, string> = {
        approve: 'Approved once. Nothing was sent yet: retry the request from your client within the approval window and it will go through.',
        deny: 'Denied. The held prompt was discarded.',
        cancel: 'Cancelled. The held prompt was discarded.',
        whitelist: `Allowlisted ${String(r.allowlisted ?? 0)} value(s) and approved once. Retry the request from your client.`,
        blacklist: `Registered ${String(r.registered ?? 0)} value(s) as always blocked and denied this request.`,
      };
      await done(msg[action]);
    });

  return (
    <div className="content narrow">
      <PageHeader
        back={{ href: href('/approvals'), label: 'Approvals' }}
        title="Review required"
        subtitle={
          pending
            ? 'This prompt contains sensitive information and needs your decision before it can be sent to the model.'
            : `This request is ${expired && a.status === 'PENDING' ? 'expired' : a.status.toLowerCase()}.`
        }
        actions={<Badge tone={expired && a.status === 'PENDING' ? 'neutral' : statusTone(a.status)}>{expired && a.status === 'PENDING' ? 'EXPIRED' : a.status}</Badge>}
      />

      {result ? <Notice tone={result.tone}>{result.text}</Notice> : null}
      <ErrorText error={act.error} />

      <div className="stack-s">
        <div className="row between">
          <h3>Prompt (redacted locally)</h3>
          {payload_available ? (
            <Button variant="link" onClick={() => setRevealOpen(true)}>
              Reveal original
            </Button>
          ) : (
            <span className="muted small">Original payload {expired || !open ? 'destroyed' : 'unavailable'}</span>
          )}
        </div>
        <RedactedText text={a.redacted_preview} />
      </div>

      {revealed !== null ? (
        <Card title="Original request (owner only)" actions={<Button small onClick={() => setRevealed(null)}>Hide</Button>}>
          <p className="muted small" style={{ marginBottom: 10 }}>
            Shown only in this tab. It is not stored by the dashboard and disappears when you hide it or leave the page.
          </p>
          <Code>{JSON.stringify(revealed, null, 2)}</Code>
        </Card>
      ) : null}

      {open ? (
        <div className="stack-s">
          <div className="decision-actions">
            {pending ? (
              <Button variant="primary" busy={act.busy} disabled={unapprovable} title={unapprovable ? 'A non-overridable rule still blocks this request' : undefined} onClick={() => decide('approve')}>
                Approve once
              </Button>
            ) : null}
            <Button onClick={() => setEditing(true)} disabled={!payload_available}>
              Edit prompt
            </Button>
            {pending ? <Button onClick={() => setConfirm('blacklist')}>Always block</Button> : null}
            {pending ? <Button onClick={() => setConfirm('deny')}>Deny</Button> : null}
            {pending ? (
              <Button onClick={() => setConfirm('whitelist')} disabled={unapprovable}>
                Allowlist values
              </Button>
            ) : null}
            <Button onClick={() => setRuleOpen(true)}>Save as rule</Button>
            {a.status === 'APPROVED' ? <Button onClick={() => setConfirm('cancel')}>Cancel approval</Button> : null}
          </div>
          <p className="muted small">
            Approving does not resend anything. Your client already received an approval-required error and must retry the same request before the approval expires.
          </p>
          {unapprovable ? <Notice tone="warn">A rule that cannot be overridden still blocks this request, so it cannot be approved. Edit the prompt instead.</Notice> : null}
        </div>
      ) : (
        <Button onClick={() => setRuleOpen(true)}>Save as rule</Button>
      )}

      <Card title="Decision details">
        <KV
          rows={[
            ['Detected', a.categories.length ? a.categories.map(categoryLabel).join(', ') : 'Uncertain content'],
            ['Reason', a.reason ? withEntityLabels(a.reason) : a.reason],
            ['Policy', <span className="mono small" title={a.policy_hash}>{shortHash(a.policy_hash, 16)}</span>],
            ['Action', ev ? ev.action : 'REQUIRE_APPROVAL'],
            ['Severity', ev ? <Severity s={ev.severity} /> : null],
            ['Rules', ev?.policy_ids.length ? ev.policy_ids.join(', ') : null],
            ['Destination', a.destination_id],
            ['Endpoint', <span className="mono">{a.endpoint}</span>],
            ['App', <span className="mono">{a.app_id}</span>],
            ['Time', dateTime(a.created_at)],
            ['Expires', `${dateTime(a.expires_at)} (${timeAgo(a.expires_at)})`],
            ['Decided', a.decided_at ? `${dateTime(a.decided_at)} by ${a.decided_by ?? 'owner'}` : null],
            ['Event', ev ? <a href={href('/activity/' + encodeURIComponent(ev.event_id))}>{ev.event_id}</a> : null],
            ['Request hash', <span className="mono small">{shortHash(a.request_hash, 20)}</span>],
          ]}
        />
      </Card>

      {revealOpen ? (
        <ConfirmDialog
          title="Reveal the original prompt?"
          confirmLabel="Reveal"
          onClose={() => setRevealOpen(false)}
          onConfirm={async () => {
            const r = await post<{ body: unknown }>(`/approvals/${encodeURIComponent(id)}/reveal`, { confirm: true });
            setRevealed(r.body);
          }}
        >
          <p>This decrypts the held request and shows it in plaintext on this screen, including the sensitive values that were redacted.</p>
          <p>Only the device owner can do this. The reveal is recorded in the audit log (the log records that it happened, not the content).</p>
        </ConfirmDialog>
      ) : null}

      {confirm === 'deny' ? (
        <ConfirmDialog title="Deny this request?" confirmLabel="Deny" danger onClose={() => setConfirm(null)} onConfirm={() => decide('deny')}>
          <p>The held payload is destroyed. If the client retries, it will be evaluated again from scratch.</p>
        </ConfirmDialog>
      ) : null}
      {confirm === 'cancel' ? (
        <ConfirmDialog title="Cancel this approval?" confirmLabel="Cancel approval" danger onClose={() => setConfirm(null)} onConfirm={() => decide('cancel')}>
          <p>A retry from the client will no longer go through.</p>
        </ConfirmDialog>
      ) : null}
      {confirm === 'blacklist' ? (
        <ConfirmDialog title="Always block these values?" confirmLabel="Always block" danger onClose={() => setConfirm(null)} onConfirm={() => decide('blacklist')}>
          <p>Every flagged value in this prompt is added to your exact-value vault with action BLOCK, and this request is denied. Future prompts containing them are blocked before leaving the device.</p>
        </ConfirmDialog>
      ) : null}
      {confirm === 'whitelist' ? (
        <ConfirmDialog title="Allowlist these values?" confirmLabel="Allowlist and approve" onClose={() => setConfirm(null)} onConfirm={() => decide('whitelist')}>
          <p>Flagged noncritical values in this prompt are added to the allowlist so they stop being flagged, and this request is approved once. Critical values (government IDs, keys, passwords) are never allowlisted.</p>
        </ConfirmDialog>
      ) : null}

      {editing ? (
        <EditDialog
          id={id}
          preview={a.redacted_preview}
          revealed={revealed}
          onReveal={(b) => setRevealed(b)}
          onClose={() => setEditing(false)}
          onDone={async (text, tone) => {
            setEditing(false);
            await done(text, tone);
          }}
        />
      ) : null}
      {ruleOpen ? <SaveRuleDialog categories={a.categories} onClose={() => setRuleOpen(false)} onSaved={(t) => setResult({ tone: 'ok', text: t })} /> : null}
    </div>
  );
}

function EditDialog(props: {
  id: string;
  preview: string;
  revealed: unknown;
  onReveal: (b: unknown) => void;
  onClose: () => void;
  onDone: (text: string, tone: 'ok' | 'info') => Promise<void>;
}) {
  const initial = props.revealed !== null ? JSON.stringify(props.revealed, null, 2) : JSON.stringify({ messages: [{ role: 'user', content: props.preview }] }, null, 2);
  const [text, setText] = useState(initial);
  const act = useAction();
  const [outcome, setOutcome] = useState<string | null>(null);
  return (
    <Dialog
      wide
      title="Edit and resubmit"
      onClose={props.onClose}
      footer={
        <>
          <Button onClick={props.onClose}>Close</Button>
          <Button
            variant="primary"
            busy={act.busy}
            onClick={() =>
              act.run(async () => {
                let body: unknown;
                try {
                  body = JSON.parse(text);
                } catch (e) {
                  throw new Error('The request must be valid JSON: ' + (e as Error).message);
                }
                const r = await post<{ ok: boolean; response?: unknown; error?: { message?: string; code?: string } }>(`/approvals/${encodeURIComponent(props.id)}/resubmit`, { body });
                if (r.ok) {
                  await props.onDone('Edited request passed policy and was sent from the dashboard. The original held request was cancelled.', 'ok');
                } else {
                  setOutcome(`${r.error?.code ?? 'Error'}: ${r.error?.message ?? 'the edited request was not sent'}`);
                }
              })
            }
          >
            Resubmit
          </Button>
        </>
      }
    >
      <p>
        Edit the OpenAI request body, then resubmit. The original is cancelled and the edit is classified again from scratch. The model reply goes to the dashboard, not to your client.
      </p>
      {props.revealed === null ? (
        <Notice tone="info">
          The editor starts from the redacted preview, so the black-bar markers are literal text. To edit the original, reveal it first (audited).{' '}
          <RevealInline id={props.id} onRevealed={(b) => { props.onReveal(b); setText(JSON.stringify(b, null, 2)); }} />
        </Notice>
      ) : null}
      <textarea className="input mono" rows={16} value={text} onChange={(e) => setText(e.target.value)} spellCheck={false} aria-label="Edited request JSON" />
      {outcome ? <Notice tone="error">{outcome}</Notice> : null}
      <ErrorText error={act.error} />
    </Dialog>
  );
}

function RevealInline({ id, onRevealed }: { id: string; onRevealed: (b: unknown) => void }) {
  const [ask, setAsk] = useState(false);
  const act = useAction();
  if (!ask)
    return (
      <Button variant="link" onClick={() => setAsk(true)}>
        Reveal original
      </Button>
    );
  return (
    <span className="row gap-s" style={{ marginTop: 8 }}>
      <span>Decrypt and show the original here? This is recorded in the audit log.</span>
      <Button
        small
        variant="primary"
        busy={act.busy}
        onClick={() =>
          act.run(async () => {
            const r = await post<{ body: unknown }>(`/approvals/${encodeURIComponent(id)}/reveal`, { confirm: true });
            onRevealed(r.body);
          })
        }
      >
        Reveal
      </Button>
      <Button small onClick={() => setAsk(false)}>
        Cancel
      </Button>
      {act.error ? <span className="small" style={{ color: 'var(--red)' }}>{act.error}</span> : null}
    </span>
  );
}

function SaveRuleDialog({ categories, onClose, onSaved }: { categories: string[]; onClose: () => void; onSaved: (t: string) => void }) {
  const builtin = categories.filter((c) => !c.includes(':'));
  const [kind, setKind] = useState<RuleKind>(builtin.length ? 'category' : 'exact');
  const [label, setLabel] = useState('');
  const [value, setValue] = useState('');
  const [valueAction, setValueAction] = useState<Action>('BLOCK');
  const [cat, setCat] = useState(builtin[0] ?? '');
  const [catAction, setCatAction] = useState<Action>('BLOCK');
  const [entLabel, setEntLabel] = useState('');
  const [entDesc, setEntDesc] = useState('');
  const [entAction, setEntAction] = useState<Action>('TOKENIZE');
  const [typed, setTyped] = useState<null | { cat: string; action: Action }>(null);
  const cats = useLoad(() => get<{ categories: CategoryState[] }>('/categories'), []);
  const ents = useLoad(() => get<EntitiesResponse>('/entities').then((r) => (registerBuiltinLabels(r.builtin_labels), r)), []);
  const act = useAction();
  const entProblem = entLabel.trim() ? entityLabelProblem(entLabel, ents.data?.entities.map((e) => e.entity.label) ?? []) : null;

  const save = () =>
    act.run(async () => {
      if (kind === 'exact') {
        if (!label.trim() || !value) throw new Error('Label and value are required.');
        await post('/secrets', { label, value, kind: 'value', action: valueAction, allow: false });
        setValue('');
        onSaved(`Saved exact value "${label}" with action ${valueAction}.`);
      } else if (kind === 'category') {
        const info = cats.data?.categories.find((c) => c.id === cat);
        try {
          await put('/categories/' + encodeURIComponent(cat), { action: catAction });
        } catch (e) {
          if (e instanceof ApiError && e.status === 428 && info?.critical) {
            setTyped({ cat, action: catAction });
            return;
          }
          throw e;
        }
        onSaved(`${info?.label ?? cat} now uses ${catAction}.`);
      } else {
        if (!entLabel.trim()) throw new Error('Describe what to look for.');
        if (entProblem) throw new Error(entProblem);
        const label = normalizeEntityLabel(entLabel);
        const r = await post<{ entity_id: string; span_model_loaded: boolean }>('/entities', { label, action: entAction, ...(entDesc.trim() ? { description: entDesc.trim() } : {}) });
        invalidateEntityLabels();
        onSaved(`Saved custom entity type "${label}" with action ${entAction}.${r.span_model_loaded ? '' : ' No detection model is loaded yet, so matching prompts are held for review until you install one.'}`);
      }
      onClose();
    });

  if (typed) {
    return (
      <TypedConfirmDialog
        title={`Relax a critical protection?`}
        phrase={`DISABLE ${typed.cat}`}
        confirmLabel="Relax protection"
        cancelLabel="Keep protection on"
        onClose={() => setTyped(null)}
        onConfirm={async (phrase) => {
          await put('/categories/' + encodeURIComponent(typed.cat), { action: typed.action, confirm: phrase });
          onSaved(`${typed.cat} now uses ${typed.action}. This critical exception is recorded in the audit log.`);
          onClose();
        }}
      >
        <p>{typed.cat} is a critical category and is blocked by default. Setting it to {typed.action} lets matching values leave this device. The change is versioned and audited, and you can turn it back on under Categories.</p>
      </TypedConfirmDialog>
    );
  }

  return (
    <Dialog
      wide
      title="Save as a future rule"
      onClose={onClose}
      footer={
        <>
          <Button onClick={onClose}>Cancel</Button>
          <Button variant="primary" busy={act.busy} onClick={save}>
            Save
          </Button>
        </>
      }
    >
      <Segmented<RuleKind>
        label="Rule type"
        value={kind}
        onChange={setKind}
        options={[
          { value: 'exact', label: 'Exact value' },
          { value: 'category', label: 'Category action' },
          { value: 'entity', label: 'Custom entity type' },
        ]}
      />
      {kind === 'exact' ? (
        <div className="form-grid">
          <Field label="Label" hint="Shown in the vault list. Do not put the value here.">
            <input className="input" value={label} onChange={(e) => setLabel(e.target.value)} />
          </Field>
          <Field label="Action">
            <ActionSelect value={valueAction} onChange={setValueAction} options={['BLOCK', 'REDACT', 'TOKENIZE', 'REQUIRE_APPROVAL']} />
          </Field>
          <div className="full">
            <Field label="Exact value" hint="Stored encrypted in the local vault. It is never shown again.">
              <SecretInput value={value} onChange={setValue} />
            </Field>
          </div>
        </div>
      ) : null}
      {kind === 'category' ? (
        <div className="form-grid">
          <Field label="Category">
            <select className="input" value={cat} onChange={(e) => setCat(e.target.value)}>
              {(cats.data?.categories ?? []).map((c) => (
                <option key={c.id} value={c.id}>
                  {c.label}
                  {builtin.includes(c.id) ? ' (in this prompt)' : ''}
                </option>
              ))}
            </select>
          </Field>
          <Field label="Action">
            <ActionSelect value={catAction} onChange={setCatAction} />
          </Field>
        </div>
      ) : null}
      {kind === 'entity' ? (
        <div className="stack">
          <p className="muted small">
            For information with no fixed format that the detection model should find in any prompt, like "insurance policy number". For one exact string, use Exact value instead.
          </p>
          <div className="form-grid">
            <Field label="What to look for" hint={entProblem ?? 'In plain words.'}>
              <input className="input" value={entLabel} onChange={(e) => setEntLabel(e.target.value)} placeholder="insurance policy number" maxLength={60} aria-invalid={!!entProblem} />
            </Field>
            <Field label="Action">
              <ActionSelect value={entAction} onChange={setEntAction} options={ENTITY_ACTIONS} />
            </Field>
            <div className="full">
              <Field label="Description (optional)">
                <input className="input" value={entDesc} onChange={(e) => setEntDesc(e.target.value)} />
              </Field>
            </div>
          </div>
          {ents.data && !ents.data.span_model_loaded ? <SpanModelMissing /> : null}
        </div>
      ) : null}
      <ErrorText error={act.error} />
    </Dialog>
  );
}

export function ActionSelect({ value, onChange, options = ACTIONS }: { value: Action; onChange: (a: Action) => void; options?: Action[] }) {
  return (
    <select className="input" value={value} onChange={(e) => onChange(e.target.value as Action)}>
      {options.map((a) => (
        <option key={a} value={a}>
          {a}
        </option>
      ))}
    </select>
  );
}
