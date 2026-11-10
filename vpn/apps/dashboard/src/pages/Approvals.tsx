import { useState } from 'react';
import { get, type PendingApproval } from '../api';
import { categoryLabel, timeAgo } from '../format';
import { useEntityLabels } from '../entities';
import { href } from '../router';
import { Badge, Card, Check, Empty, ErrorText, Loading, PageHeader, useLoad } from '../ui';

export function statusTone(s: string) {
  return s === 'PENDING' ? 'dark' : s === 'DENIED' ? 'red' : s === 'APPROVED' || s === 'CONSUMED' ? 'outline' : 'neutral';
}

export default function ApprovalsPage() {
  useEntityLabels();
  const [all, setAll] = useState(false);
  const list = useLoad(() => get<{ approvals: PendingApproval[] }>(`/approvals?all=${all}`), [all], 10_000);
  return (
    <div className="content">
      <PageHeader
        title="Approvals"
        subtitle="Prompts held locally for your decision. The client got an immediate approval-required error; approving does not resend, the client must retry."
      />
      <Card actions={<Check checked={all} onChange={setAll}>Show decided and expired</Check>} title={all ? 'All approvals' : 'Waiting for review'}>
        <ErrorText error={list.error} />
        {list.loading && !list.data ? (
          <Loading />
        ) : !list.data?.approvals.length ? (
          <Empty>{all ? 'No approvals recorded.' : 'Nothing is waiting for review.'}</Empty>
        ) : (
          <div className="list">
            {list.data.approvals.map((a) => {
              const expired = a.status === 'PENDING' && new Date(a.expires_at).getTime() < Date.now();
              return (
                <a key={a.approval_id} className="list-item" href={href('/approvals/' + encodeURIComponent(a.approval_id))}>
                  <div className="li-main">
                    <div className="li-title">{a.categories.length ? a.categories.map(categoryLabel).join(', ') : 'Review required'}</div>
                    <div className="li-sub ellipsis">{a.reason}</div>
                  </div>
                  <span className="li-meta">
                    {a.status === 'PENDING' && !expired ? `expires ${timeAgo(a.expires_at)}` : timeAgo(a.created_at)}
                  </span>
                  <Badge tone={expired ? 'neutral' : statusTone(a.status)}>{expired ? 'EXPIRED' : a.status}</Badge>
                </a>
              );
            })}
          </div>
        )}
      </Card>
    </div>
  );
}
