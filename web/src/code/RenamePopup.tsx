import type { EditorView } from '@codemirror/view'
import { useEffect, useRef, useState } from 'react'
import { createPortal } from 'react-dom'

import { renameAt } from './editorPopups'

// RenamePopup renames a symbol as IntelliJ does, where it is: a field under
// it holding its name, which Enter renames everywhere its language server
// finds it, and Escape or a click away leaves as it was.
export function RenamePopup({
  view,
  from,
  to,
  onClose,
}: {
  view: EditorView
  from: number
  to: number
  onClose: () => void
}) {
  const [word] = useState(() => view.state.sliceDoc(from, to))
  const [name, setName] = useState(word)
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState('')
  const box = useRef<HTMLDivElement>(null)
  const [at] = useState(() => view.coordsAtPos(from))
  const close = () => {
    onClose()
    view.focus()
  }
  const closeRef = useRef(close)
  useEffect(() => {
    closeRef.current = close
  })
  useEffect(() => {
    const away = (e: MouseEvent) => {
      if (box.current && !box.current.contains(e.target as Node)) closeRef.current()
    }
    document.addEventListener('mousedown', away, true)
    return () => document.removeEventListener('mousedown', away, true)
  }, [])

  const submit = () => {
    if (!name || name === word) return close()
    setBusy(true)
    setError('')
    renameAt(view, from, name).then(close, (e: Error) => {
      setBusy(false)
      setError(e.message)
    })
  }
  if (!at) return null
  return createPortal(
    <div
      ref={box}
      className="code-rename"
      role="dialog"
      aria-label={`Rename ${word}`}
      style={{ left: Math.max(8, at.left - 6), top: at.bottom + 4 }}
    >
      <input
        className="code-popup-input"
        aria-label="New name"
        autoFocus
        spellCheck={false}
        value={name}
        disabled={busy}
        onFocus={(e) => e.target.select()}
        onChange={(e) => setName(e.target.value)}
        onKeyDown={(e) => {
          if (e.key === 'Escape') {
            e.preventDefault()
            close()
          } else if (e.key === 'Enter') {
            e.preventDefault()
            submit()
          }
        }}
      />
      {error ? (
        <div className="code-rename-hint code-gotoline-bad">{error}</div>
      ) : (
        <div className="code-rename-hint">
          {busy ? 'Renaming…' : <>Enter renames {word} everywhere it is used · Escape cancels</>}
        </div>
      )}
    </div>,
    document.body,
  )
}
