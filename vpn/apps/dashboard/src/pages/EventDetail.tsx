import { get, type AuditEvent } from '../api';
import { ActionBadge, Severity } from '../components';
import { categoryLabel, dateTime, eventTitle } from '../format';
import { useEntityLabels } from '../entities';
import { href } from '../router';
import { Card, ErrorText, KV, Loading, PageHeader, useLoad } from '../ui';

export default function EventDetailPage({ id }: { id: string }) {
  useEntityLabels();
  const ev = useLoad(() => get<AuditEvent>('/events/' + encodeURIComponent(id)), [id]);
  const e = ev.data;
  return (
    <div className="content narrow">
      <PageHeader back={{ href: href('/activity'), label: 'Activity' }} title={e ? eventTitle(e) : 'Event'} subtitle={e?.summary} actions={e ? <ActionBadge action={e.action} /> : null} />
      <ErrorText error={ev.error} />
      {!e ? (
        ev.loading ? <Loading /> : null
      ) : (
        <>
          <Card title="Decision details">
            <KV
              rows={[
                ['Time', dateTime(e.timestamp)],
                ['Action', e.action],
                ['Severity', <Severity s={e.severity} />],
                ['Detected', e.categories.length ? e.categories.map(categoryLabel).join(', ') : 'Nothing sensitive'],
                ['Findings', String(e.finding_count)],
                ['Policies', e.policy_ids.length ? e.policy_ids.map((p) => <a key={p} href={href('/policies?rule=' + encodeURIComponent(p))} style={{ marginRight: 8 }}>{p}</a>) : null],
                ['Policy hash', <span className="mono small break">{e.policy_hash}</span>],
                ['Error code', e.error_code ? <span className="mono">{e.error_code}</span> : null],
                ['Approval', e.approval_id ? <a href={href('/approvals/' + encodeURIComponent(e.approval_id))}>{e.approval_status ?? 'View'} ({e.approval_id})</a> : null],
              ]}
            />
          </Card>
          <Card title="Request">
            <KV
              rows={[
                ['Kind', e.kind],
                ['Direction', e.direction],
                ['App', <span className="mono">{e.app_id}</span>],
                ['Agent', e.agent_id],
                ['User', e.user_id],
                ['Destination', e.destination_id],
                ['Endpoint', e.endpoint ? <span className="mono">{e.endpoint}</span> : null],
                ['Latency', `${e.latency_ms} ms`],
                ['Recognizers', e.recognizer_versions.length ? <span className="mono small">{e.recognizer_versions.join(', ')}</span> : null],
                ['Event ID', <span className="mono small">{e.event_id}</span>],
              ]}
            />
          </Card>
          <p className="muted small">Audit events hold metadata only. The prompt and any matched values are not stored.</p>
        </>
      )}
    </div>
  );
}
