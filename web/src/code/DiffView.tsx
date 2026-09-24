import { MergeView } from '@codemirror/merge'
import { EditorState, type Extension } from '@codemirror/state'
import { EditorView, lineNumbers } from '@codemirror/view'
import { syntaxHighlighting, defaultHighlightStyle } from '@codemirror/language'
import { useEffect, useRef, useState } from 'react'

import type { CodeClient } from './client'
import { codeTheme, languageFor } from './CodeEditor'

const diffSide = (lang: Extension): Extension => [
  lineNumbers(),
  syntaxHighlighting(defaultHighlightStyle, { fallback: true }),
  codeTheme,
  lang,
  EditorState.readOnly.of(true),
  EditorView.editable.of(false),
]

// DiffView shows a file as it was at HEAD beside its working copy, and follows
// the file as it changes, whoever changes it.
export function DiffView({ client, path }: { client: CodeClient; path: string }) {
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
        const d = await client.diff(path)
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
  }, [client, path])

  return (
    <div className="code-diff">
      <div className="code-diff-heads">
        <span>HEAD</span>
        <span>Working tree</span>
      </div>
      {error && <div className="code-note">{error}</div>}
      <div ref={host} className="code-diff-host" />
    </div>
  )
}
