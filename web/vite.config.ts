import react from '@vitejs/plugin-react'
import { defineConfig } from 'vite'

// The build lands inside the Go tree, where internal/webui embeds it into
// hangar-server. In development Vite serves the UI itself, behind the Caddy
// in compose.yaml, which sends /api/ to the Go server on the same origin.
export default defineConfig({
  plugins: [react()],
  build: {
    outDir: '../internal/webui/dist/app',
    emptyOutDir: true,
  },
  server: {
    host: true,
    port: 5173,
    strictPort: true,
  },
})
