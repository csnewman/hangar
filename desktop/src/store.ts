// What the app remembers between runs: the servers it knows, and for each
// server whose window was open, that window's environment tabs and where it
// was on screen. It is a JSON file in the app's data directory, written
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

// SavedWindow is a server's window: the URL of each environment tab, in
// order, which tab was in hand (0 is the control panel), and whether it was
// closed rather than left open when the app quit.
export interface SavedWindow {
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
  return state.windows.filter((w) => server(w.server))
}

export function savedWindow(serverId: string): SavedWindow | undefined {
  return state.windows.find((w) => w.server === serverId)
}

// saveWindow records a server's window as it is. open says whether it is
// to be reopened next run; its tabs and place are kept either way, for the
// next time it is opened.
export function saveWindow(w: SavedWindow, open: boolean) {
  state.windows = state.windows.filter((x) => x.server !== w.server)
  state.windows.push({ ...w, closed: !open })
  save()
}

export function reopenable(): SavedWindow[] {
  return savedWindows().filter((w) => !w.closed)
}
