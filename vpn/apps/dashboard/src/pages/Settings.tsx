import { useEffect, useState } from 'react';
import { del, get, logout, post, put, type EngineSettings, type NotificationSettings, type ProviderEntry, type SettingsResponse, type DetectorSettings } from '../api';
import { useShell } from '../App';
import { dateTime, shortHash, titleCase } from '../format';
import { href } from '../router';
import { Badge, Button, Card, Check, ConfirmDialog, ErrorText, Field, Loading, Notice, PageHeader, Radio, Toggle, TypedConfirmDialog, useAction, useLoad } from '../ui';

const NOTIFY_KINDS = ['block', 'review', 'redaction', 'private_route', 'error'];

export default function SettingsPage() {
  const shell = useShell();
  const s = useLoad(() => get<SettingsResponse>('/settings'), []);
  const providers = useLoad(() => get<{ providers: ProviderEntry[] }>('/providers'), []);
  const [engine, setEngine] = useState<EngineSettings | null>(null);
  const [detector, setDetector] = useState<DetectorSettings | null>(null);
  const [notif, setNotif] = useState<NotificationSettings | null>(null);
  const [saved, setSaved] = useState<string | null>(null);
  const [dialog, setDialog] = useState<null | 'stop' | 'release' | 'rotate' | 'delete-events'>(null);
  const save = useAction();
  const misc = useAction();

  useEffect(() => {
    if (s.data) {
      setEngine(structuredClone(s.data.engine));
      setDetector(structuredClone(s.data.detector));
      setNotif(structuredClone(s.data.notifications));
    }
  }, [s.data]);

  if (!s.data || !engine || !detector || !notif) {
    return (
      <div className="content narrow">
        <PageHeader title="Settings" />
        <ErrorText error={s.error} />
        <Loading />
      </div>
    );
  }

  const dests = providers.data?.providers.map((p) => p.config) ?? [];
  const teeDests = dests.filter((d) => d.declared_tier === 'ATTESTED_TEE');
  const fb = engine.fallback;
  const teeInvalid = fb.mode === 'TEE_PRIVATE_ROUTE' && !fb.tee_consent;
  const setFb = (p: Partial<EngineSettings['fallback']>) => setEngine({ ...engine, fallback: { ...fb, ...p } });
  const dirty =
    JSON.stringify(engine) !== JSON.stringify(s.data.engine) || JSON.stringify(detector) !== JSON.stringify(s.data.detector) || JSON.stringify(notif) !== JSON.stringify(s.data.notifications);

  const doSave = () =>
    save.run(async () => {
      if (teeInvalid) throw new Error('Verified NEAR TEE auto-route needs your explicit consent. Tick the consent box or choose block-and-review.');
      if (engine.retention_days < 1 || engine.pending_ttl_minutes < 1) throw new Error('Retention and pending TTL must be at least 1.');
      const { emergency_stop: _e, owner_id: _o, ...enginePatch } = engine;
      const r = await put<SettingsResponse>('/settings', { engine: enginePatch, detector, notifications: notif });
      s.setData(r);
      setSaved('Settings saved.');
      await shell.refresh();
    });

  const stopped = s.data.engine.emergency_stop;

  return (
    <div className="content narrow">
      <PageHeader title="Settings" subtitle="Everything chosen during setup can be changed here. Changes apply immediately and are recorded in the audit log." />

      <Card title="Emergency stop">
        <div className="row between gap">
          <p className="muted">
            {stopped ? 'Engaged. Every model request is blocked locally until you release it.' : 'Blocks all model traffic through Prompt VPN instantly. Use it if you think something is leaking.'}
          </p>
          {stopped ? (
            <Button variant="primary" onClick={() => setDialog('release')}>
              Release stop
            </Button>
          ) : (
            <Button variant="danger" onClick={() => setDialog('stop')}>
              Stop all traffic
            </Button>
          )}
        </div>
      </Card>

      {saved ? <Notice tone="ok">{saved}</Notice> : null}

      <Card title="Uncertain findings and detector failures">
        <div className="stack">
          <div className={`radio-card${fb.mode === 'BLOCK_AND_REVIEW' ? ' selected' : ''}`}>
            <Radio name="fallback" checked={fb.mode === 'BLOCK_AND_REVIEW'} onChange={() => setFb({ mode: 'BLOCK_AND_REVIEW' })}>
              <strong>Block locally and review</strong>
              <div className="muted small">The request stops on this computer, you get a notification, and it waits in Approvals.</div>
            </Radio>
          </div>
          <div className={`radio-card${fb.mode === 'TEE_PRIVATE_ROUTE' ? ' selected' : ''}`}>
            <Radio name="fallback" checked={fb.mode === 'TEE_PRIVATE_ROUTE'} onChange={() => setFb({ mode: 'TEE_PRIVATE_ROUTE' })}>
              <strong>Verified NEAR TEE auto-route</strong>
              <div className="muted small">Sends the unredacted prompt off this device to a NEAR model running in an attested TEE. Requires your consent.</div>
            </Radio>
          </div>
          <Check checked={fb.tee_consent} onChange={(v) => setFb({ tee_consent: v })}>
            I consent to sending uncertain prompts, unredacted, to a verified NEAR TEE. The data leaves this device; the TEE attestation is what protects it.
          </Check>
          {teeInvalid ? <Notice tone="warn">TEE auto-route is selected but consent is not given. Saving is blocked until you tick the box or choose block-and-review.</Notice> : null}
          {fb.mode === 'TEE_PRIVATE_ROUTE' && !teeDests.length ? <Notice tone="warn">No ATTESTED_TEE destination is configured, so uncertain prompts will be blocked. Add NEAR AI under Models & destinations.</Notice> : null}
          <Check checked={fb.local_private_handling} onChange={(v) => setFb({ local_private_handling: v })}>
            Allow a local Ollama generation model to handle prompts that need private handling
          </Check>
          <Field label="Uncertainty threshold" hint="Findings below this confidence count as uncertain (0 to 1).">
            <input className="input" type="number" min={0} max={1} step={0.05} value={fb.uncertain_threshold} onChange={(e) => setFb({ uncertain_threshold: Number(e.target.value) })} style={{ maxWidth: 160 }} />
          </Field>
        </div>
      </Card>

      <Card title="Destinations">
        <div className="form-grid">
          <Field label="Default destination">
            <select className="input" value={engine.default_destination ?? ''} onChange={(e) => setEngine({ ...engine, default_destination: e.target.value || null })}>
              <option value="">None (requests are not forwarded)</option>
              {dests.map((d) => (
                <option key={d.id} value={d.id}>
                  {d.display_name} ({d.declared_tier})
                </option>
              ))}
            </select>
          </Field>
          <Field label="Private route destination" hint="Only attested TEE destinations">
            <select className="input" value={engine.private_destination ?? ''} onChange={(e) => setEngine({ ...engine, private_destination: e.target.value || null })}>
              <option value="">None</option>
              {teeDests.map((d) => (
                <option key={d.id} value={d.id}>
                  {d.display_name}
                </option>
              ))}
            </select>
          </Field>
          <Field label="Private route model" hint="Blank uses the destination default">
            <input className="input mono" value={engine.private_model ?? ''} onChange={(e) => setEngine({ ...engine, private_model: e.target.value || null })} />
          </Field>
          <div />
          <div className="full stack-s">
            <span className="field-label">Failover order</span>
            <span className="hint">Tried in this order when the default is unavailable. Policy still decides what each tier may receive.</span>
            <div className="row gap wrap">
              {dests
                .filter((d) => d.id !== engine.default_destination)
                .map((d) => (
                  <Check
                    key={d.id}
                    checked={engine.failover.includes(d.id)}
                    onChange={(v) => setEngine({ ...engine, failover: v ? [...engine.failover, d.id] : engine.failover.filter((x) => x !== d.id) })}
                  >
                    {d.display_name} <span className="muted small">({d.declared_tier})</span>
                  </Check>
                ))}
            </div>
            {engine.failover.length ? <span className="small muted">Order: {engine.failover.join(' then ')}</span> : null}
          </div>
        </div>
      </Card>

      <Card title="Retention and processing">
        <div className="form-grid">
          <Field label="Audit retention (days)" hint="Metadata only. Default 30.">
            <input className="input" type="number" min={1} value={engine.retention_days} onChange={(e) => setEngine({ ...engine, retention_days: Number(e.target.value) })} />
          </Field>
          <Field label="Held prompt lifetime (minutes)" hint="Encrypted pending payloads are destroyed after this. Default 15.">
            <input className="input" type="number" min={1} value={engine.pending_ttl_minutes} onChange={(e) => setEngine({ ...engine, pending_ttl_minutes: Number(e.target.value) })} />
          </Field>
          <div className="full stack-s">
            <Check checked={engine.restore_placeholders} onChange={(v) => setEngine({ ...engine, restore_placeholders: v })}>
              Restore tokenized values in model replies before they reach your client
            </Check>
          </div>
        </div>
      </Card>

      <Card title="Detection model">
        <div className="form-grid">
          <div className="full">
            <p className="muted small">
              Active model: <span className="mono">{s.data.span_model.model || 'none'}</span>. Install or change the size under <a href={href('/models')}>Models & destinations</a>.
            </p>
          </div>
          <div className="full stack-s">
            <Check checked={detector.span_enabled} onChange={(v) => setDetector({ ...detector, span_enabled: v })}>
              Use the detection model on prompts
            </Check>
            <Check checked={detector.span_required} onChange={(v) => setDetector({ ...detector, span_required: v })}>
              Hold prompts for review if the detection model is unavailable (fail closed)
            </Check>
            <Check checked={engine.outbound_span_model} disabled={!detector.span_enabled} onChange={(v) => setEngine({ ...engine, outbound_span_model: v })}>
              Also scan model output with the detection model (adds about 20 ms per reply)
            </Check>
          </div>
          <Field label="Detection model timeout (ms)" hint="A prompt that takes longer counts as a detector failure. Default 10000.">
            <input className="input" type="number" min={500} step={500} value={detector.span_timeout_ms} onChange={(e) => setDetector({ ...detector, span_timeout_ms: Number(e.target.value) })} />
          </Field>
        </div>
      </Card>

      <Card title="Notifications">
        <div className="stack">
          <div className="row between">
            <span>Desktop notifications for flagged prompts. Banners never include the sensitive values.</span>
            <Toggle label="Desktop notifications" checked={notif.enabled} onChange={(v) => setNotif({ ...notif, enabled: v })} />
          </div>
          <div className="row gap wrap">
            {NOTIFY_KINDS.map((k) => (
              <Check key={k} disabled={!notif.enabled} checked={notif.kinds.includes(k)} onChange={(v) => setNotif({ ...notif, kinds: v ? [...notif.kinds, k] : notif.kinds.filter((x) => x !== k) })}>
                {titleCase(k)}
              </Check>
            ))}
          </div>
          <Field label="Minimum seconds between notifications">
            <input className="input" type="number" min={0} value={notif.min_interval_secs} onChange={(e) => setNotif({ ...notif, min_interval_secs: Number(e.target.value) })} style={{ maxWidth: 160 }} />
          </Field>
        </div>
      </Card>

      <div className="row gap-s" style={{ justifyContent: 'flex-end', position: 'sticky', bottom: 16 }}>
        <ErrorText error={save.error} />
        <Button onClick={() => s.data && (setEngine(structuredClone(s.data.engine)), setDetector(structuredClone(s.data.detector)), setNotif(structuredClone(s.data.notifications)))} disabled={!dirty}>
          Discard changes
        </Button>
        <Button variant="primary" busy={save.busy} disabled={!dirty} onClick={doSave}>
          Save settings
        </Button>
      </div>

      <Card title="Setup and data">
        <div className="list">
          <div className="option-row">
            <div className="li-main">
              <div className="li-title">Re-run setup</div>
              <div className="li-sub">
                Walk through the six setup steps again. Exact values, custom entities, apps and history are kept.
                {s.data.onboarding.completed_at ? ` Last completed ${dateTime(s.data.onboarding.completed_at)}.` : ''}
              </div>
            </div>
            <a className="btn btn-sm" href={href('/onboarding')} style={{ textDecoration: 'none' }}>
              Open setup
            </a>
          </div>
          <div className="option-row">
            <div className="li-main">
              <div className="li-title">Rotate vault key</div>
              <div className="li-sub">Creates a new data key for future encryption. Existing entries stay readable.</div>
            </div>
            <Button small onClick={() => setDialog('rotate')}>
              Rotate
            </Button>
          </div>
          <div className="option-row">
            <div className="li-main">
              <div className="li-title">Delete audit history</div>
              <div className="li-sub">Removes all audit events. A record of the deletion is kept.</div>
            </div>
            <Button small variant="danger" onClick={() => setDialog('delete-events')}>
              Delete events
            </Button>
          </div>
          <div className="option-row">
            <div className="li-main">
              <div className="li-title">Sign out</div>
              <div className="li-sub">Ends this dashboard session. Run promptvpn dashboard to sign in again.</div>
            </div>
            <Button
              small
              busy={misc.busy}
              onClick={() =>
                misc.run(async () => {
                  await logout();
                  window.location.reload();
                })
              }
            >
              Sign out
            </Button>
          </div>
        </div>
        <ErrorText error={misc.error} />
      </Card>

      <Card title="Consent record">
        {s.data.consents.length === 0 ? (
          <p className="muted">No consent decisions recorded.</p>
        ) : (
          <table className="table">
            <thead>
              <tr>
                <th>Consent</th>
                <th>Decision</th>
                <th>Policy</th>
                <th>When</th>
              </tr>
            </thead>
            <tbody>
              {s.data.consents.map((c, i) => (
                <tr key={i}>
                  <td>{titleCase(c.kind)}</td>
                  <td>{c.granted ? <Badge tone="dark">granted</Badge> : <Badge tone="neutral">declined</Badge>}</td>
                  <td className="mono small">{shortHash(c.policy_hash, 12)}</td>
                  <td className="small">{dateTime(c.created_at)}</td>
                </tr>
              ))}
            </tbody>
          </table>
        )}
      </Card>

      {dialog === 'stop' ? (
        <ConfirmDialog
          title="Stop all model traffic?"
          danger
          confirmLabel="Stop all traffic"
          onClose={() => setDialog(null)}
          onConfirm={async () => {
            await post('/emergency-stop', { engaged: true });
            await s.reload();
            await shell.refresh();
          }}
        >
          <p>Every client request through Prompt VPN is refused until you release the stop. Nothing is deleted.</p>
        </ConfirmDialog>
      ) : null}
      {dialog === 'release' ? (
        <ConfirmDialog
          title="Release the emergency stop?"
          confirmLabel="Release"
          onClose={() => setDialog(null)}
          onConfirm={async () => {
            await post('/emergency-stop', { engaged: false });
            await s.reload();
            await shell.refresh();
          }}
        >
          <p>Requests start flowing again under your current policy.</p>
        </ConfirmDialog>
      ) : null}
      {dialog === 'rotate' ? (
        <ConfirmDialog
          title="Rotate the vault key?"
          confirmLabel="Rotate key"
          onClose={() => setDialog(null)}
          onConfirm={async () => {
            const r = await post<{ active_key: string }>('/vault/rotate');
            setSaved(`Vault key rotated. Active key: ${r.active_key}.`);
          }}
        >
          <p>New secrets and held prompts are encrypted with a fresh key. Older entries remain readable with their original key.</p>
        </ConfirmDialog>
      ) : null}
      {dialog === 'delete-events' ? (
        <TypedConfirmDialog
          title="Delete all audit events?"
          phrase="DELETE EVENTS"
          confirmLabel="Delete events"
          onClose={() => setDialog(null)}
          onConfirm={async () => {
            const r = await del<{ deleted: number }>('/events');
            setSaved(`Deleted ${r.deleted} audit events.`);
            await shell.refresh();
          }}
        >
          <p>This removes the local audit history. Export it from Activity first if you need a copy.</p>
        </TypedConfirmDialog>
      ) : null}
    </div>
  );
}
