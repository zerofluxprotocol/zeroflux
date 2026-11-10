// Shared cache of custom entity labels, so any page can render `CUSTOM:<id>`
// as the owner's label ("insurance policy number") instead of a raw id.
import { useEffect, useSyncExternalStore } from 'react';
import { get, type EntitiesResponse } from './api';

let labels = new Map<string, string>();
let loaded = false;
let inflight: Promise<void> | null = null;
const listeners = new Set<() => void>();

function emit() {
  listeners.forEach((fn) => fn());
}

/** Fetch /entities once per page load (or again after `invalidateEntityLabels`). */
export function loadEntityLabels(): Promise<void> {
  if (loaded) return Promise.resolve();
  if (!inflight) {
    inflight = get<EntitiesResponse>('/entities')
      .then((r) => {
        labels = new Map(r.entities.map((e) => [e.entity.id, e.entity.label]));
        registerBuiltinLabels(r.builtin_labels);
        loaded = true;
        emit();
      })
      .catch(() => {
        /* labels fall back to "Custom entity" */
      })
      .finally(() => {
        inflight = null;
      });
  }
  return inflight;
}

/** Replace the cache with a fresh list (call after adding or removing an entity). */
export function setEntityLabels(r: EntitiesResponse): void {
  labels = new Map(r.entities.map((e) => [e.entity.id, e.entity.label]));
  loaded = true;
  emit();
}

export function invalidateEntityLabels(): void {
  loaded = false;
  void loadEntityLabels();
}

export function entityLabel(id: string): string | undefined {
  return labels.get(id);
}

let version = 0;
listeners.add(() => {
  version++;
});

/** Subscribe a component to the label cache; re-renders when labels arrive. */
export function useEntityLabels(): number {
  useEffect(() => {
    void loadEntityLabels();
  }, []);
  return useSyncExternalStore(
    (fn) => {
      listeners.add(fn);
      return () => listeners.delete(fn);
    },
    () => version,
  );
}

/** Labels the daemon reports as built-in (authoritative). */
const serverBuiltin = new Set<string>();
export function registerBuiltinLabels(labels: string[] | undefined) {
  for (const l of labels ?? []) serverBuiltin.add(l);
}
export function isServerBuiltinLabel(l: string): boolean {
  return serverBuiltin.has(l);
}
