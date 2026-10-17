import { useEffect, useRef, useState } from 'react'

import { desktop } from './desktop'
import { usePhone } from './phone'

// In a browser on a computer each environment's workspace is a browser tab
// of its own, and Hangar's own pages another, each named so that a link to
// one goes to the tab already showing it rather than opening a second. The
// desktop app and a phone keep their own ways of moving between them.
const separate = () => !desktop

export const homeTab = 'hangar-home'

export const environmentTab = (id: string) => `hangar-env-${id}`

// useEnvironmentTarget gives the target of a link to an environment's
// workspace: its own tab, where there is one.
export function useEnvironmentTarget(): (id: string) => string | undefined {
  const phone = usePhone()
  return (id) => (separate() && !phone ? environmentTab(id) : undefined)
}

// nameTab names this browser tab, so links to what it shows come back
// to it.
export function nameTab(name: string) {
  if (separate()) window.name = name
}

// otherTab is the target of a link from a workspace to another tab: the
// one named, or in the desktop app a new one, which the app moves to the
// tab already showing it.
export const otherTab = (name: string) => (separate() ? name : '_blank')

const workspacePath = (id: string) => `/environments/${id}`

// openEnvironment shows an environment's workspace, where it is called in a
// click: the tab already showing it, brought forward without reloading it,
// or a new tab. show, if given, is what the workspace opens on, such as its
// Environment tab.
export function openEnvironment(id: string, show?: WorkspaceShow) {
  const url = workspacePath(id) + (show ? `?show=${show}` : '')
  if (!separate()) {
    window.open(url, '_blank')
    return
  }
  const w = window.open('', environmentTab(id))
  if (!w) {
    window.open(url, environmentTab(id))
    return
  }
  let blank = true
  try {
    blank = w.location.href === 'about:blank'
  } catch {
    // A tab of that name on another origin is replaced.
  }
  if (blank) w.location.href = url
  else if (show) tellWorkspace(id, show)
  w.focus()
}

// What a workspace can be asked to show, when it opens or while it is open.
export type WorkspaceShow = 'environment'

type TabMessage =
  { type: 'who' } | { type: 'here' | 'gone'; id: string } | { type: 'show'; id: string; show: WorkspaceShow }

// Browser tabs of this origin tell each other which workspaces they show,
// so Hangar's own pages can say an environment is open in a tab.
const channel = typeof BroadcastChannel === 'undefined' ? null : new BroadcastChannel('hangar-tabs')

function tellWorkspace(id: string, show: WorkspaceShow) {
  channel?.postMessage({ type: 'show', id, show } satisfies TabMessage)
}

// useWorkspaceTab is called by the workspace of environment id: it says so
// to the other tabs, whenever asked, until it closes, and passes on what
// they ask it to show.
export function useWorkspaceTab(id: string, onShow: (show: WorkspaceShow) => void) {
  const handler = useRef(onShow)
  useEffect(() => {
    handler.current = onShow
  })
  useEffect(() => {
    if (!channel) return
    const here = () => channel.postMessage({ type: 'here', id } satisfies TabMessage)
    const gone = () => channel.postMessage({ type: 'gone', id } satisfies TabMessage)
    const onMessage = (e: MessageEvent<TabMessage>) => {
      if (e.data.type === 'who') here()
      else if (e.data.type === 'show' && e.data.id === id) handler.current(e.data.show)
    }
    here()
    channel.addEventListener('message', onMessage)
    window.addEventListener('pagehide', gone)
    return () => {
      channel.removeEventListener('message', onMessage)
      window.removeEventListener('pagehide', gone)
      gone()
    }
  }, [id])
}

// useOpenWorkspaces is the environments whose workspaces are open in other
// tabs of this browser.
export function useOpenWorkspaces(): ReadonlySet<string> {
  const [open, setOpen] = useState<ReadonlySet<string>>(() => new Set())
  useEffect(() => {
    if (!channel) return
    const onMessage = (e: MessageEvent<TabMessage>) => {
      const m = e.data
      if (m.type === 'here') setOpen((s) => (s.has(m.id) ? s : new Set(s).add(m.id)))
      else if (m.type === 'gone')
        setOpen((s) => {
          if (!s.has(m.id)) return s
          const next = new Set(s)
          next.delete(m.id)
          return next
        })
    }
    channel.addEventListener('message', onMessage)
    channel.postMessage({ type: 'who' } satisfies TabMessage)
    return () => channel.removeEventListener('message', onMessage)
  }, [])
  return open
}
