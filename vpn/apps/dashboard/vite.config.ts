import { defineConfig, type ProxyOptions } from 'vite';
import react from '@vitejs/plugin-react';

// The daemon embeds dist/ and serves it at /dashboard/. In development the Vite
// server proxies the admin API and the one-time login route to a running
// daemon. The daemon only accepts its own loopback Host and Origin, so the
// proxy rewrites both. Set PV_DAEMON_URL to point at a non-default port.
const daemon = process.env.PV_DAEMON_URL ?? 'http://127.0.0.1:8787';
const daemonHost = new URL(daemon).host;

const toDaemon: ProxyOptions = {
  target: daemon,
  changeOrigin: true,
  configure(proxy) {
    proxy.on('proxyReq', (req) => {
      req.setHeader('host', daemonHost);
      req.setHeader('origin', daemon);
    });
  },
};

// Same policy the daemon sends with the embedded build; `npm run preview`
// applies it so CSP violations show up before the Rust binary is rebuilt.
const CSP = "default-src 'self'; img-src 'self' data:; style-src 'self' 'unsafe-inline'; connect-src 'self'; frame-ancestors 'none'; base-uri 'none'; form-action 'self'";

export default defineConfig({
  base: '/dashboard/',
  plugins: [react()],
  build: {
    outDir: 'dist',
    emptyOutDir: true,
    assetsInlineLimit: 0,
    sourcemap: false,
  },
  server: {
    port: 5173,
    strictPort: true,
    proxy: {
      '/api': toDaemon,
      '/dashboard/login': toDaemon,
    },
  },
  preview: {
    port: 4180,
    strictPort: true,
    headers: { 'Content-Security-Policy': CSP },
    proxy: {
      '/api': toDaemon,
      '/dashboard/login': toDaemon,
    },
  },
});
