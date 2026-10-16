import { ChevronDown, ChevronRight, GitBranch, GitCommitHorizontal } from 'lucide-react'
import { useCallback, useEffect, useState, type MouseEvent } from 'react'

import { formatAgo } from '../components/format'
import { BranchPicker, type PickerAction } from './BranchPicker'
import type { Branches, BranchChanges as Changes, CodeClient, GitStatus } from './client'
import { copyText, useContextMenu } from './ContextMenu'

// DiffAgainst is what a diff compares: where a branch left its base, or a
// commit's parent, and the file's name there, with the working tree or
// the commit (to).
export type DiffAgainst = { base: string; baseLabel: string; from?: string; to?: string; toLabel?: string }

// BranchChanges is the Changes window's Branch view: everything the branch
// has changed since it left its base (the repository's main branch, or one
// chosen), committed or not, and the commits since. A file opens its diff
// against the base as it was where the branch left it. repo is the
// repository, by a path in it, the folder's own when none is given.
export function BranchChanges({
  client,
  status,
  repo,
  onDiff,
  onOpen,
}: {
  client: CodeClient
  status: GitStatus | null
  repo?: string
  onDiff: (path: string, against: DiffAgainst) => void
  onOpen: (path: string) => void
}) {
  const [base, setBase] = useState<string | undefined>(undefined)
  const [data, setData] = useState<Changes | null>(null)
  const [error, setError] = useState('')
  const [commitsOpen, setCommitsOpen] = useState(true)
  const menu = useContextMenu()

  const load = useCallback(() => {
    client.branchChanges(base, repo).then(
      (d) => {
        setData(d)
        setError('')
      },
      (e: Error) => setError(e.message),
    )
  }, [client, base, repo])
  // Again whenever what git reports changes: the status is reloaded then.
  useEffect(load, [load, status])

  const [picker, setPicker] = useState<{ anchor: DOMRect; branches: Branches } | null>(null)
  const pickBase = async (e: MouseEvent) => {
    const anchor = e.currentTarget.getBoundingClientRect()
    try {
      const b = await client.branches(repo)
      setPicker({ anchor, branches: { ...b, local: b.local.filter((br) => br.name !== b.current) } })
    } catch {
      // Without the branches there is nothing to choose from.
    }
  }
  const mainAction: PickerAction = (q) =>
    !q || 'the main branch'.includes(q.toLowerCase())
      ? {
          label: 'The main branch',
          icon: <GitBranch size={13} className="code-icon-file" />,
          checked: base === undefined,
          run: () => setBase(undefined),
        }
      : null

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
            branched at {data.mergeBase.slice(0, 7)}
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
                <span className="code-change-dir muted">{changeWhere(c)}</span>
              </div>
            ))}
            {data.changes.length === 0 && <div className="code-note">Nothing differs from {data.base}.</div>}
          </div>
        </>
      )}
      {picker && (
        <BranchPicker
          anchor={picker.anchor}
          branches={picker.branches}
          checked={base}
          actions={[mainAction]}
          onPick={(name) => setBase(name)}
          onClose={() => setPicker(null)}
        />
      )}
      {menu.menu}
    </div>
  )
}

function letterClass(l: string) {
  return { A: 'added', D: 'deleted', R: 'renamed', C: 'added' }[l] ?? 'modified'
}

const dirOf = (p: string) => (p.includes('/') ? p.slice(0, p.lastIndexOf('/')) : '')

// changeWhere is what a change's row says beside its name: its folder, or
// for a rename, the name it had, whole when it was in another folder.
export function changeWhere(c: { path: string; from?: string }): string {
  if (!c.from) return dirOf(c.path)
  return `from ${dirOf(c.from) === dirOf(c.path) ? c.from.split('/').pop() : c.from}`
}
