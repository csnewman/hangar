import { ChevronDown, Copy, File, GitBranch, History, RefreshCw, Tag, X } from 'lucide-react'
import { Fragment, useCallback, useEffect, useMemo, useRef, useState, type MouseEvent } from 'react'

import { changeWhere, type DiffAgainst } from './BranchChanges'
import { BranchPicker, type PickerAction } from './BranchPicker'
import type { Branches, CodeClient, CommitInfo, LogCommit, Ref, Repo } from './client'
import { copyText, useContextMenu, type MenuItem } from './ContextMenu'
import { layout, type GraphRow } from './graph'
import { RepoPicker } from './RepoPicker'
import { repoFor } from './repos'
import { ToolWindow } from './ToolWindow'

// How many commits are read at a time; more as the list is scrolled.
const page = 300
const rowHeight = 24
const laneWidth = 12

// GitLog is the Git Log tool window, as IntelliJ's: the history of every
// branch (or one) as a graph of how they forked and merged, newest first,
// filtered by what is typed, and the chosen commit's message and files,
// each opening its diff in that commit. It shows one file's or folder's
// history when asked, until that is cleared. Of a folder holding several
// repositories, it shows one at a time, chosen in its title bar.
export function GitLog({
  client,
  repos,
  focus,
  history,
  onDiff,
  onHide,
}: {
  client: CodeClient
  repos: Repo[]
  // focus is a commit to find and choose, each time it changes, in the
  // repository given (a path in it).
  focus?: { hash: string; repo?: string; n: number } | null
  // history is a path whose history to show, each time it changes.
  history?: { path: string; n: number } | null
  onDiff: (path: string, against: DiffAgainst) => void
  onHide: () => void
}) {
  const [ref, setRef] = useState<string | undefined>(undefined)
  const [path, setPath] = useState<string | undefined>(undefined)
  const [chosen, setChosen] = useState('')
  // The repository shown: the one the path whose history is shown is in,
  // else the one chosen.
  const repo = path !== undefined ? (repoFor(repos, path)?.path ?? '') : chosen
  useEffect(() => {
    if (!history) return
    setPath(history.path)
    setFilter('')
    setSelected(null)
  }, [history])
  const [commits, setCommits] = useState<LogCommit[]>([])
  const [more, setMore] = useState(false)
  const [error, setError] = useState('')
  const [filter, setFilter] = useState('')
  const [selected, setSelected] = useState<string | null>(null)
  const [picker, setPicker] = useState<{ anchor: DOMRect; branches: Branches } | null>(null)
  const loading = useRef(false)
  const count = useRef(0)
  count.current = commits.length

  // load reads the history from its start, as much of it as was shown,
  // or the next page of it.
  const load = useCallback(
    async (next: boolean) => {
      if (loading.current) return
      loading.current = true
      try {
        const skip = next ? count.current : 0
        const max = next ? page : Math.max(page, count.current)
        const log = await client.log(ref, skip, max, path, repo)
        setCommits((c) => (next ? [...c, ...log.commits] : log.commits))
        setMore(log.more)
        setError('')
      } catch (e) {
        setError(e instanceof Error ? e.message : String(e))
      } finally {
        loading.current = false
      }
    },
    [client, ref, path, repo],
  )
  useEffect(() => {
    count.current = 0
    load(false)
  }, [load])
  // Again whenever git reports a change: a commit, a checkout, a fetch.
  useEffect(() => {
    let timer: ReturnType<typeof setTimeout> | undefined
    const off = client.on('git.changed', () => {
      clearTimeout(timer)
      timer = setTimeout(() => load(false), 300)
    })
    return () => {
      clearTimeout(timer)
      off()
    }
  }, [client, load])

  const graph = useMemo(() => layout(commits), [commits])
  const q = filter.trim().toLowerCase()
  // Filtered, the commits are listed without the graph, which needs them all.
  const shown = useMemo(
    () =>
      commits
        .map((c, i) => ({ c, row: graph.rows[i] }))
        .filter(
          ({ c }) =>
            !q ||
            c.subject.toLowerCase().includes(q) ||
            c.author.toLowerCase().includes(q) ||
            c.email.toLowerCase().includes(q) ||
            c.hash.startsWith(q) ||
            c.refs.some((r) => r.name.toLowerCase().includes(q)),
        ),
    [commits, graph, q],
  )
  const graphWidth = q ? 0 : Math.min(graph.lanes, 16) * laneWidth + 8

  // Only the rows in view are drawn.
  const list = useRef<HTMLDivElement>(null)
  const [view, setView] = useState({ top: 0, height: 400 })
  const onScroll = () => {
    const el = list.current
    if (!el) return
    setView({ top: el.scrollTop, height: el.clientHeight })
  }
  // More is read once the rows in view come near the end of those read.
  useEffect(() => {
    if (more && !q && (view.top + view.height) / rowHeight > shown.length - 40) load(true)
  }, [view, more, q, shown.length, load])
  useEffect(() => {
    const el = list.current
    if (!el) return
    const ro = new ResizeObserver(() => setView({ top: el.scrollTop, height: el.clientHeight }))
    ro.observe(el)
    return () => ro.disconnect()
  }, [])
  const first = Math.max(0, Math.floor(view.top / rowHeight) - 10)
  const last = Math.min(shown.length, Math.ceil((view.top + view.height) / rowHeight) + 10)

  // A commit asked for is chosen at once, its details shown, and the
  // history read on, every branch's and unfiltered, until it is in the
  // list to scroll to, or the history ends.
  const [seek, setSeek] = useState<string | null>(null)
  useEffect(() => {
    if (!focus) return
    setFilter('')
    setRef(undefined)
    setPath(undefined)
    setChosen(focus.repo ?? '')
    setSelected(focus.hash)
    setSeek(focus.hash)
  }, [focus])
  useEffect(() => {
    if (!seek || q || ref !== undefined || path !== undefined) return
    const i = commits.findIndex((c) => c.hash === seek)
    if (i >= 0) {
      setSeek(null)
      const el = list.current
      if (el) {
        el.scrollTop = Math.max(0, i * rowHeight - el.clientHeight / 2)
        setView({ top: el.scrollTop, height: el.clientHeight })
      }
      return
    }
    if (more) load(true)
    else if (commits.length) setSeek(null)
  }, [seek, commits, more, q, ref, path, load])

  const index = shown.findIndex(({ c }) => c.hash === selected)
  const select = (i: number) => {
    const s = shown[Math.max(0, Math.min(shown.length - 1, i))]
    if (!s) return
    setSelected(s.c.hash)
    const el = list.current
    if (!el) return
    const top = Math.max(0, Math.min(shown.length - 1, i)) * rowHeight
    if (top < el.scrollTop) el.scrollTop = top
    else if (top + rowHeight > el.scrollTop + el.clientHeight) el.scrollTop = top + rowHeight - el.clientHeight
  }

  const menu = useContextMenu()
  const rowMenu = (e: MouseEvent, c: LogCommit) => {
    const items: MenuItem[] = [
      { label: 'Copy Hash', onSelect: () => copyText(c.hash) },
      { label: 'Copy Subject', onSelect: () => copyText(c.subject) },
    ]
    if (c.refs.length) {
      items.push('separator', ...c.refs.map((r) => ({ label: `Copy ${r.name}`, onSelect: () => copyText(r.name) })))
    }
    menu.open(e, items)
  }

  const pickRef = async (e: MouseEvent) => {
    const anchor = e.currentTarget.getBoundingClientRect()
    try {
      setPicker({ anchor, branches: await client.branches(repo) })
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err))
    }
  }
  const allAction: PickerAction = (s) =>
    !s || 'all branches'.includes(s.toLowerCase())
      ? {
          label: 'All branches',
          icon: <GitBranch size={13} className="code-icon-file" />,
          checked: ref === undefined,
          run: () => setRef(undefined),
        }
      : null
  const headAction: PickerAction = (s) =>
    !s || 'head current branch'.includes(s.toLowerCase())
      ? {
          label: 'The current branch',
          icon: <GitBranch size={13} className="code-icon-file" />,
          checked: ref === 'HEAD',
          run: () => setRef('HEAD'),
        }
      : null

  return (
    <ToolWindow
      title="Git Log"
      extra={
        <>
          <RepoPicker
            repos={repos}
            value={repo}
            onPick={(r) => {
              setChosen(r)
              setPath(undefined)
              setRef(undefined)
              setSelected(null)
            }}
          />
          <button type="button" className="code-branch-btn" title="Show another branch's history" onClick={pickRef}>
            <GitBranch size={12} />
            {ref === undefined ? 'All branches' : ref === 'HEAD' ? 'The current branch' : ref}
            <ChevronDown size={11} />
          </button>
          {path !== undefined && (
            <span className="code-log-path" title={`Only the commits that changed ${path}`}>
              <History size={12} />
              {path.split('/').pop() || path}
              <button
                type="button"
                title="Show the whole history"
                aria-label="Show the whole history"
                onClick={() => setPath(undefined)}
              >
                <X size={11} />
              </button>
            </span>
          )}
        </>
      }
      actions={
        <button type="button" className="code-tool-btn" title="Refresh" onClick={() => load(false)}>
          <RefreshCw size={13} />
        </button>
      }
      onHide={onHide}
    >
      <div className="code-log">
        <div className="code-log-main">
          <input
            className="code-log-filter"
            placeholder="Filter by message, author, hash or branch"
            spellCheck={false}
            value={filter}
            onChange={(e) => setFilter(e.target.value)}
          />
          {error && <div className="code-note code-note-error">{error}</div>}
          <div
            className="code-log-list"
            ref={list}
            tabIndex={0}
            role="listbox"
            onScroll={onScroll}
            onKeyDown={(e) => {
              const by = { ArrowDown: 1, ArrowUp: -1, PageDown: 20, PageUp: -20 }[e.key]
              if (by === undefined) return
              e.preventDefault()
              select(index < 0 ? 0 : index + by)
            }}
          >
            <div style={{ height: shown.length * rowHeight, position: 'relative' }}>
              {shown.slice(first, last).map(({ c, row }, k) => {
                const i = first + k
                return (
                  <div
                    key={c.hash}
                    role="option"
                    aria-selected={c.hash === selected}
                    className={c.hash === selected ? 'code-log-row code-log-row-on' : 'code-log-row'}
                    style={{ top: i * rowHeight, gridTemplateColumns: `${graphWidth}px 1fr auto auto` }}
                    onClick={() => setSelected(c.hash)}
                    onContextMenu={(e) => rowMenu(e, c)}
                  >
                    {graphWidth ? <GraphCell row={row} merge={c.parents.length > 1} /> : <span />}
                    <span className="code-log-subject">
                      {c.refs.map((r) => (
                        <RefBadge key={`${r.kind}:${r.name}`} r={r} />
                      ))}
                      <span>{c.subject}</span>
                    </span>
                    <span className="code-log-author">{c.author}</span>
                    <span className="code-log-date">{when(c.time)}</span>
                  </div>
                )
              })}
            </div>
            {commits.length === 0 && !error && <div className="code-note">No commits yet.</div>}
            {q && shown.length === 0 && <div className="code-note">No commit matches.</div>}
          </div>
          {q && more && (
            <div className="code-log-foot muted small">
              Filtering the {commits.length} commits read.{' '}
              <button type="button" className="code-link" onClick={() => load(true)}>
                Read {page} more
              </button>
            </div>
          )}
        </div>
        <CommitDetails client={client} repo={repo} hash={selected} onSelect={setSelected} onDiff={onDiff} />
      </div>
      {picker && (
        <BranchPicker
          anchor={picker.anchor}
          branches={picker.branches}
          checked={ref}
          actions={[allAction, headAction]}
          onPick={(name) => setRef(name)}
          onClose={() => setPicker(null)}
        />
      )}
      {menu.menu}
    </ToolWindow>
  )
}

// when is a commit's time, briefly: the time today, else the date.
function when(t: number) {
  const d = new Date(t * 1000)
  const now = new Date()
  if (d.toDateString() === now.toDateString()) return d.toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' })
  return d.toLocaleDateString([], {
    day: 'numeric',
    month: 'short',
    year: d.getFullYear() === now.getFullYear() ? undefined : 'numeric',
  })
}

const x = (lane: number) => 6 + lane * laneWidth + laneWidth / 2 - 2

// GraphCell draws one row of the graph: the lanes passing, the lines into
// its commit and out of it to its parents, and the commit itself.
function GraphCell({ row, merge }: { row: GraphRow; merge: boolean }) {
  const h = rowHeight
  const m = h / 2
  return (
    <svg className="code-log-graph" height={h} aria-hidden="true">
      {row.edges.map((e, i) => {
        const a = x(e.from)
        const b = x(e.to)
        const d =
          e.kind === 'through'
            ? `M${a} 0V${h}`
            : e.kind === 'in'
              ? a === b
                ? `M${a} 0V${m}`
                : `M${a} 0C${a} ${m} ${b} 0 ${b} ${m}`
              : a === b
                ? `M${a} ${m}V${h}`
                : `M${a} ${m}C${a} ${h} ${b} ${m} ${b} ${h}`
        return <path key={i} d={d} className={`code-log-lane code-lane-${e.color % 8}`} />
      })}
      <circle
        cx={x(row.col)}
        cy={m}
        r={merge ? 3 : 4}
        className={`code-log-node code-lane-${row.color % 8}${merge ? ' code-log-merge' : ''}`}
      />
    </svg>
  )
}

function RefBadge({ r }: { r: Ref }) {
  return (
    <span className={`code-log-ref code-log-ref-${r.kind}`} title={refTitle(r)}>
      {r.kind === 'tag' ? <Tag size={10} /> : <GitBranch size={10} />}
      {r.name}
    </span>
  )
}

function refTitle(r: Ref) {
  return {
    head: `${r.name}, checked out`,
    branch: `Branch ${r.name}`,
    remote: `Remote branch ${r.name}`,
    tag: `Tag ${r.name}`,
  }[r.kind]
}

// CommitDetails is the chosen commit: its message, who and when, its
// parents, and the files it changed, each opening its diff in it.
function CommitDetails({
  client,
  repo,
  hash,
  onSelect,
  onDiff,
}: {
  client: CodeClient
  repo: string
  hash: string | null
  onSelect: (hash: string) => void
  onDiff: (path: string, against: DiffAgainst) => void
}) {
  const [info, setInfo] = useState<CommitInfo | null>(null)
  const [error, setError] = useState('')
  useEffect(() => {
    if (!hash) return
    let live = true
    client.commitInfo(hash, repo).then(
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
  }, [client, hash, repo])

  if (!hash) return <div className="code-log-details code-note">Choose a commit to see its message and files.</div>
  if (error) return <div className="code-log-details code-note code-note-error">{error}</div>
  if (!info || info.hash !== hash) return <div className="code-log-details" />
  const short = info.hash.slice(0, 8)
  const against = (from?: string): DiffAgainst => ({
    base: info.parents[0] ?? `${info.hash}^`,
    baseLabel: info.parents[0] ? info.parents[0].slice(0, 8) : 'nothing',
    to: info.hash,
    toLabel: short,
    from,
  })
  const [subject, ...body] = info.message.split('\n')
  return (
    <div className="code-log-details">
      <div className="code-log-files">
        <div className="code-changes-head">
          <span>Files</span>
          <span className="muted small">{info.files.length}</span>
        </div>
        {info.files.map((f) => (
          <div
            key={f.path}
            className="code-change"
            title={f.from ? `${f.from} → ${f.path}` : f.path}
            onClick={() => onDiff(f.path, against(f.from))}
          >
            <span className={`code-change-letter code-change-${letterClass(f.status)}`}>{f.status}</span>
            <File size={12} className="code-icon-file" />
            <span className="code-change-name">{f.path.split('/').pop()}</span>
            <span className="code-change-dir muted">{changeWhere(f)}</span>
          </div>
        ))}
        {info.files.length === 0 && <div className="code-note">No files changed in this folder.</div>}
      </div>
      <div className="code-log-message">
        <div className="code-log-message-subject">{subject}</div>
        {body.join('\n').trim() && <pre className="code-log-message-body">{body.join('\n').trim()}</pre>}
        <div className="code-log-meta">
          <span>
            <button type="button" className="code-link" title="Copy the hash" onClick={() => copyText(info.hash)}>
              {short} <Copy size={10} />
            </button>{' '}
            by {info.author} &lt;{info.email}&gt;, {new Date(info.time * 1000).toLocaleString()}
          </span>
          {info.committer !== info.author && (
            <span>
              committed by {info.committer}, {new Date(info.committerTime * 1000).toLocaleString()}
            </span>
          )}
          {info.parents.length > 0 && (
            <span>
              {info.parents.length > 1 ? 'Parents ' : 'Parent '}
              {info.parents.map((p, i) => (
                <Fragment key={p}>
                  {i > 0 && ', '}
                  <button type="button" className="code-link" onClick={() => onSelect(p)}>
                    {p.slice(0, 8)}
                  </button>
                </Fragment>
              ))}
            </span>
          )}
        </div>
      </div>
    </div>
  )
}

function letterClass(l: string) {
  return { A: 'added', D: 'deleted', R: 'renamed', C: 'added' }[l] ?? 'modified'
}
