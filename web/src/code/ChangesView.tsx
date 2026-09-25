import { ArrowDownToLine, ArrowUpFromLine, ChevronDown, CloudDownload, GitBranch, Minus, Plus, RefreshCw, Undo2 } from 'lucide-react'
import { useState, type MouseEvent } from 'react'

import type { Change, CodeClient, GitStatus } from './client'
import { copyText, useContextMenu, type MenuItem } from './ContextMenu'
import { ToolWindow } from './ToolWindow'

// ChangesView is the Changes tool window: what git says has changed, grouped
// as IntelliJ's commit window groups it, with staging, rollback and a commit
// box. Clicking a file opens its diff. Its title bar holds the branch, which
// switches branches, and fetch, pull and push.
export function ChangesView({
  client,
  status,
  reload,
  onDiff,
  onOpen,
  onHide,
}: {
  client: CodeClient
  status: GitStatus | null
  reload: () => void
  onDiff: (path: string) => void
  onOpen: (path: string) => void
  onHide: () => void
}) {
  const [message, setMessage] = useState('')
  const [amend, setAmend] = useState(false)
  const [error, setError] = useState('')
  const [busy, setBusy] = useState('')
  const [confirm, setConfirm] = useState<string | null>(null)
  const [newBranch, setNewBranch] = useState<string | null>(null)
  const menu = useContextMenu()

  // act runs a git action, showing what it is while it runs and its error
  // if it fails.
  const act = async (fn: () => Promise<void>, doing = 'Working…') => {
    setError('')
    setBusy(doing)
    try {
      await fn()
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e))
    } finally {
      setBusy('')
      reload()
    }
  }

  const commit = () =>
    act(async () => {
      await client.commit(message, amend)
      setMessage('')
      setAmend(false)
    }, 'Committing…')

  // Amending starts from the last commit's message.
  const toggleAmend = (on: boolean) => {
    setAmend(on)
    if (on && !message.trim()) client.lastMessage().then(setMessage, () => {})
  }

  const changeMenu = (c: Change, stagedRow: boolean): MenuItem[] => {
    const name = c.path.split('/').pop() ?? c.path
    const deleted = (stagedRow ? c.index : c.worktree) === 'D'
    return [
      { label: 'Show Diff', onSelect: () => onDiff(c.path) },
      { label: 'Open File', disabled: deleted, onSelect: () => onOpen(c.path) },
      'separator',
      stagedRow
        ? { label: 'Unstage', onSelect: () => act(() => client.unstage(c.path)) }
        : { label: 'Stage', onSelect: () => act(() => client.stage(c.path)) },
      {
        label: c.index === '?' ? 'Delete Unversioned File' : 'Rollback',
        danger: true,
        confirm: c.index === '?' ? `Delete ${name}?` : `Discard all changes to ${name}?`,
        onSelect: () => act(() => client.rollback(c.path)),
      },
      'separator',
      { label: 'Copy Path', onSelect: () => copyText(`${client.root}/${c.path}`) },
      { label: 'Copy Relative Path', onSelect: () => copyText(c.path) },
    ]
  }

  // branchMenu lists the branches to switch to, loaded when it opens.
  const branchMenu = async (e: MouseEvent) => {
    const { clientX: x, clientY: y } = e
    let b
    try {
      b = await client.branches()
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err))
      return
    }
    const items: MenuItem[] = [
      { label: 'New Branch…', onSelect: () => setNewBranch('') },
      'separator',
      { heading: 'Local' },
      ...b.local.map((br) => ({
        label: br.name + (br.ahead ? ` ↑${br.ahead}` : '') + (br.behind ? ` ↓${br.behind}` : ''),
        checked: br.name === b.current,
        onSelect: () => br.name !== b.current && act(() => client.checkout(br.name), `Checking out ${br.name}…`),
      })),
    ]
    if (b.remote.length) {
      items.push(
        { heading: 'Remote' },
        ...b.remote.map((r) => ({ label: r, onSelect: () => act(() => client.checkout(r), `Checking out ${r}…`) })),
      )
    }
    menu.openAt(x, y, items)
  }

  const createBranch = () => {
    const name = newBranch?.trim()
    setNewBranch(null)
    if (name) act(() => client.createBranch(name), `Creating ${name}…`)
  }

  const changes = status?.changes ?? []
  const staged = changes.filter((c) => c.index !== '.' && c.index !== '?' && c.index !== 'U')
  const unstaged = changes.filter((c) => c.worktree !== '.' && c.index !== '?')
  const unversioned = changes.filter((c) => c.index === '?')

  const row = (c: Change, stagedRow: boolean) => {
    const letter = stagedRow ? c.index : c.worktree
    const key = `${stagedRow ? 's' : 'w'}:${c.path}`
    return (
      <div
        key={key}
        className="code-change"
        onClick={() => onDiff(c.path)}
        onContextMenu={(e) => menu.open(e, changeMenu(c, stagedRow))}
        title={c.from ? `${c.from} → ${c.path}` : c.path}
      >
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

  const canCommit = amend || (staged.length > 0 && message.trim() !== '')

  const stageAll = async () => {
    for (const c of [...unstaged, ...unversioned]) await client.stage(c.path)
  }

  return (
    <ToolWindow
      title="Changes"
      extra={
        status?.branch && (
          <button type="button" className="code-branch-btn" title="Switch or create a branch" onClick={branchMenu}>
            <GitBranch size={12} />
            {status.branch === '(detached)' ? 'detached HEAD' : status.branch}
            {!!status.ahead && <span> ↑{status.ahead}</span>}
            {!!status.behind && <span> ↓{status.behind}</span>}
            <ChevronDown size={11} />
          </button>
        )
      }
      actions={
        status?.repo && (
          <>
            <button
              type="button"
              className="code-tool-btn"
              title="Fetch"
              disabled={!!busy}
              onClick={() => act(() => client.fetch(), 'Fetching…')}
            >
              <CloudDownload size={14} />
            </button>
            <button
              type="button"
              className="code-tool-btn"
              title="Pull (fast-forward only)"
              disabled={!!busy}
              onClick={() => act(() => client.pull(), 'Pulling…')}
            >
              <ArrowDownToLine size={14} />
            </button>
            <button
              type="button"
              className="code-tool-btn"
              title="Push"
              disabled={!!busy}
              onClick={() => act(() => client.push(), 'Pushing…')}
            >
              <ArrowUpFromLine size={14} />
            </button>
            <button type="button" className="code-tool-btn" title="Refresh" onClick={reload}>
              <RefreshCw size={13} />
            </button>
          </>
        )
      }
      onHide={onHide}
    >
      {newBranch !== null && (
        <form
          className="code-new-branch"
          onSubmit={(e) => {
            e.preventDefault()
            createBranch()
          }}
        >
          <input
            autoFocus
            placeholder="New branch name"
            value={newBranch}
            onChange={(e) => setNewBranch(e.target.value)}
            onKeyDown={(e) => e.key === 'Escape' && setNewBranch(null)}
          />
          <button type="submit" disabled={!newBranch.trim()}>
            Create
          </button>
        </form>
      )}
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
            placeholder={amend ? 'Commit message' : staged.length ? 'Commit message' : 'Stage changes to commit them'}
            value={message}
            onChange={(e) => setMessage(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === 'Enter' && (e.metaKey || e.ctrlKey) && canCommit) commit()
            }}
          />
          <div className="code-commit-bar">
            <label className="code-amend" title="Replace the last commit with this one">
              <input type="checkbox" checked={amend} onChange={(e) => toggleAmend(e.target.checked)} />
              Amend
            </label>
            {busy && <span className="muted small">{busy}</span>}
            {error && !busy && <span className="code-note-error small">{error}</span>}
            <span className="code-tool-spacer" />
            <button
              type="button"
              className="btn btn-primary btn-sm"
              disabled={!!busy || !canCommit}
              onClick={commit}
              title="Commit the staged changes (⌘⏎)"
            >
              {amend ? 'Amend' : 'Commit'} {staged.length > 0 && `${staged.length} file${staged.length === 1 ? '' : 's'}`}
            </button>
          </div>
        </div>
      )}
      {menu.menu}
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
