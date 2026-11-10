import { useState } from 'react';
import { get, put, type Action, type CategoryInfo, type CategoryState } from '../api';
import { Badge, Card, ErrorText, Loading, Notice, PageHeader, Toggle, TypedConfirmDialog, useAction, useLoad } from '../ui';
import { ActionSelect } from './ApprovalDetail';

const RELAXED_CRITICAL: Action[] = ['ALLOW', 'TOKENIZE', 'REDACT', 'GENERALIZE', 'REQUIRE_APPROVAL', 'PRIVATE_ROUTE'];
const NONCRITICAL_ACTIONS: Action[] = ['ALLOW', 'TOKENIZE', 'REDACT', 'GENERALIZE', 'REQUIRE_APPROVAL', 'PRIVATE_ROUTE', 'FALLBACK', 'BLOCK'];

/** Warning + typed confirmation shown before a critical category is relaxed. */
export function CriticalWarning({
  cat,
  onConfirm,
  onClose,
  allowActionChoice = true,
}: {
  cat: CategoryInfo;
  onConfirm: (action: Action, phrase: string) => Promise<void> | void;
  onClose: () => void;
  allowActionChoice?: boolean;
}) {
  const [action, setAction] = useState<Action>('ALLOW');
  return (
    <TypedConfirmDialog title={`Turn off ${cat.label} protection?`} phrase={`DISABLE ${cat.id}`} confirmLabel="Turn off protection" cancelLabel="Keep protection on" onClose={onClose} onConfirm={(p) => onConfirm(action, p)}>
      <p>
        <strong>{cat.label}</strong> is a critical category. It is blocked by default because these values ({cat.examples.join(', ')}) can cause direct harm if they reach a remote model.
      </p>
      <p>Turning it off lets matching values leave this device under the action you choose. The change is versioned, recorded in the audit log, and can be undone instantly here.</p>
      {allowActionChoice ? (
        <label className="field">
          <span className="field-label">New action</span>
          <ActionSelect value={action} onChange={setAction} options={RELAXED_CRITICAL} />
        </label>
      ) : null}
    </TypedConfirmDialog>
  );
}

export default function CategoriesPage() {
  const cats = useLoad(() => get<{ categories: CategoryState[] }>('/categories'), []);
  const [disabling, setDisabling] = useState<CategoryState | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const act = useAction();

  const set = (c: CategoryState, action: Action, confirm?: string) =>
    act.run(async () => {
      await put('/categories/' + encodeURIComponent(c.id), confirm ? { action, confirm } : { action });
      setNotice(`${c.label}: ${action}${c.critical && confirm ? ' (critical exception recorded)' : ''}.`);
      await cats.reload();
    });

  const list = cats.data?.categories ?? [];
  const critical = list.filter((c) => c.critical);
  const other = list.filter((c) => !c.critical);

  return (
    <div className="content narrow">
      <PageHeader title="Categories" subtitle="Built-in data types Prompt VPN looks for. Critical ones are blocked by default; turning one off needs a typed confirmation." />
      {notice ? <Notice tone="ok">{notice}</Notice> : null}
      <ErrorText error={act.error || cats.error} />
      {cats.loading && !cats.data ? <Loading /> : null}

      {critical.length ? (
        <Card title="Critical protection">
          <div className="list">
            {critical.map((c) => {
              const on = c.action === 'BLOCK';
              return (
                <div className="option-row" key={c.id}>
                  <div className="li-main">
                    <div className="li-title row gap-s">
                      {c.label}
                      {c.exception ? <Badge tone="red">Exception: {c.action}</Badge> : null}
                    </div>
                    <div className="li-sub">{c.description}</div>
                    <div className="examples">{c.examples.join('  ·  ')}</div>
                  </div>
                  <Toggle
                    label={`${c.label} protection`}
                    checked={on}
                    disabled={act.busy}
                    onChange={(v) => (v ? void set(c, 'BLOCK') : setDisabling(c))}
                  />
                </div>
              );
            })}
          </div>
        </Card>
      ) : null}

      {other.length ? (
        <Card title="Other personal data">
          <div className="list">
            {other.map((c) => (
              <div className="option-row" key={c.id}>
                <div className="li-main">
                  <div className="li-title">{c.label}</div>
                  <div className="li-sub">{c.description}</div>
                  <div className="examples">{c.examples.join('  ·  ')}</div>
                </div>
                <div style={{ width: 190 }}>
                  <ActionSelect value={(c.action ?? 'ALLOW') as Action} options={NONCRITICAL_ACTIONS} onChange={(a) => void set(c, a)} />
                </div>
                <Toggle label={`${c.label} protection`} checked={c.protected} disabled={act.busy} onChange={(v) => void set(c, v ? 'TOKENIZE' : 'ALLOW')} />
              </div>
            ))}
          </div>
        </Card>
      ) : null}

      {disabling ? (
        <CriticalWarning
          cat={disabling}
          onClose={() => setDisabling(null)}
          onConfirm={async (action, phrase) => {
            await put('/categories/' + encodeURIComponent(disabling.id), { action, confirm: phrase });
            setNotice(`${disabling.label} protection is off (${action}). The exception is recorded in the audit log.`);
            await cats.reload();
          }}
        />
      ) : null}
    </div>
  );
}
