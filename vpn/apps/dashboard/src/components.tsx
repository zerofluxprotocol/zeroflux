// Domain components shared across pages.
import { Fragment } from 'react';
import type { AuditEvent } from './generated/types';
import { actionBadge, eventTitle, timeAgo } from './format';
import { href } from './router';
import { Badge, Empty } from './ui';

export function ActionBadge({ action }: { action: string }) {
  const b = actionBadge(action);
  return <Badge tone={b.tone}>{b.label}</Badge>;
}

export function Severity({ s }: { s: string }) {
  return <span className={`sev ${s}`}>{s.charAt(0).toUpperCase() + s.slice(1)}</span>;
}

export function EventList({ events, empty = 'No activity yet.' }: { events: AuditEvent[]; empty?: string }) {
  if (events.length === 0) return <Empty>{empty}</Empty>;
  return (
    <div className="list">
      {events.map((e) => (
        <a key={e.event_id} className="list-item" href={href('/activity/' + encodeURIComponent(e.event_id))}>
          <div className="li-main">
            <div className="li-title">{eventTitle(e)}</div>
            <div className="li-sub ellipsis">{e.summary}</div>
          </div>
          <span className="li-meta" title={e.timestamp}>
            {timeAgo(e.timestamp)}
          </span>
          <span style={{ width: 96, display: 'flex', justifyContent: 'flex-end' }}>
            <ActionBadge action={e.action} />
          </span>
        </a>
      ))}
    </div>
  );
}

const MARKER = /\[REDACTED_([A-Z0-9_]+)\]/g;

/** Renders `[REDACTED_X]` markers as solid bars; the category shows on hover/focus. */
export function RedactedText({ text }: { text: string }) {
  const out: React.ReactNode[] = [];
  let last = 0;
  let i = 0;
  for (const m of text.matchAll(MARKER)) {
    const idx = m.index ?? 0;
    if (idx > last) out.push(<Fragment key={`t${i}`}>{text.slice(last, idx)}</Fragment>);
    const cat = m[1];
    out.push(
      <span key={`r${i}`} className="redact-bar" data-cat={cat} title={cat} tabIndex={0} role="img" aria-label={`Redacted ${cat}`}>
        {' '.repeat(Math.max(6, Math.min(14, cat.length)))}
      </span>,
    );
    last = idx + m[0].length;
    i++;
  }
  if (last < text.length) out.push(<Fragment key="end">{text.slice(last)}</Fragment>);
  return <div className="preview">{out}</div>;
}
