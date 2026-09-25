import { autocompletion, closeBrackets, closeBracketsKeymap } from '@codemirror/autocomplete'
import {
  copyLineDown,
  defaultKeymap,
  deleteLine,
  indentWithTab,
  moveLineDown,
  moveLineUp,
  selectAll,
} from '@codemirror/commands'
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
import { gotoLine, highlightSelectionMatches, openSearchPanel, searchKeymap, selectNextOccurrence } from '@codemirror/search'
import { Compartment, EditorSelection, EditorState, Prec, type Extension } from '@codemirror/state'
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
  type Command,
  rectangularSelection,
} from '@codemirror/view'
import { useEffect, useRef, useState } from 'react'
import { createPortal } from 'react-dom'
import { yCollab, yUndoManagerKeymap } from 'y-codemirror.next'
import * as Y from 'yjs'

import type { CodeClient, SharedFile } from './client'
import { copyText, isMac, shortcut, useContextMenu, type MenuItem } from './ContextMenu'
import { baseText, gitGutter, hunkAt, rollback, setBase, staged, type Hunk } from './gitGutter'

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

// ideaKeys are IntelliJ's line keys, over CodeMirror's own where they
// differ: Ctrl+D duplicates, Ctrl+Y deletes a line (redo is Ctrl+Shift+Z),
// Alt+Shift+Up/Down moves one, Alt+J (Ctrl+G on a Mac) selects the next
// occurrence, Ctrl+G (⌘⌥G on a Mac) goes to a line.
const duplicate: Command = (view) => {
  const { state } = view
  if (state.selection.ranges.every((r) => r.empty)) return copyLineDown(view)
  view.dispatch(
    state.changeByRange((r) =>
      r.empty
        ? { range: r }
        : { changes: { from: r.to, insert: state.sliceDoc(r.from, r.to) }, range: EditorSelection.range(r.to, r.to + r.to - r.from) },
    ),
  )
  return true
}

const ideaKeys = Prec.high(
  keymap.of([
    { key: 'Mod-d', run: duplicate, preventDefault: true },
    { key: 'Mod-y', run: deleteLine, preventDefault: true },
    { key: 'Shift-Alt-ArrowUp', run: moveLineUp, preventDefault: true },
    { key: 'Shift-Alt-ArrowDown', run: moveLineDown, preventDefault: true },
    { key: 'Alt-j', mac: 'Ctrl-g', run: selectNextOccurrence, preventDefault: true },
    { key: 'Mod-g', mac: 'Mod-Alt-g', run: gotoLine, preventDefault: true },
  ]),
)

const [undoKey, , redoKey] = yUndoManagerKeymap
const selectedText = (view: EditorView) =>
  view.state.selection.ranges.map((r) => view.state.sliceDoc(r.from, r.to)).join('\n')

// CodeEditor edits one shared file. Everyone with it open sees the others'
// carets and selections, named. Beside the line numbers, a gutter marks
// what differs from git's index; a click on a mark shows what was there,
// with the change's rollback and staging.
export function CodeEditor({
  file,
  user,
  client,
  path,
  onReveal,
}: {
  file: SharedFile
  user: { name: string; color: string }
  client: CodeClient
  path: string
  onReveal: (path: string) => void
}) {
  const host = useRef<HTMLDivElement>(null)
  const menu = useContextMenu()
  const [hunk, setHunk] = useState<{ view: EditorView; h: Hunk; x: number; y: number } | null>(null)
  // crlf is whether the index's copy has CRLF endings, which a staged
  // change keeps.
  const crlf = useRef(false)

  const stage = (view: EditorView, h: Hunk) => {
    const content = staged(view.state, h)
    if (content !== null) client.setIndex(path, crlf.current ? content.replace(/\n/g, '\r\n') : content).catch(() => {})
  }

  const openMenu = (view: EditorView, e: MouseEvent) => {
    // A right-click outside the selection moves the caret there first.
    const pos = view.posAtCoords({ x: e.clientX, y: e.clientY })
    if (pos != null && !view.state.selection.ranges.some((r) => pos >= r.from && pos <= r.to)) {
      view.dispatch({ selection: { anchor: pos } })
    }
    const empty = view.state.selection.ranges.every((r) => r.empty)
    const line = view.state.doc.lineAt(view.state.selection.main.head)
    const h = hunkAt(view.state, line.from)
    const run = (cmd: Command) => () => {
      cmd(view)
      view.focus()
    }
    const items: MenuItem[] = [
      {
        label: 'Cut',
        shortcut: shortcut('Mod+X'),
        disabled: empty,
        onSelect: () => {
          navigator.clipboard?.writeText(selectedText(view)).then(() => {
            view.dispatch(view.state.replaceSelection(''))
            view.focus()
          }, () => {})
        },
      },
      { label: 'Copy', shortcut: shortcut('Mod+C'), disabled: empty, onSelect: () => copyText(selectedText(view)) },
      {
        label: 'Paste',
        shortcut: shortcut('Mod+V'),
        onSelect: () => {
          navigator.clipboard?.readText().then((text) => {
            view.dispatch(view.state.replaceSelection(text))
            view.focus()
          }, () => {})
        },
      },
      'separator',
      { label: 'Undo', shortcut: shortcut('Mod+Z'), onSelect: run(undoKey.run!) },
      { label: 'Redo', shortcut: shortcut('Mod+Shift+Z'), onSelect: run(redoKey.run!) },
      'separator',
      { label: 'Select All', shortcut: shortcut('Mod+A'), onSelect: run(selectAll) },
      { label: 'Find', shortcut: shortcut('Mod+F'), onSelect: run(openSearchPanel) },
      {
        label: 'Replace',
        onSelect: () => {
          openSearchPanel(view)
          view.dom.querySelector<HTMLInputElement>('.cm-search input[name=replace]')?.focus()
        },
      },
      { label: 'Go to Line…', shortcut: shortcut(isMac ? 'Mod+Alt+G' : 'Mod+G'), onSelect: run(gotoLine) },
      'separator',
      { label: empty ? 'Duplicate Line' : 'Duplicate Selection', shortcut: shortcut('Mod+D'), onSelect: run(duplicate) },
      { label: 'Move Line Up', shortcut: shortcut('Alt+Shift+↑'), onSelect: run(moveLineUp) },
      { label: 'Move Line Down', shortcut: shortcut('Alt+Shift+↓'), onSelect: run(moveLineDown) },
      { label: 'Delete Line', shortcut: shortcut('Mod+Y'), onSelect: run(deleteLine) },
      'separator',
      { label: 'Copy Path', onSelect: () => copyText(`${client.root}/${path}`) },
      { label: 'Copy Reference', onSelect: () => copyText(`${path}:${line.number}`) },
      { label: 'Reveal in Project', onSelect: () => onReveal(path) },
    ]
    if (h) {
      items.push(
        'separator',
        { label: 'Rollback Change', onSelect: () => rollback(view, h) },
        { label: 'Stage Change', onSelect: () => stage(view, h) },
      )
    }
    menu.open(e, items)
  }
  // The editor, made once per file, reaches the latest openMenu through
  // this.
  const openMenuRef = useRef(openMenu)
  useEffect(() => {
    openMenuRef.current = openMenu
  })

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
          ideaKeys,
          editorSetup,
          keymap.of(yUndoManagerKeymap),
          codeTheme,
          language.of([]),
          yCollab(file.text, file.awareness, { undoManager: undo }),
          gitGutter((v, h, e) => setHunk({ view: v, h, x: e.clientX, y: e.clientY })),
          EditorView.domEventHandlers({
            contextmenu: (e, v) => {
              openMenuRef.current(v, e)
              return true
            },
          }),
        ],
      }),
    })
    let disposed = false
    languageFor(file.path.split('/').pop() ?? '').then((ext) => {
      if (!disposed) view.dispatch({ effects: language.reconfigure(ext) })
    })

    // The gutter's base: the index's copy, reloaded whenever git's state
    // moves.
    const loadBase = () =>
      client.show(path, 'index').then(
        (raw) => {
          if (disposed) return
          crlf.current = raw?.includes('\r\n') ?? false
          view.dispatch({ effects: setBase.of(raw === null ? null : baseText(raw)) })
        },
        () => {},
      )
    loadBase()
    let timer: ReturnType<typeof setTimeout> | undefined
    const offGit = client.on('git.changed', () => {
      clearTimeout(timer)
      timer = setTimeout(loadBase, 150)
    })

    view.focus()
    return () => {
      disposed = true
      clearTimeout(timer)
      offGit()
      view.destroy()
      undo.destroy()
    }
  }, [file, user.name, user.color, client, path])

  return (
    <>
      <div ref={host} className="code-editor" />
      {menu.menu}
      {hunk && (
        <HunkPopover
          {...hunk}
          onClose={() => setHunk(null)}
          onRollback={() => rollback(hunk.view, hunk.h)}
          onStage={() => stage(hunk.view, hunk.h)}
        />
      )}
    </>
  )
}

// HunkPopover is what a click on a change's gutter mark shows: the lines
// the index has there, and what can be done with the change.
function HunkPopover({
  h,
  x,
  y,
  onClose,
  onRollback,
  onStage,
}: {
  h: Hunk
  x: number
  y: number
  onClose: () => void
  onRollback: () => void
  onStage: () => void
}) {
  const ref = useRef<HTMLDivElement>(null)
  useEffect(() => {
    const away = (e: Event) => {
      if (ref.current && !ref.current.contains(e.target as Node)) onClose()
    }
    const key = (e: KeyboardEvent) => e.key === 'Escape' && onClose()
    document.addEventListener('mousedown', away, true)
    document.addEventListener('keydown', key, true)
    return () => {
      document.removeEventListener('mousedown', away, true)
      document.removeEventListener('keydown', key, true)
    }
  }, [onClose])
  const added = h.chunk.fromA === h.chunk.toA
  return createPortal(
    <div ref={ref} className="code-hunk" style={{ left: x + 8, top: y + 8 }}>
      {added ? (
        <div className="code-hunk-note">Added lines</div>
      ) : (
        <pre className="code-hunk-before">{h.before.replace(/\n$/, '')}</pre>
      )}
      <div className="code-hunk-actions">
        <button
          type="button"
          onClick={() => {
            onClose()
            onRollback()
          }}
        >
          Rollback
        </button>
        <button
          type="button"
          onClick={() => {
            onClose()
            onStage()
          }}
        >
          Stage
        </button>
      </div>
    </div>,
    document.body,
  )
}
