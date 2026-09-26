// What the main process tells the app's own pages, and what they can ask
// of it (src/preload-app.ts).

export interface Env {
  id: string
  name: string
  owner: string
  own: boolean
  template: string
  phase: string
  desired: string
  reason?: string
  progress?: { step: string; done?: number; total?: number; unit?: string; rate?: number }
}

export interface ServerInfo {
  id: string
  name: string
  url: string
  compatibility?: { ok: true; server: string } | { ok: false; why: string; update: 'server' | 'app' | null }
  signedIn: boolean
  username?: string
  ssh: boolean
  error?: string
  // open is whether the server has a window open.
  open: boolean
  environments: Env[]
}

export interface TabInfo {
  id: number
  // panel is the server's control panel, the window's first tab.
  panel: boolean
  env?: string
  title: string
  favicon?: string
  loading: boolean
  active: boolean
}

export interface AppState {
  // window and server are the bar's window and its server; null for the
  // picker.
  window: string | null
  server: string | null
  tabs: TabInfo[]
  servers: ServerInfo[]
}

export interface Bridge {
  platform: string
  get(): Promise<AppState>
  onState(fn: (s: AppState) => void): void
  activate(id: number): Promise<void>
  close(id: number): Promise<void>
  move(id: number, to: number): Promise<void>
  menu(id: number): Promise<void>
  adopt(fromWindow: string, id: number, at: number): Promise<void>
  dropped(id: number): Promise<void>
  overlay(open: boolean): Promise<void>
  onOverlay(fn: () => void): void
  addServer(url: string): Promise<{ ok?: boolean; error?: string; warning?: string }>
  removeServer(id: string): Promise<void>
  openServer(id: string): Promise<void>
  openEnv(server: string, env: string): Promise<void>
  act(server: string, env: string, action: 'start' | 'stop' | 'suspend'): Promise<void>
  vscode(server: string, env: string): Promise<void>
  terminal(server: string, env: string): Promise<string | null>
}

declare global {
  interface Window {
    hangar: Bridge
  }
}

export function tone(phase: string): string {
  if (phase === 'running') return 'dot-good'
  if (phase === 'failed') return 'dot-bad'
  if (phase === 'stopped' || phase === 'suspended') return ''
  return 'dot-busy'
}

export function esc(s: string): string {
  return s.replace(/[&<>"']/g, (c) => `&#${c.charCodeAt(0)};`)
}
