import { countColumn, findColumn } from '@codemirror/state'
import { EditorView } from '@codemirror/view'
import { useState } from 'react'

import { Popup } from './Search'

// GotoLine is IntelliJ's Go to Line:Column: the caret's line and column to
// start from, a line, or a line and a column after a colon, to go to. A
// line past the end goes to the last, and a column past a line's end to
// its end.
export function GotoLine({ view, onClose }: { view: EditorView; onClose: () => void }) {
  const state = view.state
  const [start] = useState(() => {
    const head = state.selection.main.head
    const line = state.doc.lineAt(head)
    return `${line.number}:${countColumn(line.text.slice(0, head - line.from), state.tabSize) + 1}`
  })
  const [value, setValue] = useState(start)
  const lines = state.doc.lines
  const m = /^\s*(\d+)\s*(?:[:,]\s*(\d*))?\s*$/.exec(value)
  const close = () => {
    onClose()
    view.focus()
  }
  const go = () => {
    if (!m) return
    const line = state.doc.line(Math.min(Math.max(1, Number(m[1])), lines))
    const col = m[2] ? Math.max(1, Number(m[2])) : 1
    const pos = line.from + findColumn(line.text, col - 1, state.tabSize)
    view.dispatch({ selection: { anchor: pos }, effects: EditorView.scrollIntoView(pos, { y: 'center' }) })
    close()
  }
  return (
    <Popup className="code-gotoline" onClose={close}>
      <div className="code-popup-head">
        <span className="strong">Go to Line:Column</span>
      </div>
      <input
        className="code-popup-input"
        aria-label="Line and column"
        autoFocus
        spellCheck={false}
        value={value}
        onFocus={(e) => e.target.select()}
        onChange={(e) => setValue(e.target.value)}
        onKeyDown={(e) => {
          if (e.key !== 'Enter') return
          e.preventDefault()
          go()
        }}
      />
      <div className={m ? 'code-gotoline-hint' : 'code-gotoline-hint code-gotoline-bad'}>
        {m
          ? Number(m[1]) > lines
            ? `The file has ${lines} lines; this goes to the last.`
            : `${lines} lines`
          : 'A line, or a line and a column: 12 or 12:5'}
      </div>
      <div className="code-popup-actions">
        <span className="code-tool-spacer" />
        <button type="button" className="btn btn-sm" onClick={close}>
          Cancel
        </button>
        <button type="button" className="btn btn-sm btn-primary" disabled={!m} onClick={go}>
          OK
        </button>
      </div>
    </Popup>
  )
}
