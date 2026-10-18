import { EditorState, RangeSetBuilder, Compartment, type Extension } from '@codemirror/state'
import { Decoration, EditorView, lineNumbers } from '@codemirror/view'
import { CaseSensitive, File, Folder, FolderOpen, Regex, Replace, WholeWord, X } from 'lucide-react'
import { useCallback, useEffect, useMemo, useRef, useState, type KeyboardEvent, type ReactNode } from 'react'

import type { CodeClient, Match } from './client'
import { codeHighlight, codeTheme, followScheme, languageFor } from './CodeEditor'
import { isMac, shortcut } from './ContextMenu'
import type { Languages, Usages } from './lsp'
import { kindOf, workspaceSymbols, type FoundSymbol } from './symbols'

// Jump is a place in a file to open it at: a 1-based line (0 for none),
// and optionally a 1-based column, or text in the line to select. focus
// false leaves the keyboard where it is, as stepping through a list does.
// Jump is a place in a file to go to: a line (from 1; 0 is none), the
// column and text there, whether the editor takes focus, and whether its
// annotations (git blame) are shown.
export type Jump = { path: string; line: number; col?: number; find?: string; focus?: boolean; blame?: boolean }

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

type FileHit = { path: string; score: number; name: number[]; dir: number[] }

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
  title,
  recent,
  onOpen,
  onClose,
}: {
  client: CodeClient
  title: string
  recent: string[]
  onOpen: (j: Jump) => void
  onClose: () => void
}) {
  const [query, setQuery] = useState('')
  const [files, setFiles] = useState<string[] | null>(null)
  const [error, setError] = useState('')
  const [sel, setSel] = useState(0)
  useEffect(() => {
    client.searchFiles().then(
      (r) => setFiles(r.files),
      (e: Error) => setError(e.message),
    )
  }, [client])

  const m = /^(.*?)(?::(\d+)(?::(\d+))?)?$/.exec(query.trim())!
  const name = m[1]
  const line = m[2] ? Number(m[2]) : undefined
  const col = m[3] ? Number(m[3]) : undefined
  const hits = useMemo(() => {
    if (!name) return recent.map((path): FileHit => ({ path, score: 0, name: [], dir: [] }))
    return files ? rank(files, name).slice(0, 80) : []
  }, [files, name, recent])
  useEffect(() => setSel(0), [name])
  const list = useScrollIntoView(sel)

  const choose = (h: FileHit | undefined) => {
    if (!h) return
    onClose()
    onOpen({ path: h.path, line: line ?? 0, col })
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
        placeholder="File name, or name:line"
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
              key={h.path}
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
  client,
  languages,
  onOpen,
  onClose,
}: {
  client: CodeClient
  languages: Languages
  onOpen: (j: Jump) => void
  onClose: () => void
}) {
  const [query, setQuery] = useState('')
  const [found, setFound] = useState<{ query: string; symbols: FoundSymbol[] } | null>(null)
  const [sel, setSel] = useState(0)
  const servers = languages.clients()
  useEffect(() => {
    const q = query.trim()
    if (!q) {
      setFound(null)
      return
    }
    let live = true
    const timer = setTimeout(() => {
      workspaceSymbols(languages.clients(), q).then((symbols) => live && setFound({ query: q, symbols }))
    }, 150)
    return () => {
      live = false
      clearTimeout(timer)
    }
  }, [query, languages])
  const pathOf = (uri: string) => {
    const abs = decodeURIComponent(new URL(uri).pathname)
    return abs.startsWith(client.root + '/') ? abs.slice(client.root.length + 1) : abs
  }
  const hits = useMemo(() => {
    if (!found) return []
    return (
      found.symbols
        .map((s) => ({ s, m: fuzzy(found.query, s.name), path: pathOf(s.uri) }))
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
    onOpen({ path: h.path, line: h.s.line, col: h.s.col })
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
  initial,
  dir,
  replace,
  onOpen,
  onFindWindow,
  onClose,
}: {
  client: CodeClient
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
  const [result, setResult] = useState<{ matches: Match[]; truncated: boolean } | null>(null)
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState('')
  const [sel, setSel] = useState(0)
  const [scope, setScope] = useState(dir ?? '')
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
      client
        .searchText({
          query: opts.query,
          regex: opts.regex,
          caseSensitive: opts.caseSensitive,
          wholeWord: opts.wholeWord,
          mask: opts.masked ? opts.mask : '',
          dir: scope,
        })
        .then(
          (r) => {
            if (!live) return
            setResult(r)
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
  }, [client, opts.query, opts.regex, opts.caseSensitive, opts.wholeWord, opts.mask, opts.masked, scope, round])

  // replaceIn replaces the matches in the files given, or every file.
  const replaceIn = async (paths?: string[]) => {
    setConfirmAll(false)
    try {
      const r = await client.replace(query, opts.replacement, paths)
      setReplaced(
        r.replaced
          ? `Replaced ${r.replaced} match${r.replaced === 1 ? '' : 'es'} in ${r.files.length} file${r.files.length === 1 ? '' : 's'}.`
          : 'Nothing was replaced.',
      )
      setRound((n) => n + 1)
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e))
    }
  }

  const matches = result?.matches ?? []
  const fileCount = useMemo(() => new Set(matches.map((m) => m.path)).size, [matches])
  // Each match in a line, which a line's row may hold several of.
  const occurrences = useMemo(() => matches.reduce((n, m) => n + Math.max(1, m.ranges.length), 0), [matches])
  useEffect(() => setConfirmAll(false), [result])
  const list = useScrollIntoView(sel)
  const current = matches[sel]

  const choose = (m: Match | undefined) => {
    if (!m) return
    onClose()
    const first = m.ranges[0]
    onOpen({ path: m.path, line: m.line, find: first ? m.text.slice(first[0], first[1]) : undefined })
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
        {scope && (
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
            onClick={() => current && replaceIn([current.path])}
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
              {m.path.split('/').pop()} {m.line}
            </span>
          </div>
        ))}
        {error && <div className="code-popup-empty code-note-error">{error}</div>}
        {!error && result && matches.length === 0 && <div className="code-popup-empty">Nothing found.</div>}
      </div>
      {current && <Preview client={client} match={current} onOpen={() => choose(current)} />}
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
  onClose,
}: {
  client: CodeClient
  // onChoose is given the folder, or '' for the environment's own.
  onChoose: (dir: string) => void
  onClose: () => void
}) {
  const [path, setPath] = useState(client.root + '/')
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

  return (
    <Popup className="code-open-folder" onClose={onClose}>
      <div className="code-popup-head">
        <span className="strong">Open Folder</span>
        <span className="muted small">in this browser tab only</span>
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
            open(path)
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
      </div>
      {error && <div className="code-note code-note-error">{error}</div>}
      <div className="code-popup-actions">
        <span className="muted small">Tab completes · Enter opens</span>
        <span className="code-tool-spacer" />
        {client.root !== client.home && (
          <button type="button" className="btn btn-ghost" onClick={() => (onClose(), onChoose(''))}>
            <FolderOpen size={14} /> Back to {client.home}
          </button>
        )}
        <button type="button" className="btn btn-primary" onClick={() => open(path)}>
          Open
        </button>
      </div>
    </Popup>
  )
}
