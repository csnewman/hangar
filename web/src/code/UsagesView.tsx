import { ChevronDown, ChevronRight, File } from 'lucide-react'
import { useEffect, useMemo, useRef, useState } from 'react'

import type { Usage, Usages } from './lsp'
import type { Jump } from './Search'
import { ToolWindow } from './ToolWindow'

// UsagesView is the Find tool window: a symbol's usages, by file, each
// opening its file at the usage. It stays as files are opened from it, and
// the arrow keys step through the usages, opening each.
export function UsagesView({
  usages,
  onJump,
  onHide,
}: {
  usages: Usages | null
  onJump: (j: Jump) => void
  onHide: () => void
}) {
  const [sel, setSel] = useState(0)
  const [closed, setClosed] = useState<Set<string>>(new Set())
  const list = useRef<HTMLDivElement>(null)
  useEffect(() => {
    setSel(0)
    setClosed(new Set())
  }, [usages])

  const files = useMemo(() => {
    const m = new Map<string, Usage[]>()
    for (const u of usages?.usages ?? []) m.set(u.path, [...(m.get(u.path) ?? []), u])
    return [...m.entries()]
  }, [usages])
  // The usages in the order shown, for the arrow keys.
  const shown = files.flatMap(([path, us]) => (closed.has(path) ? [] : us))

  useEffect(() => {
    list.current?.querySelector('[aria-selected=true]')?.scrollIntoView({ block: 'nearest' })
  }, [sel])

  const open = (u: Usage, focus = false) => onJump({ path: u.path, line: u.line, col: u.col, focus })
  const count = usages?.usages.length ?? 0

  return (
    <ToolWindow
      title="Find"
      onHide={onHide}
      extra={
        usages && (
          <span className="muted small">
            {usages.state === 'finding'
              ? `Finding usages of ${usages.symbol}…`
              : usages.state === 'failed'
                ? `Finding usages of ${usages.symbol} failed`
                : `Usages of ${usages.symbol}: ${count} in ${files.length} file${files.length === 1 ? '' : 's'}`}
          </span>
        )
      }
    >
      <div
        className="code-usages"
        ref={list}
        tabIndex={0}
        role="listbox"
        onKeyDown={(e) => {
          const by = { ArrowDown: 1, ArrowUp: -1 }[e.key]
          if (by !== undefined && shown.length) {
            e.preventDefault()
            const next = Math.max(0, Math.min(shown.length - 1, sel + by))
            setSel(next)
            open(shown[next])
          } else if (e.key === 'Enter' && shown[sel]) open(shown[sel], true)
        }}
      >
        {!usages && (
          <div className="code-note">
            Usages found with Alt+F7, or Find Usages on an editor&rsquo;s menu, are listed here.
          </div>
        )}
        {usages?.state === 'failed' && <div className="code-note code-note-error">{usages.why}</div>}
        {usages?.state === 'found' && count === 0 && <div className="code-note">No usages.</div>}
        {files.map(([path, us]) => {
          const isOpen = !closed.has(path)
          return (
            <div key={path}>
              <div
                className="code-usages-file"
                onClick={() =>
                  setClosed((s) => {
                    const n = new Set(s)
                    if (isOpen) n.add(path)
                    else n.delete(path)
                    return n
                  })
                }
              >
                {isOpen ? <ChevronDown size={13} /> : <ChevronRight size={13} />}
                <File size={13} className="code-icon-file" />
                <span>{path.split('/').pop()}</span>
                <span className="muted small">{path.includes('/') ? path.slice(0, path.lastIndexOf('/')) : ''}</span>
                <span className="muted small">{us.length}</span>
              </div>
              {isOpen &&
                us.map((u) => {
                  const i = shown.indexOf(u)
                  const lead = u.text.length - u.text.trimStart().length
                  const start = u.col - 1 - lead
                  const text = u.text.trim()
                  return (
                    <div
                      key={`${u.line}:${u.col}`}
                      role="option"
                      aria-selected={i === sel}
                      className={i === sel ? 'code-usages-row code-usages-row-on' : 'code-usages-row'}
                      onClick={() => {
                        setSel(i)
                        open(u)
                      }}
                      onDoubleClick={() => open(u, true)}
                    >
                      <span className="code-usages-line">{u.line}</span>
                      <span className="code-usages-text">
                        {start >= 0 && u.length > 0 ? (
                          <>
                            {text.slice(0, start)}
                            <mark>{text.slice(start, start + u.length)}</mark>
                            {text.slice(start + u.length)}
                          </>
                        ) : (
                          text
                        )}
                      </span>
                    </div>
                  )
                })}
            </div>
          )
        })}
      </div>
    </ToolWindow>
  )
}
