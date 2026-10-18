import './menu.css'

import { ChevronRight } from 'lucide-react'
import {
  useCallback,
  useEffect,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
  type MouseEvent as ReactMouseEvent,
} from 'react'
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
      // onSelect runs the item; one with a submenu opens it, its items
      // given, or read when it opens.
      onSelect?: () => void
      submenu?: MenuItem[] | (() => Promise<MenuItem[]>)
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
// worked with the arrow keys and Enter too. An item with a submenu opens
// it beside itself, as IntelliJ's do: on the pointer resting on it, a
// click, or the right arrow (or Enter); the pointer passing on to another
// item closes it after a moment, time enough to cross to it, and the left
// arrow or Escape goes back from it. The item stays lit while its submenu
// is open.
export function ContextMenu({
  x,
  y,
  align = 'left',
  up = false,
  items,
  onClose,
  parent,
  focus = true,
}: {
  // focus moves the keyboard to the first item: always for a menu, for a
  // submenu only when opened from the keyboard.
  focus?: boolean
  x: number
  y: number
  align?: Align
  up?: boolean
  items: MenuItem[]
  onClose: () => void
  // parent is the item a submenu is open from: where it is, to open beside
  // it, and the way back to it.
  parent?: { rect: DOMRect; onBack: () => void; onEnter: () => void }
}) {
  const ref = useRef<HTMLDivElement>(null)
  const [pos, setPos] = useState({ left: x, top: y })
  const [asking, setAsking] = useState<number | null>(null)
  // The submenu open, from which item, with its items once they are read.
  const [sub, setSub] = useState<{ index: number; rect: DOMRect; items: MenuItem[]; focus: boolean } | null>(null)
  const timer = useRef<ReturnType<typeof setTimeout> | undefined>(undefined)
  const asked = useRef(0)

  useLayoutEffect(() => {
    const el = ref.current
    if (!el) return
    const r = el.getBoundingClientRect()
    if (parent) {
      // Beside its item, overlapping the menu's edge, else on the other
      // side where it does not fit; its first item level with the item.
      const right = parent.rect.right - 2
      const left = right + r.width > window.innerWidth - 4 ? parent.rect.left - r.width + 2 : right
      setPos({
        left: Math.max(4, left),
        top: Math.max(4, Math.min(parent.rect.top - 5, window.innerHeight - r.height - 4)),
      })
    } else {
      setPos({
        left: Math.max(4, Math.min(align === 'right' ? x - r.width : x, window.innerWidth - r.width - 4)),
        // Below the point, else as far up as it must go to fit; or above it.
        top: Math.max(4, Math.min(up ? y - r.height : y, window.innerHeight - r.height - 4)),
      })
    }
    if (focus) el.querySelector<HTMLButtonElement>('button:not(:disabled)')?.focus()
  }, [x, y, align, up, parent, focus])

  // The outermost menu closes them all on what happens away from them.
  useEffect(() => {
    if (parent) return
    const away = (e: Event) => {
      if (!(e.target instanceof Element) || !e.target.closest('.code-menu')) onClose()
    }
    document.addEventListener('mousedown', away, true)
    document.addEventListener('contextmenu', away, true)
    document.addEventListener('scroll', away, true)
    window.addEventListener('blur', onClose)
    window.addEventListener('resize', onClose)
    return () => {
      document.removeEventListener('mousedown', away, true)
      document.removeEventListener('contextmenu', away, true)
      document.removeEventListener('scroll', away, true)
      window.removeEventListener('blur', onClose)
      window.removeEventListener('resize', onClose)
    }
  }, [onClose, parent])
  useEffect(() => () => clearTimeout(timer.current), [])

  // openSub opens an item's submenu, reading its items first if they are
  // read, and moves the keyboard into it if asked.
  const openSub = (index: number, el: HTMLElement, focus: boolean) => {
    clearTimeout(timer.current)
    const item = items[index]
    if (typeof item !== 'object' || !('label' in item) || !item.submenu) return
    if (sub?.index === index) {
      if (focus && !sub.focus) setSub({ ...sub, focus: true })
      return
    }
    const rect = el.getBoundingClientRect()
    const n = ++asked.current
    const got = item.submenu
    if (Array.isArray(got)) {
      setSub({ index, rect, items: got, focus })
      return
    }
    setSub({ index, rect, items: [{ heading: 'Reading…' }], focus: false })
    got().then(
      (list) => n === asked.current && setSub({ index, rect, items: list, focus }),
      (e: unknown) =>
        n === asked.current &&
        setSub({ index, rect, items: [{ heading: e instanceof Error ? e.message : String(e) }], focus: false }),
    )
  }
  // closeSub closes the submenu after a moment, unless the pointer gets
  // to it first.
  const closeSubSoon = () => {
    clearTimeout(timer.current)
    if (sub) timer.current = setTimeout(() => setSub(null), 300)
  }

  const buttons = () => [...(ref.current?.querySelectorAll<HTMLButtonElement>(':scope > button:not(:disabled)') ?? [])]
  const onKeyDown = (e: React.KeyboardEvent) => {
    const at = buttons().indexOf(document.activeElement as HTMLButtonElement)
    if (e.key === 'ArrowDown' || e.key === 'ArrowUp') {
      e.preventDefault()
      const list = buttons()
      const next = e.key === 'ArrowDown' ? (at + 1) % list.length : (at - 1 + list.length) % list.length
      list[next]?.focus()
      setSub(null)
    } else if (e.key === 'ArrowRight') {
      const el = document.activeElement as HTMLElement
      const index = Number(el?.dataset.index)
      if (el && !Number.isNaN(index)) {
        e.preventDefault()
        openSub(index, el, true)
      }
    } else if (e.key === 'ArrowLeft' && parent) {
      e.preventDefault()
      parent.onBack()
    } else if (e.key === 'Escape') {
      e.preventDefault()
      if (parent) parent.onBack()
      else onClose()
    } else return
    e.stopPropagation()
  }

  return createPortal(
    <>
      {/* While the menu is open the page under it is still, as IntelliJ's
          is: nothing it covers lights up as the pointer crosses it, a
          click away only closes the menu, and a right-click away opens the
          menu of what was right-clicked. */}
      {!parent && (
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
      )}
      <div
        ref={ref}
        className="code-menu"
        role="menu"
        style={pos}
        onKeyDown={onKeyDown}
        onMouseEnter={() => parent?.onEnter()}
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
              data-index={i}
              aria-haspopup={item.submenu ? 'menu' : undefined}
              aria-expanded={item.submenu ? sub?.index === i : undefined}
              disabled={item.disabled}
              className={[item.danger && 'code-menu-danger', sub?.index === i && 'code-menu-open']
                .filter(Boolean)
                .join(' ')}
              onMouseEnter={(e) => {
                if (item.submenu && !item.disabled) {
                  const el = e.currentTarget
                  clearTimeout(timer.current)
                  timer.current = setTimeout(() => openSub(i, el, false), sub ? 120 : 60)
                } else closeSubSoon()
              }}
              onClick={(e) => {
                if (item.submenu) {
                  openSub(i, e.currentTarget, true)
                  return
                }
                if (item.confirm && asking !== i) {
                  setAsking(i)
                  return
                }
                onClose()
                item.onSelect?.()
              }}
              onKeyDown={(e) => {
                if (item.submenu && e.key === 'Enter') {
                  e.preventDefault()
                  e.stopPropagation()
                  openSub(i, e.currentTarget, true)
                }
              }}
            >
              <span className={item.checked ? 'code-menu-checked' : undefined}>
                {asking === i ? item.confirm : item.label}
              </span>
              {item.shortcut && asking !== i && <kbd>{item.shortcut}</kbd>}
              {item.detail && asking !== i && <span className="code-menu-detail">{item.detail}</span>}
              {item.submenu && <ChevronRight size={13} className="code-menu-arrow" aria-hidden="true" />}
            </button>
          ),
        )}
      </div>
      {sub && (
        <SubMenu
          key={sub.index}
          items={sub.items}
          rect={sub.rect}
          focus={sub.focus}
          onClose={onClose}
          onEnter={() => clearTimeout(timer.current)}
          onBack={() => {
            const index = sub.index
            setSub(null)
            ref.current?.querySelector<HTMLButtonElement>(`[data-index="${index}"]`)?.focus()
          }}
        />
      )}
    </>,
    document.body,
  )
}

// SubMenu is a submenu beside its item, the keyboard moved into it when it
// was opened from the keyboard.
function SubMenu({
  items,
  rect,
  focus,
  onClose,
  onEnter,
  onBack,
}: {
  items: MenuItem[]
  rect: DOMRect
  focus: boolean
  onClose: () => void
  onEnter: () => void
  onBack: () => void
}) {
  const parent = useMemo(() => ({ rect, onBack, onEnter }), [rect, onBack, onEnter])
  return <ContextMenu x={rect.right} y={rect.top} items={items} onClose={onClose} parent={parent} focus={focus} />
}
