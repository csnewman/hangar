import { ChevronDown, ChevronRight } from 'lucide-react'
import { useEffect, useMemo, useRef, useState } from 'react'

import { useEditorStatus, type EditorStatusStore } from './editorStatus'
import { isMarkdown } from './MarkdownPreview'
import type { Jump } from './Search'
import { innermost, kindOf, outline, type Outline, type Sym } from './symbols'
import { ToolWindow } from './ToolWindow'

// StructureView is IntelliJ's Structure tool window: the symbols of the
// file being edited, as its language server lists them (a Markdown file's
// headings without one), each inside what holds it. A click goes to one;
// the one the caret is in is marked. It follows the file as it is edited.
export function StructureView({
  store,
  active,
  onJump,
  onHide,
}: {
  store: EditorStatusStore
  // active is whether the window is shown: it reads nothing while hidden.
  active: boolean
  onJump: (j: Jump) => void
  onHide: () => void
}) {
  const status = useEditorStatus(store)
  const view = status?.view ?? null
  const path = status?.path ?? ''
  const edits = status?.edits ?? 0
  const serverState = status?.server?.state
  const [read, setRead] = useState<{ path: string; outline: Outline } | null>(null)
  const [error, setError] = useState('')
  // Folded symbols, by their names from the outermost, which edits keep.
  const [closed, setClosed] = useState<Set<string>>(new Set())

  useEffect(() => {
    if (!active || !view) return
    let live = true
    // At most every half second as the file is typed in.
    const timer = setTimeout(
      () =>
        outline(view, isMarkdown(path)).then(
          (o) => {
            if (!live) return
            setRead({ path, outline: o })
            setError('')
          },
          (e: unknown) => live && setError(e instanceof Error ? e.message : String(e)),
        ),
      read?.path === path ? 500 : 0,
    )
    return () => {
      live = false
      clearTimeout(timer)
    }
    // read is only looked at to know whether the file is new to the window.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [active, view, path, edits, serverState])

  const o = read?.path === path ? read.outline : null
  const here = useMemo(() => (o?.symbols ? innermost(o.symbols, status?.line ?? 0) : null), [o, status?.line])
  const list = useRef<HTMLDivElement>(null)
  useEffect(() => {
    list.current?.querySelector('.code-structure-here')?.scrollIntoView({ block: 'nearest' })
  }, [here])

  const row = (s: Sym, depth: number, parent: string) => {
    const k = kindOf(s.kind)
    const key = `${parent}/${s.name}`
    const open = !closed.has(key)
    return (
      <div key={`${s.line}:${s.col}:${s.name}`}>
        <div
          className={s === here ? 'code-structure-row code-structure-here' : 'code-structure-row'}
          style={{ paddingLeft: 6 + depth * 14 }}
          title={`${k.name} ${s.name}${s.detail ? ` ${s.detail}` : ''}, line ${s.line}`}
          onClick={() => onJump({ path, line: s.line, col: s.col })}
        >
          {s.children.length ? (
            <button
              type="button"
              className="code-structure-twisty"
              aria-label={open ? 'Fold' : 'Unfold'}
              onClick={(e) => {
                e.stopPropagation()
                setClosed((c) => {
                  const n = new Set(c)
                  if (open) n.add(key)
                  else n.delete(key)
                  return n
                })
              }}
            >
              {open ? <ChevronDown size={12} /> : <ChevronRight size={12} />}
            </button>
          ) : (
            <span className="code-structure-twisty" />
          )}
          <span className={`code-symbol-kind code-symbol-${k.tone}`}>{k.letter}</span>
          <span className="code-structure-name">{s.name}</span>
          {s.detail && <span className="code-structure-detail">{s.detail}</span>}
        </div>
        {open && s.children.map((c) => row(c, depth + 1, key))}
      </div>
    )
  }

  const name = path.split('/').pop()
  return (
    <ToolWindow title="Structure" onHide={onHide} extra={name && <span className="muted small">{name}</span>}>
      <div className="code-structure" ref={list}>
        {!view ? (
          <p className="code-note">Open a file to see its structure.</p>
        ) : error ? (
          <p className="code-note code-note-error">{error}</p>
        ) : !o ? (
          <p className="code-note">Reading {name}…</p>
        ) : !o.symbols ? (
          <p className="code-note">{serverState === 'starting' ? 'Waiting for its language server…' : o.why}</p>
        ) : o.symbols.length === 0 ? (
          <p className="code-note">{o.source === 'headings' ? 'No headings.' : 'No symbols.'}</p>
        ) : (
          o.symbols.map((s) => row(s, 0, ''))
        )}
      </div>
    </ToolWindow>
  )
}
