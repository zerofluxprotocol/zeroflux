import { useState } from 'react';
import { del, get, post, type Action, type NormOpts, type SecretKind, type SecretMeta } from '../api';
import { dateTime } from '../format';
import { Badge, Button, Card, Check, ConfirmDialog, Empty, ErrorText, Field, Loading, Notice, PageHeader, SecretInput, Segmented, useAction, useLoad } from '../ui';
import { ActionSelect } from './ApprovalDetail';

const DEFAULT_NORM: NormOpts = { unicode: true, casefold: true, whitespace: true, separators: false };

export default function SecretsPage() {
  const list = useLoad(() => get<{ secrets: SecretMeta[] }>('/secrets'), []);
  const [removing, setRemoving] = useState<SecretMeta | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const secrets = list.data?.secrets ?? [];
  const vault = secrets.filter((s) => !s.allow);
  const allow = secrets.filter((s) => s.allow);

  return (
    <div className="content narrow">
      <PageHeader
        title="Secrets"
        subtitle="Exact values you never want sent (customer IDs, internal hostnames, codenames), plus an allowlist of values that should stop being flagged. Values are stored encrypted and never shown again."
      />
      {notice ? <Notice tone="ok">{notice}</Notice> : null}
      <AddSecret
        onAdded={async (label, allowed) => {
          setNotice(allowed ? `Allowlisted "${label}".` : `Added "${label}" to the vault.`);
          await list.reload();
        }}
      />
      <ErrorText error={list.error} />
      {list.loading && !list.data ? <Loading /> : null}
      <Card title={`Exact-value vault (${vault.length})`}>
        <SecretList items={vault} onRemove={setRemoving} empty="No exact values registered." />
      </Card>
      <Card title={`Allowlist (${allow.length})`}>
        <SecretList items={allow} onRemove={setRemoving} empty="No allowlisted values. Values you allowlist from the review screen appear here." />
      </Card>
      {removing ? (
        <ConfirmDialog
          title={`Remove "${removing.label}"?`}
          danger
          confirmLabel="Remove"
          onClose={() => setRemoving(null)}
          onConfirm={async () => {
            await del('/secrets/' + encodeURIComponent(removing.secret_id));
            setNotice(`Removed "${removing.label}".`);
            await list.reload();
          }}
        >
          <p>{removing.allow ? 'This value will be flagged again by the normal detectors.' : 'Prompts containing this value will no longer be matched by the vault. Built-in detectors still apply.'}</p>
        </ConfirmDialog>
      ) : null}
    </div>
  );
}

function SecretList({ items, onRemove, empty }: { items: SecretMeta[]; onRemove: (s: SecretMeta) => void; empty: string }) {
  if (!items.length) return <Empty>{empty}</Empty>;
  return (
    <div className="list">
      {items.map((s) => (
        <div className="list-item" key={s.secret_id}>
          <div className="li-main">
            <div className="li-title">{s.label}</div>
            <div className="li-sub">
              <span className="mono">{s.secret_id}</span> · {s.kind} · added {dateTime(s.created_at)} · normalize{' '}
              {Object.entries(s.norm)
                .filter(([, v]) => v)
                .map(([k]) => k)
                .join(', ') || 'none'}
            </div>
          </div>
          <Badge tone={s.allow ? 'neutral' : s.action === 'BLOCK' ? 'red' : 'outline'}>{s.allow ? 'ALLOW' : s.action}</Badge>
          <Button small variant="danger" onClick={() => onRemove(s)}>
            Remove
          </Button>
        </div>
      ))}
    </div>
  );
}

function AddSecret({ onAdded }: { onAdded: (label: string, allow: boolean) => Promise<void> }) {
  const [mode, setMode] = useState<'vault' | 'allow'>('vault');
  const [label, setLabel] = useState('');
  const [value, setValue] = useState('');
  const [kind, setKind] = useState<SecretKind>('value');
  const [action, setAction] = useState<Action>('BLOCK');
  const [norm, setNorm] = useState<NormOpts>(DEFAULT_NORM);
  const act = useAction();
  return (
    <Card title="Add a value">
      <div className="stack">
        <Segmented
          label="List"
          value={mode}
          onChange={setMode}
          options={[
            { value: 'vault', label: 'Protect this value' },
            { value: 'allow', label: 'Allowlist this value' },
          ]}
        />
        <div className="form-grid">
          <Field label="Label" hint="A name you will recognize. Do not put the value here.">
            <input className="input" value={label} onChange={(e) => setLabel(e.target.value)} placeholder="Acme customer ID" />
          </Field>
          <Field label="Match type">
            <select className="input" value={kind} onChange={(e) => setKind(e.target.value as SecretKind)}>
              <option value="value">Exact value</option>
              <option value="pattern">Pattern (regular expression)</option>
              <option value="file_fingerprint">File fingerprint</option>
            </select>
          </Field>
          <div className="full">
            <Field label={kind === 'pattern' ? 'Pattern' : 'Value'} hint="Masked. Sent once to the local daemon, stored encrypted, never displayed.">
              <SecretInput value={value} onChange={setValue} />
            </Field>
          </div>
          {mode === 'vault' ? (
            <Field label="Action when found">
              <ActionSelect value={action} onChange={setAction} options={['BLOCK', 'REDACT', 'TOKENIZE', 'REQUIRE_APPROVAL', 'PRIVATE_ROUTE']} />
            </Field>
          ) : (
            <div />
          )}
          <div className="full stack-s">
            <span className="field-label">Normalize before matching</span>
            <div className="row gap wrap">
              <Check checked={norm.unicode} onChange={(v) => setNorm({ ...norm, unicode: v })}>
                Unicode lookalikes
              </Check>
              <Check checked={norm.casefold} onChange={(v) => setNorm({ ...norm, casefold: v })}>
                Ignore case
              </Check>
              <Check checked={norm.whitespace} onChange={(v) => setNorm({ ...norm, whitespace: v })}>
                Ignore whitespace
              </Check>
              <Check checked={norm.separators} onChange={(v) => setNorm({ ...norm, separators: v })}>
                Ignore separators (- . _)
              </Check>
            </div>
          </div>
        </div>
        <ErrorText error={act.error} />
        <div className="row" style={{ justifyContent: 'flex-end' }}>
          <Button
            variant="primary"
            busy={act.busy}
            disabled={!label.trim() || !value}
            onClick={() =>
              act.run(async () => {
                const allow = mode === 'allow';
                await post('/secrets', { label: label.trim(), value, kind, action: allow ? 'ALLOW' : action, allow, norm });
                setValue('');
                setLabel('');
                await onAdded(label.trim(), allow);
              })
            }
          >
            {mode === 'allow' ? 'Add to allowlist' : 'Add to vault'}
          </Button>
        </div>
      </div>
    </Card>
  );
}
