import { useState } from 'react';
import { registerBuiltinLabels } from '../entities';
import { isServerBuiltinLabel } from '../entities';
import { del, get, post, type Action, type EntitiesResponse } from '../api';
import { setEntityLabels } from '../entities';
import { dateTime } from '../format';
import { href } from '../router';
import { Badge, Button, Card, ConfirmDialog, Empty, ErrorText, Field, Loading, Notice, PageHeader, useAction, useLoad } from '../ui';
import { ActionSelect } from './ApprovalDetail';

/** TOKENIZE first: it is the default and keeps prompts useful. */
export const ENTITY_ACTIONS: Action[] = ['TOKENIZE', 'REDACT', 'REQUIRE_APPROVAL', 'BLOCK'];

export interface EntityDraft {
  label: string;
  description: string;
  action: Action;
}

/** Mirror of the daemon's label normalization: lowercase, letters/digits/spaces/hyphens, 2 to 60 chars. */
export function normalizeEntityLabel(label: string): string {
  return label
    .trim()
    .toLowerCase()
    .replace(/[^\p{L}\p{N} -]/gu, '')
    .split(/\s+/)
    .filter(Boolean)
    .join(' ');
}

/**
 * Labels the detection model already uses for built-in categories (pv-detectors
 * GLINER_LABELS). The daemon rejects these as custom labels.
 */
export const BUILTIN_SPAN_LABELS: Record<string, string> = {
  name: 'Names',
  username: 'Names',
  'email address': 'Email',
  'phone number': 'Phone numbers',
  'location address': 'Postal addresses',
  'location street': 'Postal addresses',
  dob: 'Dates of birth',
  ssn: 'SSN and government IDs',
  'passport number': 'Passport numbers',
  'driver license': "Driver's licenses",
  'credit card': 'Payment cards',
  cvv: 'Payment cards',
  'bank account': 'Bank accounts',
  'account number': 'Bank accounts',
  'routing number': 'Bank accounts',
  condition: 'Health information',
  drug: 'Health information',
  'medical code': 'Health information',
  'healthcare number': 'Health information',
  password: 'Passwords and auth secrets',
};

/** Why a label would be rejected, or null. `existing` holds normalized labels already registered. */
export function entityLabelProblem(label: string, existing: Iterable<string> = []): string | null {
  const n = normalizeEntityLabel(label);
  const len = [...n].length;
  if (len < 2 || len > 60) return 'Use 2 to 60 letters, digits or spaces.';
  if (BUILTIN_SPAN_LABELS[n] || isServerBuiltinLabel(n)) return `"${n}" is already covered by the built-in category ${BUILTIN_SPAN_LABELS[n] ?? 'detection'}. Set its action under Categories.`;
  for (const e of existing) if (e === n) return `"${n}" already exists.`;
  return null;
}

function labelHint(label: string, existing: Iterable<string>): { error: boolean; text: string } {
  const n = normalizeEntityLabel(label);
  if (!label.trim()) return { error: false, text: 'In plain words, the way you would describe it to a person.' };
  const problem = entityLabelProblem(label, existing);
  if (problem) return { error: true, text: problem };
  if (n !== label.trim()) return { error: false, text: `Saved as "${n}".` };
  return { error: false, text: 'In plain words, the way you would describe it to a person.' };
}

export function SpanModelMissing() {
  return (
    <Notice tone="warn">
      No detection model is loaded, so requests are held for review until you install one. <a href={href('/models')}>Install a detection model</a>
    </Notice>
  );
}

/** Form used here and in onboarding step 3. */
export function EntityForm({
  onSubmit,
  submitLabel,
  busy,
  existing = [],
}: {
  onSubmit: (e: EntityDraft) => Promise<boolean | void>;
  submitLabel: string;
  busy?: boolean;
  /** Labels already registered or drafted, so duplicates are caught before submit. */
  existing?: string[];
}) {
  const [label, setLabel] = useState('');
  const [description, setDescription] = useState('');
  const [action, setAction] = useState<Action>('TOKENIZE');
  const hint = labelHint(label, existing);
  return (
    <div className="stack">
      <div className="form-grid">
        <Field label="What to look for" hint={hint.text}>
          <input className="input" value={label} onChange={(e) => setLabel(e.target.value)} placeholder="insurance policy number" maxLength={60} aria-invalid={hint.error} />
        </Field>
        <Field label="Action when found">
          <ActionSelect value={action} onChange={setAction} options={ENTITY_ACTIONS} />
        </Field>
        <div className="full">
          <Field label="Description (optional)" hint="A note for you and other owners. Detection uses the label.">
            <input className="input" value={description} onChange={(e) => setDescription(e.target.value)} placeholder="Policy numbers from our insurer, like HX-2291-0042" />
          </Field>
        </div>
      </div>
      <div className="row" style={{ justifyContent: 'flex-end' }}>
        <Button
          variant="primary"
          busy={busy}
          disabled={!label.trim() || hint.error}
          onClick={async () => {
            const ok = await onSubmit({ label: normalizeEntityLabel(label), description: description.trim(), action });
            if (ok !== false) {
              setLabel('');
              setDescription('');
              setAction('TOKENIZE');
            }
          }}
        >
          {submitLabel}
        </Button>
      </div>
    </div>
  );
}

export default function EntitiesPage() {
  const list = useLoad(async () => {
    const r = await get<EntitiesResponse>('/entities').then((r) => (registerBuiltinLabels(r.builtin_labels), r));
    setEntityLabels(r);
    return r;
  }, []);
  const add = useAction();
  const [notice, setNotice] = useState<string | null>(null);
  const [removing, setRemoving] = useState<{ id: string; label: string } | null>(null);
  const entities = list.data?.entities ?? [];

  return (
    <div className="content narrow">
      <PageHeader
        title="Custom entities"
        subtitle={
          <>
            Kinds of information with no fixed format, like insurance policy numbers or project codenames. The local detection model finds them in free text. Exact strings you already know belong under <a href={href('/secrets')}>Secrets</a>.
          </>
        }
      />
      {list.data && !list.data.span_model_loaded ? <SpanModelMissing /> : null}
      {notice ? <Notice tone="ok">{notice}</Notice> : null}
      <Card title="Add a custom entity type">
        <EntityForm
          submitLabel="Add entity type"
          busy={add.busy}
          existing={entities.map((x) => x.entity.label)}
          onSubmit={async (e) => {
            const r = await add.run(async () => {
              await post<{ entity_id: string; span_model_loaded: boolean }>('/entities', { label: e.label, action: e.action, ...(e.description ? { description: e.description } : {}) });
              setNotice(`Added "${e.label}". Matches use ${e.action}.`);
              await list.reload();
              return true;
            });
            return r === true;
          }}
        />
        <ErrorText error={add.error} />
      </Card>

      <Card title={`Entity types (${entities.length})`}>
        <ErrorText error={list.error} />
        {list.loading && !list.data ? (
          <Loading />
        ) : !entities.length ? (
          <Empty>No custom entity types yet.</Empty>
        ) : (
          <div className="list">
            {entities.map(({ entity: e, action, created_at }) => (
              <div className="list-item" key={e.id}>
                <div className="li-main">
                  <div className="li-title">{e.label}</div>
                  {e.description ? <div className="li-sub">{e.description}</div> : null}
                  <div className="faint small">
                    <span className="mono">CUSTOM:{e.id}</span> · added {dateTime(created_at)}
                  </div>
                </div>
                <Badge tone={action === 'BLOCK' ? 'red' : action === 'REQUIRE_APPROVAL' ? 'dark' : 'outline'}>{action}</Badge>
                <Button small variant="danger" onClick={() => setRemoving({ id: e.id, label: e.label })}>
                  Remove
                </Button>
              </div>
            ))}
          </div>
        )}
      </Card>

      {removing ? (
        <ConfirmDialog
          title={`Remove "${removing.label}"?`}
          danger
          confirmLabel="Remove"
          onClose={() => setRemoving(null)}
          onConfirm={async () => {
            await del('/entities/' + encodeURIComponent(removing.id));
            setNotice(`Removed "${removing.label}".`);
            await list.reload();
          }}
        >
          <p>The detection model stops looking for this type and its policy rule is deactivated. Past activity keeps the record.</p>
        </ConfirmDialog>
      ) : null}
    </div>
  );
}
