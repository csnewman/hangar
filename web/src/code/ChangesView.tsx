import { GitBranch, Minus, Plus, RefreshCw, Undo2 } from 'lucide-react'
import { useState } from 'react'

import type { Change, CodeClient, GitStatus } from './client'
import { ToolWindow } from './ToolWindow'

// ChangesView is the Changes tool window: what git says has changed, grouped
// as IntelliJ's commit window groups it, with staging, rollback and a commit
// box. Clicking a file opens its diff.
export function ChangesView({
  client,
  status,
  reload,
  onDiff,
  onHide,
}: {
  client: CodeClient
  status: GitStatus | null
  reload: () => void
  onDiff: (path: string) => void
  onHide: () => void
}) {
  const [message, setMessage] = useState('')
  const [error, setError] = useState('')
  const [busy, setBusy] = useState(false)
  const [confirm, setConfirm] = useState<string | null>(null)

  const act = async (fn: () => Promise<void>) => {
    setError('')
    setBusy(true)
    try {
      await fn()
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e))
    } finally {
      setBusy(false)
      reload()
    }
  }

  const changes = status?.changes ?? []
  const staged = changes.filter((c) => c.index !== '.' && c.index !== '?' && c.index !== 'U')
  const unstaged = changes.filter((c) => c.worktree !== '.' && c.index !== '?')
  const unversioned = changes.filter((c) => c.index === '?')

  const row = (c: Change, stagedRow: boolean) => {
    const letter = stagedRow ? c.index : c.worktree
    const key = `${stagedRow ? 's' : 'w'}:${c.path}`
    return (
      <div key={key} className="code-change" onClick={() => onDiff(c.path)} title={c.from ? `${c.from} → ${c.path}` : c.path}>
        <span className={`code-change-letter code-change-${letterClass(letter)}`}>{letter === '?' ? 'U' : letter}</span>
        <span className="code-change-name">{c.path.split('/').pop()}</span>
        <span className="code-change-dir muted">{c.path.includes('/') ? c.path.slice(0, c.path.lastIndexOf('/')) : ''}</span>
        <span className="code-change-actions" onClick={(e) => e.stopPropagation()}>
          {stagedRow ? (
            <button type="button" className="code-tool-btn" title="Unstage" onClick={() => act(() => client.unstage(c.path))}>
              <Minus size={13} />
            </button>
          ) : (
            <button type="button" className="code-tool-btn" title="Stage" onClick={() => act(() => client.stage(c.path))}>
              <Plus size={13} />
            </button>
          )}
          {confirm === key ? (
            <button
              type="button"
              className="code-tool-btn code-danger"
              title="Discard this file's changes"
              onClick={() => {
                setConfirm(null)
                act(() => client.rollback(c.path))
              }}
            >
              Discard?
            </button>
          ) : (
            <button type="button" className="code-tool-btn" title="Rollback…" onClick={() => setConfirm(key)}>
              <Undo2 size={13} />
            </button>
          )}
        </span>
      </div>
    )
  }

  const section = (title: string, list: Change[], stagedRows: boolean, bulk?: { label: string; run: () => Promise<void> }) =>
    list.length > 0 && (
      <div className="code-changes-section">
        <div className="code-changes-head">
          <span>{title}</span>
          <span className="muted small">{list.length}</span>
          <span className="code-tool-spacer" />
          {bulk && (
            <button type="button" className="code-link" onClick={() => act(bulk.run)}>
              {bulk.label}
            </button>
          )}
        </div>
        {list.map((c) => row(c, stagedRows))}
      </div>
    )

  const stageAll = async () => {
    for (const c of [...unstaged, ...unversioned]) await client.stage(c.path)
  }

  return (
    <ToolWindow
      title="Changes"
      extra={
        status?.branch && (
          <span className="code-branch" title="Current branch">
            <GitBranch size={12} />
            {status.branch}
            {!!status.ahead && <span className="muted"> ↑{status.ahead}</span>}
            {!!status.behind && <span className="muted"> ↓{status.behind}</span>}
          </span>
        )
      }
      actions={
        <button type="button" className="code-tool-btn" title="Refresh" onClick={reload}>
          <RefreshCw size={13} />
        </button>
      }
      onHide={onHide}
    >
      <div className="code-changes">
        {status && !status.repo && <div className="code-note">This folder is not a git repository.</div>}
        {status?.repo && changes.length === 0 && <div className="code-note">No changes.</div>}
        {section('Staged', staged, true)}
        {section('Changes', unstaged, false, unstaged.length ? { label: 'Stage all', run: stageAll } : undefined)}
        {section('Unversioned files', unversioned, false)}
      </div>
      {status?.repo && (
        <div className="code-commit">
          <textarea
            className="code-commit-message"
            placeholder={staged.length ? 'Commit message' : 'Stage changes to commit them'}
            value={message}
            onChange={(e) => setMessage(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === 'Enter' && (e.metaKey || e.ctrlKey) && staged.length && message.trim()) {
                act(async () => {
                  await client.commit(message)
                  setMessage('')
                })
              }
            }}
          />
          <div className="code-commit-bar">
            {error && <span className="code-note-error small">{error}</span>}
            <span className="code-tool-spacer" />
            <button
              type="button"
              className="btn btn-primary btn-sm"
              disabled={busy || !staged.length || !message.trim()}
              onClick={() =>
                act(async () => {
                  await client.commit(message)
                  setMessage('')
                })
              }
              title="Commit the staged changes (⌘⏎)"
            >
              Commit {staged.length > 0 && `${staged.length} file${staged.length === 1 ? '' : 's'}`}
            </button>
          </div>
        </div>
      )}
    </ToolWindow>
  )
}

function letterClass(l: string) {
  switch (l) {
    case 'A':
      return 'added'
    case 'D':
      return 'deleted'
    case '?':
      return 'unversioned'
    case 'U':
      return 'conflict'
    case 'R':
      return 'renamed'
    default:
      return 'modified'
  }
}
