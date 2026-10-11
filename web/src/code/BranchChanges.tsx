import { ChevronDown, ChevronRight, GitCommitHorizontal } from 'lucide-react'
import { useCallback, useEffect, useState, type MouseEvent } from 'react'

import { formatAgo } from '../components/format'
import type { BranchChanges as Changes, CodeClient, GitStatus } from './client'
import { copyText, useContextMenu, type MenuItem } from './ContextMenu'

// DiffAgainst is what a diff of a branch's change compares with: where the
// branch left its base, and the file's name there.
export type DiffAgainst = { base: string; baseLabel: string; from?: string }

// BranchChanges is the Changes window's Branch view: everything the branch
// has changed since it left its base (the repository's main branch, or one
// chosen), committed or not, and the commits since. A file opens its diff
// against the base as it was where the branch left it.
export function BranchChanges({
  client,
  status,
  onDiff,
  onOpen,
}: {
  client: CodeClient
  status: GitStatus | null
  onDiff: (path: string, against: DiffAgainst) => void
  onOpen: (path: string) => void
}) {
  const [base, setBase] = useState<string | undefined>(undefined)
  const [data, setData] = useState<Changes | null>(null)
  const [error, setError] = useState('')
  const [commitsOpen, setCommitsOpen] = useState(true)
  const menu = useContextMenu()

  const load = useCallback(() => {
    client.branchChanges(base).then(
      (d) => {
        setData(d)
        setError('')
      },
      (e: Error) => setError(e.message),
    )
  }, [client, base])
  // Again whenever what git reports changes: the status is reloaded then.
  useEffect(load, [load, status])

  const pickBase = async (e: MouseEvent) => {
    const { clientX: x, clientY: y } = e
    let b
    try {
      b = await client.branches()
    } catch {
      return
    }
    const items: MenuItem[] = [
      { label: 'The main branch', checked: base === undefined, onSelect: () => setBase(undefined) },
      'separator',
      { heading: 'Local' },
      ...b.local
        .filter((br) => br.name !== b.current)
        .map((br) => ({ label: br.name, checked: base === br.name, onSelect: () => setBase(br.name) })),
    ]
    if (b.remote.length) {
      items.push(
        { heading: 'Remote' },
        ...b.remote.map((r) => ({ label: r, checked: base === r, onSelect: () => setBase(r) })),
      )
    }
    menu.openAt(x, y, items)
  }

  if (status && !status.repo) return <div className="code-note">This folder is not a git repository.</div>
  const label = data ? `${data.base} at ${data.mergeBase.slice(0, 7)}` : ''
  const against = (from?: string): DiffAgainst => ({ base: data!.mergeBase, baseLabel: label, from })

  return (
    <div className="code-changes">
      <div className="code-branch-base">
        <span className="muted small">Compared with</span>
        <button type="button" className="code-branch-btn" title="Compare with another branch" onClick={pickBase}>
          {data?.base ?? base ?? 'main'}
          <ChevronDown size={11} />
        </button>
        {data && (
          <span
            className="muted small"
            title={`This branch split from ${data.base} at ${data.mergeBase.slice(0, 7)}: the changes are everything since that commit, so later commits on ${data.base} are not among them.`}
          >
            · branched at {data.mergeBase.slice(0, 7)}
          </span>
        )}
      </div>
      {error && <div className="code-note code-note-error">{error}</div>}
      {data && (
        <>
          <div className="code-changes-section">
            <div className="code-changes-head code-changes-toggle" onClick={() => setCommitsOpen((o) => !o)}>
              {commitsOpen ? <ChevronDown size={12} /> : <ChevronRight size={12} />}
              <span>Commits</span>
              <span className="muted small">{data.commits.length}</span>
            </div>
            {commitsOpen &&
              data.commits.map((c) => (
                <div
                  key={c.hash}
                  className="code-change code-commit-row"
                  title={`${c.hash} by ${c.author}`}
                  onContextMenu={(e) => menu.open(e, [{ label: 'Copy Hash', onSelect: () => copyText(c.hash) }])}
                >
                  <GitCommitHorizontal size={13} className="code-icon-diff" />
                  <span className="code-change-name">{c.subject}</span>
                  <span className="code-change-dir muted">
                    {c.author}, {formatAgo(new Date(c.time * 1000).toISOString())}
                  </span>
                </div>
              ))}
            {commitsOpen && data.commits.length === 0 && <div className="code-note">No commits yet.</div>}
          </div>
          <div className="code-changes-section">
            <div className="code-changes-head">
              <span>Files changed</span>
              <span className="muted small">{data.changes.length}</span>
            </div>
            {data.changes.map((c) => (
              <div
                key={c.path}
                className="code-change"
                onClick={() => onDiff(c.path, against(c.from))}
                onContextMenu={(e) =>
                  menu.open(e, [
                    { label: 'Show Diff', onSelect: () => onDiff(c.path, against(c.from)) },
                    { label: 'Open File', disabled: c.status === 'D', onSelect: () => onOpen(c.path) },
                    'separator',
                    { label: 'Copy Relative Path', onSelect: () => copyText(c.path) },
                  ])
                }
                title={c.from ? `${c.from} → ${c.path}` : c.path}
              >
                <span className={`code-change-letter code-change-${letterClass(c.status)}`}>{c.status}</span>
                <span className="code-change-name">{c.path.split('/').pop()}</span>
                <span className="code-change-dir muted">
                  {c.path.includes('/') ? c.path.slice(0, c.path.lastIndexOf('/')) : ''}
                </span>
              </div>
            ))}
            {data.changes.length === 0 && <div className="code-note">Nothing differs from {data.base}.</div>}
          </div>
        </>
      )}
      {menu.menu}
    </div>
  )
}

function letterClass(l: string) {
  return { A: 'added', D: 'deleted', R: 'renamed', C: 'added' }[l] ?? 'modified'
}
