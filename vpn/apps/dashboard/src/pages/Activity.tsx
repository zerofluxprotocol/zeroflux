import { useState } from 'react';
import { getText, get, post, type AuditEvent } from '../api';
import { ActionBadge, Severity } from '../components';
import { categoryLabel, dateTime, download, eventTitle, timeAgo } from '../format';
import { useEntityLabels } from '../entities';
import { navigate } from '../router';
import { Button, Card, Empty, ErrorText, Field, Loading, Notice, PageHeader, useAction, useLoad } from '../ui';

const PAGE = 50;
const ACTION_FILTERS = ['', 'ALLOW', 'TRANSFORM', 'BLOCK', 'REQUIRE_APPROVAL', 'PRIVATE_ROUTE', 'OUTPUT_BLOCK', 'ERROR'];
const SEVERITIES = ['', 'low', 'moderate', 'high', 'critical'];
const KINDS = ['', 'request', 'response', 'approval', 'policy', 'settings', 'secret', 'entity', 'provider', 'model', 'app', 'attestation', 'vault', 'admin'];

export default function ActivityPage() {
  useEntityLabels();
  const [action, setAction] = useState('');
  const [severity, setSeverity] = useState('');
  const [kind, setKind] = useState('');
  const [offset, setOffset] = useState(0);
  const q = new URLSearchParams();
  if (action) q.set('action', action);
  if (severity) q.set('severity', severity);
  if (kind) q.set('kind', kind);
  q.set('limit', String(PAGE));
  q.set('offset', String(offset));
  const events = useLoad(() => get<{ events: AuditEvent[] }>('/events?' + q.toString()), [action, severity, kind, offset]);
  const act = useAction();
  const [verify, setVerify] = useState<{ ok: boolean; verified_events?: number; error?: string } | null>(null);

  const reset = (fn: () => void) => {
    fn();
    setOffset(0);
  };

  return (
    <div className="content">
      <PageHeader
        title="Activity"
        subtitle="Metadata for every decision. Prompt text and secret values are never recorded here."
        actions={
          <>
            <Button
              small
              busy={act.busy}
              onClick={() => act.run(async () => setVerify(await post('/events/verify')))}
            >
              Verify audit chain
            </Button>
            <Button small onClick={() => act.run(async () => download('promptvpn-events.jsonl', await getText('/events/export'), 'application/x-ndjson'))}>
              Export
            </Button>
          </>
        }
      />
      <ErrorText error={act.error} />
      {verify ? (
        verify.ok ? (
          <Notice tone="ok">Audit chain intact: {verify.verified_events} events verified.</Notice>
        ) : (
          <Notice tone="error">Audit chain verification failed: {verify.error}</Notice>
        )
      ) : null}

      <Card>
        <div className="row gap wrap" style={{ marginBottom: 12 }}>
          <Field label="Action">
            <select className="input" value={action} onChange={(e) => reset(() => setAction(e.target.value))}>
              {ACTION_FILTERS.map((a) => (
                <option key={a} value={a}>
                  {a || 'All actions'}
                </option>
              ))}
            </select>
          </Field>
          <Field label="Severity">
            <select className="input" value={severity} onChange={(e) => reset(() => setSeverity(e.target.value))}>
              {SEVERITIES.map((a) => (
                <option key={a} value={a}>
                  {a || 'All severities'}
                </option>
              ))}
            </select>
          </Field>
          <Field label="Kind">
            <select className="input" value={kind} onChange={(e) => reset(() => setKind(e.target.value))}>
              {KINDS.map((a) => (
                <option key={a} value={a}>
                  {a || 'All kinds'}
                </option>
              ))}
            </select>
          </Field>
        </div>
        <ErrorText error={events.error} />
        {events.loading && !events.data ? (
          <Loading />
        ) : events.data && events.data.events.length === 0 ? (
          <Empty>No events match these filters.</Empty>
        ) : (
          <div className="table-wrap">
            <table className="table">
              <thead>
                <tr>
                  <th>Event</th>
                  <th>Categories</th>
                  <th>Severity</th>
                  <th>App</th>
                  <th>Time</th>
                  <th className="right">Result</th>
                </tr>
              </thead>
              <tbody>
                {events.data?.events.map((e) => (
                  <tr
                    key={e.event_id}
                    className="clickable"
                    tabIndex={0}
                    onClick={() => navigate('/activity/' + encodeURIComponent(e.event_id))}
                    onKeyDown={(ev) => ev.key === 'Enter' && navigate('/activity/' + encodeURIComponent(e.event_id))}
                  >
                    <td>
                      <div style={{ fontWeight: 600 }}>{eventTitle(e)}</div>
                      <div className="muted small">{e.summary}</div>
                    </td>
                    <td className="small">{e.categories.length ? e.categories.map(categoryLabel).join(', ') : <span className="faint">None</span>}</td>
                    <td>
                      <Severity s={e.severity} />
                    </td>
                    <td className="small mono">{e.app_id}</td>
                    <td className="small nowrap" title={dateTime(e.timestamp)}>
                      {timeAgo(e.timestamp)}
                    </td>
                    <td className="right">
                      <ActionBadge action={e.action} />
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        )}
        <div className="row between" style={{ marginTop: 12 }}>
          <Button small disabled={offset === 0} onClick={() => setOffset(Math.max(0, offset - PAGE))}>
            Newer
          </Button>
          <span className="muted small">
            {offset + 1} to {offset + (events.data?.events.length ?? 0)}
          </span>
          <Button small disabled={(events.data?.events.length ?? 0) < PAGE} onClick={() => setOffset(offset + PAGE)}>
            Older
          </Button>
        </div>
      </Card>
    </div>
  );
}
