import { useShell } from '../App';
import { EventList } from '../components';
import { num } from '../format';
import { href } from '../router';
import { Card, CopyButton, Loading, Notice } from '../ui';

export default function OverviewPage() {
  const { overview: ov } = useShell();
  if (!ov) return <div className="content"><Loading /></div>;
  const s = ov.stats;
  const headline = ov.emergency_stop ? 'Emergency stop is on' : ov.protected ? 'Your prompts are protected' : 'Protection is set up, routing is not';
  const sub = ov.emergency_stop
    ? 'All model traffic is blocked locally until you release the stop in Settings.'
    : ov.protected
      ? 'Sensitive information is detected, redacted locally, and kept private.'
      : 'Requests are inspected, but no default destination is configured, so nothing is forwarded.';
  return (
    <div className="content">
      <header className="page-head">
        <h1>{headline}</h1>
        <p className="lede">{sub}</p>
      </header>

      {ov.pending_approvals > 0 ? (
        <Notice tone="warn">
          <div className="row between gap">
            <span>
              {ov.pending_approvals} {ov.pending_approvals === 1 ? 'prompt is' : 'prompts are'} waiting for your review.
            </span>
            <a href={href('/approvals')}>Review now</a>
          </div>
        </Notice>
      ) : null}

      <Card>
        <div className="stats">
          <div className="stat">
            <div className="n">{num(s.total_requests)}</div>
            <div className="l">Prompts processed</div>
          </div>
          <div className="stat">
            <div className="n">{num(s.transformed)}</div>
            <div className="l">Redactions</div>
          </div>
          <div className="stat">
            <div className="n">{num(s.blocked)}</div>
            <div className="l">Blocked</div>
          </div>
          <div className="stat">
            <div className="n">{num(s.review + s.private_routed)}</div>
            <div className="l">Reviewed or private-routed</div>
          </div>
        </div>
      </Card>

      <Card title="Recent activity" actions={<a className="muted small" href={href('/activity')}>View all</a>}>
        <EventList events={ov.recent} empty="No prompts yet. Point a client at the base URL below to start." />
      </Card>

      <Card title="Connect a client">
        <div className="stack">
          <p className="muted">Any OpenAI-compatible client works. Use this base URL with a local API key from Apps.</p>
          <div className="keybox">
            <code>{ov.base_url}</code>
            <CopyButton text={ov.base_url} />
          </div>
          <p className="muted small">
            Coverage: Prompt VPN inspects requests sent to this API, including tool-call arguments and tool results inside them. It does not intercept tools your agent executes separately (shell, HTTP, browser).
          </p>
        </div>
      </Card>
      {s.errors > 0 ? <p className="muted small">{num(s.errors)} requests ended in an error. See Activity filtered by Error.</p> : null}
    </div>
  );
}
