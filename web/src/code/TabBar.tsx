import {
  Activity,
  Binary,
  ChevronDown,
  File,
  FileText,
  FileVideo,
  GitCompare,
  Image as ImageIcon,
  Monitor,
  Settings2,
  X,
} from 'lucide-react'
import { useCallback, useEffect, useLayoutEffect, useRef, useState, type MouseEvent as ReactMouseEvent } from 'react'
import { createPortal } from 'react-dom'

import { useContextMenu } from './ContextMenu'
import { sameTarget, tabDrag, useTabDrag, type DropTarget } from './tabDrag'
import { opensAs, viewNames, type FileView } from './views'

// A tab is a file, its changes, a container's logs (path is then the
// container's ID, and title its name), the environment's desktop, which
// is connected only while its tab is the one shown, or one of the
// environment's own pages: its details and settings (environment), and
// its usage over time and processes (performance). A diff is against
// HEAD, or against base (a commit, labelled baseLabel), where the file was
// named from; and of the working tree, or of a commit (to, labelled
// toLabel), as a commit's own changes are. A file opens in the view its
// content calls for, or the one chosen with Open With (view).
export type Tab = {
  kind: 'file' | 'diff' | 'compare' | 'logs' | 'desktop' | 'environment' | 'performance'
  path: string
  // root is the project the path is in, when it is not the tab's own
  // folder: its path is from that project's folder.
  root?: string
  // other is what a compare tab shows the file beside: another file, or
  // the clipboard (clipboard:<id>).
  other?: string
  view?: FileView
  title?: string
  base?: string
  baseLabel?: string
  from?: string
  to?: string
  toLabel?: string
}

export const tabKey = (t: Tab) =>
  (t.root ? `${t.root}|` : '') +
  (t.kind === 'compare'
    ? `compare:${t.other}|${t.path}`
    : t.base
      ? `diff@${t.base}${t.to ? `..${t.to}` : ''}:${t.path}`
      : `${t.kind}${t.view ? `+${t.view}` : ''}:${t.path}`)

export const tabName = (t: Tab) => t.title ?? t.path.split('/').pop() ?? t.path

// tabDetail tells apart tabs of one name: a file's folder, and whether it
// is its changes or a container's logs.
function tabDetail(t: Tab): string | undefined {
  const dir = t.path.includes('/') ? t.path.slice(0, t.path.lastIndexOf('/')) : ''
  if (t.kind === 'diff') return [diffWhat(t), dir].filter(Boolean).join(' · ')
  if (t.kind === 'compare')
    return `compared with ${t.other?.startsWith('clipboard:') ? 'the clipboard' : (t.other?.split('/').pop() ?? '')}`
  if (t.kind === 'logs') return 'logs'
  if (t.view) return [viewNames[t.view], dir].filter(Boolean).join(' · ')
  return dir || undefined
}

// diffWhat says which changes a diff tab shows.
function diffWhat(t: Tab): string {
  if (t.to) return `changes in ${t.toLabel ?? t.to}`
  return t.base ? `changes since ${t.baseLabel ?? t.base}` : 'changes'
}

// tabIcon is what a tab shows before its name: what kind of tab it is, and
// for a file, the view it is in.
function tabIcon(t: Tab) {
  return t.kind === 'diff' ? (
    <GitCompare size={13} className="code-icon-diff" />
  ) : t.kind === 'logs' ? (
    <FileText size={13} className="code-icon-file" />
  ) : t.kind === 'desktop' ? (
    <Monitor size={13} className="code-icon-file" />
  ) : t.kind === 'environment' ? (
    <Settings2 size={13} className="code-icon-file" />
  ) : t.kind === 'performance' ? (
    <Activity size={13} className="code-icon-file" />
  ) : t.view === 'hex' ? (
    <Binary size={13} className="code-icon-file" />
  ) : (t.view ?? opensAs(t.path)) === 'image' ? (
    <ImageIcon size={13} className="code-icon-file" />
  ) : (t.view ?? opensAs(t.path)) === 'pdf' ? (
    <FileText size={13} className="code-icon-file" />
  ) : (t.view ?? opensAs(t.path)) === 'media' ? (
    <FileVideo size={13} className="code-icon-file" />
  ) : (
    <File size={13} className="code-icon-file" />
  )
}

// DragGhost is the tab being dragged, following the pointer, over
// everything.
function DragGhost({ tab, x, y }: { tab?: Tab; x: number; y: number }) {
  if (!tab) return null
  return createPortal(
    <div className="code-drag-ghost" style={{ left: x + 12, top: y + 12 }}>
      {tabIcon(tab)}
      {tabName(tab)}
    </div>,
    document.body,
  )
}

// A drag of a tab along the bar: the tab, and where it would go, as the
// index among the tabs it would take; out when it would go elsewhere.
type Drag = { key: string; to: number; out: boolean; x: number; y: number }

// TabBar is the editor's tabs, in one row as IntelliJ keeps them. The row
// scrolls, with the wheel too, under a thin bar that shows while the
// pointer is over it; the tabs that do not fit are listed from a button at
// its end. Tabs are dragged along it to reorder them, or out of it: onto
// the other group's tabs or editor, to move there, or onto the right of
// this group's editor while it is the only one, to split it. The active one
// is scrolled into view whenever it is opened or chosen.
export function TabBar({
  tabs,
  active,
  opened,
  group = 0,
  groups = 1,
  projectLabel,
  onActivate,
  onClose,
  onMove,
  onDropOut,
  onMenu,
}: {
  tabs: Tab[]
  active: string | null
  // Counts the tabs opened, the active one again included.
  opened: number
  onActivate: (key: string) => void
  onClose: (key: string) => void
  // projectLabel names the project a tab's file is in, where there are
  // several.
  projectLabel?: (t: Tab) => string | undefined
  // group is which group of editors the bar is, of how many.
  group?: number
  groups?: number
  // onMove puts a tab at an index among the others.
  onMove: (key: string, to: number) => void
  // onDropOut puts a tab dragged out of the bar where it was dropped.
  onDropOut?: (key: string, target: DropTarget) => void
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
  // move is left to be a click. Held over either end of the row, it scrolls
  // the row.
  const press = (e: React.PointerEvent, key: string) => {
    const b = bar.current
    if (e.button !== 0 || !b || (e.target as HTMLElement).closest('button')) return
    const x0 = e.clientX
    let to: number | null = null
    let target: DropTarget | null = null
    let x = x0
    let y = e.clientY
    let edge: number | undefined
    // indexIn is where along a row of tabs the pointer would put the tab.
    const indexIn = (row: HTMLElement) => {
      const els = [...row.querySelectorAll<HTMLElement>('[role=tab]')].filter((el) => el.dataset.key !== key)
      const i = els.findIndex((el) => {
        const r = el.getBoundingClientRect()
        return x < r.left + r.width / 2
      })
      return i < 0 ? els.length : i
    }
    const place = () => {
      to = indexIn(b)
      // Out of the row: the other group's tabs or editor, or the right of
      // this one's to split it. Anywhere else it stays in this row.
      const at = document.elementFromPoint(x, y)
      const row = at?.closest<HTMLElement>('.code-tabs[data-group]')
      const area = at?.closest<HTMLElement>('[data-editor-group]')
      let next: DropTarget | null = null
      if (row && Number(row.dataset.group) !== group) next = { group: Number(row.dataset.group), index: indexIn(row) }
      else if (!row && area) {
        const g = Number(area.dataset.editorGroup)
        const body = area.querySelector('.code-editor-body')?.getBoundingClientRect()
        if (g !== group) next = { group: g }
        else if (groups === 1 && tabs.length > 1 && body && y > body.top && x > body.left + body.width * 0.6) {
          next = { split: true }
        }
      }
      if (!sameTarget(next, target)) {
        target = next
        tabDrag.set({ key, from: group, target })
      }
      setDrag({ key, to, out: !!target, x, y })
    }
    const scroll = () => {
      const box = b.getBoundingClientRect()
      // Only over the row itself: a tab taken elsewhere leaves it be.
      const over = y >= box.top - 6 && y <= box.bottom + 6 && x >= box.left - 24 && x <= box.right + 24
      const by = !over ? 0 : x < box.left + 30 ? -12 : x > box.right - 30 ? 12 : 0
      if (by) {
        b.scrollLeft += by
        place()
      }
    }
    const move = (ev: PointerEvent) => {
      x = ev.clientX
      y = ev.clientY
      if (to === null && Math.abs(x - x0) < 5 && Math.abs(y - e.clientY) < 5) return
      if (to === null) {
        edge = window.setInterval(scroll, 30)
        // A frame (an HTML file's preview) would take the pointer's moves.
        document.body.classList.add('code-tab-dragging')
      }
      place()
    }
    const up = () => {
      document.removeEventListener('pointermove', move, true)
      document.removeEventListener('pointerup', up, true)
      clearInterval(edge)
      document.body.classList.remove('code-tab-dragging')
      if (to === null) return
      setDrag(null)
      tabDrag.set(null)
      if (target) onDropOut?.(key, target)
      else onMove(key, to)
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
  // the last: one from this row, or one from the other group's coming
  // here.
  const incoming = useTabDrag()
  const coming =
    incoming && incoming.from !== group && incoming.target && 'group' in incoming.target
      ? incoming.target.group === group && incoming.target.index !== undefined
        ? { key: incoming.key, to: incoming.target.index }
        : null
      : null
  const shown = coming ?? (drag && !drag.out ? drag : null)
  const others = shown ? tabs.filter((t) => tabKey(t) !== shown.key) : []
  const dropBefore = shown && shown.to < others.length ? tabKey(others[shown.to]) : null
  const dropAfter = shown && shown.to >= others.length && others.length ? tabKey(others[others.length - 1]) : null

  return (
    <div className={drag ? 'code-tabbar code-tabbar-dragging' : 'code-tabbar'}>
      {drag && <DragGhost tab={tabs.find((t) => tabKey(t) === drag.key)} x={drag.x} y={drag.y} />}
      <div className="code-tabs-scroll">
        <div
          className="code-tabs"
          role="tablist"
          data-group={group}
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
            if (t.kind === 'file' && t.path.startsWith('/') && !t.root) cls.push('code-tab-outside')
            const label = projectLabel?.(t)
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
                {tabIcon(t)}
                <span>{name}</span>
                {label && <span className="code-tab-project">{label}</span>}
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
