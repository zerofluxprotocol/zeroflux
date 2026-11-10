import { useRef, useState } from 'react';
import { del, get, post, type Action, type DestinationTier, type PlaygroundResult, type PolicyHistoryEntry, type PolicyRule } from '../api';
import type { Direction, OverrideMode, ScopeKind, Sensitivity } from '../generated/types';
import { ActionBadge } from '../components';
import { withEntityLabels, categoryLabel, dateTime, download, shortHash } from '../format';
import { useEntityLabels } from '../entities';
import { href, useRoute } from '../router';
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
  Loading,
  Notice,
  PageHeader,
  useAction,
  useLoad,
} from '../ui';
import { ActionSelect } from './ApprovalDetail';

const isCritical = (r: PolicyRule) => r.id.startsWith('builtin-critical-');

function blankRule(): PolicyRule {
  return {
    schema_version: 1,
    id: '',
    version: 0,
    scope: { kind: 'user', id: 'local-owner' },
    selector: { categories: [], destinations: ['external'], direction: 'inbound' },
    action: 'REDACT',
    severity: 'moderate',
    notify: ['local_user'],
    override: 'ALLOWED',
    retention_days: 30,
    enabled: true,
    description: '',
    origin: 'owner',
    critical_exception: false,
  };
}

export default function PoliciesPage() {
  useEntityLabels();
  const { query } = useRoute();
  const focus = query.get('rule');
  const pol = useLoad(() => get<{ rules: PolicyRule[]; effective_policy_hash: string }>('/policies'), []);
  const [editing, setEditing] = useState<PolicyRule | null>(null);
  const [history, setHistory] = useState<string | null>(null);
  const [deleting, setDeleting] = useState<PolicyRule | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const act = useAction();
  const fileRef = useRef<HTMLInputElement>(null);

  const rules = [...(pol.data?.rules ?? [])].sort((a, b) => Number(isCritical(b)) - Number(isCritical(a)) || (a.origin ?? '').localeCompare(b.origin ?? '') || a.id.localeCompare(b.id));

  const onImport = (f: File) =>
    act.run(async () => {
      const text = await f.text();
      let parsed: { rules?: PolicyRule[] } | PolicyRule[];
      try {
        parsed = JSON.parse(text);
      } catch {
        throw new Error('That file is not valid JSON.');
      }
      const list = Array.isArray(parsed) ? parsed : parsed.rules ?? [];
      const v = await post<{ valid: boolean; error?: string; rules?: number }>('/policies/validate', { rules: list });
      if (!v.valid) throw new Error('Import rejected: ' + v.error);
      const r = await post<{ imported: number }>('/policies/import', { rules: list });
      setNotice(`Imported ${r.imported} rule(s). Critical built-in rules in the file were skipped; change those under Categories.`);
      await pol.reload();
    });

  return (
    <div className="content">
      <PageHeader
        title="Policies"
        subtitle="Rules decide what happens to each detected category. More specific scopes win; critical rules marked FORBIDDEN cannot be weakened by any other rule."
        actions={
          <>
            <Button small onClick={() => act.run(async () => download('promptvpn-policy.json', JSON.stringify(await get('/policies/export'), null, 2)))}>
              Export
            </Button>
            <Button small onClick={() => fileRef.current?.click()}>
              Import
            </Button>
            <Button small variant="primary" onClick={() => setEditing(blankRule())}>
              New rule
            </Button>
            <input
              ref={fileRef}
              type="file"
              accept="application/json,.json"
              hidden
              onChange={(e) => {
                const f = e.target.files?.[0];
                if (f) void onImport(f);
                e.target.value = '';
              }}
            />
          </>
        }
      />
      {notice ? <Notice tone="ok">{notice}</Notice> : null}
      <ErrorText error={act.error || pol.error} />

      <Card pad={false}>
        <div style={{ padding: '16px 22px 0' }} className="row between">
          <h2>Rules</h2>
          {pol.data ? (
            <span className="muted small mono" title={pol.data.effective_policy_hash}>
              effective {shortHash(pol.data.effective_policy_hash, 16)}
            </span>
          ) : null}
        </div>
        {pol.loading && !pol.data ? (
          <div style={{ padding: 22 }}>
            <Loading />
          </div>
        ) : rules.length === 0 ? (
          <Empty>No rules.</Empty>
        ) : (
          <div className="table-wrap" style={{ padding: '8px 12px 12px' }}>
            <table className="table">
              <thead>
                <tr>
                  <th>Rule</th>
                  <th>Selects</th>
                  <th>Scope</th>
                  <th>Override</th>
                  <th>Action</th>
                  <th />
                </tr>
              </thead>
              <tbody>
                {rules.map((r) => (
                  <tr key={r.id} style={focus === r.id ? { outline: '2px solid var(--black)' } : undefined}>
                    <td>
                      <div className="mono small" style={{ fontWeight: 600 }}>
                        {r.id} <span className="faint">v{r.version}</span>
                      </div>
                      <div className="muted small">{r.description}</div>
                      <div className="row gap-s" style={{ marginTop: 4 }}>
                        <Badge tone="neutral">{r.origin || 'owner'}</Badge>
                        {r.enabled === false ? <Badge tone="outline">disabled</Badge> : null}
                        {r.critical_exception ? <Badge tone="red">critical exception</Badge> : null}
                      </div>
                    </td>
                    <td className="small">
                      {r.selector.categories.map(categoryLabel).join(', ')}
                      <div className="muted">
                        {(r.selector.destinations ?? ['any']).join(', ')} / {r.selector.direction ?? 'inbound'}
                      </div>
                    </td>
                    <td className="small">
                      {r.scope.kind}
                      <div className="muted mono">{r.scope.id}</div>
                    </td>
                    <td className="small">{r.override ?? 'ALLOWED'}</td>
                    <td>
                      <ActionBadge action={r.action} />
                      <div className="muted small" style={{ marginTop: 4 }}>
                        {r.action}
                      </div>
                    </td>
                    <td className="right nowrap">
                      <div className="row gap-s" style={{ justifyContent: 'flex-end' }}>
                        <Button small variant="ghost" onClick={() => setHistory(r.id)}>
                          History
                        </Button>
                        {isCritical(r) ? (
                          <a className="small muted" href={href('/categories')} title="Critical protections change under Categories with a typed confirmation">
                            Categories
                          </a>
                        ) : (
                          <>
                            <Button small onClick={() => setEditing(structuredClone(r))}>
                              Edit
                            </Button>
                            <Button small variant="danger" onClick={() => setDeleting(r)}>
                              Delete
                            </Button>
                          </>
                        )}
                      </div>
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        )}
      </Card>

      <Playground />

      {editing ? (
        <RuleEditor
          rule={editing}
          onClose={() => setEditing(null)}
          onSaved={async (r) => {
            setEditing(null);
            setNotice(`Saved ${r.id} as version ${r.version}.`);
            await pol.reload();
          }}
        />
      ) : null}
      {history ? <HistoryDialog id={history} onClose={() => setHistory(null)} /> : null}
      {deleting ? (
        <ConfirmDialog
          title={`Delete ${deleting.id}?`}
          danger
          confirmLabel="Delete rule"
          onClose={() => setDeleting(null)}
          onConfirm={async () => {
            await del('/policies/' + encodeURIComponent(deleting.id));
            setNotice(`Deactivated ${deleting.id}. Its history is kept.`);
            await pol.reload();
          }}
        >
          <p>The rule is deactivated and the effective policy is recomputed immediately. Earlier versions stay in history.</p>
        </ConfirmDialog>
      ) : null}
    </div>
  );
}

const split = (s: string) =>
  s
    .split(',')
    .map((x) => x.trim())
    .filter(Boolean);

function RuleEditor({ rule, onClose, onSaved }: { rule: PolicyRule; onClose: () => void; onSaved: (r: PolicyRule) => void }) {
  const isNew = rule.version === 0;
  const [r, setR] = useState<PolicyRule>(rule);
  const [cats, setCats] = useState(rule.selector.categories.join(', '));
  const [dests, setDests] = useState((rule.selector.destinations ?? ['any']).join(', '));
  const [notify, setNotify] = useState((rule.notify ?? []).join(', '));
  const [minConf, setMinConf] = useState(rule.selector.min_confidence == null ? '' : String(rule.selector.min_confidence));
  const act = useAction();
  const [validation, setValidation] = useState<string | null>(null);

  const build = (): PolicyRule => ({
    ...r,
    selector: {
      ...r.selector,
      categories: split(cats),
      destinations: split(dests).length ? split(dests) : ['any'],
      min_confidence: minConf === '' ? null : Number(minConf),
    },
    notify: split(notify),
  });

  return (
    <Dialog
      wide
      title={isNew ? 'New rule' : `Edit ${rule.id}`}
      onClose={onClose}
      footer={
        <>
          <Button onClick={onClose}>Cancel</Button>
          <Button
            busy={act.busy}
            onClick={() =>
              act.run(async () => {
                const v = await post<{ valid: boolean; error?: string }>('/policies/validate', { rules: [build()] });
                setValidation(v.valid ? 'Rule is valid.' : 'Invalid: ' + v.error);
              })
            }
          >
            Validate
          </Button>
          <Button variant="primary" busy={act.busy} onClick={() => act.run(async () => onSaved(await post<PolicyRule>('/policies', build())))}>
            Save
          </Button>
        </>
      }
    >
      <div className="form-grid">
        <Field label="Rule ID" hint="Letters, numbers and dashes. Saving an existing ID creates a new version.">
          <input className="input mono" value={r.id} disabled={!isNew} onChange={(e) => setR({ ...r, id: e.target.value })} placeholder="team-codenames" />
        </Field>
        <Field label="Action">
          <ActionSelect value={r.action} onChange={(a: Action) => setR({ ...r, action: a })} />
        </Field>
        <div className="full">
          <Field label="Description">
            <input className="input" value={r.description ?? ''} onChange={(e) => setR({ ...r, description: e.target.value })} />
          </Field>
        </div>
        <Field label="Categories" hint="Comma separated. * for all, CUSTOM:* for all custom entities, EXACT_SECRET:<id> for one value.">
          <input className="input mono" value={cats} onChange={(e) => setCats(e.target.value)} placeholder="EMAIL, PHONE" />
        </Field>
        <Field label="Destinations" hint="any, external, local, attested_tee, external_unverified, or dest:<id>">
          <input className="input mono" value={dests} onChange={(e) => setDests(e.target.value)} />
        </Field>
        <Field label="Direction">
          <select className="input" value={r.selector.direction ?? 'inbound'} onChange={(e) => setR({ ...r, selector: { ...r.selector, direction: e.target.value as Direction } })}>
            <option value="inbound">inbound (to the model)</option>
            <option value="outbound">outbound (model replies)</option>
            <option value="both">both</option>
          </select>
        </Field>
        <Field label="Minimum confidence" hint="Optional, 0 to 1">
          <input className="input" type="number" min={0} max={1} step={0.05} value={minConf} onChange={(e) => setMinConf(e.target.value)} />
        </Field>
        <Field label="Scope">
          <select className="input" value={r.scope.kind} onChange={(e) => setR({ ...r, scope: { ...r.scope, kind: e.target.value as ScopeKind } })}>
            {(['user', 'agent', 'application', 'request'] as ScopeKind[]).map((k) => (
              <option key={k}>{k}</option>
            ))}
          </select>
        </Field>
        <Field label="Scope ID" hint="local-owner, an agent ID, or an app ID">
          <input className="input mono" value={r.scope.id} onChange={(e) => setR({ ...r, scope: { ...r.scope, id: e.target.value } })} />
        </Field>
        <Field label="Override" hint="Whether narrower scopes or approvals may weaken this rule">
          <select className="input" value={r.override ?? 'ALLOWED'} onChange={(e) => setR({ ...r, override: e.target.value as OverrideMode })}>
            {(['ALLOWED', 'APPROVAL_ONCE', 'POLICY_OWNER_ONLY', 'FORBIDDEN'] as OverrideMode[]).map((k) => (
              <option key={k}>{k}</option>
            ))}
          </select>
        </Field>
        <Field label="Severity">
          <select className="input" value={r.severity} onChange={(e) => setR({ ...r, severity: e.target.value as Sensitivity })}>
            {(['low', 'moderate', 'high', 'critical'] as Sensitivity[]).map((k) => (
              <option key={k}>{k}</option>
            ))}
          </select>
        </Field>
        <Field label="Notify" hint="Comma separated, e.g. local_user">
          <input className="input mono" value={notify} onChange={(e) => setNotify(e.target.value)} />
        </Field>
        <Field label="Retention days">
          <input className="input" type="number" min={1} value={r.retention_days ?? 30} onChange={(e) => setR({ ...r, retention_days: Number(e.target.value) })} />
        </Field>
        <div className="full">
          <Check checked={r.enabled !== false} onChange={(v) => setR({ ...r, enabled: v })}>
            Enabled
          </Check>
        </div>
      </div>
      {validation ? <Notice tone={validation.startsWith('Invalid') ? 'error' : 'ok'}>{validation}</Notice> : null}
      <ErrorText error={act.error} />
    </Dialog>
  );
}

function HistoryDialog({ id, onClose }: { id: string; onClose: () => void }) {
  const h = useLoad(() => get<{ history: PolicyHistoryEntry[] }>(`/policies/${encodeURIComponent(id)}/history`), [id]);
  return (
    <Dialog wide title={`History of ${id}`} onClose={onClose} footer={<Button onClick={onClose}>Close</Button>}>
      <ErrorText error={h.error} />
      {!h.data ? (
        <Loading />
      ) : h.data.history.length === 0 ? (
        <Empty>No stored versions. Built-in defaults have no history until changed.</Empty>
      ) : (
        <table className="table">
          <thead>
            <tr>
              <th>Version</th>
              <th>Action</th>
              <th>Changed</th>
              <th>By</th>
              <th>State</th>
            </tr>
          </thead>
          <tbody>
            {[...h.data.history].sort((a, b) => b.rule.version - a.rule.version).map((e) => (
              <tr key={e.rule.version + e.created_at}>
                <td className="mono">v{e.rule.version}</td>
                <td>
                  {e.rule.action}
                  {e.rule.critical_exception ? <span className="muted small"> (critical exception)</span> : null}
                </td>
                <td className="small">{dateTime(e.created_at)}</td>
                <td className="small">{e.actor}</td>
                <td>{e.active ? <Badge tone="dark">active</Badge> : <Badge tone="neutral">superseded</Badge>}</td>
              </tr>
            ))}
          </tbody>
        </table>
      )}
    </Dialog>
  );
}

function Playground() {
  useEntityLabels();
  const [text, setText] = useState('Email jane@example.com about invoice 4111 1111 1111 1111.');
  const [tier, setTier] = useState<DestinationTier>('EXTERNAL_UNVERIFIED');
  const [direction, setDirection] = useState<'inbound' | 'outbound'>('inbound');
  const [models, setModels] = useState(false);
  const [res, setRes] = useState<PlaygroundResult | null>(null);
  const act = useAction();
  return (
    <Card title="Playground">
      <div className="stack">
        <p className="muted small">Runs detection and the current policy on text you type. Nothing is sent anywhere and nothing is logged.</p>
        <textarea className="input" rows={4} value={text} onChange={(e) => setText(e.target.value)} aria-label="Playground text" />
        <div className="row gap wrap" style={{ alignItems: 'flex-end' }}>
          <Field label="Destination tier">
            <select className="input" value={tier} onChange={(e) => setTier(e.target.value as DestinationTier)}>
              <option value="EXTERNAL_UNVERIFIED">EXTERNAL_UNVERIFIED</option>
              <option value="ATTESTED_TEE">ATTESTED_TEE</option>
              <option value="LOCAL">LOCAL</option>
            </select>
          </Field>
          <Field label="Direction">
            <select className="input" value={direction} onChange={(e) => setDirection(e.target.value as 'inbound' | 'outbound')}>
              <option value="inbound">inbound</option>
              <option value="outbound">outbound</option>
            </select>
          </Field>
          <div style={{ paddingBottom: 9 }}>
            <Check checked={models} onChange={setModels}>
              Use the detection model
            </Check>
          </div>
          <div className="grow" />
          <Button variant="primary" busy={act.busy} onClick={() => act.run(async () => setRes(await post<PlaygroundResult>('/policies/test', { text, destination_tier: tier, direction, use_models: models })))}>
            Test
          </Button>
        </div>
        <ErrorText error={act.error} />
        {res ? (
          <div className="stack">
            <div className="row gap-s">
              <span className="muted">Decision</span>
              <ActionBadge action={res.decision.disposition} />
              <span className="mono small">{res.decision.disposition}</span>
              <span className="muted small">severity {res.decision.severity}</span>
            </div>
            {res.findings.length === 0 ? (
              <Empty>Nothing detected.</Empty>
            ) : (
              <table className="table">
                <thead>
                  <tr>
                    <th>Category</th>
                    <th>Matched text</th>
                    <th>Recognizer</th>
                    <th>Confidence</th>
                    <th>Action</th>
                    <th>Rule</th>
                  </tr>
                </thead>
                <tbody>
                  {res.findings.map((f) => {
                    const fd = res.decision.findings.find((x) => x.finding_id === f.finding.finding_id);
                    return (
                      <tr key={f.finding.finding_id}>
                        <td>{categoryLabel(f.finding.category)}</td>
                        <td className="mono small">{f.text}</td>
                        <td className="small">
                          {f.finding.recognizer_id} <span className="muted">({f.finding.classifier_type})</span>
                        </td>
                        <td className="small">{f.finding.confidence.toFixed(2)}</td>
                        <td>{f.action ?? fd?.action}</td>
                        <td className="mono small">
                          {fd?.policy_id ? `${fd.policy_id} v${fd.policy_version}` : ''} {fd?.override_mode ? <span className="muted">{fd.override_mode}</span> : null}
                        </td>
                      </tr>
                    );
                  })}
                </tbody>
              </table>
            )}
            {res.decision.explanations.length ? (
              <ul className="small muted" style={{ margin: 0, paddingLeft: 18 }}>
                {res.decision.explanations.map((x, i) => (
                  <li key={i}>{withEntityLabels(x)}</li>
                ))}
              </ul>
            ) : null}
            {res.failures.length ? <Notice tone="warn">Recognizer failures: {res.failures.join('; ')}</Notice> : null}
            <p className="muted small mono">recognizers: {res.recognizers.join(', ')}</p>
          </div>
        ) : null}
      </div>
    </Card>
  );
}
