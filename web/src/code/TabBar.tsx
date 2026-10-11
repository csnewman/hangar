import { Binary, ChevronDown, File, FileText, GitCompare, Image as ImageIcon, Monitor, X } from 'lucide-react'
import { useCallback, useEffect, useLayoutEffect, useRef, useState, type MouseEvent as ReactMouseEvent } from 'react'

import { useContextMenu } from './ContextMenu'
import { opensAsImage, viewNames, type FileView } from './views'

// A tab is a file, its changes, a container's logs (path is then the
// container's ID, and title its name), or the environment's desktop, which
// is connected only while its tab is the one shown. A diff is against
// HEAD, or against base (a commit, labelled baseLabel), where the file was
// named from; and of the working tree, or of a commit (to, labelled
// toLabel), as a commit's own changes are. A file opens in the view its
// content calls for, or the one chosen with Open With (view).
export type Tab = {
  kind: 'file' | 'diff' | 'logs' | 'desktop'
  path: string
  view?: FileView
  title?: string
  base?: string
  baseLabel?: string
  from?: string
  to?: string
  toLabel?: string
}

export const tabKey = (t: Tab) =>
  t.base ? `diff@${t.base}${t.to ? `..${t.to}` : ''}:${t.path}` : `${t.kind}${t.view ? `+${t.view}` : ''}:${t.path}`

export const tabName = (t: Tab) => t.title ?? t.path.split('/').pop() ?? t.path

// tabDetail tells apart tabs of one name: a file's folder, and whether it
// is its changes or a container's logs.
function tabDetail(t: Tab): string | undefined {
  const dir = t.path.includes('/') ? t.path.slice(0, t.path.lastIndexOf('/')) : ''
  if (t.kind === 'diff') return [diffWhat(t), dir].filter(Boolean).join(' · ')
  if (t.kind === 'logs') return 'logs'
  if (t.view) return [viewNames[t.view], dir].filter(Boolean).join(' · ')
  return dir || undefined
}

// diffWhat says which changes a diff tab shows.
function diffWhat(t: Tab): string {
  if (t.to) return `changes in ${t.toLabel ?? t.to}`
  return t.base ? `changes since ${t.baseLabel ?? t.base}` : 'changes'
}

// A drag of a tab along the bar: the tab, and where it would go, as the
// index among the tabs it would take.
type Drag = { key: string; to: number }

// TabBar is the editor's tabs, in one row as IntelliJ keeps them. The row
// scrolls, with the wheel too, under a thin bar that shows while the
// pointer is over it; the tabs that do not fit are listed from a button at
// its end. Tabs are dragged along it to reorder them, and the active one is
// scrolled into view whenever it is opened or chosen.
export function TabBar({
  tabs,
  active,
  opened,
  onActivate,
  onClose,
  onMove,
  onMenu,
}: {
  tabs: Tab[]
  active: string | null
  // Counts the tabs opened, the active one again included.
  opened: number
  onActivate: (key: string) => void
  onClose: (key: string) => void
  // onMove puts a tab at an index among the others.
  onMove: (key: string, to: number) => void
  onMenu: (e: ReactMouseEvent, t: Tab) => void
}) {
  const bar = useRef<HTMLDivElement>(null)
  const [thumb, setThumb] = useState<{ left: number; width: number } | null>(null)
  const [hidden, setHidden] = useState<string[]>([])
  const [drag, setDrag] = useState<Drag | null>(null)
  const list = useContextMenu()

  // measure finds whether the row overflows: the thumb's size and place
  // if it does, and which tabs are out of view.
  const measure = useCallback(() => {
    const b = bar.current
    if (!b) return
    const { scrollWidth: sw, clientWidth: cw, scrollLeft: sl } = b
    if (sw <= cw + 1) {
      setThumb(null)
      setHidden([])
      return
    }
    const width = Math.max(24, (cw * cw) / sw)
    setThumb({ left: (sl / (sw - cw)) * (cw - width), width })
    const box = b.getBoundingClientRect()
    const out: string[] = []
    b.querySelectorAll<HTMLElement>('[role=tab]').forEach((el) => {
      const r = el.getBoundingClientRect()
      if (r.left < box.left - 1 || r.right > box.left + cw + 1) out.push(el.dataset.key!)
    })
    setHidden((h) => (h.length === out.length && h.every((k, i) => k === out[i]) ? h : out))
  }, [])

  useLayoutEffect(measure, [measure, tabs])
  useEffect(() => {
    const b = bar.current
    if (!b) return
    const ro = new ResizeObserver(measure)
    ro.observe(b)
    return () => ro.disconnect()
  }, [measure])

  // The active tab is scrolled into the row's view whenever a tab is
  // opened, chosen or opened again, by the row alone, so nothing around
  // it moves.
  useEffect(() => {
    const b = bar.current
    const el = b?.querySelector<HTMLElement>('[aria-selected=true]')
    if (!b || !el) return
    const box = b.getBoundingClientRect()
    const r = el.getBoundingClientRect()
    if (r.left < box.left) b.scrollLeft -= box.left - r.left
    else if (r.right > box.left + b.clientWidth) b.scrollLeft += r.right - box.left - b.clientWidth
    // Again once the row overflows, the button taking room from it.
  }, [active, tabs.length, opened, hidden.length > 0])

  // A tab pressed and moved a few pixels is dragged; one that does not
  // move is left to be a click. Near either end of the row it scrolls.
  const press = (e: React.PointerEvent, key: string) => {
    const b = bar.current
    if (e.button !== 0 || !b || (e.target as HTMLElement).closest('button')) return
    const x0 = e.clientX
    let to: number | null = null
    let x = x0
    let edge: number | undefined
    const place = () => {
      const els = [...b.querySelectorAll<HTMLElement>('[role=tab]')].filter((el) => el.dataset.key !== key)
      const i = els.findIndex((el) => {
        const r = el.getBoundingClientRect()
        return x < r.left + r.width / 2
      })
      to = i < 0 ? els.length : i
      setDrag({ key, to })
    }
    const scroll = () => {
      const box = b.getBoundingClientRect()
      const by = x < box.left + 30 ? -12 : x > box.right - 30 ? 12 : 0
      if (by) {
        b.scrollLeft += by
        place()
      }
    }
    const move = (ev: PointerEvent) => {
      x = ev.clientX
      if (to === null && Math.abs(x - x0) < 5) return
      if (to === null) edge = window.setInterval(scroll, 30)
      place()
    }
    const up = () => {
      document.removeEventListener('pointermove', move, true)
      document.removeEventListener('pointerup', up, true)
      clearInterval(edge)
      if (to === null) return
      setDrag(null)
      onMove(key, to)
      // The click that ends a drag is not a click on the tab.
      const swallow = (c: MouseEvent) => {
        c.stopPropagation()
        c.preventDefault()
      }
      document.addEventListener('click', swallow, {
        capture: true,
        once: true,
      })
      setTimeout(() => document.removeEventListener('click', swallow, true), 0)
    }
    document.addEventListener('pointermove', move, true)
    document.addEventListener('pointerup', up, true)
  }

  // The thumb is dragged as a scroll bar's is.
  const pressThumb = (e: React.PointerEvent) => {
    const b = bar.current
    if (e.button !== 0 || !b || !thumb) return
    e.preventDefault()
    const x0 = e.clientX
    const s0 = b.scrollLeft
    const per = (b.scrollWidth - b.clientWidth) / (b.clientWidth - thumb.width)
    const move = (ev: PointerEvent) => {
      b.scrollLeft = s0 + (ev.clientX - x0) * per
    }
    const up = () => {
      document.removeEventListener('pointermove', move, true)
      document.removeEventListener('pointerup', up, true)
    }
    document.addEventListener('pointermove', move, true)
    document.addEventListener('pointerup', up, true)
  }

  const showHidden = (e: ReactMouseEvent) => {
    const r = e.currentTarget.getBoundingClientRect()
    list.openAt(
      r.right,
      r.bottom + 2,
      tabs
        .filter((t) => hidden.includes(tabKey(t)))
        .map((t) => ({
          label: tabName(t),
          detail: tabDetail(t),
          checked: tabKey(t) === active,
          onSelect: () => onActivate(tabKey(t)),
        })),
      'right',
    )
  }

  // Where the dragged tab would go, shown as a line before a tab or after
  // the last.
  const others = drag ? tabs.filter((t) => tabKey(t) !== drag.key) : []
  const dropBefore = drag && drag.to < others.length ? tabKey(others[drag.to]) : null
  const dropAfter = drag && drag.to >= others.length && others.length ? tabKey(others[others.length - 1]) : null

  return (
    <div className={drag ? 'code-tabbar code-tabbar-dragging' : 'code-tabbar'}>
      <div className="code-tabs-scroll">
        <div
          className="code-tabs"
          role="tablist"
          ref={bar}
          onScroll={measure}
          onWheel={(e) => {
            if (bar.current && Math.abs(e.deltaY) > Math.abs(e.deltaX)) bar.current.scrollLeft += e.deltaY
          }}
        >
          {tabs.map((t) => {
            const key = tabKey(t)
            const name = tabName(t)
            const cls = ['code-tab']
            if (key === active) cls.push('code-tab-on')
            if (drag?.key === key) cls.push('code-tab-dragged')
            if (key === dropBefore) cls.push('code-tab-drop-before')
            if (key === dropAfter) cls.push('code-tab-drop-after')
            return (
              <div
                key={key}
                data-key={key}
                role="tab"
                aria-selected={key === active}
                className={cls.join(' ')}
                title={
                  t.kind === 'diff'
                    ? `${t.path}: ${diffWhat(t)}`
                    : t.kind === 'logs'
                      ? `Logs of ${name}`
                      : t.view
                        ? `${t.path} in the ${viewNames[t.view]}`
                        : t.path
                }
                onPointerDown={(e) => press(e, key)}
                onClick={() => onActivate(key)}
                onContextMenu={(e) => onMenu(e, t)}
                onAuxClick={(e) => {
                  if (e.button === 1) onClose(key)
                }}
              >
                {t.kind === 'diff' ? (
                  <GitCompare size={13} className="code-icon-diff" />
                ) : t.kind === 'logs' ? (
                  <FileText size={13} className="code-icon-file" />
                ) : t.kind === 'desktop' ? (
                  <Monitor size={13} className="code-icon-file" />
                ) : t.view === 'hex' ? (
                  <Binary size={13} className="code-icon-file" />
                ) : t.view === 'image' || (!t.view && opensAsImage(t.path)) ? (
                  <ImageIcon size={13} className="code-icon-file" />
                ) : (
                  <File size={13} className="code-icon-file" />
                )}
                <span>{name}</span>
                <button
                  type="button"
                  className="code-tab-close"
                  aria-label={`Close ${name}`}
                  onClick={(e) => {
                    e.stopPropagation()
                    onClose(key)
                  }}
                >
                  <X size={12} />
                </button>
              </div>
            )
          })}
        </div>
        {thumb && (
          <div
            className="code-tabs-thumb"
            style={{ left: thumb.left, width: thumb.width }}
            onPointerDown={pressThumb}
          />
        )}
      </div>
      {hidden.length > 0 && (
        <button
          type="button"
          className="code-tabs-hidden"
          title={`Show hidden tabs (${hidden.length})`}
          onClick={showHidden}
        >
          <ChevronDown size={13} />
          <span>{hidden.length}</span>
        </button>
      )}
      {list.menu}
    </div>
  )
}
