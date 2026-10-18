import { ChevronDown, ChevronRight, File } from 'lucide-react'
import { useEffect, useMemo, useRef, useState } from 'react'

import type { CodeClient } from './client'
import { projectName } from './projects'
import type { Usage, Usages } from './lsp'
import { Preview, type Jump } from './Search'
import { ToolWindow } from './ToolWindow'

// UsagesView is the Find tool window, as IntelliJ's: a symbol's usages, or
// a Find in Files' matches, by file, and beside them the chosen one in its
// file. Clicking or the arrow keys choose one and preview it; a double
// click or Enter opens its file there. It stays as files are opened from
// it.
export function UsagesView({
  clientFor,
  usages,
  onJump,
  onHide,
}: {
  // clientFor is a usage's project's connection, to preview it.
  clientFor: (root?: string) => CodeClient
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
    for (const u of usages?.usages ?? []) {
      const key = u.root ? `${u.root}/${u.path}` : u.path
      m.set(key, [...(m.get(key) ?? []), u])
    }
    return [...m.entries()]
  }, [usages])
  // The usages in the order shown, for the arrow keys.
  const shown = files.flatMap(([path, us]) => (closed.has(path) ? [] : us))

  useEffect(() => {
    list.current?.querySelector('[aria-selected=true]')?.scrollIntoView({ block: 'nearest' })
  }, [sel])

  const open = (u: Usage) => onJump({ path: u.path, root: u.root, line: u.line, col: u.col, focus: true })
  const count = usages?.usages.length ?? 0
  const text = usages?.kind === 'text'
  const what = text ? `“${usages?.symbol}”` : `usages of ${usages?.symbol}`
  const chosen = shown[sel]
  const summary = () => {
    if (!usages) return ''
    if (usages.state === 'finding') return `Finding ${what}…`
    if (usages.state === 'failed') return `Finding ${what} failed`
    const n = `${count}${usages.more ? '+' : ''}`
    const inFiles = `in ${files.length} file${files.length === 1 ? '' : 's'}`
    return text
      ? `${what}: ${n} match${count === 1 ? '' : 'es'} ${inFiles}`
      : `Usages of ${usages.symbol}: ${n} ${inFiles}`
  }

  return (
    <ToolWindow
      title="Find"
      onHide={onHide}
      extra={
        usages && (
          <span className="muted small">
            {summary()}
          </span>
        )
      }
    >
      <div className="code-usages-split">
        <div
          className="code-usages"
          ref={list}
          tabIndex={0}
          role="listbox"
          onKeyDown={(e) => {
            const by = { ArrowDown: 1, ArrowUp: -1 }[e.key]
            if (by !== undefined && shown.length) {
              e.preventDefault()
              setSel(Math.max(0, Math.min(shown.length - 1, sel + by)))
            } else if (e.key === 'Enter' && shown[sel]) open(shown[sel])
          }}
        >
          {!usages && (
            <div className="code-note">
              Usages found with Alt+F7, or Find Usages on an editor&rsquo;s menu, are listed here.
            </div>
          )}
          {usages?.state === 'failed' && <div className="code-note code-note-error">{usages.why}</div>}
          {usages?.state === 'found' && count === 0 && (
            <div className="code-note">{text ? 'Nothing found.' : 'No usages.'}</div>
          )}
          {usages?.more && (
            <div className="code-note">Only the first matches are listed; a narrower search finds the rest.</div>
          )}
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
                  <span>{us[0].path.split('/').pop()}</span>
                  {us[0].root && <span className="code-tab-project">{projectName(us[0].root)}</span>}
                  <span className="muted small">
                    {us[0].path.includes('/') ? us[0].path.slice(0, us[0].path.lastIndexOf('/')) : ''}
                  </span>
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
                        onClick={() => setSel(i)}
                        onDoubleClick={() => open(u)}
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
        {chosen && (
          <Preview
            key={`${chosen.root ?? ''}:${chosen.path}`}
            client={clientFor(chosen.root)}
            match={{
              path: chosen.path,
              line: chosen.line,
              text: chosen.text,
              ranges: chosen.length > 0 ? [[chosen.col - 1, chosen.col - 1 + chosen.length]] : [],
            }}
            onOpen={() => open(chosen)}
          />
        )}
      </div>
    </ToolWindow>
  )
}
