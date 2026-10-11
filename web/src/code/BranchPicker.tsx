import { Check, Cloud, GitBranch, Plus } from 'lucide-react'
import { useEffect, useLayoutEffect, useMemo, useRef, useState, type ReactNode } from 'react'
import { createPortal } from 'react-dom'

import type { Branches } from './client'
import { marked, match } from './match'

// How many of each kind of branch are listed at once; typing narrows them.
const shown = 8

// An action listed above the branches: New Branch, or the main branch as
// a diff's base. Given what has been typed, it says what it would do with
// it, or nothing to stay out of the list.
export type PickerAction = (query: string) => { label: string; icon?: ReactNode; checked?: boolean; run: () => void } | null

type Row =
  | { kind: 'action'; label: string; icon?: ReactNode; checked?: boolean; run: () => void }
  | { kind: 'branch'; name: string; remote: boolean; ranges: [number, number][]; hint?: string; checked: boolean }
  | { kind: 'heading'; label: string }
  | { kind: 'more'; label: string }

// BranchPicker is a branch chooser opened below a button: a search box
// over the branches, local then remote, each list most recently committed
// to first and cut short, with the count of the rest. The arrow keys and
// Enter choose, Escape or a click elsewhere closes it.
export function BranchPicker({
  anchor,
  branches,
  checked,
  actions = [],
  onPick,
  onClose,
}: {
  anchor: DOMRect
  branches: Branches
  // The branch marked as the one in force.
  checked?: string
  actions?: PickerAction[]
  onPick: (name: string, remote: boolean) => void
  onClose: () => void
}) {
  const [query, setQuery] = useState('')
  const [sel, setSel] = useState(0)
  const ref = useRef<HTMLDivElement>(null)
  const [pos, setPos] = useState({ left: anchor.left, top: anchor.bottom + 4 })

  const rows = useMemo(() => {
    const q = query.trim()
    const out: Row[] = []
    for (const a of actions) {
      const r = a(q)
      if (r) out.push({ kind: 'action', ...r })
    }
    const group = (label: string, names: { name: string; hint?: string }[], remote: boolean) => {
      const found = names
        .map((b, i) => ({ ...b, i, m: match(b.name, q) }))
        .filter((b) => b.m)
        .sort((a, b) => b.m!.score - a.m!.score || a.i - b.i)
      if (!found.length) return
      out.push({ kind: 'heading', label })
      for (const b of found.slice(0, shown)) {
        out.push({ kind: 'branch', name: b.name, remote, ranges: b.m!.ranges, hint: b.hint, checked: b.name === checked })
      }
      if (found.length > shown) out.push({ kind: 'more', label: `${found.length - shown} more, type to narrow` })
    }
    group(
      'Local',
      branches.local.map((b) => ({
        name: b.name,
        hint: [b.ahead ? `↑${b.ahead}` : '', b.behind ? `↓${b.behind}` : ''].filter(Boolean).join(' ') || undefined,
      })),
      false,
    )
    group(
      'Remote',
      branches.remote.map((name) => ({ name })),
      true,
    )
    return out
  }, [query, branches, checked, actions])

  // The rows Enter and the arrow keys move between.
  const choosable = rows.flatMap((r, i) => (r.kind === 'action' || r.kind === 'branch' ? [i] : []))
  // Once something is typed, the best matching branch is the one Enter
  // takes, before any action.
  useEffect(() => {
    const first = query.trim() ? choosable.findIndex((i) => rows[i].kind === 'branch') : -1
    setSel(Math.max(0, first))
    // On the query alone: the rows are made anew on every render.
  }, [query])
  const on = choosable[Math.min(sel, choosable.length - 1)]

  // Below the button, its left edge with the button's, kept in the window;
  // above the button when there is no room below.
  useLayoutEffect(() => {
    const el = ref.current
    if (!el) return
    const r = el.getBoundingClientRect()
    const below = anchor.bottom + 4
    setPos({
      left: Math.max(4, Math.min(anchor.left, window.innerWidth - r.width - 4)),
      top: below + r.height > window.innerHeight - 4 ? Math.max(4, anchor.top - 4 - r.height) : below,
    })
  }, [anchor, rows.length])

  useEffect(() => {
    const away = (e: Event) => {
      if (ref.current && !ref.current.contains(e.target as Node)) onClose()
    }
    document.addEventListener('mousedown', away, true)
    window.addEventListener('blur', onClose)
    window.addEventListener('resize', onClose)
    return () => {
      document.removeEventListener('mousedown', away, true)
      window.removeEventListener('blur', onClose)
      window.removeEventListener('resize', onClose)
    }
  }, [onClose])

  useEffect(() => {
    ref.current?.querySelector('.code-popup-row-on')?.scrollIntoView({ block: 'nearest' })
  }, [on])

  const choose = (i: number | undefined) => {
    const r = i === undefined ? undefined : rows[i]
    if (!r) return
    if (r.kind === 'action') {
      onClose()
      r.run()
    } else if (r.kind === 'branch') {
      onClose()
      onPick(r.name, r.remote)
    }
  }

  return createPortal(
    <div
      ref={ref}
      className="code-branch-picker"
      style={pos}
      onContextMenu={(e) => e.preventDefault()}
      onKeyDown={(e) => {
        if (e.key === 'Escape') {
          e.preventDefault()
          e.stopPropagation()
          onClose()
        } else if (e.key === 'ArrowDown' || e.key === 'ArrowUp') {
          e.preventDefault()
          if (!choosable.length) return
          const at = Math.min(sel, choosable.length - 1)
          setSel(e.key === 'ArrowDown' ? (at + 1) % choosable.length : (at - 1 + choosable.length) % choosable.length)
        } else if (e.key === 'Enter') {
          e.preventDefault()
          choose(on)
        }
      }}
    >
      <input
        className="code-popup-input"
        autoFocus
        spellCheck={false}
        placeholder="Search branches"
        value={query}
        onChange={(e) => setQuery(e.target.value)}
      />
      <div className="code-branch-picker-list" role="listbox">
        {rows.map((r, i) =>
          r.kind === 'heading' ? (
            <div key={`h:${r.label}`} className="code-menu-heading">
              {r.label}
            </div>
          ) : r.kind === 'more' ? (
            <div key={`m:${i}`} className="code-branch-picker-more">
              {r.label}
            </div>
          ) : (
            <div
              key={r.kind === 'action' ? `a:${i}` : `${r.remote ? 'r' : 'l'}:${r.name}`}
              role="option"
              aria-selected={i === on}
              className={i === on ? 'code-popup-row code-popup-row-on' : 'code-popup-row'}
              onMouseMove={() => setSel(choosable.indexOf(i))}
              onClick={() => choose(i)}
            >
              {r.kind === 'action' ? (
                (r.icon ?? <Plus size={13} />)
              ) : r.remote ? (
                <Cloud size={13} className="code-icon-file" />
              ) : (
                <GitBranch size={13} className="code-icon-file" />
              )}
              <span className="code-branch-picker-name">
                {r.kind === 'action' ? r.label : marked(r.name, r.ranges)}
              </span>
              {r.kind === 'branch' && r.hint && <span className="muted small">{r.hint}</span>}
              {r.checked && <Check size={13} className="code-branch-picker-check" />}
            </div>
          ),
        )}
        {choosable.length === 0 && <div className="code-popup-empty">No branch matches.</div>}
      </div>
    </div>,
    document.body,
  )
}
