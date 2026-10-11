import { MergeView } from '@codemirror/merge'
import { EditorState, type Extension } from '@codemirror/state'
import { EditorView, lineNumbers } from '@codemirror/view'
import { useEffect, useRef, useState } from 'react'

import type { CodeClient } from './client'
import { codeHighlight, codeTheme, followScheme, languageFor } from './CodeEditor'

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
// copy, and follows the file as it changes, whoever changes it.
export function DiffView({
  client,
  path,
  base,
  baseLabel,
  from,
}: {
  client: CodeClient
  path: string
  base?: string
  baseLabel?: string
  from?: string
}) {
  const host = useRef<HTMLDivElement>(null)
  const [error, setError] = useState('')

  useEffect(() => {
    const el = host.current
    if (!el) return
    let view: MergeView | null = null
    let disposed = false
    let lang: Extension = []
    const dir = path.includes('/') ? path.slice(0, path.lastIndexOf('/')) : ''

    const load = async () => {
      try {
        const d = await client.diff(path, base, from)
        if (disposed) return
        setError('')
        const scroll = view?.b.scrollDOM.scrollTop ?? 0
        view?.destroy()
        view = new MergeView({
          parent: el,
          a: { doc: d.head.replace(/\r\n/g, '\n'), extensions: diffSide(lang) },
          b: { doc: d.working.replace(/\r\n/g, '\n'), extensions: diffSide(lang) },
          collapseUnchanged: { margin: 3, minSize: 6 },
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
  }, [client, path, base, from])

  return (
    <div className="code-diff">
      <div className="code-diff-heads">
        <span>{base ? (baseLabel ?? base.slice(0, 8)) : 'HEAD'}</span>
        <span>Working tree</span>
      </div>
      {error && <div className="code-note">{error}</div>}
      <div ref={host} className="code-diff-host" />
    </div>
  )
}
