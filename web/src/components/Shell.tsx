import { useEffect, useState } from 'react'
import { Outlet, useMatch } from 'react-router'

import { desktop } from '../desktop'
import { usePhone } from '../phone'
import { homeTab, nameTab } from '../workspaceLinks'
import { PhoneTabs } from './PhoneTabs'
import { Sidebar } from './Sidebar'

const collapsedKey = 'hangar.sidebar.collapsed'
const width = 220
const collapsedWidth = 52

function stored(key: string): string | null {
  try {
    return localStorage.getItem(key)
  } catch {
    return null
  }
}

function store(key: string, value: string) {
  try {
    localStorage.setItem(key, value)
  } catch {
    // A browser that keeps nothing starts from the defaults each time.
  }
}

// Shell is the frame every signed-in page sits in: Hangar's sections down
// the left, folded to their icons if this browser was asked to, and the page
// beside them. An environment's workspace has the whole page, in a tab of its
// own, and its status bar leads back here. On a phone the sections are a bar
// of tabs at the foot instead, left out on an environment's own screens,
// which have their way back at the top.
export function Shell() {
  const phone = usePhone()
  const environment = useMatch('/environments/:id/*')
  const creating = useMatch('/environments/new')
  const inEnvironment = environment !== null && creating === null
  const [collapsed, setCollapsed] = useState(() => stored(collapsedKey) === 'true')

  const toggle = () =>
    setCollapsed((c) => {
      store(collapsedKey, String(!c))
      return !c
    })

  useEffect(() => {
    if (!inEnvironment && !phone) nameTab(homeTab)
  }, [inEnvironment, phone])

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === 'b' && !e.shiftKey && !e.altKey) {
        // The terminal and editors take the keyboard for themselves.
        const t = e.target as HTMLElement | null
        if (t?.closest('.xterm, .cm-editor, iframe, input, textarea')) return
        e.preventDefault()
        toggle()
      }
    }
    window.addEventListener('keydown', onKey)
    return () => window.removeEventListener('keydown', onKey)
  }, [])

  // An environment's workspace is that environment and nothing else: the
  // browser's tabs, or the desktop app's, move between environments, and
  // the tab with Hangar's own pages keeps the sidebar.
  if (desktop?.tab === 'environment' || (inEnvironment && !phone)) {
    return (
      <div className="app">
        <main className="main">
          <Outlet />
        </main>
      </div>
    )
  }

  if (phone) {
    return (
      <div className="app app-phone">
        <main className="main">
          <Outlet />
        </main>
        {!inEnvironment && <PhoneTabs />}
      </div>
    )
  }

  return (
    <div className="app">
      <div
        className="app-body"
        style={{ gridTemplateColumns: `${collapsed ? collapsedWidth : width}px minmax(0, 1fr)` }}
      >
        <Sidebar collapsed={collapsed} onToggle={toggle} />
        <main className="main">
          <Outlet />
        </main>
      </div>
    </div>
  )
}
