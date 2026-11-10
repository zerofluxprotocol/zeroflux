import { createContext, useContext, useEffect, useState, type ReactNode } from 'react';
import { ApiError, get, loadSession, onUnauthorized, type Overview, type SessionInfo, type SettingsResponse } from './api';
import { href, navigate, useRoute } from './router';
import { Loading, useLoad } from './ui';
import OverviewPage from './pages/Overview';
import ActivityPage from './pages/Activity';
import EventDetailPage from './pages/EventDetail';
import ApprovalsPage from './pages/Approvals';
import ApprovalDetailPage from './pages/ApprovalDetail';
import PoliciesPage from './pages/Policies';
import CategoriesPage from './pages/Categories';
import SecretsPage from './pages/Secrets';
import EntitiesPage from './pages/Entities';
import AppsPage from './pages/Apps';
import ModelsPage from './pages/Models';
import SettingsPage from './pages/Settings';
import HealthPage from './pages/Health';
import OnboardingPage from './pages/Onboarding';

interface Shell {
  overview: Overview | null;
  refresh: () => Promise<void>;
  version: string;
}

const ShellCtx = createContext<Shell>({ overview: null, refresh: async () => {}, version: '' });
export const useShell = () => useContext(ShellCtx);

const NAV: { path: string; label: string }[] = [
  { path: '/overview', label: 'Overview' },
  { path: '/activity', label: 'Activity' },
  { path: '/approvals', label: 'Approvals' },
  { path: '/policies', label: 'Policies' },
  { path: '/categories', label: 'Categories' },
  { path: '/secrets', label: 'Secrets' },
  { path: '/entities', label: 'Custom entities' },
  { path: '/apps', label: 'Apps' },
  { path: '/models', label: 'Models & destinations' },
  { path: '/settings', label: 'Settings' },
  { path: '/health', label: 'Health' },
];

export default function App() {
  const [session, setSession] = useState<SessionInfo | null>(null);
  const [state, setState] = useState<'loading' | 'unauth' | 'error' | 'ready'>('loading');
  const [error, setError] = useState<string>('');

  useEffect(() => {
    const off = onUnauthorized(() => setState('unauth'));
    loadSession()
      .then((s) => {
        setSession(s);
        setState('ready');
      })
      .catch((e) => {
        if (e instanceof ApiError && e.status === 401) setState('unauth');
        else {
          setError(e instanceof Error ? e.message : String(e));
          setState('error');
        }
      });
    return () => {
      off();
    };
  }, []);

  if (state === 'loading') {
    return (
      <div className="gate">
        <Loading what="Connecting to the local daemon" />
      </div>
    );
  }
  if (state === 'unauth') return <SignedOut />;
  if (state === 'error') return <SignedOut error={error} />;
  return <Authed version={session?.version ?? ''} />;
}

function SignedOut({ error }: { error?: string }) {
  return (
    <div className="gate">
      <div className="card">
        <div className="brand">prompt vpn</div>
        <h1 style={{ fontSize: 24 }}>{error ? 'Cannot reach Prompt VPN' : 'Open the dashboard from your terminal'}</h1>
        {error ? <p className="muted">{error}</p> : null}
        <p className="muted">
          The dashboard only accepts a one-time login link issued by the local daemon. Run this in a terminal on this computer:
        </p>
        <pre className="code">promptvpn dashboard</pre>
        <p className="muted small">The link expires after two minutes and works once. Your session lasts 12 hours.</p>
      </div>
    </div>
  );
}

function Authed({ version }: { version: string }) {
  const { parts } = useRoute();
  const overview = useLoad(() => get<Overview>('/overview'), [], 10_000);
  const settings = useLoad(() => get<SettingsResponse>('/settings'), []);

  // First run: open onboarding automatically.
  useEffect(() => {
    if (settings.data && !settings.data.onboarding.completed && parts[0] !== 'onboarding') navigate('/onboarding');
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [settings.data]);

  useEffect(() => {
    if (parts.length === 0) navigate('/overview');
  }, [parts.length]);

  const shell: Shell = { overview: overview.data, refresh: overview.reload, version };

  if (parts[0] === 'onboarding') {
    return (
      <ShellCtx.Provider value={shell}>
        <OnboardingPage
          onDone={() => {
            void settings.reload();
            void overview.reload();
          }}
        />
      </ShellCtx.Provider>
    );
  }

  return (
    <ShellCtx.Provider value={shell}>
      <div className="window">
        <header className="topbar">
          <a className="brand" href={href('/overview')}>
            prompt vpn
          </a>
          <div className="top-right">
            {version ? <span className="faint">v{version}</span> : null}
            <StatusChip ov={overview.data} />
          </div>
        </header>
        <div className="body">
          <aside className="sidebar">
            <nav aria-label="Main">
              {NAV.map((n) => {
                const active = '/' + (parts[0] ?? '') === n.path;
                return (
                  <a key={n.path} href={href(n.path)} className={`nav-item${active ? ' active' : ''}`} aria-current={active ? 'page' : undefined}>
                    <span>{n.label}</span>
                    {n.path === '/approvals' && overview.data && overview.data.pending_approvals > 0 ? (
                      <span className="nav-count" aria-label={`${overview.data.pending_approvals} pending`}>
                        {overview.data.pending_approvals}
                      </span>
                    ) : null}
                  </a>
                );
              })}
            </nav>
          </aside>
          <main className="main">
            <Page parts={parts} />
          </main>
        </div>
      </div>
    </ShellCtx.Provider>
  );
}

function StatusChip({ ov }: { ov: Overview | null }) {
  if (!ov) return <span className="status-chip warn"><span className="dot" />Checking</span>;
  if (ov.emergency_stop)
    return (
      <a className="status-chip off" href={href('/settings')} style={{ textDecoration: 'none' }}>
        <span className="dot" />
        Emergency stop
      </a>
    );
  if (!ov.protected)
    return (
      <a className="status-chip warn" href={href('/health')} style={{ textDecoration: 'none' }} title="No default destination configured">
        <span className="dot" />
        Not routing
      </a>
    );
  return (
    <span className="status-chip">
      <span className="dot" />
      Protected
    </span>
  );
}

function Redirect({ to }: { to: string }) {
  useEffect(() => navigate(to), [to]);
  return null;
}

function Page({ parts }: { parts: string[] }): ReactNode {
  const [a, b] = parts;
  switch (a) {
    case undefined:
    case 'overview':
      return <OverviewPage />;
    case 'activity':
      return b ? <EventDetailPage id={decodeURIComponent(b)} /> : <ActivityPage />;
    case 'approvals':
      return b ? <ApprovalDetailPage id={decodeURIComponent(b)} /> : <ApprovalsPage />;
    case 'policies':
      return <PoliciesPage />;
    case 'categories':
      return <CategoriesPage />;
    case 'secrets':
      return <SecretsPage />;
    case 'entities':
      return <EntitiesPage />;
    case 'concepts': // old bookmark
      return <Redirect to="/entities" />;
    case 'apps':
      return <AppsPage />;
    case 'models':
      return <ModelsPage />;
    case 'settings':
      return <SettingsPage />;
    case 'health':
      return <HealthPage />;
    default:
      return (
        <div className="content">
          <h1>Not found</h1>
          <a href={href('/overview')}>Back to overview</a>
        </div>
      );
  }
}
