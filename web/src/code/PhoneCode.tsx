import { EditorState } from '@codemirror/state'
import { EditorView, lineNumbers } from '@codemirror/view'
import {
  ArrowDownToLine,
  ArrowUpFromLine,
  ChevronDown,
  ChevronLeft,
  ChevronRight,
  ChevronUp,
  Eye,
  HardDrive,
  House,
  File,
  Folder,
  FolderGit2,
  GitBranch,
  GitCommitHorizontal,
  History as HistoryIcon,
  Table2,
  WrapText,
} from 'lucide-react'
import { useCallback, useEffect, useRef, useState, type ReactNode } from 'react'
import { Link, useLocation, useNavigate, useSearchParams } from 'react-router'

import type { Environment } from '../api'
import { formatAgo } from '../components/format'
import { useEnv } from '../pages/Environment'
import { changeWhere } from './BranchChanges'
import type { Change, CodeClient, CommitInfo, Entry, GitStatus, LogCommit, Repo } from './client'
import { codeHighlight, codeTheme, followScheme, languageFor } from './CodeEditor'
import { DiffView, type DiffStats } from './DiffView'
import { isMarkdown, MarkdownPreview } from './MarkdownPreview'
import { commitAll, usable, within } from './repos'
import { followSettings, spelling } from './spell'
import { isTable, TableView } from './TableView'
import { useGitStatus } from './useGitStatus'

// PhoneCode is the Code tab on a phone, as screens a phone moves between,
// each with one header and one way back. Its first is Changes: what is
// uncommitted, each ticked to go in the next commit, and the commits not
// yet pushed. A change opens its diff, stepping through the others from
// there; a commit opens its files. Files, the environment's screen's other
// way in here, browses the working tree.
//
// Which screen shows is in the URL, so the phone's back gesture leaves a
// diff for the list it came from, and a reload keeps it.
export function PhoneCode({ client }: { client: CodeClient }) {
  const env = useEnv()
  const [params, setParams] = useSearchParams()
  const navigate = useNavigate()
  const location = useLocation()
  const [status, reload] = useGitStatus(client)

  const go = (next: Record<string, string | undefined>, replace = false) =>
    setParams(Object.fromEntries(Object.entries(next).filter((e): e is [string, string] => !!e[1])), { replace })
  // Back to the screen before, or to Changes when this one was opened by
  // a link.
  const back = () => (location.key !== 'default' ? navigate(-1) : setParams({}, { replace: true }))

  const diff = params.get('diff')
  if (diff !== null) {
    const against: Against = {
      base: params.get('base') ?? undefined,
      bl: params.get('bl') ?? undefined,
      from: params.get('from') ?? undefined,
      to: params.get('to') ?? undefined,
      tl: params.get('tl') ?? undefined,
      repo: params.get('repo') ?? undefined,
    }
    return (
      <DiffScreen
        key={`${diff}:${against.to}`}
        client={client}
        status={status}
        reload={reload}
        path={diff}
        against={against}
        onBack={back}
        // Stepping replaces the diff, so back still leads to the list.
        onStep={(path, a) => go({ diff: path, ...a }, true)}
      />
    )
  }
  const file = params.get('file')
  if (file !== null)
    return <FileScreen client={client} path={file} onBack={back} onFile={(path) => go({ file: path })} />
  const repo = params.get('repo') ?? undefined
  const commit = params.get('commit')
  if (commit !== null) {
    return (
      <CommitScreen
        client={client}
        hash={commit}
        repo={repo}
        onBack={back}
        onDiff={(path, a) => go({ diff: path, ...a })}
        onCommit={(hash) => go({ commit: hash, repo })}
      />
    )
  }
  const view = params.get('view')
  if (view === 'history') {
    return (
      <HistoryScreen
        client={client}
        status={status}
        repo={repo ?? ''}
        onRepo={(r) => go({ view: 'history', repo: r }, true)}
        onBack={back}
        onCommit={(hash) => go({ commit: hash, repo })}
      />
    )
  }
  if (view === 'files') {
    return (
      <FilesScreen
        env={env}
        client={client}
        status={status}
        dir={params.get('dir') ?? ''}
        onDir={(dir) => go({ view: 'files', dir })}
        onFile={(path) => go({ file: path })}
      />
    )
  }
  return (
    <ChangesScreen
      env={env}
      client={client}
      status={status}
      reload={reload}
      onDiff={(path) => go({ diff: path })}
      onCommit={(hash, repo) => go({ commit: hash, repo })}
      onHistory={(repo) => go({ view: 'history', repo })}
    />
  )
}

// Against is what a diff compares the file with, as the URL carries it:
// a commit (base) and its label (bl), the file's name there (from), and a
// later commit (to) and its label (tl) in place of the working tree, and
// the repository those commits are in. A diff with none of them is the
// working tree's with HEAD.
type Against = { base?: string; bl?: string; from?: string; to?: string; tl?: string; repo?: string }

// staged is whether a change has anything staged, and partly whether
// some of it is not.
const staged = (c: Change) => c.index !== '.' && c.index !== '?'
const partly = (c: Change) => staged(c) && c.worktree !== '.'

// ChangesScreen is the first screen: the branch, what is uncommitted, a
// box to commit what is ticked, and the commits not yet pushed. A folder
// holding several repositories shows each as a group of its own, with
// its branch and what it has to push or pull; one message commits every
// group with files ticked.
function ChangesScreen({
  env,
  client,
  status,
  reload,
  onDiff,
  onCommit,
  onHistory,
}: {
  env: Environment
  client: CodeClient
  status: GitStatus | null
  reload: () => void
  onDiff: (path: string) => void
  onCommit: (hash: string, repo?: string) => void
  onHistory: (repo?: string) => void
}) {
  const [busy, setBusy] = useState('')
  const [error, setError] = useState('')
  const [message, setMessage] = useState('')
  // act runs a git action, saying what it is while it runs and its error
  // if it fails.
  const act = async (fn: () => Promise<void>, doing = '') => {
    setError('')
    setBusy(doing || 'Working…')
    try {
      await fn()
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e))
    } finally {
      setBusy('')
      reload()
    }
  }

  const repos = status?.repos ?? []
  const one = repos.length === 1 ? repos[0] : undefined
  const unpushed = useUnpushed(client, one)
  const changes = status?.changes ?? []
  const included = changes.filter(staged)
  const into = repos.filter((r) => r.changes.some(staged)).length
  const commit = () =>
    act(async () => {
      await commitAll(client, repos, message, false)
      setMessage('')
    }, 'Committing…')

  const rows = (r: Repo) =>
    r.changes.map((c) => (
      <ChangeRow key={c.path} change={c} repo={r} busy={!!busy} act={act} client={client} onDiff={onDiff} />
    ))

  return (
    <div className="phone-code">
      <TopHead env={env} title="Changes">
        {one ? (
          <span className="phone-chip">
            <GitBranch size={15} />
            <span>
              {branchName(one)} · {one.path ? `${client.root}/${one.path}` : client.root}
            </span>
          </span>
        ) : (
          repos.length > 1 && (
            <span className="phone-chip">
              <FolderGit2 size={15} />
              <span>
                {client.root} · {repos.length} repositories
              </span>
            </span>
          )
        )}
      </TopHead>
      {status && !status.repo && (
        <div className="phone-empty">
          {client.root} is not a git repository. <Link to="?view=files">See its files</Link>
        </div>
      )}
      {(busy || error) && <div className={busy ? 'phone-code-busy' : 'notice phone-code-error'}>{busy || error}</div>}
      {one && (
        <>
          <h2 className="phone-heading">Uncommitted · {changes.length}</h2>
          {one.error && <div className="notice phone-code-error">{one.error}</div>}
          {changes.length > 0 ? (
            <div className="phone-list">{rows(one)}</div>
          ) : (
            !one.error && <div className="phone-empty phone-code-none">Nothing to commit.</div>
          )}
        </>
      )}
      {repos.length > 1 &&
        repos.map((r) => (
          <section key={r.path} className="phone-repo">
            <div className="phone-repo-head">
              <FolderGit2 size={16} className="phone-repo-icon" />
              <span className="phone-repo-name">{r.name}</span>
              {r.kind !== 'folder' && <span className="phone-row-sub">{r.kind}</span>}
              {r.branch && (
                <span className="phone-repo-branch">
                  <GitBranch size={13} />
                  <span>{branchName(r)}</span>
                  {!!r.ahead && <span className="phone-row-sub">↑{r.ahead}</span>}
                  {!!r.behind && <span className="phone-row-sub">↓{r.behind}</span>}
                </span>
              )}
            </div>
            <div className="phone-list">
              {r.uninitialised ? (
                <div className="phone-row phone-repo-note">
                  <span className="phone-row-main phone-row-sub">Named, but never cloned.</span>
                  <button
                    type="button"
                    className="btn btn-sm"
                    disabled={!!busy}
                    onClick={() => act(() => client.initSubmodule(r.path), `Checking out ${r.name}…`)}
                  >
                    Check out
                  </button>
                </div>
              ) : r.error ? (
                <div className="phone-row phone-repo-note phone-row-sub">{r.error}</div>
              ) : r.changes.length === 0 ? (
                <div className="phone-row phone-repo-note phone-row-sub">Nothing to commit</div>
              ) : (
                rows(r)
              )}
              {!!r.ahead && (
                <div className="phone-row phone-repo-note">
                  <button type="button" className="phone-repo-count" onClick={() => onHistory(r.path)}>
                    {r.ahead} commit{r.ahead === 1 ? '' : 's'} not pushed
                  </button>
                  <button
                    type="button"
                    className="btn btn-sm"
                    disabled={!!busy}
                    onClick={() => act(() => client.push(r.path), `Pushing ${r.name}…`)}
                  >
                    <ArrowUpFromLine size={15} />
                    Push
                  </button>
                </div>
              )}
              {!!r.behind && (
                <div className="phone-row phone-repo-note">
                  <span className="phone-row-main phone-row-sub">
                    {r.behind} commit{r.behind === 1 ? '' : 's'} not pulled
                  </span>
                  <button
                    type="button"
                    className="btn btn-sm"
                    disabled={!!busy}
                    onClick={() => act(() => client.pull(r.path), `Pulling ${r.name}…`)}
                  >
                    <ArrowDownToLine size={15} />
                    Pull
                  </button>
                </div>
              )}
            </div>
          </section>
        ))}
      {changes.length > 0 && (
        <>
          <textarea
            className="phone-commit-message"
            rows={2}
            placeholder={included.length ? 'Commit message' : 'Tick the files to commit'}
            aria-label="Commit message"
            value={message}
            onChange={(e) => setMessage(e.target.value)}
          />
          <button
            type="button"
            className="btn btn-primary phone-wide-btn"
            disabled={!!busy || !included.length || !message.trim()}
            onClick={commit}
          >
            Commit {included.length} file{included.length === 1 ? '' : 's'}
            {into > 1 && ` in ${into} repositories`}
          </button>
        </>
      )}

      {one && !!one.ahead && (
        <>
          <h2 className="phone-heading">Not pushed · {one.ahead}</h2>
          <div className="phone-list">
            {unpushed.map((c) => (
              <CommitRow key={c.hash} commit={c} onOpen={() => onCommit(c.hash, one.path || undefined)} />
            ))}
            <button
              type="button"
              className="phone-row phone-row-accent"
              disabled={!!busy}
              onClick={() => act(() => client.push(one.path), 'Pushing…')}
            >
              <ArrowUpFromLine size={18} />
              <span className="phone-row-main">Push</span>
            </button>
          </div>
        </>
      )}
      {one && !!one.behind && (
        <>
          <h2 className="phone-heading">Not pulled · {one.behind}</h2>
          <div className="phone-list">
            <button
              type="button"
              className="phone-row phone-row-accent"
              disabled={!!busy}
              onClick={() => act(() => client.pull(one.path), 'Pulling…')}
            >
              <ArrowDownToLine size={18} />
              <span className="phone-row-main">Pull</span>
            </button>
          </div>
        </>
      )}

      {status?.repo && (
        <div className="phone-list phone-code-gap">
          <button type="button" className="phone-row" onClick={() => onHistory(one?.path || undefined)}>
            <HistoryIcon size={18} />
            <span className="phone-row-main">History</span>
            <ChevronRight size={16} className="phone-row-chev" />
          </button>
        </div>
      )}
    </div>
  )
}

const branchName = (r: Repo) => (r.branch === '(detached)' ? 'detached HEAD' : r.branch)

// useUnpushed is the commits a repository's HEAD is ahead of its upstream
// by.
function useUnpushed(client: CodeClient, repo: Repo | undefined): LogCommit[] {
  const ahead = repo?.ahead ?? 0
  const path = repo?.path
  const [unpushed, setUnpushed] = useState<LogCommit[]>([])
  const head = `${repo?.branch}:${ahead}:${repo?.behind}`
  useEffect(() => {
    if (!ahead) return
    let live = true
    client.log('HEAD', 0, ahead, undefined, path).then(
      (l) => live && setUnpushed(l.commits),
      () => {},
    )
    return () => {
      live = false
    }
  }, [client, ahead, path, head])
  return ahead ? unpushed : []
}

// ChangeRow is an uncommitted change: ticked to go in the next commit,
// and opening its diff. Its folder is named from its repository.
function ChangeRow({
  change: c,
  repo,
  busy,
  act,
  client,
  onDiff,
}: {
  change: Change
  repo: Repo
  busy: boolean
  act: (fn: () => Promise<void>) => void
  client: CodeClient
  onDiff: (path: string) => void
}) {
  const where = changeWhere(within(repo, c))
  return (
    <div className="phone-row phone-change">
      <label className="phone-change-check">
        <input
          type="checkbox"
          checked={staged(c)}
          ref={(el) => {
            if (el) el.indeterminate = partly(c)
          }}
          disabled={busy}
          aria-label={`Include ${c.path} in the commit`}
          onChange={() => act(() => (staged(c) && !partly(c) ? client.unstage(c.path) : client.stage(c.path)))}
        />
      </label>
      <button type="button" className="phone-change-open" onClick={() => onDiff(c.path)}>
        <Badge letter={letterOf(c)} />
        <span className="phone-row-main">
          <span className="phone-change-name">{c.path.split('/').pop()}</span>
          {where && <Tail text={where} />}
        </span>
        {c.submodule && <span className="phone-change-tag">new commits</span>}
        <ChevronRight size={16} className="phone-row-chev" />
      </button>
    </div>
  )
}

// HistoryScreen is the commits reachable from HEAD, newest first, a page
// at a time, of one of the folder's repositories (by its path).
function HistoryScreen({
  client,
  status,
  repo,
  onRepo,
  onBack,
  onCommit,
}: {
  client: CodeClient
  status: GitStatus | null
  repo: string
  onRepo: (repo: string) => void
  onBack: () => void
  onCommit: (hash: string) => void
}) {
  const choices = usable(status?.repos ?? [])
  const shown = choices.find((r) => r.path === repo)
  const [commits, setCommits] = useState<LogCommit[] | null>(null)
  const [more, setMore] = useState(false)
  const [error, setError] = useState('')
  const [loading, setLoading] = useState(false)
  const page = 50

  const load = useCallback(
    async (skip: number) => {
      try {
        const log = await client.log('HEAD', skip, page, undefined, repo)
        setCommits((c) => (skip === 0 ? log.commits : [...(c ?? []), ...log.commits]))
        setMore(log.more)
        setError('')
      } catch (e) {
        setError(e instanceof Error ? e.message : String(e))
      } finally {
        setLoading(false)
      }
    },
    [client, repo],
  )
  // From the top again whenever the repository changes: HEAD may have
  // moved.
  const head = `${shown?.branch}:${shown?.ahead}:${shown?.behind}`
  useEffect(() => {
    load(0)
  }, [load, head])

  return (
    <div className="phone-code-screen">
      <ScreenBar title="History" sub={shown?.branch} onBack={onBack} />
      <div className="phone-code">
        {choices.length > 1 && (
          <label className="phone-chip phone-repo-pick">
            <FolderGit2 size={15} />
            <select aria-label="Repository" value={repo} onChange={(e) => onRepo(e.target.value)}>
              {choices.map((r) => (
                <option key={r.path} value={r.path}>
                  {r.name}
                </option>
              ))}
            </select>
            <ChevronDown size={15} />
          </label>
        )}
        {error && <div className="notice phone-code-error">{error}</div>}
        {commits && (
          <div className="phone-list">
            {commits.map((c) => (
              <CommitRow key={c.hash} commit={c} onOpen={() => onCommit(c.hash)} />
            ))}
            {commits.length === 0 && <div className="phone-empty phone-list-empty">No commits yet.</div>}
          </div>
        )}
        {more && (
          <button
            type="button"
            className="btn phone-code-more"
            disabled={loading}
            onClick={() => {
              setLoading(true)
              load(commits?.length ?? 0)
            }}
          >
            {loading ? 'Loading…' : 'Show more'}
          </button>
        )}
      </div>
    </div>
  )
}

// FilesScreen is a folder of the working tree, its folders first, a
// changed file marked as in Changes.
function FilesScreen({
  env,
  client,
  status,
  dir,
  onDir,
  onFile,
}: {
  env: Environment
  client: CodeClient
  status: GitStatus | null
  dir: string
  onDir: (dir: string) => void
  onFile: (path: string) => void
}) {
  const [entries, setEntries] = useState<Entry[] | null>(null)
  const [error, setError] = useState('')
  // The workspace's projects, offered at the top of the folder's own list,
  // beside the home folder and the machine's root.
  const [projects, setProjects] = useState<string[]>([])
  useEffect(() => {
    if (dir) return
    let live = true
    client.workspace().then(
      (w) => live && setProjects(w.folders),
      () => {},
    )
    return () => {
      live = false
    }
  }, [client, dir])
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

  const changes = status?.changes ?? []
  const changed = new Map(changes.map((c) => [c.path, letterOf(c)]))
  const holdsChanges = (path: string) => changes.some((c) => c.path.startsWith(path + '/'))
  const join = (name: string) => (dir === '/' ? `/${name}` : dir ? `${dir}/${name}` : name)
  // A folder from / is outside the tab's own: its path is shown from /.
  const absolute = dir.startsWith('/')
  const parts = absolute ? dir.split('/').slice(1).filter(Boolean) : dir ? dir.split('/') : []
  const crumb = (i: number) => (absolute ? '/' + parts.slice(0, i + 1).join('/') : parts.slice(0, i + 1).join('/'))
  const places = [
    ...projects.map((p) => ({ name: p.slice(p.lastIndexOf('/') + 1), path: p === client.root ? '' : p, sub: p })),
    { name: 'Home', path: client.home === client.root ? '' : client.home, sub: client.home },
    { name: 'Computer', path: '/', sub: '/' },
  ].filter((p, i, all) => all.findIndex((o) => o.sub === p.sub) === i)

  return (
    <div className="phone-code">
      <TopHead env={env} title="Files">
        <div className="phone-code-crumbs">
          <button type="button" className="code-link" onClick={() => onDir(absolute ? '/' : '')}>
            {absolute ? 'Computer' : client.root.split('/').pop() || '/'}
          </button>
          {parts.map((p, i) => (
            <span key={i}>
              {' / '}
              <button type="button" className="code-link" onClick={() => onDir(crumb(i))}>
                {p}
              </button>
            </span>
          ))}
        </div>
      </TopHead>
      {error && <div className="notice phone-code-error">{error}</div>}
      {!dir && (
        <div className="phone-list">
          {places.map((p) => (
            <button key={p.sub} type="button" className="phone-row" onClick={() => onDir(p.path)}>
              {p.name === 'Computer' ? (
                <HardDrive size={18} />
              ) : p.name === 'Home' ? (
                <House size={18} />
              ) : (
                <FolderGit2 size={18} />
              )}
              <span className="phone-row-main">
                <span>{p.name}</span>
                <Tail text={p.sub} />
              </span>
              <ChevronRight size={16} className="phone-row-chev" />
            </button>
          ))}
        </div>
      )}
      {entries && (
        <div className="phone-list">
          {entries.map((e) => {
            const path = join(e.name)
            const letter = e.dir ? (holdsChanges(path) ? '•' : '') : changed.get(path)
            return (
              <button
                key={e.name}
                type="button"
                className={e.ignored ? 'phone-row phone-file-ignored' : 'phone-row'}
                onClick={() => (e.dir ? onDir(path) : onFile(path))}
              >
                {e.dir ? <Folder size={18} /> : <File size={18} />}
                <span className="phone-row-main phone-change-name">{e.name}</span>
                {letter && <Badge letter={letter} />}
                {e.dir && <ChevronRight size={16} className="phone-row-chev" />}
              </button>
            )
          })}
          {entries.length === 0 && <div className="phone-empty phone-list-empty">This folder is empty.</div>}
        </div>
      )}
    </div>
  )
}

// CommitScreen is a commit: its message, who and when, and the files it
// changed, each opening its diff with the commit's first parent.
function CommitScreen({
  client,
  hash,
  repo,
  onBack,
  onDiff,
  onCommit,
}: {
  client: CodeClient
  hash: string
  repo?: string
  onBack: () => void
  onDiff: (path: string, against: Against) => void
  onCommit: (hash: string) => void
}) {
  const info = useCommit(client, hash, repo)
  const short = hash.slice(0, 7)
  const [subject, ...body] = (info.data?.message ?? '').split('\n')
  return (
    <div className="phone-code-screen">
      <ScreenBar title={`Commit ${short}`} onBack={onBack} />
      <div className="phone-code">
        {info.error && <div className="notice phone-code-error">{info.error}</div>}
        {info.data && (
          <>
            <div className="phone-card phone-commit-info">
              <div className="phone-commit-subject">{subject}</div>
              {body.join('\n').trim() && <pre className="phone-commit-body">{body.join('\n').trim()}</pre>}
              <div className="phone-row-sub">
                {info.data.author}, {formatAgo(new Date(info.data.time * 1000).toISOString())}
                {info.data.committer !== info.data.author && ` · committed by ${info.data.committer}`}
              </div>
              {info.data.parents.length > 0 && (
                <div className="phone-row-sub">
                  {info.data.parents.length > 1 ? 'Parents ' : 'Parent '}
                  {info.data.parents.map((p, i) => (
                    <span key={p}>
                      {i > 0 && ', '}
                      <button type="button" className="code-link" onClick={() => onCommit(p)}>
                        {p.slice(0, 7)}
                      </button>
                    </span>
                  ))}
                </div>
              )}
            </div>
            <h2 className="phone-heading">Files · {info.data.files.length}</h2>
            <div className="phone-list">
              {info.data.files.map((f) => (
                <button
                  key={f.path}
                  type="button"
                  className="phone-row"
                  onClick={() => onDiff(f.path, commitAgainst(info.data!, f.from, repo))}
                >
                  <Badge letter={f.status} />
                  <span className="phone-row-main">
                    <span className="phone-change-name">{f.path.split('/').pop()}</span>
                    {changeWhere(f) && <Tail text={changeWhere(f)} />}
                  </span>
                  <ChevronRight size={16} className="phone-row-chev" />
                </button>
              ))}
              {info.data.files.length === 0 && (
                <div className="phone-empty phone-list-empty">No files changed in this folder.</div>
              )}
            </div>
          </>
        )}
      </div>
    </div>
  )
}

// commitAgainst is what a commit's file is compared with: the commit's
// first parent, under the name the file had there.
function commitAgainst(info: CommitInfo, from?: string, repo?: string): Against {
  return {
    base: info.parents[0] ?? `${info.hash}^`,
    bl: info.parents[0] ? info.parents[0].slice(0, 7) : 'nothing',
    to: info.hash,
    tl: info.hash.slice(0, 7),
    from,
    repo,
  }
}

function useCommit(client: CodeClient, hash: string | undefined, repo?: string) {
  const [state, setState] = useState<{ hash?: string; data?: CommitInfo; error?: string }>({})
  useEffect(() => {
    if (!hash) return
    let live = true
    client.commitInfo(hash, repo).then(
      (data) => live && setState({ hash, data }),
      (e: Error) => live && setState({ hash, error: e.message }),
    )
    return () => {
      live = false
    }
  }, [client, hash, repo])
  return state.hash === hash ? state : {}
}

// DiffScreen is a file's diff, one column wide, its lines wrapped unless
// asked not to be, with the way to the files changed before and after it:
// the uncommitted ones, as Changes lists them, or the commit's. An
// uncommitted file's changes can be reverted from here.
function DiffScreen({
  client,
  status,
  reload,
  path,
  against,
  onBack,
  onStep,
}: {
  client: CodeClient
  status: GitStatus | null
  reload: () => void
  path: string
  against: Against
  onBack: () => void
  onStep: (path: string, against: Against) => void
}) {
  const [wrap, setWrap] = useWrap()
  const [stats, setStats] = useState<DiffStats | null>(null)
  const [confirm, setConfirm] = useState(false)
  const [error, setError] = useState('')
  const commit = useCommit(client, against.to, against.repo)

  const files: { path: string; from?: string }[] = against.to
    ? (commit.data?.files ?? [])
    : against.base
      ? []
      : (status?.changes ?? [])
  const at = files.findIndex((f) => f.path === path)
  const step = (by: number) => {
    const f = files[at + by]
    if (!f) return
    onStep(f.path, commit.data ? commitAgainst(commit.data, f.from, against.repo) : {})
  }
  const local = !against.base && !against.to
  // A submodule's commit is moved in the submodule, not reverted here.
  const revertable = local && !status?.changes.find((c) => c.path === path)?.submodule
  const revert = async () => {
    setConfirm(false)
    // What to show once it is gone: the next file, else the one before.
    const next = files[at + 1] ?? files[at - 1]
    try {
      await client.rollback(path)
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e))
      return
    }
    reload()
    if (next) onStep(next.path, {})
    else onBack()
  }

  const sub = [against.to && against.tl, at >= 0 && `${at + 1} of ${files.length}`].filter(Boolean)
  return (
    <div className="phone-code-screen">
      <ScreenBar
        title={path.split('/').pop() ?? path}
        sub={
          <>
            {sub.join(' · ')}
            {stats && (
              <>
                {sub.length > 0 && ' · '}
                <span className="phone-diff-added">+{stats.added}</span>{' '}
                <span className="phone-diff-removed">−{stats.removed}</span>
              </>
            )}
          </>
        }
        onBack={onBack}
      >
        <WrapButton wrap={wrap} onWrap={setWrap} />
      </ScreenBar>
      {error && <div className="notice phone-code-error">{error}</div>}
      <div className="phone-code-fill">
        <DiffView
          client={client}
          path={path}
          base={against.base}
          baseLabel={against.bl}
          from={against.from}
          to={against.to}
          toLabel={against.tl}
          unified
          wrap={wrap}
          onStats={setStats}
        />
      </div>
      {files.length > 0 && (
        <div className={revertable ? 'phone-diff-foot' : 'phone-diff-foot phone-diff-foot-two'}>
          <button type="button" className="btn" disabled={at <= 0} onClick={() => step(-1)}>
            <ChevronUp size={16} />
            Previous
          </button>
          {revertable &&
            (confirm ? (
              <button type="button" className="btn btn-danger" onClick={revert}>
                Discard?
              </button>
            ) : (
              <button
                type="button"
                className="btn phone-diff-revert"
                disabled={at < 0}
                onClick={() => setConfirm(true)}
              >
                Revert…
              </button>
            ))}
          <button type="button" className="btn" disabled={at < 0 || at >= files.length - 1} onClick={() => step(1)}>
            Next
            <ChevronDown size={16} />
          </button>
        </div>
      )}
    </div>
  )
}

// FileScreen is a file's text, to read, its misspellings underlined. A
// Markdown file shows rendered and a CSV or TSV file as a table; the
// button beside Wrap shows its text.
function FileScreen({
  client,
  path,
  onBack,
  onFile,
}: {
  client: CodeClient
  path: string
  onBack: () => void
  onFile: (path: string) => void
}) {
  const [wrap, setWrap] = useWrap()
  const host = useRef<HTMLDivElement>(null)
  const [error, setError] = useState('')
  const [text, setText] = useState<string | null>(null)
  const viewable = isMarkdown(path) || isTable(path)
  const [rendered, setRendered] = useState(true)
  const showText = !viewable || !rendered
  useEffect(() => followSettings(client), [client])
  useEffect(() => {
    let live = true
    setText(null)
    client.read(path).then(
      (t) => {
        if (!live) return
        if (t === null) setError('This file is too large to show, or not text.')
        else setText(t)
      },
      (e: Error) => live && setError(e.message),
    )
    return () => {
      live = false
    }
  }, [client, path])
  useEffect(() => {
    const el = host.current
    if (!el || text === null || !showText) return
    let view: EditorView | null = null
    let live = true
    languageFor(path.split('/').pop() ?? '').then((lang) => {
      if (!live) return
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
          spelling(path),
        ],
      })
    })
    return () => {
      live = false
      view?.destroy()
    }
  }, [path, text, wrap, showText])
  return (
    <div className="phone-code-screen">
      <ScreenBar title={path.split('/').pop() ?? path} sub={<bdi>{dirOf(path)}</bdi>} onBack={onBack}>
        {viewable && (
          <button
            type="button"
            className={rendered ? 'phone-icon-btn phone-icon-btn-on' : 'phone-icon-btn'}
            aria-pressed={rendered}
            aria-label={isTable(path) ? 'Show as a table' : 'Show rendered'}
            title={isTable(path) ? 'Show as a table' : 'Show rendered'}
            onClick={() => setRendered((r) => !r)}
          >
            {isTable(path) ? <Table2 size={20} /> : <Eye size={20} />}
          </button>
        )}
        {showText && <WrapButton wrap={wrap} onWrap={setWrap} />}
      </ScreenBar>
      {error && <div className="phone-empty">{error}</div>}
      {!error && text !== null && !showText && (
        <div className="phone-code-fill phone-code-rendered">
          {isTable(path) ? (
            <TableView path={path} text={text} />
          ) : (
            <MarkdownPreview client={client} path={path} text={text} onOpen={onFile} />
          )}
        </div>
      )}
      <div ref={host} className="phone-code-fill phone-code-text" hidden={!!error || !showText} />
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

// TopHead heads a screen the environment's screen leads to: the way back
// to it, the screen's title, and what is under the title. Opened from a
// terminal (the link's state names it, from), the way back is to that.
function TopHead({ env, title, children }: { env: Environment; title: string; children?: ReactNode }) {
  const location = useLocation()
  const navigate = useNavigate()
  const from = (location.state as { from?: string } | null)?.from
  return (
    <div className="phone-code-head">
      {from ? (
        <button type="button" className="phone-back" onClick={() => navigate(-1)}>
          <ChevronLeft size={22} />
          <span>{from}</span>
        </button>
      ) : (
        <Link to={`/environments/${env.id}`} className="phone-back">
          <ChevronLeft size={22} />
          <span>{env.name}</span>
        </Link>
      )}
      <h1 className="phone-title">{title}</h1>
      {children}
    </div>
  )
}

// ScreenBar heads a screen another of the Code tab's leads to: the way
// back, its title and what it is of, and its own buttons, if any.
function ScreenBar({
  title,
  sub,
  onBack,
  children,
}: {
  title: string
  sub?: ReactNode
  onBack: () => void
  children?: ReactNode
}) {
  return (
    <div className="phone-nav">
      <button type="button" className="phone-icon-btn" aria-label="Back" onClick={onBack}>
        <ChevronLeft size={24} />
      </button>
      <div className="phone-nav-title">
        <span>{title}</span>
        {sub && <span className="phone-row-sub">{sub}</span>}
      </div>
      <div className="phone-nav-end">{children}</div>
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
          <code>{commit.hash.slice(0, 7)}</code> · {commit.author},{' '}
          {formatAgo(new Date(commit.time * 1000).toISOString())}
        </span>
      </span>
      <ChevronRight size={16} className="phone-row-chev" />
    </button>
  )
}

// letterOf is the letter a change is shown by: its working tree's, else
// its index's, U for a file git does not track.
function letterOf(c: Change): string {
  if (c.index === '?') return 'U'
  return c.worktree !== '.' ? c.worktree : c.index
}

function Badge({ letter }: { letter: string }) {
  const kind = { A: 'added', C: 'added', D: 'deleted', R: 'renamed', U: 'unversioned' }[letter] ?? 'modified'
  return <span className={`phone-badge phone-badge-${kind}`}>{letter}</span>
}

const dirOf = (p: string) => (p.includes('/') ? p.slice(0, p.lastIndexOf('/')) : '')
