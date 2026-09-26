// What the app remembers between runs: the servers it knows, and each
// window that was open -- its server, its environment tabs and where it was
// on screen. It is a JSON file in the app's data directory, written
// whole whenever it changes.

import { app } from 'electron'
import { randomUUID } from 'node:crypto'
import { mkdirSync, readFileSync, renameSync, writeFileSync } from 'node:fs'
import { dirname, join } from 'node:path'

export interface Server {
  id: string
  // url is the server's origin: https://hangar.example.com.
  url: string
  // name is how the app shows it: its host, unless renamed.
  name: string
}

export interface Bounds {
  x: number
  y: number
  width: number
  height: number
}

// SavedWindow is one window of a server: the URL of each environment tab,
// in order, which tab was in hand (0 is the control panel), and whether it
// was closed rather than left open when the app quit. A server's last
// window is kept when closed, so opening the server again brings its tabs
// back.
export interface SavedWindow {
  id: string
  server: string
  tabs: string[]
  active: number
  bounds?: Bounds
  closed?: boolean
}

interface State {
  servers: Server[]
  windows: SavedWindow[]
}

const file = () => join(app.getPath('userData'), 'state.json')

let state: State = { servers: [], windows: [] }

export function load() {
  try {
    const s = JSON.parse(readFileSync(file(), 'utf8')) as Partial<State>
    state = {
      servers: Array.isArray(s.servers) ? s.servers : [],
      windows: Array.isArray(s.windows) ? s.windows : [],
    }
  } catch {
    // First run, or a file that cannot be read: start empty.
  }
}

function save() {
  const f = file()
  mkdirSync(dirname(f), { recursive: true })
  writeFileSync(f + '.tmp', JSON.stringify(state, null, 2))
  renameSync(f + '.tmp', f)
}

export function servers(): Server[] {
  return state.servers
}

export function server(id: string | null | undefined): Server | undefined {
  return state.servers.find((s) => s.id === id)
}

export function serverFor(url: string): Server | undefined {
  try {
    const origin = new URL(url).origin
    return state.servers.find((s) => s.url === origin)
  } catch {
    return undefined
  }
}

export function addServer(url: string, name: string): Server {
  const existing = serverFor(url)
  if (existing) return existing
  const s = { id: randomUUID(), url, name }
  state.servers.push(s)
  save()
  return s
}

export function removeServer(id: string) {
  state.servers = state.servers.filter((s) => s.id !== id)
  state.windows = state.windows.filter((w) => w.server !== id)
  save()
}

export function savedWindows(): SavedWindow[] {
  return state.windows.filter((w) => w.id && server(w.server))
}

// closedWindow is the window a server was last left with, to open it as it
// was.
export function closedWindow(serverId: string): SavedWindow | undefined {
  return state.windows.find((w) => w.server === serverId && w.closed)
}

// saveWindow records a window as it is: to reopen next run when open, and
// as its server's closed window otherwise.
export function saveWindow(w: SavedWindow, open: boolean) {
  state.windows = state.windows.filter((x) => x.id !== w.id && !(!open && x.server === w.server && x.closed))
  state.windows.push({ ...w, closed: !open })
  save()
}

// forgetWindow drops a window that closed while its server has another.
export function forgetWindow(id: string) {
  state.windows = state.windows.filter((x) => x.id !== id)
  save()
}

export function reopenable(): SavedWindow[] {
  return savedWindows().filter((w) => !w.closed)
}
