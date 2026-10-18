import { fileURLToPath } from 'node:url'

import react from '@vitejs/plugin-react'
import { defineConfig } from 'vite'

// The build lands inside the Go tree, where internal/webui embeds it into
// hangar-server. In development Vite serves the UI itself, behind the Caddy
// in compose.yaml, which sends /api/ to the Go server on the same origin.
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
    outDir: '../internal/webui/dist/app',
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
