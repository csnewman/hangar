import { EditorState } from '@codemirror/state'
import { EditorView, lineNumbers } from '@codemirror/view'
import {
  ArrowDownToLine,
  ArrowUpFromLine,
  ChevronLeft,
  ChevronRight,
  CloudDownload,
  File,
  Folder,
  GitBranch,
  GitCommitHorizontal,
  Minus,
  Plus,
  WrapText,
} from 'lucide-react'
import { useCallback, useEffect, useRef, useState, type ReactNode } from 'react'
import { useLocation, useNavigate, useSearchParams } from 'react-router'

import { formatAgo } from '../components/format'
import { changeWhere } from './BranchChanges'
import type { BranchChanges, Change, CodeClient, CommitInfo, Entry, GitStatus, LogCommit } from './client'
import { codeHighlight, codeTheme, followScheme, languageFor } from './CodeEditor'
import { DiffView } from './DiffView'
import { useGitStatus } from './useGitStatus'

// PhoneCode is the Code tab on a phone: what a phone is for there, reading
// rather than writing. Its first screen has the branch, with fetch, pull and
// push, and four lists: the local changes, staged and committed from it;
// what the branch has changed since it left the main branch; the history;
// and the files. A change opens its diff, a commit its files, a file its
// text, each a screen of its own.
//
// Which screen shows is in the URL, so the phone's back gesture leaves a
// diff for the list it came from, and a reload keeps it.
export function PhoneCode({ client }: { client: CodeClient }) {
  const [params, setParams] = useSearchParams()
  const navigate = useNavigate()
  const location = useLocation()
  const [status, reload] = useGitStatus(client)

  const go = (next: Record<string, string | undefined>) =>
    setParams(Object.fromEntries(Object.entries(next).filter((e): e is [string, string] => !!e[1])))
  // Back to the screen before, or the first screen when this one was
  // opened by a link.
  const back = () => (location.key !== 'default' ? navigate(-1) : setParams({}, { replace: true }))

  const diff = params.get('diff')
  if (diff !== null) {
    return (
      <DiffScreen
        client={client}
        path={diff}
        base={params.get('base') ?? undefined}
        baseLabel={params.get('bl') ?? undefined}
        from={params.get('from') ?? undefined}
        to={params.get('to') ?? undefined}
        toLabel={params.get('tl') ?? undefined}
        onBack={back}
      />
    )
  }
  const file = params.get('file')
  if (file !== null) return <FileScreen client={client} path={file} onBack={back} />
  const commit = params.get('commit')
  if (commit !== null) {
    return (
      <CommitScreen
        client={client}
        hash={commit}
        onBack={back}
        onDiff={(path, d) => go({ diff: path, ...d })}
        onCommit={(hash) => go({ commit: hash })}
      />
    )
  }

  const view = (params.get('view') ?? 'changes') as View
  return (
    <Home
      client={client}
      status={status}
      reload={reload}
      view={view}
      dir={params.get('dir') ?? ''}
      onView={(v) => setParams(v === 'changes' ? {} : { view: v }, { replace: true })}
      onDir={(dir) => go({ view: 'files', dir })}
      onDiff={(path, d) => go({ diff: path, ...d })}
      onCommit={(hash) => go({ commit: hash })}
      onFile={(path) => go({ file: path })}
    />
  )
}

type View = 'changes' | 'branch' | 'history' | 'files'
const views: { id: View; label: string }[] = [
  { id: 'changes', label: 'Changes' },
  { id: 'branch', label: 'Branch' },
  { id: 'history', label: 'History' },
  { id: 'files', label: 'Files' },
]

// Against is what a diff compares the file with, as the URL carries it:
// a commit (base) and its label (bl), the file's name there (from), and a
// later commit (to) and its label (tl) in place of the working tree.
type Against = { base?: string; bl?: string; from?: string; to?: string; tl?: string }

function Home({
  client,
  status,
  reload,
  view,
  dir,
  onView,
  onDir,
  onDiff,
  onCommit,
  onFile,
}: {
  client: CodeClient
  status: GitStatus | null
  reload: () => void
  view: View
  dir: string
  onView: (v: View) => void
  onDir: (dir: string) => void
  onDiff: (path: string, against?: Against) => void
  onCommit: (hash: string) => void
  onFile: (path: string) => void
}) {
  const [busy, setBusy] = useState('')
  const [error, setError] = useState('')
  // act runs a git action, saying what it is while it runs and its error
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

  const repo = status?.repo
  return (
    <div className="phone-code">
      {repo && (
        <div className="phone-code-branch">
          <GitBranch size={16} />
          <span className="phone-code-branch-name">
            {status.branch === '(detached)' ? 'detached HEAD' : status.branch}
          </span>
          {!!status.ahead && <span className="muted">↑{status.ahead}</span>}
          {!!status.behind && <span className="muted">↓{status.behind}</span>}
          <span className="code-tool-spacer" />
          <IconButton label="Fetch" disabled={!!busy} onClick={() => act(() => client.fetch(), 'Fetching…')}>
            <CloudDownload size={19} />
          </IconButton>
          <IconButton
            label="Pull (fast-forward only)"
            disabled={!!busy}
            onClick={() => act(() => client.pull(), 'Pulling…')}
          >
            <ArrowDownToLine size={19} />
          </IconButton>
          <IconButton label="Push" disabled={!!busy} onClick={() => act(() => client.push(), 'Pushing…')}>
            <ArrowUpFromLine size={19} />
          </IconButton>
        </div>
      )}
      {(busy || error) && <div className={busy ? 'phone-code-busy' : 'notice phone-code-error'}>{busy || error}</div>}
      <div className="phone-segments" role="tablist">
        {views
          .filter((v) => repo || v.id === 'files')
          .map((v) => (
            <button key={v.id} type="button" role="tab" aria-selected={view === v.id} onClick={() => onView(v.id)}>
              {v.label}
              {v.id === 'changes' && !!status?.changes.length && (
                <span className="phone-segment-count">{status.changes.length}</span>
              )}
            </button>
          ))}
      </div>
      {status && !repo && view !== 'files' ? (
        <div className="phone-empty">
          This folder is not a git repository.{' '}
          <button type="button" className="code-link" onClick={() => onView('files')}>
            See its files
          </button>
        </div>
      ) : view === 'files' ? (
        <Files client={client} dir={dir} status={status} onDir={onDir} onFile={onFile} />
      ) : view === 'history' ? (
        <History client={client} status={status} onCommit={onCommit} />
      ) : view === 'branch' ? (
        <Branch client={client} status={status} onDiff={onDiff} onCommit={onCommit} />
      ) : (
        status && <Local client={client} status={status} busy={!!busy} act={act} onDiff={onDiff} />
      )}
    </div>
  )
}

// Local is the working tree's changes, staged or not, each opening its
// diff with HEAD, and a box to commit what is staged.
function Local({
  client,
  status,
  busy,
  act,
  onDiff,
}: {
  client: CodeClient
  status: GitStatus
  busy: boolean
  act: (fn: () => Promise<void>, doing?: string) => Promise<void>
  onDiff: (path: string) => void
}) {
  const [message, setMessage] = useState('')
  const [amend, setAmend] = useState(false)

  const changes = status.changes
  const staged = changes.filter((c) => c.index !== '.' && c.index !== '?' && c.index !== 'U')
  const unstaged = changes.filter((c) => c.worktree !== '.' && c.index !== '?')
  const unversioned = changes.filter((c) => c.index === '?')
  const canCommit = amend || (staged.length > 0 && message.trim() !== '')

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
  const stageAll = async () => {
    for (const c of [...unstaged, ...unversioned]) await client.stage(c.path)
  }

  const row = (c: Change, stagedRow: boolean) => {
    const letter = stagedRow ? c.index : c.worktree
    return (
      <div key={`${stagedRow ? 's' : 'w'}:${c.path}`} className="phone-row phone-change">
        <button type="button" className="phone-change-open" onClick={() => onDiff(c.path)}>
          <span className={`phone-change-letter code-change-${letterClass(letter)}`}>
            {letter === '?' ? 'U' : letter}
          </span>
          <span className="phone-row-main">
            <span className="phone-change-name">{c.path.split('/').pop()}</span>
            {changeWhere(c) && <Tail text={changeWhere(c)} />}
          </span>
        </button>
        {stagedRow ? (
          <IconButton label={`Unstage ${c.path}`} disabled={busy} onClick={() => act(() => client.unstage(c.path))}>
            <Minus size={19} />
          </IconButton>
        ) : (
          <IconButton label={`Stage ${c.path}`} disabled={busy} onClick={() => act(() => client.stage(c.path))}>
            <Plus size={19} />
          </IconButton>
        )}
      </div>
    )
  }

  const section = (title: string, list: Change[], stagedRows: boolean, extra?: ReactNode) =>
    list.length > 0 && (
      <>
        <div className="phone-heading phone-code-heading">
          <span>
            {title} · {list.length}
          </span>
          {extra}
        </div>
        <div className="phone-list">{list.map((c) => row(c, stagedRows))}</div>
      </>
    )

  return (
    <>
      {changes.length === 0 && <div className="phone-empty">No changes.</div>}
      {section('Staged', staged, true)}
      {section(
        'Changes',
        unstaged,
        false,
        <button type="button" className="code-link" disabled={busy} onClick={() => act(stageAll)}>
          Stage all
        </button>,
      )}
      {section('Unversioned', unversioned, false)}
      <div className="phone-card phone-commit">
        <textarea
          className="phone-commit-message"
          rows={3}
          placeholder={amend || staged.length ? 'Commit message' : 'Stage changes to commit them'}
          value={message}
          onChange={(e) => setMessage(e.target.value)}
        />
        <div className="phone-commit-bar">
          <label className="phone-commit-amend">
            <input type="checkbox" checked={amend} onChange={(e) => toggleAmend(e.target.checked)} />
            Amend
          </label>
          <button type="button" className="btn btn-primary" disabled={busy || !canCommit} onClick={commit}>
            {amend ? 'Amend' : 'Commit'}
            {staged.length > 0 && ` ${staged.length} file${staged.length === 1 ? '' : 's'}`}
          </button>
        </div>
      </div>
    </>
  )
}

// Branch is what the branch has changed since it left the main branch:
// its commits, and the files, each opening its diff with the main branch
// where the branch left it.
function Branch({
  client,
  status,
  onDiff,
  onCommit,
}: {
  client: CodeClient
  status: GitStatus | null
  onDiff: (path: string, against: Against) => void
  onCommit: (hash: string) => void
}) {
  const [data, setData] = useState<BranchChanges | null>(null)
  const [error, setError] = useState('')
  // Again whenever what git reports changes: the status is reloaded then.
  useEffect(() => {
    let live = true
    client.branchChanges().then(
      (d) => {
        if (!live) return
        setData(d)
        setError('')
      },
      (e: Error) => live && setError(e.message),
    )
    return () => {
      live = false
    }
  }, [client, status])

  if (error) return <div className="notice phone-code-error">{error}</div>
  if (!data) return null
  const label = `${data.base} at ${data.mergeBase.slice(0, 7)}`
  return (
    <>
      <p className="phone-code-note muted">
        Compared with {data.base}, where this branch left it at <code>{data.mergeBase.slice(0, 7)}</code>.
      </p>
      <div className="phone-heading">Commits · {data.commits.length}</div>
      <div className="phone-list">
        {data.commits.map((c) => (
          <CommitRow key={c.hash} commit={c} onOpen={() => onCommit(c.hash)} />
        ))}
        {data.commits.length === 0 && <div className="phone-empty phone-list-empty">No commits yet.</div>}
      </div>
      <div className="phone-heading">Files changed · {data.changes.length}</div>
      <div className="phone-list">
        {data.changes.map((c) => (
          <ChangeRow
            key={c.path}
            change={c}
            onOpen={() => onDiff(c.path, { base: data.mergeBase, bl: label, from: c.from })}
          />
        ))}
        {data.changes.length === 0 && (
          <div className="phone-empty phone-list-empty">Nothing differs from {data.base}.</div>
        )}
      </div>
    </>
  )
}

// History is the commits reachable from HEAD, newest first, a page at a
// time.
function History({
  client,
  status,
  onCommit,
}: {
  client: CodeClient
  status: GitStatus | null
  onCommit: (hash: string) => void
}) {
  const [commits, setCommits] = useState<LogCommit[] | null>(null)
  const [more, setMore] = useState(false)
  const [error, setError] = useState('')
  const [loading, setLoading] = useState(false)
  const page = 50

  const load = useCallback(
    async (skip: number) => {
      try {
        const log = await client.log('HEAD', skip, page)
        setCommits((c) => (skip === 0 ? log.commits : [...(c ?? []), ...log.commits]))
        setMore(log.more)
        setError('')
      } catch (e) {
        setError(e instanceof Error ? e.message : String(e))
      } finally {
        setLoading(false)
      }
    },
    [client],
  )
  // From the top again whenever the repository changes: HEAD may have
  // moved.
  const head = `${status?.branch}:${status?.ahead}:${status?.behind}`
  useEffect(() => {
    load(0)
  }, [load, head])

  if (error) return <div className="notice phone-code-error">{error}</div>
  if (!commits) return null
  return (
    <>
      <div className="phone-list phone-code-first">
        {commits.map((c) => (
          <CommitRow key={c.hash} commit={c} onOpen={() => onCommit(c.hash)} />
        ))}
        {commits.length === 0 && <div className="phone-empty phone-list-empty">No commits yet.</div>}
      </div>
      {more && (
        <button
          type="button"
          className="btn phone-code-more"
          disabled={loading}
          onClick={() => {
            setLoading(true)
            load(commits.length)
          }}
        >
          {loading ? 'Loading…' : 'Show more'}
        </button>
      )}
    </>
  )
}

// Files is a folder of the working tree, its folders first.
function Files({
  client,
  dir,
  status,
  onDir,
  onFile,
}: {
  client: CodeClient
  dir: string
  status: GitStatus | null
  onDir: (dir: string) => void
  onFile: (path: string) => void
}) {
  const [entries, setEntries] = useState<Entry[] | null>(null)
  const [error, setError] = useState('')
  useEffect(() => {
    let live = true
    client.list(dir).then(
      (list) => {
        if (!live) return
        setEntries([...list].sort((a, b) => Number(b.dir) - Number(a.dir) || a.name.localeCompare(b.name)))
        setError('')
      },
      (e: Error) => live && setError(e.message),
    )
    return () => {
      live = false
    }
  }, [client, dir, status])

  // A file's change, and a folder holding changes, are marked as in the
  // Changes list.
  const changed = new Map(
    (status?.changes ?? []).map((c) => [c.path, c.index === '?' ? '?' : c.worktree !== '.' ? c.worktree : c.index]),
  )
  const holdsChanges = (path: string) => (status?.changes ?? []).some((c) => c.path.startsWith(path + '/'))
  const join = (name: string) => (dir ? `${dir}/${name}` : name)
  const parts = dir ? dir.split('/') : []

  return (
    <>
      <div className="phone-code-crumbs">
        <button type="button" className="code-link" onClick={() => onDir('')}>
          {client.root.split('/').pop() || '/'}
        </button>
        {parts.map((p, i) => (
          <span key={i}>
            {' / '}
            <button type="button" className="code-link" onClick={() => onDir(parts.slice(0, i + 1).join('/'))}>
              {p}
            </button>
          </span>
        ))}
      </div>
      {error && <div className="notice phone-code-error">{error}</div>}
      {entries && (
        <div className="phone-list">
          {entries.map((e) => {
            const path = join(e.name)
            const letter = e.dir ? (holdsChanges(path) ? '•' : '') : changed.get(path)
            return (
              <button
                key={e.name}
                type="button"
                className={e.ignored ? 'phone-row phone-file phone-file-ignored' : 'phone-row phone-file'}
                onClick={() => (e.dir ? onDir(path) : onFile(path))}
              >
                {e.dir ? <Folder size={18} /> : <File size={18} />}
                <span className="phone-row-main phone-change-name">{e.name}</span>
                {letter && (
                  <span className={`phone-change-letter code-change-${letterClass(letter)}`}>
                    {letter === '?' ? 'U' : letter}
                  </span>
                )}
                {e.dir && <ChevronRight size={16} className="phone-row-chev" />}
              </button>
            )
          })}
          {entries.length === 0 && <div className="phone-empty phone-list-empty">This folder is empty.</div>}
        </div>
      )}
    </>
  )
}

// CommitScreen is a commit: its message, who and when, and the files it
// changed, each opening its diff with the commit's first parent.
function CommitScreen({
  client,
  hash,
  onBack,
  onDiff,
  onCommit,
}: {
  client: CodeClient
  hash: string
  onBack: () => void
  onDiff: (path: string, against: Against) => void
  onCommit: (hash: string) => void
}) {
  const [info, setInfo] = useState<CommitInfo | null>(null)
  const [error, setError] = useState('')
  useEffect(() => {
    let live = true
    client.commitInfo(hash).then(
      (i) => {
        if (!live) return
        setInfo(i)
        setError('')
      },
      (e: Error) => live && setError(e.message),
    )
    return () => {
      live = false
    }
  }, [client, hash])

  const short = hash.slice(0, 8)
  const [subject, ...body] = (info?.message ?? '').split('\n')
  return (
    <div className="phone-code-screen">
      <ScreenBar title={`Commit ${short}`} onBack={onBack} />
      <div className="phone-code">
        {error && <div className="notice phone-code-error">{error}</div>}
        {info && (
          <>
            <div className="phone-card phone-commit-info">
              <div className="phone-commit-subject">{subject}</div>
              {body.join('\n').trim() && <pre className="phone-commit-body">{body.join('\n').trim()}</pre>}
              <div className="phone-row-sub">
                {info.author}, {formatAgo(new Date(info.time * 1000).toISOString())}
                {info.committer !== info.author && ` · committed by ${info.committer}`}
              </div>
              {info.parents.length > 0 && (
                <div className="phone-row-sub">
                  {info.parents.length > 1 ? 'Parents ' : 'Parent '}
                  {info.parents.map((p, i) => (
                    <span key={p}>
                      {i > 0 && ', '}
                      <button type="button" className="code-link" onClick={() => onCommit(p)}>
                        {p.slice(0, 8)}
                      </button>
                    </span>
                  ))}
                </div>
              )}
            </div>
            <div className="phone-heading">Files · {info.files.length}</div>
            <div className="phone-list">
              {info.files.map((f) => (
                <ChangeRow
                  key={f.path}
                  change={f}
                  onOpen={() =>
                    onDiff(f.path, {
                      base: info.parents[0] ?? `${info.hash}^`,
                      bl: info.parents[0] ? info.parents[0].slice(0, 8) : 'nothing',
                      to: info.hash,
                      tl: short,
                      from: f.from,
                    })
                  }
                />
              ))}
              {info.files.length === 0 && (
                <div className="phone-empty phone-list-empty">No files changed in this folder.</div>
              )}
            </div>
          </>
        )}
      </div>
    </div>
  )
}

// DiffScreen is a file's diff, one column wide, its lines wrapped unless
// asked not to be.
function DiffScreen({
  client,
  path,
  onBack,
  ...against
}: {
  client: CodeClient
  path: string
  base?: string
  baseLabel?: string
  from?: string
  to?: string
  toLabel?: string
  onBack: () => void
}) {
  const [wrap, setWrap] = useWrap()
  return (
    <div className="phone-code-screen">
      <ScreenBar title={path.split('/').pop() ?? path} sub={dirOf(path)} onBack={onBack}>
        <WrapButton wrap={wrap} onWrap={setWrap} />
      </ScreenBar>
      <div className="phone-code-fill">
        <DiffView client={client} path={path} {...against} unified wrap={wrap} />
      </div>
    </div>
  )
}

// FileScreen is a file's text, to read.
function FileScreen({ client, path, onBack }: { client: CodeClient; path: string; onBack: () => void }) {
  const [wrap, setWrap] = useWrap()
  const host = useRef<HTMLDivElement>(null)
  const [error, setError] = useState('')
  useEffect(() => {
    const el = host.current
    if (!el) return
    let view: EditorView | null = null
    let live = true
    Promise.all([client.read(path), languageFor(path.split('/').pop() ?? '')]).then(
      ([text, lang]) => {
        if (!live) return
        if (text === null) {
          setError('This file is too large to show, or not text.')
          return
        }
        view = new EditorView({
          parent: el,
          doc: text,
          extensions: [
            lineNumbers(),
            codeHighlight,
            followScheme,
            codeTheme,
            lang,
            EditorState.readOnly.of(true),
            EditorView.editable.of(false),
            wrap ? EditorView.lineWrapping : [],
          ],
        })
      },
      (e: Error) => live && setError(e.message),
    )
    return () => {
      live = false
      view?.destroy()
    }
  }, [client, path, wrap])
  return (
    <div className="phone-code-screen">
      <ScreenBar title={path.split('/').pop() ?? path} sub={dirOf(path)} onBack={onBack}>
        <WrapButton wrap={wrap} onWrap={setWrap} />
      </ScreenBar>
      {error && <div className="phone-empty">{error}</div>}
      <div ref={host} className="phone-code-fill phone-code-text" hidden={!!error} />
    </div>
  )
}

// useWrap is whether long lines wrap, remembered for every file.
function useWrap(): [boolean, (on: boolean) => void] {
  const key = 'hangar.code.phone.wrap'
  const [wrap, setWrap] = useState(() => {
    try {
      return localStorage.getItem(key) !== 'off'
    } catch {
      return true
    }
  })
  const set = (on: boolean) => {
    setWrap(on)
    try {
      localStorage.setItem(key, on ? 'on' : 'off')
    } catch {
      // Wrapping is chosen again next time.
    }
  }
  return [wrap, set]
}

function WrapButton({ wrap, onWrap }: { wrap: boolean; onWrap: (on: boolean) => void }) {
  return (
    <button
      type="button"
      className={wrap ? 'phone-icon-btn phone-icon-btn-on' : 'phone-icon-btn'}
      aria-pressed={wrap}
      aria-label="Wrap long lines"
      title="Wrap long lines"
      onClick={() => onWrap(!wrap)}
    >
      <WrapText size={20} />
    </button>
  )
}

// ScreenBar heads a screen of the Code tab, with the way back to the
// screen before.
function ScreenBar({
  title,
  sub,
  onBack,
  children,
}: {
  title: string
  sub?: string
  onBack: () => void
  children?: ReactNode
}) {
  return (
    <div className="phone-code-bar">
      <button type="button" className="phone-icon-btn" aria-label="Back" onClick={onBack}>
        <ChevronLeft size={24} />
      </button>
      <div className="phone-code-bar-title">
        <span>{title}</span>
        {sub && <Tail text={sub} />}
      </div>
      {children}
    </div>
  )
}

// Tail is a path under a name, cut at its start when too long, since its
// end says the most.
function Tail({ text }: { text: string }) {
  return (
    <span className="phone-row-sub phone-tail">
      <bdi>{text}</bdi>
    </span>
  )
}

function CommitRow({
  commit,
  onOpen,
}: {
  commit: { hash: string; subject: string; author: string; time: number }
  onOpen: () => void
}) {
  return (
    <button type="button" className="phone-row" onClick={onOpen}>
      <GitCommitHorizontal size={18} />
      <span className="phone-row-main">
        <span className="phone-commit-row-subject">{commit.subject}</span>
        <span className="phone-row-sub">
          {commit.author}, {formatAgo(new Date(commit.time * 1000).toISOString())} ·{' '}
          <code>{commit.hash.slice(0, 7)}</code>
        </span>
      </span>
      <ChevronRight size={16} className="phone-row-chev" />
    </button>
  )
}

function ChangeRow({
  change,
  onOpen,
}: {
  change: { path: string; status: string; from?: string }
  onOpen: () => void
}) {
  return (
    <button type="button" className="phone-row" onClick={onOpen}>
      <span className={`phone-change-letter code-change-${letterClass(change.status)}`}>{change.status}</span>
      <span className="phone-row-main">
        <span className="phone-change-name">{change.path.split('/').pop()}</span>
        {changeWhere(change) && <Tail text={changeWhere(change)} />}
      </span>
      <ChevronRight size={16} className="phone-row-chev" />
    </button>
  )
}

function IconButton({
  label,
  disabled,
  onClick,
  children,
}: {
  label: string
  disabled?: boolean
  onClick: () => void
  children: ReactNode
}) {
  return (
    <button
      type="button"
      className="phone-icon-btn"
      aria-label={label}
      title={label}
      disabled={disabled}
      onClick={onClick}
    >
      {children}
    </button>
  )
}

function letterClass(l: string) {
  return { A: 'added', C: 'added', D: 'deleted', R: 'renamed', U: 'conflict', '?': 'unversioned' }[l] ?? 'modified'
}

const dirOf = (p: string) => (p.includes('/') ? p.slice(0, p.lastIndexOf('/')) : '')
