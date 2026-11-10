import { useCallback, useEffect, useId, useRef, useState, type ReactNode } from 'react';
import { ApiError } from './api';
import type { Tone } from './format';

// ------------------------------------------------------------------ data hooks

export interface Loaded<T> {
  data: T | null;
  error: string | null;
  loading: boolean;
  reload: () => Promise<void>;
  setData: (d: T) => void;
}

export function useLoad<T>(fn: () => Promise<T>, deps: unknown[] = [], pollMs?: number): Loaded<T> {
  const [data, setData] = useState<T | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);
  const fnRef = useRef(fn);
  fnRef.current = fn;
  const reload = useCallback(async () => {
    try {
      const d = await fnRef.current();
      setData(d);
      setError(null);
    } catch (e) {
      setError(errMessage(e));
    } finally {
      setLoading(false);
    }
  }, []);
  useEffect(() => {
    setLoading(true);
    void reload();
    if (!pollMs) return;
    const t = window.setInterval(() => void reload(), pollMs);
    return () => window.clearInterval(t);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, deps);
  return { data, error, loading, reload, setData };
}

export function errMessage(e: unknown): string {
  if (e instanceof ApiError) return e.message;
  if (e instanceof Error) return e.message;
  return String(e);
}

/** Wraps a mutation with busy + inline error state. */
export function useAction() {
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const run = useCallback(async <T,>(fn: () => Promise<T>): Promise<T | undefined> => {
    setBusy(true);
    setError(null);
    try {
      return await fn();
    } catch (e) {
      setError(errMessage(e));
      return undefined;
    } finally {
      setBusy(false);
    }
  }, []);
  return { busy, error, setError, run };
}

// ------------------------------------------------------------------ primitives

type BtnVariant = 'primary' | 'secondary' | 'ghost' | 'danger' | 'link';

export function Button(props: {
  children: ReactNode;
  onClick?: () => void;
  variant?: BtnVariant;
  type?: 'button' | 'submit';
  disabled?: boolean;
  busy?: boolean;
  small?: boolean;
  title?: string;
}) {
  const { variant = 'secondary', small, busy, disabled, type = 'button', title } = props;
  return (
    <button
      type={type}
      title={title}
      className={`btn btn-${variant}${small ? ' btn-sm' : ''}`}
      disabled={disabled || busy}
      onClick={props.onClick}
    >
      {busy ? <span className="spinner" aria-hidden /> : null}
      {props.children}
    </button>
  );
}

export function Toggle(props: { checked: boolean; onChange: (v: boolean) => void; label: string; disabled?: boolean }) {
  return (
    <button
      type="button"
      role="switch"
      aria-checked={props.checked}
      aria-label={props.label}
      disabled={props.disabled}
      className={`toggle${props.checked ? ' on' : ''}`}
      onClick={() => props.onChange(!props.checked)}
    >
      <span className="knob" />
    </button>
  );
}

export function Badge({ children, tone = 'neutral', title }: { children: ReactNode; tone?: Tone; title?: string }) {
  return (
    <span className={`badge badge-${tone}`} title={title}>
      {children}
    </span>
  );
}

export function TierBadge({ tier }: { tier: string }) {
  const tone: Tone = tier === 'LOCAL' ? 'neutral' : tier === 'ATTESTED_TEE' || tier === 'PRIVATE_TEE' ? 'dark' : tier === 'PRIVATE_3P_TEE' ? 'outline' : 'red';
  return <Badge tone={tone}>{tier}</Badge>;
}

export function Card({ children, title, actions, pad = true, className = '' }: { children: ReactNode; title?: ReactNode; actions?: ReactNode; pad?: boolean; className?: string }) {
  return (
    <section className={`card ${pad ? 'pad' : ''} ${className}`}>
      {title || actions ? (
        <div className="card-head">
          {title ? <h2>{title}</h2> : <span />}
          {actions ? <div className="row gap-s">{actions}</div> : null}
        </div>
      ) : null}
      {children}
    </section>
  );
}

export function PageHeader({ title, subtitle, back, actions }: { title: ReactNode; subtitle?: ReactNode; back?: { href: string; label: string }; actions?: ReactNode }) {
  return (
    <header className="page-head">
      {back ? (
        <a className="back" href={back.href}>
          <span aria-hidden>&larr;</span> {back.label}
        </a>
      ) : null}
      <div className="row between wrap gap">
        <h1>{title}</h1>
        {actions ? <div className="row gap-s">{actions}</div> : null}
      </div>
      {subtitle ? <p className="lede">{subtitle}</p> : null}
    </header>
  );
}

export function ErrorText({ error }: { error: string | null | undefined }) {
  if (!error) return null;
  return (
    <div className="alert alert-error" role="alert">
      {error}
    </div>
  );
}

export function Notice({ children, tone = 'info' }: { children: ReactNode; tone?: 'info' | 'warn' | 'ok' | 'error' }) {
  return <div className={`alert alert-${tone}`}>{children}</div>;
}

export function Loading({ what = 'Loading' }: { what?: string }) {
  return (
    <div className="muted loading">
      <span className="spinner" aria-hidden /> {what}
    </div>
  );
}

export function Empty({ children }: { children: ReactNode }) {
  return <div className="empty">{children}</div>;
}

export function Field({ label, hint, children, inline }: { label: ReactNode; hint?: ReactNode; children: ReactNode; inline?: boolean }) {
  return (
    <label className={`field${inline ? ' inline' : ''}`}>
      <span className="field-label">{label}</span>
      {children}
      {hint ? <span className="hint">{hint}</span> : null}
    </label>
  );
}

/** Masked input for secret values and API keys. Never autofilled or spellchecked. */
export function SecretInput(props: { value: string; onChange: (v: string) => void; placeholder?: string; name?: string }) {
  return (
    <input
      type="password"
      className="input mono"
      value={props.value}
      name={props.name}
      placeholder={props.placeholder}
      autoComplete="new-password"
      spellCheck={false}
      autoCapitalize="off"
      autoCorrect="off"
      onChange={(e) => props.onChange(e.target.value)}
    />
  );
}

export function Check(props: { checked: boolean; onChange: (v: boolean) => void; children: ReactNode; disabled?: boolean }) {
  return (
    <label className={`check${props.disabled ? ' disabled' : ''}`}>
      <input type="checkbox" checked={props.checked} disabled={props.disabled} onChange={(e) => props.onChange(e.target.checked)} />
      <span>{props.children}</span>
    </label>
  );
}

export function Radio(props: { name: string; checked: boolean; onChange: () => void; children: ReactNode; disabled?: boolean }) {
  return (
    <label className={`radio${props.checked ? ' selected' : ''}${props.disabled ? ' disabled' : ''}`}>
      <input type="radio" name={props.name} checked={props.checked} disabled={props.disabled} onChange={props.onChange} />
      <span>{props.children}</span>
    </label>
  );
}

export function Segmented<T extends string>(props: { value: T; options: { value: T; label: string }[]; onChange: (v: T) => void; label: string }) {
  return (
    <div className="segmented" role="radiogroup" aria-label={props.label}>
      {props.options.map((o) => (
        <button
          type="button"
          key={o.value}
          role="radio"
          aria-checked={props.value === o.value}
          className={props.value === o.value ? 'active' : ''}
          onClick={() => props.onChange(o.value)}
        >
          {o.label}
        </button>
      ))}
    </div>
  );
}

export function CopyButton({ text, label = 'Copy' }: { text: string; label?: string }) {
  const [done, setDone] = useState(false);
  return (
    <Button
      small
      onClick={async () => {
        try {
          await navigator.clipboard.writeText(text);
        } catch {
          // Fallback for browsers that refuse the async clipboard API.
          const ta = document.createElement('textarea');
          ta.value = text;
          document.body.appendChild(ta);
          ta.select();
          document.execCommand('copy');
          ta.remove();
        }
        setDone(true);
        setTimeout(() => setDone(false), 1500);
      }}
    >
      {done ? 'Copied' : label}
    </Button>
  );
}

export function KV({ rows }: { rows: [ReactNode, ReactNode][] }) {
  return (
    <dl className="kv">
      {rows.map(([k, v], i) => (
        <div className="kv-row" key={i}>
          <dt>{k}</dt>
          <dd>{v === null || v === undefined || v === '' ? <span className="muted">None</span> : v}</dd>
        </div>
      ))}
    </dl>
  );
}

export function Code({ children }: { children: string }) {
  return <pre className="code">{children}</pre>;
}

// ------------------------------------------------------------------ dialogs

export function Dialog(props: { title: string; children: ReactNode; onClose: () => void; footer?: ReactNode; wide?: boolean }) {
  const id = useId();
  const ref = useRef<HTMLDivElement>(null);
  useEffect(() => {
    const prev = document.activeElement as HTMLElement | null;
    const first = ref.current?.querySelector<HTMLElement>('input, textarea, select, button');
    first?.focus();
    const onKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape') props.onClose();
    };
    window.addEventListener('keydown', onKey);
    return () => {
      window.removeEventListener('keydown', onKey);
      prev?.focus?.();
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);
  return (
    <div className="overlay" onMouseDown={(e) => e.target === e.currentTarget && props.onClose()}>
      <div className={`dialog${props.wide ? ' wide' : ''}`} role="dialog" aria-modal="true" aria-labelledby={id} ref={ref}>
        <h2 id={id}>{props.title}</h2>
        <div className="dialog-body">{props.children}</div>
        {props.footer ? <div className="dialog-foot">{props.footer}</div> : null}
      </div>
    </div>
  );
}

export function ConfirmDialog(props: {
  title: string;
  children: ReactNode;
  confirmLabel: string;
  danger?: boolean;
  onConfirm: () => Promise<void> | void;
  onClose: () => void;
}) {
  const act = useAction();
  return (
    <Dialog
      title={props.title}
      onClose={props.onClose}
      footer={
        <>
          <Button onClick={props.onClose}>Cancel</Button>
          <Button
            variant={props.danger ? 'danger' : 'primary'}
            busy={act.busy}
            onClick={async () => {
              const ok = await act.run(async () => {
                await props.onConfirm();
                return true;
              });
              if (ok) props.onClose();
            }}
          >
            {props.confirmLabel}
          </Button>
        </>
      }
    >
      {props.children}
      <ErrorText error={act.error} />
    </Dialog>
  );
}

/** Warning dialog that requires typing an exact phrase, e.g. `DISABLE API_KEY`. */
export function TypedConfirmDialog(props: {
  title: string;
  phrase: string;
  children: ReactNode;
  confirmLabel: string;
  cancelLabel?: string;
  onConfirm: (phrase: string) => Promise<void> | void;
  onClose: () => void;
}) {
  const [typed, setTyped] = useState('');
  const act = useAction();
  const match = typed === props.phrase;
  return (
    <Dialog
      title={props.title}
      onClose={props.onClose}
      footer={
        <>
          <Button onClick={props.onClose}>{props.cancelLabel ?? 'Cancel'}</Button>
          <Button
            variant="danger"
            disabled={!match}
            busy={act.busy}
            onClick={async () => {
              const ok = await act.run(async () => {
                await props.onConfirm(typed);
                return true;
              });
              if (ok) props.onClose();
            }}
          >
            {props.confirmLabel}
          </Button>
        </>
      }
    >
      {props.children}
      <Field label={<>Type <code>{props.phrase}</code> to confirm</>}>
        <input className="input mono" value={typed} onChange={(e) => setTyped(e.target.value)} autoComplete="off" spellCheck={false} aria-label="Confirmation phrase" />
      </Field>
      <ErrorText error={act.error} />
    </Dialog>
  );
}
