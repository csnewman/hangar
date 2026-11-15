import { fileURLToPath } from 'node:url'

import react from '@vitejs/plugin-react'
import { defineConfig } from 'vite'

// The build lands in dist/, which hangar-server embeds (crates/server's
// http/web.rs). In development Vite serves the UI itself, behind the Caddy
// in compose.yaml, which sends /api/ to the server on the same origin.
export default defineConfig({
  plugins: [react()],
  resolve: {
    // The spelling dictionaries' files, which their packages do not export.
    alias: {
      '@dict/en': fileURLToPath(new URL('./node_modules/dictionary-en', import.meta.url)),
      '@dict/cspell': fileURLToPath(new URL('./node_modules/@cspell', import.meta.url)),
    },
  },
  build: {
    outDir: 'dist',
    emptyOutDir: true,
  },
  server: {
    host: true,
    port: 5173,
    strictPort: true,
    // The UI on a phone, through `tailscale serve` on this machine: Vite
    // answers only the hosts it is told of, *.localhost among them.
    allowedHosts: ['.ts.net'],
  },
})
