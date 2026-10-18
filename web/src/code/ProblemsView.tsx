import { ChevronDown, ChevronRight, CircleX, File, Info, TriangleAlert } from 'lucide-react'
import { useEffect, useMemo, useRef, useState } from 'react'

import type { Languages, Problem } from './lsp'
import type { Jump } from './Search'
import { ToolWindow } from './ToolWindow'

// useProblems is every problem the projects' language servers have found,
// kept up as they find more.
export function useProblems(all: Languages[]): Problem[] {
  const all_ = useRef(all)
  all_.current = all
  const key = all.map((_, i) => i).join()
  const gather = () => all_.current.flatMap((l) => l.allProblems())
  const [snap, setSnap] = useState(gather)
  useEffect(() => {
    setSnap(gather())
    // Servers publish file by file, often many at once: drawn once for each
    // burst.
    let timer: ReturnType<typeof setTimeout> | undefined
    const offs = all_.current.map((l) =>
      l.onProblems(() => {
        clearTimeout(timer)
        timer = setTimeout(() => setSnap(gather()), 100)
      }),
    )
    return () => {
      clearTimeout(timer)
      offs.forEach((off) => off())
    }
    // The servers change with the projects, which the list follows.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [key, ...all])
  return snap
}

// Problem counts by how bad: what the stripe button and the title show.
export function counts(problems: Problem[]) {
  let errors = 0
  let warnings = 0
  for (const p of problems) {
    if (p.severity === 1) errors++
    else if (p.severity === 2) warnings++
  }
  return { errors, warnings }
}

const severityIcon = (s: number) =>
  s === 1 ? (
    <CircleX size={13} className="code-problem-error" />
  ) : s === 2 ? (
    <TriangleAlert size={13} className="code-problem-warning" />
  ) : (
    <Info size={13} className="code-problem-info" />
  )

// ProblemsView is IntelliJ's Problems tool window: what the language
// servers have found wrong, by file, errors first. Which files a server
// reports on is its own choice: gopls reports on its whole workspace,
// most others on the files open. Hints are left out unless asked for.
export function ProblemsView({
  problems,
  onJump,
  onHide,
}: {
  problems: Problem[]
  onJump: (j: Jump) => void
  onHide: () => void
}) {
  const [hints, setHints] = useState(false)
  const [closed, setClosed] = useState<Set<string>>(new Set())
  const files = useMemo(() => {
    const m = new Map<string, Problem[]>()
    for (const p of problems) {
      if (p.severity > 3 && !hints) continue
      const key = `${p.root}\0${p.path}`
      m.set(key, [...(m.get(key) ?? []), p])
    }
    const worst = (ps: Problem[]) => Math.min(...ps.map((p) => p.severity))
    return [...m.entries()]
      .map(
        ([path, ps]) => [path, ps.sort((a, b) => a.severity - b.severity || a.line - b.line || a.col - b.col)] as const,
      )
      .sort(([a, pa], [b, pb]) => worst(pa) - worst(pb) || a.localeCompare(b))
  }, [problems, hints])
  const { errors, warnings } = counts(problems)
  const projects = new Set(problems.map((p) => p.root)).size
  const shown = files.reduce((n, [, ps]) => n + ps.length, 0)

  return (
    <ToolWindow
      title="Problems"
      onHide={onHide}
      extra={
        <span className="muted small">
          {errors} error{errors === 1 ? '' : 's'}, {warnings} warning{warnings === 1 ? '' : 's'}
        </span>
      }
      actions={
        <button
          type="button"
          className={hints ? 'code-tool-btn code-tool-btn-on' : 'code-tool-btn'}
          aria-pressed={hints}
          title="Show hints and suggestions too"
          onClick={() => setHints((h) => !h)}
        >
          Hints
        </button>
      }
    >
      <div className="code-problems">
        {shown === 0 && (
          <p className="code-note">
            No problems found. A language server reports on the files it has looked at: open a file to start its
            language's.
          </p>
        )}
        {files.map(([key, ps]) => {
          const path = ps[0].path
          const open = !closed.has(key)
          const cut = path.lastIndexOf('/') + 1
          const project = projects > 1 ? ps[0].root.slice(ps[0].root.lastIndexOf('/') + 1) : ''
          return (
            <div key={key}>
              <div
                className="code-problems-file"
                onClick={() =>
                  setClosed((c) => {
                    const n = new Set(c)
                    if (open) n.add(key)
                    else n.delete(key)
                    return n
                  })
                }
              >
                {open ? <ChevronDown size={12} /> : <ChevronRight size={12} />}
                <File size={13} className="code-icon-file" />
                <span className="strong">{path.slice(cut)}</span>
                {project && <span className="code-tab-project">{project}</span>}
                <span className="muted">{path.slice(0, cut).replace(/\/$/, '')}</span>
                <span className="muted small">{ps.length}</span>
              </div>
              {open &&
                ps.map((p, i) => (
                  <div
                    key={i}
                    className="code-problems-row"
                    title={`${p.message}${p.source ? ` (${p.source}${p.code ? ` ${p.code}` : ''})` : ''}`}
                    onClick={() => onJump({ path: p.path, root: p.root, line: p.line, col: p.col })}
                  >
                    {severityIcon(p.severity)}
                    <span className="code-problems-message">{p.message.split('\n')[0]}</span>
                    {p.source && <span className="muted small">{p.source}</span>}
                    <span className="muted small">
                      :{p.line}:{p.col}
                    </span>
                  </div>
                ))}
            </div>
          )
        })}
      </div>
    </ToolWindow>
  )
}
