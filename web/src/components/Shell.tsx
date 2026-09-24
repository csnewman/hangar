import { PanelLeftClose, PanelLeftOpen } from 'lucide-react'
import { useEffect, useRef, useState, type PointerEvent as ReactPointerEvent } from 'react'
import { Outlet } from 'react-router'

import { Logo } from './Logo'
import { Sidebar } from './Sidebar'
import { UserMenu } from './UserMenu'

const widthKey = 'hangar.sidebar.width'
const collapsedKey = 'hangar.sidebar.collapsed'
const defaultWidth = 252
const minWidth = 180
const maxWidth = 480
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

// Shell is the frame every signed-in page sits in: a bar across the top, the
// inventory down the left, and the page beside it. The inventory is as wide
// as it is dragged, or folded to its icons, and this browser remembers which.
export function Shell() {
  const [width, setWidth] = useState(() => {
    const w = Number(stored(widthKey))
    return w >= minWidth && w <= maxWidth ? w : defaultWidth
  })
  const [collapsed, setCollapsed] = useState(() => stored(collapsedKey) === 'true')
  const [dragging, setDragging] = useState(false)
  const start = useRef<{ x: number; width: number } | null>(null)

  const toggle = () =>
    setCollapsed((c) => {
      store(collapsedKey, String(!c))
      return !c
    })

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

  const onPointerDown = (e: ReactPointerEvent<HTMLDivElement>) => {
    e.preventDefault()
    e.currentTarget.setPointerCapture(e.pointerId)
    start.current = { x: e.clientX, width }
    setDragging(true)
  }
  const onPointerMove = (e: ReactPointerEvent<HTMLDivElement>) => {
    if (!start.current) return
    setWidth(Math.min(maxWidth, Math.max(minWidth, start.current.width + e.clientX - start.current.x)))
  }
  const onPointerUp = () => {
    if (!start.current) return
    start.current = null
    setDragging(false)
    setWidth((w) => {
      store(widthKey, String(w))
      return w
    })
  }
  const reset = () => {
    setWidth(defaultWidth)
    store(widthKey, String(defaultWidth))
  }

  return (
    <div className={dragging ? 'app app-dragging' : 'app'}>
      <header className="topbar">
        <button
          type="button"
          className="icon-btn sidebar-toggle"
          onClick={toggle}
          title={collapsed ? 'Show the sidebar (Ctrl+B)' : 'Fold the sidebar (Ctrl+B)'}
          aria-label={collapsed ? 'Show the sidebar' : 'Fold the sidebar'}
        >
          {collapsed ? <PanelLeftOpen size={17} /> : <PanelLeftClose size={17} />}
        </button>
        <div className="brand">
          <Logo />
          <span>Hangar</span>
        </div>
        <div className="topbar-spacer" />
        <UserMenu />
      </header>
      <div
        className="app-body"
        style={{ gridTemplateColumns: `${collapsed ? collapsedWidth : width}px minmax(0, 1fr)` }}
      >
        <div className="sidebar-frame">
          <Sidebar collapsed={collapsed} />
          {!collapsed && (
            <div
              className="sidebar-handle"
              role="separator"
              aria-orientation="vertical"
              aria-label="Resize the sidebar"
              title="Drag to resize; double-click to reset"
              onPointerDown={onPointerDown}
              onPointerMove={onPointerMove}
              onPointerUp={onPointerUp}
              onPointerCancel={onPointerUp}
              onDoubleClick={reset}
            />
          )}
        </div>
        <main className="main">
          <Outlet />
        </main>
      </div>
    </div>
  )
}
