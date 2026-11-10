import { useState } from 'react';
import { del, get, patch, post, type AppRecord, type ProviderEntry } from '../api';
import { dateTime } from '../format';
import { Badge, Button, Card, Code, ConfirmDialog, CopyButton, Dialog, Empty, ErrorText, Field, Loading, Notice, PageHeader, useAction, useLoad } from '../ui';

export function clientSnippet(baseUrl: string, key: string): string {
  return `# Environment variables (most OpenAI-compatible clients)
export OPENAI_BASE_URL="${baseUrl}"
export OPENAI_API_KEY="${key}"

# Python
from openai import OpenAI
client = OpenAI(base_url="${baseUrl}", api_key="${key}")

# curl
curl ${baseUrl}/chat/completions \\
  -H "Authorization: Bearer ${key}" \\
  -H "Content-Type: application/json" \\
  -d '{"model": "default", "messages": [{"role": "user", "content": "hello"}]}'`;
}

export function KeyOnce({ apiKey, baseUrl }: { apiKey: string; baseUrl: string }) {
  return (
    <div className="stack">
      <Notice tone="warn">This key is shown once. Copy it now; Prompt VPN stores only its hash.</Notice>
      <div className="keybox">
        <code>{apiKey}</code>
        <CopyButton text={apiKey} />
      </div>
      <div className="keybox">
        <code>{baseUrl}</code>
        <CopyButton text={baseUrl} label="Copy URL" />
      </div>
      <Code>{clientSnippet(baseUrl, apiKey)}</Code>
    </div>
  );
}

export default function AppsPage() {
  const apps = useLoad(() => get<{ apps: AppRecord[]; base_url: string }>('/apps'), []);
  const providers = useLoad(() => get<{ providers: ProviderEntry[] }>('/providers'), []);
  const [name, setName] = useState('');
  const [agent, setAgent] = useState('');
  const [dest, setDest] = useState('');
  const [created, setCreated] = useState<{ api_key: string; base_url: string; app: AppRecord } | null>(null);
  const [revoking, setRevoking] = useState<AppRecord | null>(null);
  const [editing, setEditing] = useState<AppRecord | null>(null);
  const act = useAction();
  const dests = providers.data?.providers.map((p) => p.config) ?? [];
  const baseUrl = apps.data?.base_url ?? '';
  const active = apps.data?.apps.filter((a) => !a.revoked_at) ?? [];
  const revoked = apps.data?.apps.filter((a) => a.revoked_at) ?? [];

  return (
    <div className="content narrow">
      <PageHeader title="Apps" subtitle="Each client or agent gets its own local API key, so activity and rules can be scoped per app. Keys only work against this computer." />
      <Card title="Base URL">
        <div className="stack">
          <div className="keybox">
            <code>{baseUrl || '...'}</code>
            {baseUrl ? <CopyButton text={baseUrl} /> : null}
          </div>
          <p className="muted small">
            For guided client setup, an agent can use the setup skill in <code>skills/promptvpn-setup</code>; it makes reversible config edits only after you approve them.
          </p>
        </div>
      </Card>

      <Card title="Create a key">
        <div className="form-grid">
          <Field label="App name">
            <input className="input" value={name} onChange={(e) => setName(e.target.value)} placeholder="cursor-laptop" />
          </Field>
          <Field label="Agent ID" hint="Optional, used by agent-scoped rules">
            <input className="input" value={agent} onChange={(e) => setAgent(e.target.value)} />
          </Field>
          <Field label="Destination" hint="Optional. Leave blank to use the default.">
            <select className="input" value={dest} onChange={(e) => setDest(e.target.value)}>
              <option value="">Default destination</option>
              {dests.map((d) => (
                <option key={d.id} value={d.id}>
                  {d.display_name} ({d.declared_tier})
                </option>
              ))}
            </select>
          </Field>
          <div className="row" style={{ alignItems: 'flex-end', justifyContent: 'flex-end' }}>
            <Button
              variant="primary"
              busy={act.busy}
              disabled={!name.trim()}
              onClick={() =>
                act.run(async () => {
                  const r = await post<{ app: AppRecord; api_key: string; base_url: string }>('/apps', { name: name.trim(), agent_id: agent || null, destination_id: dest || null });
                  setCreated(r);
                  setName('');
                  setAgent('');
                  setDest('');
                  await apps.reload();
                })
              }
            >
              Create key
            </Button>
          </div>
        </div>
        <ErrorText error={act.error} />
      </Card>

      <Card title="Active keys">
        <ErrorText error={apps.error} />
        {apps.loading && !apps.data ? (
          <Loading />
        ) : !active.length ? (
          <Empty>No active keys.</Empty>
        ) : (
          <div className="list">
            {active.map((a) => (
              <div className="list-item" key={a.app_id}>
                <div className="li-main">
                  <div className="li-title">{a.name}</div>
                  <div className="li-sub">
                    <span className="mono">{a.app_id}</span> · created {dateTime(a.created_at)}
                    {a.agent_id ? ` · agent ${a.agent_id}` : ''} · destination {a.destination_id ?? 'default'}
                  </div>
                </div>
                <Button small onClick={() => setEditing(a)}>
                  Routing
                </Button>
                <Button small variant="danger" onClick={() => setRevoking(a)}>
                  Revoke
                </Button>
              </div>
            ))}
          </div>
        )}
      </Card>
      {revoked.length ? (
        <Card title="Revoked">
          <div className="list">
            {revoked.map((a) => (
              <div className="list-item" key={a.app_id}>
                <div className="li-main">
                  <div className="li-title muted">{a.name}</div>
                  <div className="li-sub mono">{a.app_id}</div>
                </div>
                <Badge>revoked {dateTime(a.revoked_at)}</Badge>
              </div>
            ))}
          </div>
        </Card>
      ) : null}

      {created ? (
        <Dialog wide title={`Key for ${created.app.name}`} onClose={() => setCreated(null)} footer={<Button variant="primary" onClick={() => setCreated(null)}>I copied it</Button>}>
          <KeyOnce apiKey={created.api_key} baseUrl={created.base_url} />
        </Dialog>
      ) : null}
      {revoking ? (
        <ConfirmDialog
          title={`Revoke ${revoking.name}?`}
          danger
          confirmLabel="Revoke key"
          onClose={() => setRevoking(null)}
          onConfirm={async () => {
            await del('/apps/' + encodeURIComponent(revoking.app_id));
            await apps.reload();
          }}
        >
          <p>Clients using this key start getting 401 errors immediately. This cannot be undone; create a new key instead.</p>
        </ConfirmDialog>
      ) : null}
      {editing ? <RoutingDialog app={editing} dests={dests.map((d) => ({ id: d.id, label: `${d.display_name} (${d.declared_tier})` }))} onClose={() => setEditing(null)} onSaved={() => apps.reload()} /> : null}
    </div>
  );
}

function RoutingDialog({ app, dests, onClose, onSaved }: { app: AppRecord; dests: { id: string; label: string }[]; onClose: () => void; onSaved: () => Promise<void> }) {
  const [agent, setAgent] = useState(app.agent_id ?? '');
  const [dest, setDest] = useState(app.destination_id ?? '');
  const act = useAction();
  return (
    <Dialog
      title={`Routing for ${app.name}`}
      onClose={onClose}
      footer={
        <>
          <Button onClick={onClose}>Cancel</Button>
          <Button
            variant="primary"
            busy={act.busy}
            onClick={() =>
              act.run(async () => {
                await patch('/apps/' + encodeURIComponent(app.app_id), { agent_id: agent || null, destination_id: dest || null });
                await onSaved();
                onClose();
              })
            }
          >
            Save
          </Button>
        </>
      }
    >
      <Field label="Agent ID">
        <input className="input" value={agent} onChange={(e) => setAgent(e.target.value)} />
      </Field>
      <Field label="Destination">
        <select className="input" value={dest} onChange={(e) => setDest(e.target.value)}>
          <option value="">Default destination</option>
          {dests.map((d) => (
            <option key={d.id} value={d.id}>
              {d.label}
            </option>
          ))}
        </select>
      </Field>
      <ErrorText error={act.error} />
    </Dialog>
  );
}
