import './menu.css'

import { useCallback, useEffect, useLayoutEffect, useRef, useState, type MouseEvent as ReactMouseEvent } from 'react'
import { createPortal } from 'react-dom'

// An item runs its action and closes the menu. One with `confirm` asks
// first: the first click shows that question in its place, the second
// runs it. A heading names the items under it; `checked` marks the one in
// force, such as the current branch. `detail` says more, quietly, as a
// shortcut is shown.
export type MenuItem =
  | {
      label: string
      shortcut?: string
      detail?: string
      onSelect: () => void
      disabled?: boolean
      danger?: boolean
      confirm?: string
      checked?: boolean
    }
  | { heading: string }
  | 'separator'

export const isMac =
  typeof navigator !== 'undefined' && /Mac|iPhone|iPad/.test(navigator.platform || navigator.userAgent)

// shortcut names a key combination as the platform writes it: "Ctrl+D", or
// "⌘D" on a Mac. Mod is Ctrl, or ⌘ on a Mac.
export function shortcut(keys: string): string {
  if (!isMac) return keys.replace(/Mod/g, 'Ctrl')
  return keys
    .split('+')
    .map((k) => ({ Mod: '⌘', Ctrl: '⌃', Alt: '⌥', Shift: '⇧' })[k] ?? k)
    .join('')
}

export function copyText(text: string) {
  navigator.clipboard?.writeText(text).catch(() => {})
}

// A menu opens with its left edge at x, or its right edge for 'right'; and
// its top at y, or its bottom when it opens up, as one from a bar at the
// foot of the window does.
type Align = 'left' | 'right'

// n counts the menus opened, so each, a submenu opened in its place too,
// is placed afresh.
type Open = { x: number; y: number; items: MenuItem[]; align?: Align; up?: boolean; n: number }

let opened = 0

// useContextMenu is a menu opened at the pointer: `open` from an
// onContextMenu handler, and `menu` rendered anywhere.
export function useContextMenu() {
  const [state, setState] = useState<Open | null>(null)
  const open = useCallback((e: ReactMouseEvent | MouseEvent, items: MenuItem[]) => {
    e.preventDefault()
    e.stopPropagation()
    setState({ x: e.clientX, y: e.clientY, items, n: ++opened })
  }, [])
  // openAt opens it at a point, for a menu whose items load first or one
  // under a button, or over one.
  const openAt = useCallback(
    (x: number, y: number, items: MenuItem[], align?: Align, up?: boolean) =>
      setState({ x, y, items, align, up, n: ++opened }),
    [],
  )
  const close = useCallback(() => setState(null), [])
  const menu = state && (
    <ContextMenu
      key={state.n}
      x={state.x}
      y={state.y}
      align={state.align}
      up={state.up}
      items={state.items}
      onClose={close}
    />
  )
  return { open, openAt, close, menu }
}

// ContextMenu is a menu at a point, kept inside the window. It closes on a
// click elsewhere, Escape, scrolling or the window losing focus, and is
// worked with the arrow keys and Enter too.
export function ContextMenu({
  x,
  y,
  align = 'left',
  up = false,
  items,
  onClose,
}: {
  x: number
  y: number
  align?: Align
  up?: boolean
  items: MenuItem[]
  onClose: () => void
}) {
  const ref = useRef<HTMLDivElement>(null)
  const [pos, setPos] = useState({ left: x, top: y })
  const [asking, setAsking] = useState<number | null>(null)

  useLayoutEffect(() => {
    const el = ref.current
    if (!el) return
    const r = el.getBoundingClientRect()
    setPos({
      left: Math.max(4, Math.min(align === 'right' ? x - r.width : x, window.innerWidth - r.width - 4)),
      // Below the point, else as far up as it must go to fit; or above it.
      top: Math.max(4, Math.min(up ? y - r.height : y, window.innerHeight - r.height - 4)),
    })
    el.querySelector<HTMLButtonElement>('button:not(:disabled)')?.focus()
  }, [x, y, align, up])

  useEffect(() => {
    const away = (e: Event) => {
      if (ref.current && !ref.current.contains(e.target as Node)) onClose()
    }
    const key = (e: KeyboardEvent) => {
      if (e.key === 'Escape') {
        e.preventDefault()
        onClose()
      }
    }
    document.addEventListener('mousedown', away, true)
    document.addEventListener('contextmenu', away, true)
    document.addEventListener('scroll', away, true)
    document.addEventListener('keydown', key, true)
    window.addEventListener('blur', onClose)
    window.addEventListener('resize', onClose)
    return () => {
      document.removeEventListener('mousedown', away, true)
      document.removeEventListener('contextmenu', away, true)
      document.removeEventListener('scroll', away, true)
      document.removeEventListener('keydown', key, true)
      window.removeEventListener('blur', onClose)
      window.removeEventListener('resize', onClose)
    }
  }, [onClose])

  const move = (e: React.KeyboardEvent) => {
    if (e.key !== 'ArrowDown' && e.key !== 'ArrowUp') return
    e.preventDefault()
    const buttons = [...(ref.current?.querySelectorAll<HTMLButtonElement>('button:not(:disabled)') ?? [])]
    const i = buttons.indexOf(document.activeElement as HTMLButtonElement)
    const next = e.key === 'ArrowDown' ? (i + 1) % buttons.length : (i - 1 + buttons.length) % buttons.length
    buttons[next]?.focus()
  }

  return createPortal(
    <>
      {/* While the menu is open the page under it is still, as IntelliJ's
          is: nothing it covers lights up as the pointer crosses it, a
          click away only closes the menu, and a right-click away opens the
          menu of what was right-clicked. */}
      <div
        className="code-menu-shade"
        onMouseDown={(e) => e.preventDefault()}
        onContextMenu={(e) => {
          e.preventDefault()
          const { clientX, clientY } = e
          setTimeout(() => {
            document
              .elementFromPoint(clientX, clientY)
              ?.dispatchEvent(
                new MouseEvent('contextmenu', { bubbles: true, cancelable: true, clientX, clientY, button: 2 }),
              )
          }, 0)
        }}
      />
      <div
        ref={ref}
        className="code-menu"
        role="menu"
        style={pos}
        onKeyDown={move}
        onContextMenu={(e) => e.preventDefault()}
      >
        {items.map((item, i) =>
          item === 'separator' ? (
            <div key={i} className="code-menu-sep" role="separator" />
          ) : 'heading' in item ? (
            <div key={i} className="code-menu-heading">
              {item.heading}
            </div>
          ) : (
            <button
              key={i}
              type="button"
              role="menuitem"
              disabled={item.disabled}
              className={item.danger ? 'code-menu-danger' : undefined}
              onClick={() => {
                if (item.confirm && asking !== i) {
                  setAsking(i)
                  return
                }
                onClose()
                item.onSelect()
              }}
            >
              <span className={item.checked ? 'code-menu-checked' : undefined}>
                {asking === i ? item.confirm : item.label}
              </span>
              {item.shortcut && asking !== i && <kbd>{item.shortcut}</kbd>}
              {item.detail && asking !== i && <span className="code-menu-detail">{item.detail}</span>}
            </button>
          ),
        )}
      </div>
    </>,
    document.body,
  )
}
