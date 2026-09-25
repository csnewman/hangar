// The app's side of Hangar's client API (/api/v1). Only the app's own parts
// -- the menu bar, notifications, the switcher, opening VS Code or a
// terminal -- call it; each server's control panel is the server's own web
// UI, shown in a tab, and moves with the server.
//
// Calls go through the server's session, the same one its tabs use, so
// signing in on a tab signs the app in: there is no token to keep.

import { net, session } from 'electron'

import type { Server } from './store'

// apiVersion is the version of the client API this app speaks.
export const apiVersion = 'v1'

// partition is where a server's cookies live: one per server, shared by its
// tabs, kept across runs.
export const partition = (s: Server) => `persist:server-${s.id}`

export interface Environment {
  id: string
  name: string
  owner_id: string
  owner: string
  template: string
  phase: string
  desired: string
  reason?: string
  progress?: { step: string; done?: number; total?: number; unit?: string; rate?: number }
  editor_path?: string
  display?: string
  updated_at: string
}

export interface Me {
  id: string
  username: string
  display_name: string
  admin: boolean
  ssh?: { host: string; port: number; host_key: string }
}

// Compatibility is whether the app and a server can work together, and if
// not, which of the two is behind.
export type Compatibility =
  | { ok: true; server: string }
  | { ok: false; why: string; update: 'server' | 'app' | null }

// checkServer asks a server which client API versions it serves. It needs
// no sign-in.
export async function checkServer(url: string): Promise<Compatibility> {
  let res: Response
  try {
    res = await net.fetch(`${url}/api/${apiVersion}/version`, { cache: 'no-store' })
  } catch (err) {
    return { ok: false, why: `cannot be reached: ${(err as Error).message}`, update: null }
  }
  if (res.status === 404) {
    return {
      ok: false,
      why: `serves no /api/${apiVersion}, so the menu bar and notifications cannot follow it. Update the server.`,
      update: 'server',
    }
  }
  if (!res.ok) return { ok: false, why: `answered ${res.status}`, update: null }
  let v: { versions?: string[]; server?: string }
  try {
    v = await res.json()
  } catch {
    return { ok: false, why: 'is not a Hangar server', update: null }
  }
  if (!v.versions?.includes(apiVersion)) {
    return {
      ok: false,
      why: `does not serve the API this app speaks (${apiVersion}; it serves ${v.versions?.join(', ')}). Update the app.`,
      update: 'app',
    }
  }
  return { ok: true, server: v.server ?? '' }
}

// SignedOut is a call made with no session, or an expired one.
export class SignedOut extends Error {
  constructor() {
    super('not signed in')
  }
}

export async function call<T>(s: Server, method: string, path: string): Promise<T> {
  const ses = session.fromPartition(partition(s))
  const res = await ses.fetch(`${s.url}/api/${apiVersion}${path}`, {
    method,
    credentials: 'include',
    cache: 'no-store',
  })
  if (res.status === 401) throw new SignedOut()
  const text = await res.text()
  let data: unknown
  try {
    data = text ? JSON.parse(text) : undefined
  } catch {
    data = undefined
  }
  if (!res.ok) throw new Error((data as { error?: string } | undefined)?.error ?? `${res.status} ${res.statusText}`)
  return data as T
}

export const me = (s: Server) => call<Me>(s, 'GET', '/me')
export const environments = (s: Server) => call<Environment[]>(s, 'GET', '/environments')
export const act = (s: Server, id: string, action: 'start' | 'stop' | 'suspend') =>
  call<Environment>(s, 'POST', `/environments/${encodeURIComponent(id)}/${action}`)
