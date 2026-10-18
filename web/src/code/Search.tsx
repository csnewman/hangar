import { EditorState, RangeSetBuilder, Compartment, type Extension } from '@codemirror/state'
import { Decoration, EditorView, lineNumbers } from '@codemirror/view'
import { CaseSensitive, File, Folder, FolderOpen, GitBranch, Regex, Replace, WholeWord, X } from 'lucide-react'
import { useCallback, useEffect, useMemo, useRef, useState, type KeyboardEvent, type ReactNode } from 'react'

import type { CodeClient, Match } from './client'
import { codeHighlight, codeTheme, followScheme, languageFor } from './CodeEditor'
import { isMac, shortcut } from './ContextMenu'
import type { Languages, Usages } from './lsp'
import { kindOf, workspaceSymbols, type FoundSymbol } from './symbols'

// Jump is a place in a file to go to: a line (from 1; 0 is none), the
// column and text there, whether the editor takes focus, and whether its
// annotations (git blame) are shown.
// root is the project the path is in; none is the tab's own folder, or a
// path from / outside every project.
export type Jump = {
  path: string
  line: number
  col?: number
  find?: string
  focus?: boolean
  blame?: boolean
  root?: string
}

// Popup is the frame the Code tab's search popups share, as IntelliJ's:
// centred near the top, over everything, gone on Escape or a click away.
export function Popup({
  className,
  onClose,
  children,
}: {
  className?: string
  onClose: () => void
  children: ReactNode
}) {
  return (
    <div
      className="code-popup-shade"
      onMouseDown={(e) => {
        if (e.target === e.currentTarget) onClose()
      }}
    >
      <div
        className={`code-popup ${className ?? ''}`}
        role="dialog"
        onKeyDown={(e) => {
          if (e.key === 'Escape') {
            e.stopPropagation()
            onClose()
          }
        }}
      >
        {children}
      </div>
    </div>
  )
}

// Highlighted is text with the characters at the given indices marked.
function Highlighted({ text, at }: { text: string; at: Iterable<number> }) {
  const set = new Set(at)
  if (set.size === 0) return <>{text}</>
  const out: ReactNode[] = []
  let run = ''
  let marked = false
  const flush = () => {
    if (!run) return
    out.push(marked ? <mark key={out.length}>{run}</mark> : run)
    run = ''
  }
  for (let i = 0; i < text.length; i++) {
    const m = set.has(i)
    if (m !== marked) {
      flush()
      marked = m
    }
    run += text[i]
  }
  flush()
  return <>{out}</>
}

// moveSelection is the list keys every popup shares: up and down, by a
// page, to the ends.
function moveSelection(e: KeyboardEvent, sel: number, count: number, set: (n: number) => void): boolean {
  const by = { ArrowDown: 1, ArrowUp: -1, PageDown: 10, PageUp: -10 }[e.key]
  if (by === undefined || count === 0) return false
  e.preventDefault()
  set(Math.max(0, Math.min(count - 1, sel + by)))
  return true
}

function useScrollIntoView(sel: number) {
  const list = useRef<HTMLDivElement>(null)
  useEffect(() => {
    list.current?.querySelector('[aria-selected=true]')?.scrollIntoView({ block: 'nearest' })
  }, [sel])
  return list
}

// --- Go to File ---------------------------------------------------------

const boundary = (s: string, i: number) =>
  i === 0 || '/_-. '.includes(s[i - 1]) || (s[i] >= 'A' && s[i] <= 'Z' && s[i - 1] >= 'a' && s[i - 1] <= 'z')

// fuzzy matches a query against text as IntelliJ's Go to File does, the
// query's characters in order but not necessarily together, preferring
// them together and at the starts of words. It returns the match's score
// and where it matched, or null.
export function fuzzy(query: string, text: string): { score: number; at: number[] } | null {
  const q = query.toLowerCase()
  const t = text.toLowerCase()
  if (!q) return { score: 0, at: [] }
  const sub = t.indexOf(q)
  if (sub >= 0) {
    const at = Array.from({ length: q.length }, (_, i) => sub + i)
    return { score: 100 + (boundary(text, sub) ? 40 : 0) + (sub === 0 ? 20 : 0) - text.length / 100, at }
  }
  // Each character at the next start of a word that has it, else wherever
  // it next appears.
  const at: number[] = []
  let from = 0
  for (const c of q) {
    let found = -1
    for (let i = from; i < t.length; i++) {
      if (t[i] === c && boundary(text, i)) {
        found = i
        break
      }
    }
    const next = t.indexOf(c, from)
    if (next < 0) return null
    if (found < 0 || (at.length > 0 && next === at[at.length - 1] + 1)) found = next
    at.push(found)
    from = found + 1
  }
  let score = 0
  for (let i = 0; i < at.length; i++) {
    if (boundary(text, at[i])) score += 8
    if (i > 0 && at[i] === at[i - 1] + 1) score += 5
  }
  score -= (at[at.length - 1] - at[0]) / 4 + text.length / 100
  return { score, at }
}

type FileHit = { path: string; score: number; name: number[]; dir: number[]; root?: string }

// rank orders the files a query matches: by their names, unless the query
// names folders too, then by their paths.
function rank(files: string[], query: string): FileHit[] {
  const q = query.replace(/\s+/g, '')
  const byPath = q.includes('/')
  const hits: FileHit[] = []
  for (const path of files) {
    const cut = path.lastIndexOf('/') + 1
    if (byPath) {
      const m = fuzzy(q, path)
      if (!m) continue
      hits.push({
        path,
        score: m.score,
        name: m.at.filter((i) => i >= cut).map((i) => i - cut),
        dir: m.at.filter((i) => i < cut),
      })
    } else {
      const m = fuzzy(q, path.slice(cut))
      if (m) hits.push({ path, score: m.score + 1000, name: m.at, dir: [] })
    }
  }
  hits.sort((a, b) => b.score - a.score || a.path.length - b.path.length || a.path.localeCompare(b.path))
  return hits
}

// GotoFile finds a file by name: IntelliJ's Go to File, which "name:line"
// also opens at a line.
export function GotoFile({
  client,
  projects = [],
  title,
  recent,
  onOpen,
  onClose,
}: {
  client: CodeClient
  // projects are the workspace's, each one's files offered.
  projects?: SearchProject[]
  title: string
  recent: { path: string; root?: string }[]
  onOpen: (j: Jump) => void
  onClose: () => void
}) {
  const [query, setQuery] = useState('')
  const [files, setFiles] = useState<{ root: string; files: string[] }[] | null>(null)
  const [error, setError] = useState('')
  const [sel, setSel] = useState(0)
  const all: SearchProject[] = projects.length ? projects : [{ root: client.root, name: '', client }]
  const allKey = all.map((p) => p.root).join('\n')
  useEffect(() => {
    Promise.all(all.map((p) => p.client.searchFiles().then((r) => ({ root: p.root, files: r.files })))).then(
      setFiles,
      (e: Error) => setError(e.message),
    )
    // The projects are named by allKey.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [allKey])
  const several = all.length > 1

  const m = /^(.*?)(?::(\d+)(?::(\d+))?)?$/.exec(query.trim())!
  const name = m[1]
  const line = m[2] ? Number(m[2]) : undefined
  const col = m[3] ? Number(m[3]) : undefined
  const hits = useMemo(() => {
    if (!name) return recent.map((r): FileHit => ({ path: r.path, root: r.root, score: 0, name: [], dir: [] }))
    // A path from / or ~ is any file on the machine.
    if (name.startsWith('/') || name.startsWith('~/')) {
      const abs = name.startsWith('~/') ? client.home + name.slice(1) : name
      return [{ path: abs, score: 0, name: [], dir: [] }]
    }
    if (!files) return []
    const roots = files.map((f) => f.root)
    return files
      .flatMap((f) =>
        rank(
          f.files.filter((p) => innermost(roots, f.root, p)),
          name,
        ).map((h) => ({ ...h, root: f.root })),
      )
      .sort((a, b) => b.score - a.score || a.path.length - b.path.length)
      .slice(0, 80)
  }, [files, name, recent, client.home])
  useEffect(() => setSel(0), [name])
  const list = useScrollIntoView(sel)

  const choose = (h: FileHit | undefined) => {
    if (!h) return
    onClose()
    onOpen({ path: h.path, root: h.root, line: line ?? 0, col })
  }

  return (
    <Popup className="code-goto" onClose={onClose}>
      <div className="code-popup-head">
        <span className="strong">{title}</span>
        <span className="muted small">
          {name ? (files ? `${hits.length === 80 ? '80+' : hits.length} found` : 'Listing files…') : 'Recent files'}
        </span>
      </div>
      <input
        className="code-popup-input"
        autoFocus
        spellCheck={false}
        placeholder="File name, or name:line; a path from / or ~ for any file"
        value={query}
        onChange={(e) => setQuery(e.target.value)}
        onKeyDown={(e) => {
          if (moveSelection(e, sel, hits.length, setSel)) return
          if (e.key === 'Enter') {
            e.preventDefault()
            choose(hits[sel])
          }
        }}
      />
      <div className="code-popup-list" ref={list} role="listbox">
        {hits.map((h, i) => {
          const cut = h.path.lastIndexOf('/') + 1
          return (
            <div
              key={`${h.root ?? ''}:${h.path}`}
              role="option"
              aria-selected={i === sel}
              className={i === sel ? 'code-popup-row code-popup-row-on' : 'code-popup-row'}
              onMouseMove={() => i !== sel && setSel(i)}
              onClick={() => choose(h)}
            >
              <File size={14} className="code-icon-file" />
              <span className="code-goto-name">
                <Highlighted text={h.path.slice(cut)} at={h.name} />
              </span>
              {several && h.root && <span className="code-tab-project">{nameOfRoot(h.root)}</span>}
              <span className="code-goto-dir">
                <Highlighted text={h.path.slice(0, cut).replace(/\/$/, '')} at={h.dir} />
              </span>
            </div>
          )
        })}
        {error && <div className="code-popup-empty code-note-error">{error}</div>}
        {!error && name && files && hits.length === 0 && <div className="code-popup-empty">Nothing matches.</div>}
      </div>
      <div className="code-popup-foot muted small">
        ↑↓ to choose · Enter to open · {shortcut('Mod+Shift+F')} finds text in files
      </div>
    </Popup>
  )
}

// --- Go to Symbol -------------------------------------------------------

// GotoSymbol finds a class, function or other symbol by name, anywhere in
// the workspace: IntelliJ's Go to Symbol, asking every language server
// running.
export function GotoSymbol({
  languages,
  place,
  onOpen,
  onClose,
}: {
  // languages are every project's servers.
  languages: Languages[]
  // place is where a file from / is: in a project, or not.
  place: (abs: string) => { path: string; root?: string }
  onOpen: (j: Jump) => void
  onClose: () => void
}) {
  const [query, setQuery] = useState('')
  const [found, setFound] = useState<{ query: string; symbols: FoundSymbol[] } | null>(null)
  const [sel, setSel] = useState(0)
  const clients = () => languages.flatMap((l) => l.clients())
  const servers = clients()
  useEffect(() => {
    const q = query.trim()
    if (!q) {
      setFound(null)
      return
    }
    let live = true
    const timer = setTimeout(() => {
      workspaceSymbols(clients(), q).then((symbols) => live && setFound({ query: q, symbols }))
    }, 150)
    return () => {
      live = false
      clearTimeout(timer)
    }
    // The servers are read afresh for each search.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [query])
  const at = (uri: string) => place(decodeURIComponent(new URL(uri).pathname))
  const hits = useMemo(() => {
    if (!found) return []
    return (
      found.symbols
        .map((s) => ({ s, m: fuzzy(found.query, s.name), ...at(s.uri) }))
        .filter((h) => h.m)
        // The project's own first, as IntelliJ lists them, then libraries'.
        .sort(
          (a, b) =>
            Number(a.path.startsWith('/')) - Number(b.path.startsWith('/')) ||
            b.m!.score - a.m!.score ||
            a.s.name.localeCompare(b.s.name),
        )
        .slice(0, 100)
    )
    // pathOf reads only the root.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [found])
  useEffect(() => setSel(0), [found])
  const list = useScrollIntoView(sel)
  const choose = (i: number) => {
    const h = hits[i]
    if (!h) return
    onClose()
    onOpen({ path: h.path, root: h.root, line: h.s.line, col: h.s.col })
  }

  return (
    <Popup className="code-goto" onClose={onClose}>
      <div className="code-popup-head">
        <span className="strong">Go to Symbol</span>
        <span className="muted small">
          {servers.length === 0
            ? 'No language server is running'
            : found
              ? `${hits.length === 100 ? '100+' : hits.length} found`
              : `${servers.length} language server${servers.length === 1 ? '' : 's'}`}
        </span>
      </div>
      <input
        className="code-popup-input"
        autoFocus
        spellCheck={false}
        placeholder="Class, function or other symbol"
        value={query}
        onChange={(e) => setQuery(e.target.value)}
        onKeyDown={(e) => {
          if (moveSelection(e, sel, hits.length, setSel)) return
          if (e.key === 'Enter') {
            e.preventDefault()
            choose(sel)
          }
        }}
      />
      <div className="code-popup-list" ref={list} role="listbox">
        {hits.map((h, i) => {
          const k = kindOf(h.s.kind)
          return (
            <div
              key={`${h.s.uri}:${h.s.line}:${h.s.col}:${h.s.name}`}
              role="option"
              aria-selected={i === sel}
              className={i === sel ? 'code-popup-row code-popup-row-on' : 'code-popup-row'}
              title={`${k.name} ${h.s.name}`}
              onMouseMove={() => i !== sel && setSel(i)}
              onClick={() => choose(i)}
            >
              <span className={`code-symbol-kind code-symbol-${k.tone}`}>{k.letter}</span>
              <span className="code-goto-name">
                <Highlighted text={h.s.name} at={h.m!.at} />
                {h.s.container && <span className="muted"> {h.s.container}</span>}
              </span>
              <span className="code-goto-dir">
                {h.path}:{h.s.line}
              </span>
            </div>
          )
        })}
        {servers.length === 0 && (
          <div className="code-popup-empty">Open a file to start its language&rsquo;s server, then search here.</div>
        )}
        {found && hits.length === 0 && <div className="code-popup-empty">Nothing matches.</div>}
      </div>
      <div className="code-popup-foot muted small">
        ↑↓ to choose · Enter to open · {shortcut('Mod+Shift+O')} goes to a file
      </div>
    </Popup>
  )
}

// --- Find in Files ------------------------------------------------------

// SearchProject is a project the search popups look in.
export type SearchProject = { root: string; name: string; client: CodeClient }

// Found is a match, and the project it was found in.
type Found = Match & { root: string }

const nameOfRoot = (root: string) => root.slice(root.lastIndexOf('/') + 1)

// innermost says whether a file found in a project is that project's own,
// not a project's nested inside it (the home folder holding the others),
// so a file is listed once, under the project nearest it.
function innermost(roots: string[], root: string, path: string): boolean {
  const abs = `${root}/${path}`
  return !roots.some((r) => r !== root && r.startsWith(root + '/') && abs.startsWith(r + '/'))
}

type FindOptions = {
  query: string
  regex: boolean
  caseSensitive: boolean
  wholeWord: boolean
  mask: string
  masked: boolean
  replacement: string
}

// The last search, so Find in Files opens where it was left.
let lastFind: FindOptions = {
  query: '',
  regex: false,
  caseSensitive: false,
  wholeWord: false,
  mask: '',
  masked: false,
  replacement: '',
}

// FindInFiles searches the folder's files for text, as IntelliJ's Find in
// Files: matches as they are found, each previewed in its file. Opened on
// a folder it searches that folder, and opened to replace (IntelliJ's
// Replace in Files) it replaces the matches in a file or every file.
export function FindInFiles({
  client,
  projects = [],
  root,
  initial,
  dir,
  replace,
  onOpen,
  onFindWindow,
  onClose,
}: {
  client: CodeClient
  // projects are the workspace's, every one searched unless one is
  // chosen; root is the one the folder searched (dir) is in.
  projects?: SearchProject[]
  root?: string
  // initial is text to search for, such as the editor's selection.
  initial?: string
  // dir is a folder to search, from the root.
  dir?: string
  replace?: boolean
  onOpen: (j: Jump) => void
  // onFindWindow lists the matches in the Find window, to go through.
  onFindWindow: (u: Usages) => void
  onClose: () => void
}) {
  const [opts, setOpts] = useState<FindOptions>(() => (initial ? { ...lastFind, query: initial } : lastFind))
  const [result, setResult] = useState<{ matches: Found[]; truncated: boolean } | null>(null)
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState('')
  const [sel, setSel] = useState(0)
  const [scope, setScope] = useState(dir ?? '')
  // where is the project searched, '' for every one.
  const all: SearchProject[] = projects.length ? projects : [{ root: client.root, name: '', client }]
  const [where, setWhere] = useState(() =>
    dir !== undefined || all.length === 1 ? (root ?? all.find((p) => p.client === client)?.root ?? all[0].root) : '',
  )
  const targets = where ? all.filter((p) => p.root === where) : all
  const clientOf = (r: string) => all.find((p) => p.root === r)?.client ?? client
  const [replacing, setReplacing] = useState(!!replace)
  // round counts replacements, after each of which the search runs again.
  const [round, setRound] = useState(0)
  const [replaced, setReplaced] = useState('')
  const [confirmAll, setConfirmAll] = useState(false)
  const set = (o: Partial<FindOptions>) => {
    setConfirmAll(false)
    setOpts((x) => ({ ...x, ...o }))
  }
  useEffect(() => {
    lastFind = opts
  }, [opts])
  const query = {
    query: opts.query,
    regex: opts.regex,
    caseSensitive: opts.caseSensitive,
    wholeWord: opts.wholeWord,
    mask: opts.masked ? opts.mask : '',
    dir: scope,
  }

  useEffect(() => {
    if (!opts.query) {
      setResult(null)
      setError('')
      return
    }
    let live = true
    const timer = setTimeout(() => {
      setBusy(true)
      Promise.all(
        targets.map((p) =>
          p.client
            .searchText({
              query: opts.query,
              regex: opts.regex,
              caseSensitive: opts.caseSensitive,
              wholeWord: opts.wholeWord,
              mask: opts.masked ? opts.mask : '',
              dir: where ? scope : '',
            })
            .then((r) => ({ ...r, matches: r.matches.map((m) => ({ ...m, root: p.root })) })),
        ),
      )
        .then(
          (rs) => {
            if (!live) return
            const roots = targets.map((p) => p.root)
            setResult({
              matches: rs.flatMap((r) => r.matches).filter((m) => innermost(roots, m.root, m.path)),
              truncated: rs.some((r) => r.truncated),
            })
            setError('')
            setSel(0)
          },
          (e: Error) => live && setError(e.message),
        )
        .finally(() => live && setBusy(false))
    }, 200)
    return () => {
      live = false
      clearTimeout(timer)
    }
    // The replacement is not part of what is searched for.
    // targets follow where, and the projects.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [
    where,
    all.length,
    opts.query,
    opts.regex,
    opts.caseSensitive,
    opts.wholeWord,
    opts.mask,
    opts.masked,
    scope,
    round,
  ])

  // replaceIn replaces the matches in the file given, or every file of
  // every project searched.
  const replaceIn = async (only?: Found) => {
    setConfirmAll(false)
    try {
      const rs = await Promise.all(
        (only ? targets.filter((p) => p.root === only.root) : targets).map((p) =>
          p.client.replace({ ...query, dir: where ? scope : '' }, opts.replacement, only ? [only.path] : undefined),
        ),
      )
      const n = rs.reduce((a, r) => a + r.replaced, 0)
      const files = rs.reduce((a, r) => a + r.files.length, 0)
      setReplaced(
        n
          ? `Replaced ${n} match${n === 1 ? '' : 'es'} in ${files} file${files === 1 ? '' : 's'}.`
          : 'Nothing was replaced.',
      )
      setRound((n) => n + 1)
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e))
    }
  }

  const matches = result?.matches ?? []
  const fileCount = useMemo(() => new Set(matches.map((m) => `${m.root}/${m.path}`)).size, [matches])
  // Each match in a line, which a line's row may hold several of.
  const occurrences = useMemo(() => matches.reduce((n, m) => n + Math.max(1, m.ranges.length), 0), [matches])
  useEffect(() => setConfirmAll(false), [result])
  const list = useScrollIntoView(sel)
  const current = matches[sel]

  const choose = (m: Found | undefined) => {
    if (!m) return
    onClose()
    const first = m.ranges[0]
    onOpen({ path: m.path, root: m.root, line: m.line, find: first ? m.text.slice(first[0], first[1]) : undefined })
  }

  // toFindWindow moves the matches to the Find window, as IntelliJ's Open
  // in Find Window does: each match in a line its own row.
  const toFindWindow = () => {
    if (!result || !matches.length) return
    onClose()
    onFindWindow({
      kind: 'text',
      symbol: opts.query,
      state: 'found',
      more: result.truncated,
      usages: matches.flatMap((m) =>
        (m.ranges.length ? m.ranges : [[0, 0] as [number, number]]).map(([s, e]) => ({
          path: m.path,
          root: m.root,
          line: m.line,
          col: s + 1,
          length: e - s,
          text: m.text,
        })),
      ),
    })
  }

  const toggle = (key: 'caseSensitive' | 'wholeWord' | 'regex', label: string, icon: ReactNode) => (
    <button
      type="button"
      className={opts[key] ? 'code-popup-toggle code-popup-toggle-on' : 'code-popup-toggle'}
      title={label}
      aria-label={label}
      aria-pressed={opts[key]}
      onClick={() => set({ [key]: !opts[key] })}
    >
      {icon}
    </button>
  )

  return (
    <Popup className="code-find" onClose={onClose}>
      <div className="code-popup-head">
        <span className="strong">{replacing ? 'Replace in Files' : 'Find in Files'}</span>
        {all.length > 1 && (
          <select
            className="code-project-picker"
            aria-label="Where"
            value={where}
            onChange={(e) => {
              setWhere(e.target.value)
              setScope('')
            }}
          >
            <option value="">Every project</option>
            {all.map((p) => (
              <option key={p.root} value={p.root}>
                {p.name}
              </option>
            ))}
          </select>
        )}
        {scope && where && (
          <span className="code-log-path" title={`Only the files in ${scope}`}>
            <Folder size={12} />
            {scope}
            <button
              type="button"
              title="Search every folder"
              aria-label="Search every folder"
              onClick={() => setScope('')}
            >
              <X size={11} />
            </button>
          </span>
        )}
        <span className="muted small">
          {busy
            ? 'Searching…'
            : result
              ? `${matches.length}${result.truncated ? '+' : ''} match${matches.length === 1 ? '' : 'es'} in ${fileCount} file${fileCount === 1 ? '' : 's'}`
              : ''}
        </span>
        <span className="code-tool-spacer" />
        <label className="code-find-mask">
          <input type="checkbox" checked={opts.masked} onChange={(e) => set({ masked: e.target.checked })} />
          File mask
        </label>
        <input
          className="code-find-mask-input"
          spellCheck={false}
          placeholder="*.go, !*_test.go"
          value={opts.mask}
          disabled={!opts.masked}
          onChange={(e) => set({ mask: e.target.value })}
        />
      </div>
      <div className="code-find-query">
        <input
          className="code-popup-input"
          autoFocus
          spellCheck={false}
          placeholder="Text to find"
          value={opts.query}
          onFocus={(e) => e.target.select()}
          onChange={(e) => set({ query: e.target.value })}
          onKeyDown={(e) => {
            if (moveSelection(e, sel, matches.length, setSel)) return
            if (e.key === 'Enter' && (isMac ? e.metaKey : e.ctrlKey)) {
              e.preventDefault()
              toFindWindow()
            } else if (e.key === 'Enter') {
              e.preventDefault()
              choose(current)
            }
          }}
        />
        {toggle('caseSensitive', 'Match case', <CaseSensitive size={15} />)}
        {toggle('wholeWord', 'Words', <WholeWord size={15} />)}
        {toggle('regex', 'Regex', <Regex size={15} />)}
        <button
          type="button"
          className={replacing ? 'code-popup-toggle code-popup-toggle-on' : 'code-popup-toggle'}
          title="Replace"
          aria-label="Replace"
          aria-pressed={replacing}
          onClick={() => setReplacing((x) => !x)}
        >
          <Replace size={15} />
        </button>
      </div>
      {replacing && (
        <div className="code-find-query">
          <input
            className="code-popup-input"
            spellCheck={false}
            placeholder={opts.regex ? 'Replace with ($1 for a group)' : 'Replace with'}
            value={opts.replacement}
            onChange={(e) => set({ replacement: e.target.value })}
          />
          <button
            type="button"
            className="btn btn-sm"
            disabled={!current || busy}
            title={current ? `Replace the matches in ${current.path}` : undefined}
            onClick={() => current && replaceIn(current)}
          >
            Replace in File
          </button>
          <button
            type="button"
            className={confirmAll ? 'btn btn-sm btn-danger' : 'btn btn-sm'}
            disabled={!matches.length || busy}
            onClick={() => (confirmAll ? replaceIn() : setConfirmAll(true))}
          >
            {confirmAll
              ? result?.truncated
                ? 'Replace every match?'
                : `Replace ${occurrences} match${occurrences === 1 ? '' : 'es'} in ${fileCount} file${fileCount === 1 ? '' : 's'}?`
              : 'Replace All'}
          </button>
        </div>
      )}
      {replaced && <div className="code-find-replaced muted small">{replaced}</div>}
      <div className="code-popup-list code-find-list" ref={list} role="listbox">
        {matches.map((m, i) => (
          <div
            key={`${m.path}:${m.line}:${i}`}
            role="option"
            aria-selected={i === sel}
            className={i === sel ? 'code-popup-row code-popup-row-on' : 'code-popup-row'}
            onMouseDown={() => setSel(i)}
            onDoubleClick={() => choose(m)}
          >
            <span className="code-find-text">
              <Ranges text={m.text} ranges={m.ranges} />
            </span>
            <span className="code-find-where">
              {!where && all.length > 1 && <span className="code-tab-project">{nameOfRoot(m.root)} </span>}
              {m.path.split('/').pop()} {m.line}
            </span>
          </div>
        ))}
        {error && <div className="code-popup-empty code-note-error">{error}</div>}
        {!error && result && matches.length === 0 && <div className="code-popup-empty">Nothing found.</div>}
      </div>
      {current && (
        <Preview
          key={`${current.root}:${current.path}`}
          client={clientOf(current.root)}
          match={current}
          onOpen={() => choose(current)}
        />
      )}
      <div className="code-popup-foot code-find-foot">
        <span className="muted small">Enter opens the match</span>
        <span className="code-tool-spacer" />
        <button
          type="button"
          className="btn btn-sm"
          disabled={!matches.length}
          title={`List every match in the Find window (${shortcut('Mod+Enter')})`}
          onClick={toFindWindow}
        >
          Open in Find Window
        </button>
      </div>
    </Popup>
  )
}

function Ranges({ text, ranges }: { text: string; ranges: [number, number][] }) {
  const at: number[] = []
  for (const [s, e] of ranges) for (let i = s; i < e; i++) at.push(i)
  return <Highlighted text={text} at={at} />
}

const hitMark = Decoration.mark({ class: 'code-find-hit' })
const hitLine = Decoration.line({ class: 'code-find-line' })

// matchDecorations marks a match's line, and the matches in it where the
// line as listed can be found in the file's.
function matchDecorations(state: EditorState, m: Match) {
  const b = new RangeSetBuilder<Decoration>()
  if (m.line < 1 || m.line > state.doc.lines) return b.finish()
  const line = state.doc.line(m.line)
  b.add(line.from, line.from, hitLine)
  const off = line.text.indexOf(m.text)
  if (off >= 0) {
    for (const [s, e] of m.ranges) {
      if (e > s) b.add(line.from + off + s, line.from + off + e, hitMark)
    }
  }
  return b.finish()
}

// Preview shows a match in its file, read-only, the way Find in Files
// shows it below the list and the Find window beside it.
export function Preview({ client, match, onOpen }: { client: CodeClient; match: Match; onOpen: () => void }) {
  const host = useRef<HTMLDivElement>(null)
  const view = useRef<EditorView | null>(null)
  const texts = useRef(new Map<string, Promise<string | null>>())
  const shown = useRef('')
  const [note, setNote] = useState('')
  const language = useMemo(() => new Compartment(), [])
  const marks = useMemo(() => new Compartment(), [])

  // A file's state: made afresh for each file shown, so its gutter, its
  // folds and its scroll are its own, nothing kept from the file before.
  const stateFor = useCallback(
    (doc: string) =>
      EditorState.create({
        doc,
        extensions: [
          lineNumbers(),
          codeHighlight,
          followScheme,
          codeTheme,
          language.of([]),
          marks.of([]),
          EditorState.readOnly.of(true),
          EditorView.editable.of(false),
          EditorView.domEventHandlers({ dblclick: () => (onOpenRef.current(), true) }),
        ],
      }),
    [language, marks],
  )
  useEffect(() => {
    const v = new EditorView({ parent: host.current!, state: stateFor('') })
    view.current = v
    shown.current = ''
    return () => v.destroy()
  }, [stateFor])
  const onOpenRef = useRef(onOpen)
  useEffect(() => {
    onOpenRef.current = onOpen
  })

  useEffect(() => {
    let live = true
    let text = texts.current.get(match.path)
    if (!text) {
      text = client.read(match.path).catch(() => null)
      texts.current.set(match.path, text)
    }
    text.then(async (t) => {
      const v = view.current
      if (!live || !v) return
      if (t === null) {
        setNote(`${match.path} cannot be shown.`)
        return
      }
      setNote('')
      if (shown.current !== match.path) {
        shown.current = match.path
        v.setState(stateFor(t.replace(/\r\n/g, '\n')))
        const ext: Extension = await languageFor(match.path.split('/').pop() ?? '')
        if (!live) return
        v.dispatch({ effects: language.reconfigure(ext) })
      }
      const deco = matchDecorations(v.state, match)
      const pos = match.line >= 1 && match.line <= v.state.doc.lines ? v.state.doc.line(match.line).from : 0
      v.dispatch({
        effects: [marks.reconfigure(EditorView.decorations.of(deco)), EditorView.scrollIntoView(pos, { y: 'center' })],
      })
    })
    return () => {
      live = false
    }
  }, [client, match, language, marks, stateFor])

  return (
    <div className="code-find-preview">
      <div className="code-find-preview-head muted small">
        <span>{match.path}</span>
        <span className="code-tool-spacer" />
        <span>Enter or double-click to open</span>
      </div>
      <div ref={host} className="code-find-preview-body" />
      {note && <div className="code-popup-empty">{note}</div>}
    </div>
  )
}

// --- Open Folder --------------------------------------------------------

// OpenFolder chooses another folder for this browser tab to work in, by
// path, completing folder names as they are typed.
export function OpenFolder({
  client,
  onChoose,
  onAdd,
  have = [],
  onClose,
}: {
  client: CodeClient
  // onChoose is given the folder, or '' for the environment's own, to open
  // alone in this tab.
  onChoose: (dir: string) => void
  // onAdd adds the folder to the workspace; without it the popup only
  // opens folders alone.
  onAdd?: (dir: string) => void
  // have are the workspace's folders already, not offered again.
  have?: string[]
  onClose: () => void
}) {
  const [path, setPath] = useState(client.root + '/')
  // The git repositories in the home folder, to add with a click.
  const [repos, setRepos] = useState<string[]>([])
  useEffect(() => {
    if (!onAdd) return
    let live = true
    client.repositories().then(
      (r) => live && setRepos(r),
      () => {},
    )
    return () => {
      live = false
    }
  }, [client, onAdd])
  const offered = repos.filter((r) => !have.includes(r))
  const [dirs, setDirs] = useState<string[]>([])
  const [error, setError] = useState('')
  const [sel, setSel] = useState(-1)
  const parent = path.slice(0, path.lastIndexOf('/') + 1) || '/'
  const partial = path.slice(path.lastIndexOf('/') + 1).toLowerCase()

  useEffect(() => {
    let live = true
    client.list(parent).then(
      (ents) => live && setDirs(ents.filter((e) => e.dir).map((e) => e.name)),
      () => live && setDirs([]),
    )
    return () => {
      live = false
    }
  }, [client, parent])
  const shown = dirs.filter(
    (d) => d.toLowerCase().startsWith(partial) && (partial.startsWith('.') || !d.startsWith('.')),
  )
  useEffect(() => setSel(-1), [parent, partial])
  const list = useScrollIntoView(sel)

  const open = async (dir: string) => {
    const want = dir.length > 1 ? dir.replace(/\/+$/, '') : dir
    if (!want.startsWith('/')) {
      setError('A folder is an absolute path.')
      return
    }
    try {
      await client.list(want)
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e))
      return
    }
    onClose()
    onChoose(want === client.home ? '' : want)
  }
  // add adds a folder to the workspace, once it is known to be one.
  const add = async (dir: string) => {
    const want = dir.length > 1 ? dir.replace(/\/+$/, '') : dir
    if (!want.startsWith('/')) {
      setError('A folder is an absolute path.')
      return
    }
    if (have.includes(want)) {
      setError(`${want} is in the workspace already.`)
      return
    }
    try {
      await client.list(want)
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e))
      return
    }
    onClose()
    onAdd?.(want)
  }

  return (
    <Popup className="code-open-folder" onClose={onClose}>
      <div className="code-popup-head">
        <span className="strong">{onAdd ? 'Add Folder to Workspace' : 'Open Folder'}</span>
        <span className="muted small">{onAdd ? 'a project beside the others' : 'in this browser tab only'}</span>
      </div>
      <input
        className="code-popup-input"
        autoFocus
        spellCheck={false}
        value={path}
        onChange={(e) => {
          setPath(e.target.value)
          setError('')
        }}
        onKeyDown={(e) => {
          if (moveSelection(e, sel, shown.length, setSel)) return
          if (e.key === 'Tab' || (e.key === 'Enter' && sel >= 0)) {
            e.preventDefault()
            const d = shown[Math.max(sel, 0)]
            if (d) setPath(parent + d + '/')
          } else if (e.key === 'Enter') {
            e.preventDefault()
            if (onAdd) add(path)
            else open(path)
          }
        }}
      />
      <div className="code-popup-list" ref={list} role="listbox">
        {shown.map((d, i) => (
          <div
            key={d}
            role="option"
            aria-selected={i === sel}
            className={i === sel ? 'code-popup-row code-popup-row-on' : 'code-popup-row'}
            onMouseMove={() => i !== sel && setSel(i)}
            onClick={() => setPath(parent + d + '/')}
            onDoubleClick={() => open(parent + d)}
          >
            <Folder size={14} className="code-icon-dir" />
            <span>{d}</span>
          </div>
        ))}
        {shown.length === 0 && <div className="code-popup-empty">No folders here.</div>}
        {offered.length > 0 && <div className="code-popup-section">Repositories on this machine</div>}
        {offered.map((r) => (
          <div
            key={r}
            className="code-popup-row"
            title={`Add ${r}`}
            onClick={() => setPath(r + '/')}
            onDoubleClick={() => add(r)}
          >
            <GitBranch size={14} className="code-icon-dir" />
            <span>{r.slice(r.lastIndexOf('/') + 1)}</span>
            <span className="code-goto-dir">
              {r.startsWith(client.home + '/') ? '~' + r.slice(client.home.length) : r}
            </span>
          </div>
        ))}
      </div>
      {error && <div className="code-note code-note-error">{error}</div>}
      <div className="code-popup-actions">
        <span className="muted small">Tab completes · Enter {onAdd ? 'adds' : 'opens'}</span>
        <span className="code-tool-spacer" />
        {!onAdd && client.root !== client.home && (
          <button type="button" className="btn btn-ghost" onClick={() => (onClose(), onChoose(''))}>
            <FolderOpen size={14} /> Back to {client.home}
          </button>
        )}
        <button
          type="button"
          className={onAdd ? 'btn btn-ghost' : 'btn btn-primary'}
          title="Show only this folder, in this browser tab"
          onClick={() => open(path)}
        >
          {onAdd ? 'Open Alone in This Tab' : 'Open'}
        </button>
        {onAdd && (
          <button type="button" className="btn btn-primary" onClick={() => add(path)}>
            Add to Workspace
          </button>
        )}
      </div>
    </Popup>
  )
}
