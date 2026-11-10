import { useEffect, useState } from 'react';

// Hash routing (#/activity/abc) so any server path falls back to index.html.
export function currentPath(): string {
  const h = window.location.hash.replace(/^#/, '');
  return h.startsWith('/') ? h : '/' + h;
}

export function useRoute(): { path: string; parts: string[]; query: URLSearchParams } {
  const [path, setPath] = useState(currentPath());
  useEffect(() => {
    const on = () => setPath(currentPath());
    window.addEventListener('hashchange', on);
    return () => window.removeEventListener('hashchange', on);
  }, []);
  const [p, q = ''] = path.split('?');
  return { path: p, parts: p.split('/').filter(Boolean), query: new URLSearchParams(q) };
}

export function navigate(to: string): void {
  const target = '#' + (to.startsWith('/') ? to : '/' + to);
  if (window.location.hash !== target) window.location.hash = target;
}

export const href = (to: string) => '#' + (to.startsWith('/') ? to : '/' + to);
