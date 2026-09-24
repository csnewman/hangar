import { autocompletion, closeBrackets, closeBracketsKeymap } from '@codemirror/autocomplete'
import { defaultKeymap, indentWithTab } from '@codemirror/commands'
import {
  bracketMatching,
  foldGutter,
  foldKeymap,
  indentOnInput,
  LanguageDescription,
  syntaxHighlighting,
  defaultHighlightStyle,
} from '@codemirror/language'
import { languages } from '@codemirror/language-data'
import { highlightSelectionMatches, searchKeymap } from '@codemirror/search'
import { Compartment, EditorState, type Extension } from '@codemirror/state'
import {
  crosshairCursor,
  drawSelection,
  dropCursor,
  EditorView,
  highlightActiveLine,
  highlightActiveLineGutter,
  highlightSpecialChars,
  keymap,
  lineNumbers,
  rectangularSelection,
} from '@codemirror/view'
import { useEffect, useRef } from 'react'
import { yCollab, yUndoManagerKeymap } from 'y-codemirror.next'
import * as Y from 'yjs'

import type { SharedFile } from './client'

// editorSetup is CodeMirror's basic setup, less its own undo history: a
// shared document's undo is Yjs's, which undoes only this window's edits.
export const editorSetup: Extension = [
  lineNumbers(),
  highlightActiveLineGutter(),
  highlightSpecialChars(),
  foldGutter(),
  drawSelection(),
  dropCursor(),
  EditorState.allowMultipleSelections.of(true),
  indentOnInput(),
  syntaxHighlighting(defaultHighlightStyle, { fallback: true }),
  bracketMatching(),
  closeBrackets(),
  autocompletion({ activateOnTyping: false }),
  rectangularSelection(),
  crosshairCursor(),
  highlightActiveLine(),
  highlightSelectionMatches(),
  keymap.of([...closeBracketsKeymap, ...defaultKeymap, ...searchKeymap, ...foldKeymap, indentWithTab]),
]

// codeTheme follows the page's colours, so light and dark need no second
// theme.
export const codeTheme = EditorView.theme({
  '&': { height: '100%', fontSize: '13px', backgroundColor: 'var(--code-bg)', color: 'var(--text)' },
  '.cm-scroller': { fontFamily: 'var(--code-font)', lineHeight: '1.55' },
  '.cm-content': { caretColor: 'var(--accent)' },
  '.cm-gutters': { backgroundColor: 'var(--code-bg)', color: 'var(--muted)', border: 'none' },
  '.cm-activeLine': { backgroundColor: 'var(--code-active-line)' },
  '.cm-activeLineGutter': { backgroundColor: 'var(--code-active-line)', color: 'var(--text)' },
  '&.cm-focused .cm-selectionBackground, .cm-selectionBackground, ::selection': {
    backgroundColor: 'var(--code-selection) !important',
  },
  '.cm-cursor': { borderLeftColor: 'var(--accent)' },
  '.cm-foldGutter .cm-gutterElement': { color: 'var(--muted)' },
  '.cm-panels': { backgroundColor: 'var(--surface)', color: 'var(--text)' },
})

// languageFor loads the language a file's name says it is written in.
export async function languageFor(name: string): Promise<Extension> {
  const desc = LanguageDescription.matchFilename(languages, name)
  if (!desc) return []
  try {
    return await desc.load()
  } catch {
    return []
  }
}

// CodeEditor edits one shared file. Everyone with it open sees the others'
// carets and selections, named.
export function CodeEditor({ file, user }: { file: SharedFile; user: { name: string; color: string } }) {
  const host = useRef<HTMLDivElement>(null)

  useEffect(() => {
    const el = host.current
    if (!el) return
    file.awareness.setLocalStateField('user', { name: user.name, color: user.color, colorLight: user.color + '33' })
    const language = new Compartment()
    const undo = new Y.UndoManager(file.text)
    const view = new EditorView({
      parent: el,
      state: EditorState.create({
        doc: file.text.toString(),
        extensions: [
          editorSetup,
          keymap.of(yUndoManagerKeymap),
          codeTheme,
          language.of([]),
          yCollab(file.text, file.awareness, { undoManager: undo }),
        ],
      }),
    })
    let disposed = false
    languageFor(file.path.split('/').pop() ?? '').then((ext) => {
      if (!disposed) view.dispatch({ effects: language.reconfigure(ext) })
    })
    view.focus()
    return () => {
      disposed = true
      view.destroy()
      undo.destroy()
    }
  }, [file, user.name, user.color])

  return <div ref={host} className="code-editor" />
}
