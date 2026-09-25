// What the app remembers between runs: the servers it knows and the tabs
// that were open. It is a JSON file in the app's data directory, written
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

export interface SavedTab {
  // server is the id of the server the tab shows, or null for the app's own
  // home page.
  server: string | null
  url: string
}

interface State {
  servers: Server[]
  tabs: SavedTab[]
  active: number
  bounds?: { x: number; y: number; width: number; height: number }
}

const file = () => join(app.getPath('userData'), 'state.json')

let state: State = { servers: [], tabs: [], active: 0 }

export function load() {
  try {
    const s = JSON.parse(readFileSync(file(), 'utf8')) as Partial<State>
    state = {
      servers: Array.isArray(s.servers) ? s.servers : [],
      tabs: Array.isArray(s.tabs) ? s.tabs : [],
      active: typeof s.active === 'number' ? s.active : 0,
      bounds: s.bounds,
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

export function server(id: string | null): Server | undefined {
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

export function renameServer(id: string, name: string) {
  const s = server(id)
  if (s) {
    s.name = name
    save()
  }
}

export function removeServer(id: string) {
  state.servers = state.servers.filter((s) => s.id !== id)
  state.tabs = state.tabs.filter((t) => t.server !== id)
  save()
}

export function savedTabs(): { tabs: SavedTab[]; active: number } {
  return { tabs: state.tabs, active: state.active }
}

export function saveTabs(tabs: SavedTab[], active: number) {
  state.tabs = tabs
  state.active = active
  save()
}

export function bounds() {
  return state.bounds
}

export function saveBounds(b: State['bounds']) {
  state.bounds = b
  save()
}
