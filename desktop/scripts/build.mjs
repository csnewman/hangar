// Bundles the app: the main process and its preloads for Node, the
// window's own pages for the browser. Everything lands in dist/.
import { build } from 'esbuild'
import { cpSync, mkdirSync } from 'node:fs'

const common = { bundle: true, sourcemap: true, logLevel: 'warning', target: 'es2023' }

await Promise.all([
  build({ ...common, entryPoints: ['src/main.ts'], outfile: 'dist/main.js', platform: 'node', format: 'cjs', external: ['electron'] }),
  build({ ...common, entryPoints: ['src/preload-app.ts'], outfile: 'dist/preload-app.js', platform: 'node', format: 'cjs', external: ['electron'] }),
  build({ ...common, entryPoints: ['src/preload-page.ts'], outfile: 'dist/preload-page.js', platform: 'node', format: 'cjs', external: ['electron'] }),
  build({ ...common, entryPoints: ['src/ui/tabs.ts', 'src/ui/home.ts'], outdir: 'dist/ui', platform: 'browser', format: 'esm' }),
])
mkdirSync('dist/ui', { recursive: true })
for (const f of ['tabs.html', 'home.html', 'app.css']) cpSync(`src/ui/${f}`, `dist/ui/${f}`)
cpSync('assets', 'dist/assets', { recursive: true })
