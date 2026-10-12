import { MergeView } from '@codemirror/merge'
import { EditorState, type Extension } from '@codemirror/state'
import { EditorView, lineNumbers } from '@codemirror/view'
import { useEffect, useMemo, useRef, useState } from 'react'

import { formatBytes } from '../components/format'
import type { CodeClient } from './client'
import { codeHighlight, codeTheme, followScheme, languageFor } from './CodeEditor'
import { toURL } from './ImageView'
import { imageType } from './views'

// diffTheme colours the merge view's changes by the page's --diff-*
// colours (code.css), over the merge view's own, which follow CodeMirror's
// light or dark setting rather than the page's.
const diffTheme = EditorView.theme({
  '&.cm-merge-a .cm-changedLine, .cm-deletedChunk': { backgroundColor: 'var(--diff-deleted-line)' },
  '&.cm-merge-b .cm-changedLine': { backgroundColor: 'var(--diff-inserted-line)' },
  '&.cm-merge-a .cm-changedText, .cm-deletedChunk .cm-deletedText': {
    background: 'var(--diff-deleted-text) !important',
  },
  '&.cm-merge-b .cm-changedText': { background: 'var(--diff-inserted-text) !important' },
  '&.cm-merge-a .cm-changedLineGutter, .cm-deletedLineGutter': { background: 'var(--code-git-conflict) !important' },
  '&.cm-merge-b .cm-changedLineGutter': { background: 'var(--code-git-added) !important' },
  '.cm-collapsedLines': {
    color: 'var(--muted) !important',
    background: 'var(--surface-2) !important',
    borderTop: '1px solid var(--border)',
    borderBottom: '1px solid var(--border)',
  },
})

const diffSide = (lang: Extension): Extension => [
  lineNumbers(),
  codeHighlight,
  followScheme,
  codeTheme,
  diffTheme,
  lang,
  EditorState.readOnly.of(true),
  EditorView.editable.of(false),
]

// DiffView shows a file as it was at HEAD, or at base, beside its working
// copy, or beside a later commit's (to), and follows the file as it
// changes, whoever changes it.
export function DiffView({
  client,
  path,
  base,
  baseLabel,
  from,
  to,
  toLabel,
}: {
  client: CodeClient
  path: string
  base?: string
  baseLabel?: string
  from?: string
  to?: string
  toLabel?: string
}) {
  const host = useRef<HTMLDivElement>(null)
  const [error, setError] = useState('')
  // What the diff alone does not show: a new name, or nothing changed.
  const [about, setAbout] = useState<{ from?: string; same: boolean } | null>(null)
  // A file of bytes is shown by BinarySides; this counts its loads.
  const [binary, setBinary] = useState<{ from?: string; n: number } | null>(null)

  useEffect(() => {
    const el = host.current
    if (!el) return
    let view: MergeView | null = null
    let disposed = false
    let lang: Extension = []
    const dir = path.includes('/') ? path.slice(0, path.lastIndexOf('/')) : ''

    const load = async () => {
      try {
        const d = await client.diff(path, base, from, to)
        if (disposed) return
        setError('')
        if (d.binary) {
          view?.destroy()
          view = null
          setAbout(d.from ? { from: d.from, same: false } : null)
          setBinary((b) => ({ from: d.from, n: (b?.n ?? 0) + 1 }))
          return
        }
        setBinary(null)
        const scroll = view?.b.scrollDOM.scrollTop ?? 0
        const a = d.head.replace(/\r\n/g, '\n')
        const b = d.working.replace(/\r\n/g, '\n')
        setAbout({ from: d.from, same: a === b })
        view?.destroy()
        view = new MergeView({
          parent: el,
          a: { doc: a, extensions: diffSide(lang) },
          b: { doc: b, extensions: diffSide(lang) },
          // A file the same on both sides is shown whole, not as one fold.
          collapseUnchanged: a === b ? undefined : { margin: 3, minSize: 6 },
          gutter: true,
        })
        view.b.scrollDOM.scrollTop = scroll
      } catch (e) {
        if (!disposed) setError(e instanceof Error ? e.message : String(e))
      }
    }
    languageFor(path.split('/').pop() ?? '').then((l) => {
      lang = l
      load()
    })
    let timer: ReturnType<typeof setTimeout> | undefined
    const later = () => {
      clearTimeout(timer)
      timer = setTimeout(load, 250)
    }
    const offFs = client.on('fs.changed', ({ dirs }) => {
      if (dirs.some((d) => d === client.root + (dir ? '/' + dir : '') || d.endsWith('/' + dir))) later()
    })
    const offGit = client.on('git.changed', later)
    return () => {
      disposed = true
      clearTimeout(timer)
      offFs()
      offGit()
      view?.destroy()
    }
  }, [client, path, base, from, to])

  return (
    <div className="code-diff">
      {about && (about.from || about.same) && (
        <div className="code-diff-about">
          {about.from ? (
            <>
              Renamed from <code>{about.from}</code> to <code>{path}</code>.
              {about.same && ' The content is unchanged.'}
            </>
          ) : (
            'The content is unchanged.'
          )}
        </div>
      )}
      <div className="code-diff-heads">
        <span>
          {base ? (baseLabel ?? base.slice(0, 8)) : 'HEAD'}
          {about?.from && <span className="code-diff-name">{about.from}</span>}
        </span>
        <span>
          {to ? (toLabel ?? to.slice(0, 8)) : 'Working tree'}
          {about?.from && <span className="code-diff-name">{path}</span>}
        </span>
      </div>
      {error && <div className="code-note">{error}</div>}
      {binary && (
        <BinarySides key={binary.n} client={client} path={path} base={base} from={binary.from} to={to} />
      )}
      <div ref={host} className="code-diff-host" hidden={!!binary} />
    </div>
  )
}

// BinarySides shows a file of bytes as it was and as it is: an image as
// both images, anything else by its size, as git does.
function BinarySides({
  client,
  path,
  base,
  from,
  to,
}: {
  client: CodeClient
  path: string
  base?: string
  from?: string
  to?: string
}) {
  const [sides, setSides] = useState<{ size: number; data: string }[] | null>(null)
  const type = imageType(path)
  useEffect(() => {
    let live = true
    Promise.all([
      client.blob(base ?? 'HEAD', from ?? path),
      to
        ? client.blob(to, path)
        : client.bytes(path, 0, 32 << 20).then(
            (b) => ({ size: b.size, data: b.data }),
            () => ({ size: 0, data: '' }),
          ),
    ]).then((s) => live && setSides(s))
    return () => {
      live = false
    }
  }, [client, path, base, from, to])
  const urls = useMemo(() => (sides && type ? sides.map((s) => (s.data ? toURL(s.data, type) : null)) : []), [sides, type])
  useEffect(() => () => urls.forEach((u) => u && URL.revokeObjectURL(u)), [urls])
  if (!sides) return null
  const same = sides[0].data === sides[1].data
  return (
    <div className="code-diff-binary">
      {sides.map((s, i) => (
        <div key={i} className="code-diff-binary-side">
          {!s.data ? (
            <p className="code-note">{i === 0 ? 'The file is new.' : 'The file is deleted.'}</p>
          ) : type ? (
            <img src={urls[i] ?? undefined} alt="" className="code-image-chess" />
          ) : (
            <p className="code-note">Binary file</p>
          )}
          {s.data && (
            <span className="muted small">
              {formatBytes(s.size)} ({s.size.toLocaleString()} bytes)
            </span>
          )}
        </div>
      ))}
      {same && <p className="code-note code-diff-binary-same">The bytes are the same.</p>}
    </div>
  )
}

// The clipboard's text, as it was when a file was compared with it, by the
// id a compare tab names it by. It lasts as long as the page.
export const clipboards = new Map<string, string>()
let clipboardIds = 0

// keepClipboard keeps text for a compare tab, returning what the tab's
// other side is: clipboard:<id>.
export function keepClipboard(text: string): string {
  const id = `clipboard:${++clipboardIds}`
  clipboards.set(id, text)
  return id
}

// sideName is how a compare tab's side is named: a file by its path, the
// clipboard as that.
export function sideName(side: string): string {
  return side.startsWith('clipboard:') ? 'Clipboard' : side
}

// CompareView shows two texts side by side: two files, or a file and what
// was on the clipboard, as IntelliJ's Compare Files and Compare with
// Clipboard do. The files are followed as they change.
export function CompareView({ client, left, right }: { client: CodeClient; left: string; right: string }) {
  const host = useRef<HTMLDivElement>(null)
  const [error, setError] = useState('')
  const [same, setSame] = useState(false)

  useEffect(() => {
    const el = host.current
    if (!el) return
    let view: MergeView | null = null
    let disposed = false
    let lang: Extension = []
    const text = async (side: string): Promise<string> => {
      if (side.startsWith('clipboard:')) {
        const t = clipboards.get(side)
        if (t === undefined) throw new Error('What was on the clipboard is gone with the page it was compared in.')
        return t
      }
      const t = await client.read(side)
      if (t === null) throw new Error(`${side.split('/').pop()} is too large to compare, or not text.`)
      return t
    }
    const load = async () => {
      try {
        const [a, b] = (await Promise.all([text(left), text(right)])).map((t) => t.replace(/\r\n/g, '\n'))
        if (disposed) return
        setError('')
        setSame(a === b)
        const scroll = view?.b.scrollDOM.scrollTop ?? 0
        view?.destroy()
        view = new MergeView({
          parent: el,
          a: { doc: a, extensions: diffSide(lang) },
          b: { doc: b, extensions: diffSide(lang) },
          collapseUnchanged: a === b ? undefined : { margin: 3, minSize: 6 },
          gutter: true,
        })
        view.b.scrollDOM.scrollTop = scroll
      } catch (e) {
        if (!disposed) setError(e instanceof Error ? e.message : String(e))
      }
    }
    languageFor(right.split('/').pop() ?? '').then((l) => {
      lang = l
      load()
    })
    let timer: ReturnType<typeof setTimeout> | undefined
    const off = client.on('fs.changed', () => {
      clearTimeout(timer)
      timer = setTimeout(load, 250)
    })
    return () => {
      disposed = true
      clearTimeout(timer)
      off()
      view?.destroy()
    }
  }, [client, left, right])

  return (
    <div className="code-diff">
      {same && !error && <div className="code-diff-about">The contents are the same.</div>}
      <div className="code-diff-heads">
        <span>{sideName(left)}</span>
        <span>{sideName(right)}</span>
      </div>
      {error && <div className="code-note">{error}</div>}
      <div ref={host} className="code-diff-host" hidden={!!error} />
    </div>
  )
}
