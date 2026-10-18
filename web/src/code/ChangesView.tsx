import {
  ArrowDownToLine,
  ArrowUpFromLine,
  ChevronDown,
  CloudDownload,
  FolderGit2,
  GitBranch,
  Minus,
  Plus,
  RefreshCw,
  Undo2,
} from 'lucide-react'
import { useState, type MouseEvent, type ReactNode } from 'react'

import { BranchChanges, changeWhere, type DiffAgainst } from './BranchChanges'
import { BranchPicker, type PickerAction } from './BranchPicker'
import type { Branches, Change, CodeClient, GitStatus, Repo } from './client'
import { copyText, useContextMenu, type MenuItem } from './ContextMenu'
import { RepoPicker } from './RepoPicker'
import { amendable, commitAll, isStaged, usable, within } from './repos'
import { ToolWindow } from './ToolWindow'

// ChangesView is the Changes tool window: what git says has changed, grouped
// as IntelliJ's commit window groups it, with staging, rollback and a commit
// box. Clicking a file opens its diff. Its title bar holds the branch, which
// switches branches, and fetch, pull and push. Its Branch view shows
// instead what the branch has changed since it left the main branch.
//
// A folder holding several repositories -- submodules, clones within it,
// or a folder of clones -- shows each as a group of its own, its branch and
// fetch, pull and push on the group's head. One commit commits every
// repository with something staged.
export function ChangesView({
  client,
  status,
  reload,
  onDiff,
  onOpen,
  onHide,
  projectPicker,
}: {
  client: CodeClient
  status: GitStatus | null
  reload: () => void
  onDiff: (path: string, against?: DiffAgainst) => void
  onOpen: (path: string) => void
  onHide: () => void
  // projectPicker chooses which project's changes are shown, when there
  // are more.
  projectPicker?: ReactNode
}) {
  const [view, setView] = useState<'local' | 'branch'>('local')
  const [branchRepo, setBranchRepo] = useState('')
  const [message, setMessage] = useState('')
  const [amend, setAmend] = useState(false)
  const [error, setError] = useState('')
  const [busy, setBusy] = useState('')
  const [confirm, setConfirm] = useState<string | null>(null)
  const [newBranch, setNewBranch] = useState<{ repo: string; name: string } | null>(null)
  const menu = useContextMenu()

  const repos = status?.repos ?? []
  const one = repos.length === 1 ? repos[0] : undefined

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
      await commitAll(client, repos, message, amend)
      setMessage('')
      setAmend(false)
    }, 'Committing…')

  // Amending starts from the last commit's message.
  const toggleAmend = (on: boolean) => {
    setAmend(on)
    const r = amendable(repos)
    if (on && r && !message.trim()) client.lastMessage(r.path).then(setMessage, () => {})
  }

  const changeMenu = (c: Change, stagedRow: boolean): MenuItem[] => {
    const name = c.path.split('/').pop() ?? c.path
    const deleted = (stagedRow ? c.index : c.worktree) === 'D'
    return [
      { label: 'Show Diff', onSelect: () => onDiff(c.path) },
      { label: 'Open File', disabled: deleted || c.submodule, onSelect: () => onOpen(c.path) },
      'separator',
      stagedRow
        ? { label: 'Unstage', onSelect: () => act(() => client.unstage(c.path)) }
        : { label: 'Stage', onSelect: () => act(() => client.stage(c.path)) },
      // A submodule's commit is moved in the submodule.
      ...(c.submodule
        ? []
        : [
            {
              label: c.index === '?' ? 'Delete Unversioned File' : 'Rollback',
              danger: true,
              confirm: c.index === '?' ? `Delete ${name}?` : `Discard all changes to ${name}?`,
              onSelect: () => act(() => client.rollback(c.path)),
            },
          ]),
      'separator',
      { label: 'Copy Path', onSelect: () => copyText(`${client.root}/${c.path}`) },
      { label: 'Copy Relative Path', onSelect: () => copyText(c.path) },
    ]
  }

  // branchMenu lists a repository's branches to switch to, loaded when it
  // opens.
  const [picker, setPicker] = useState<{ anchor: DOMRect; branches: Branches; repo: string } | null>(null)
  const branchMenu = async (e: MouseEvent, repo: string) => {
    const anchor = e.currentTarget.getBoundingClientRect()
    try {
      setPicker({ anchor, branches: await client.branches(repo), repo })
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err))
    }
  }
  // New Branch, named as typed when no branch has that name.
  const newBranchAction: PickerAction = (q) => {
    const repo = picker?.repo ?? ''
    return q && !picker?.branches.local.some((b) => b.name === q)
      ? { label: `New Branch “${q}”`, run: () => act(() => client.createBranch(q, repo), `Creating ${q}…`) }
      : { label: 'New Branch…', run: () => setNewBranch({ repo, name: '' }) }
  }

  const createBranch = () => {
    const name = newBranch?.name.trim()
    const repo = newBranch?.repo
    setNewBranch(null)
    if (name) act(() => client.createBranch(name, repo), `Creating ${name}…`)
  }

  const changes = status?.changes ?? []
  const staged = changes.filter(isStaged)

  const row = (r: Repo, c: Change, stagedRow: boolean) => {
    const letter = stagedRow ? c.index : c.worktree
    const key = `${stagedRow ? 's' : 'w'}:${c.path}`
    const shown = within(r, c)
    return (
      <div
        key={key}
        className="code-change"
        onClick={() => onDiff(c.path)}
        onContextMenu={(e) => menu.open(e, changeMenu(c, stagedRow))}
        title={c.submodule ? `${c.path}: its commit has moved` : c.from ? `${c.from} → ${c.path}` : c.path}
      >
        <span className={`code-change-letter code-change-${letterClass(letter)}`}>{letter === '?' ? 'U' : letter}</span>
        <span className="code-change-name">{c.path.split('/').pop()}</span>
        {c.submodule && <span className="code-change-tag">new commits</span>}
        <span className="code-change-dir muted">{changeWhere(shown)}</span>
        <span className="code-change-actions" onClick={(e) => e.stopPropagation()}>
          {stagedRow ? (
            <button
              type="button"
              className="code-tool-btn"
              title="Unstage"
              onClick={() => act(() => client.unstage(c.path))}
            >
              <Minus size={13} />
            </button>
          ) : (
            <button
              type="button"
              className="code-tool-btn"
              title="Stage"
              onClick={() => act(() => client.stage(c.path))}
            >
              <Plus size={13} />
            </button>
          )}
          {c.submodule ? null : confirm === key ? (
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

  const section = (
    r: Repo,
    title: string,
    list: Change[],
    stagedRows: boolean,
    bulk?: { label: string; run: () => Promise<void> },
  ) =>
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
        {list.map((c) => row(r, c, stagedRows))}
      </div>
    )

  // sections are a repository's changes: staged, not, and untracked.
  const sections = (r: Repo) => {
    const unstaged = r.changes.filter((c) => c.worktree !== '.' && c.index !== '?')
    const unversioned = r.changes.filter((c) => c.index === '?')
    const stageAll = async () => {
      for (const c of [...unstaged, ...unversioned]) await client.stage(c.path)
    }
    return (
      <>
        {section(r, 'Staged', r.changes.filter(isStaged), true)}
        {section(r, 'Changes', unstaged, false, unstaged.length ? { label: 'Stage all', run: stageAll } : undefined)}
        {section(r, 'Unversioned files', unversioned, false)}
      </>
    )
  }

  const branchButton = (r: Repo) =>
    r.branch && (
      <button
        type="button"
        className="code-branch-btn"
        title="Switch or create a branch"
        onClick={(e) => branchMenu(e, r.path)}
      >
        <GitBranch size={12} />
        {r.branch === '(detached)' ? 'detached HEAD' : r.branch}
        {!!r.ahead && <span> ↑{r.ahead}</span>}
        {!!r.behind && <span> ↓{r.behind}</span>}
        <ChevronDown size={11} />
      </button>
    )

  const remoteButtons = (r: Repo) => (
    <>
      <button
        type="button"
        className="code-tool-btn"
        title="Fetch"
        disabled={!!busy}
        onClick={() => act(() => client.fetch(r.path), 'Fetching…')}
      >
        <CloudDownload size={14} />
      </button>
      <button
        type="button"
        className="code-tool-btn"
        title="Pull (fast-forward only)"
        disabled={!!busy}
        onClick={() => act(() => client.pull(r.path), 'Pulling…')}
      >
        <ArrowDownToLine size={14} />
      </button>
      <button
        type="button"
        className="code-tool-btn"
        title="Push"
        disabled={!!busy}
        onClick={() => act(() => client.push(r.path), 'Pushing…')}
      >
        <ArrowUpFromLine size={14} />
      </button>
    </>
  )

  // group is one repository of several: its head, then its changes.
  const group = (r: Repo) => (
    <div key={r.path} className="code-repo">
      <div className="code-repo-head" title={r.path || client.root}>
        <FolderGit2 size={13} className="code-repo-icon" />
        <span className="code-repo-name">{r.name}</span>
        {r.kind !== 'folder' && <span className="muted small">{r.kind}</span>}
        {branchButton(r)}
        <span className="code-tool-spacer" />
        {!r.uninitialised && !r.error && remoteButtons(r)}
      </div>
      {r.uninitialised ? (
        <div className="code-note">
          Not checked out.{' '}
          <button
            type="button"
            className="code-link"
            disabled={!!busy}
            onClick={() => act(() => client.initSubmodule(r.path), `Checking out ${r.name}…`)}
          >
            Check out
          </button>
        </div>
      ) : r.error ? (
        <div className="code-note code-note-error">{r.error}</div>
      ) : r.changes.length === 0 ? (
        <div className="code-note">No changes.</div>
      ) : (
        sections(r)
      )}
    </div>
  )

  const target = amendable(repos)
  const canCommit = amend ? !!target : staged.length > 0 && message.trim() !== ''
  const into = new Set(repos.filter((r) => r.changes.some(isStaged)).map((r) => r.path)).size

  return (
    <ToolWindow
      title="Changes"
      extra={
        <>
          {projectPicker}
          {one && branchButton(one)}
        </>
      }
      actions={
        status?.repo && (
          <>
            {one && remoteButtons(one)}
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
            value={newBranch.name}
            onChange={(e) => setNewBranch({ ...newBranch, name: e.target.value })}
            onKeyDown={(e) => e.key === 'Escape' && setNewBranch(null)}
          />
          <button type="submit" disabled={!newBranch.name.trim()}>
            Create
          </button>
        </form>
      )}
      {picker && (
        <BranchPicker
          anchor={picker.anchor}
          branches={picker.branches}
          checked={picker.branches.current}
          actions={[newBranchAction]}
          onPick={(name) =>
            name !== picker.branches.current && act(() => client.checkout(name, picker.repo), `Checking out ${name}…`)
          }
          onClose={() => setPicker(null)}
        />
      )}
      {status?.repo && (
        <div className="code-changes-mode" role="tablist">
          <button type="button" role="tab" aria-selected={view === 'local'} onClick={() => setView('local')}>
            Local
          </button>
          <button
            type="button"
            role="tab"
            aria-selected={view === 'branch'}
            title="Everything the branch has changed since it left the main branch"
            onClick={() => setView('branch')}
          >
            Branch
          </button>
        </div>
      )}
      {view === 'branch' && status?.repo ? (
        <>
          {usable(repos).length > 1 && (
            <div className="code-branch-base">
              <span className="muted small">Repository</span>
              <RepoPicker repos={repos} value={branchRepo} onPick={setBranchRepo} />
            </div>
          )}
          <BranchChanges
            key={branchRepo}
            client={client}
            status={status}
            repo={usable(repos).some((r) => r.path === branchRepo) ? branchRepo : usable(repos)[0]?.path}
            onDiff={onDiff}
            onOpen={onOpen}
          />
        </>
      ) : (
        <div className="code-changes">
          {status && !status.repo && <div className="code-note">This folder is not a git repository.</div>}
          {one ? (
            <>
              {one.changes.length === 0 && !one.error && <div className="code-note">No changes.</div>}
              {one.error && <div className="code-note code-note-error">{one.error}</div>}
              {sections(one)}
            </>
          ) : (
            repos.map(group)
          )}
        </div>
      )}
      {status?.repo && view === 'local' && (
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
            <label
              className="code-amend"
              title={
                target || !amend
                  ? 'Replace the last commit with this one'
                  : 'Stage changes in one repository to amend its last commit'
              }
            >
              <input type="checkbox" checked={amend} onChange={(e) => toggleAmend(e.target.checked)} />
              Amend{amend && target && repos.length > 1 ? ` ${target.name}` : ''}
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
              {amend ? 'Amend' : 'Commit'}{' '}
              {staged.length > 0 && `${staged.length} file${staged.length === 1 ? '' : 's'}`}
              {into > 1 && !amend && ` in ${into} repositories`}
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
