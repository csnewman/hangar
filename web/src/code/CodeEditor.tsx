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
  HighlightStyle,
  syntaxHighlighting,
} from '@codemirror/language'
import { languages } from '@codemirror/language-data'
import { highlightSelectionMatches, selectNextOccurrence } from '@codemirror/search'
import { Compartment, countColumn, EditorSelection, EditorState, Prec, type Extension } from '@codemirror/state'
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
  ViewPlugin,
} from '@codemirror/view'
import { tags as t } from '@lezer/highlight'
import { formatDocument, LSPPlugin } from '@codemirror/lsp-client'
import { useContext, useEffect, useRef, useState } from 'react'
import { createPortal } from 'react-dom'
import { yCollab, yUndoManagerKeymap } from 'y-codemirror.next'
import * as Y from 'yjs'

import type { CodeClient, SharedFile } from './client'
import { copyText, isMac, shortcut, useContextMenu, type MenuItem } from './ContextMenu'
import { baseText, gitGutter, hunkAt, rollback, setBase, staged, type Hunk } from './gitGutter'
import { isDark, onDarkChange } from '../theme'
import { blameField, blameGutter, blameMarkers, setBlame } from './blame'
import { openGotoLine, registerPopups, startRename } from './editorPopups'
import { EditorStatusContext } from './editorStatus'
import { findBar, openFind, openReplace } from './FindBar'
import { GotoLine } from './GotoLine'
import { RenamePopup } from './RenamePopup'
import { languageId, Languages, LanguagesContext, type ServerState } from './lsp'
import type { Jump } from './Search'

// codeHighlight colours code by the page's --syn-* colours (code.css), which
// are IntelliJ's for light and dark, so one style serves both.
export const codeHighlight: Extension = syntaxHighlighting(
  HighlightStyle.define([
    {
      tag: [t.keyword, t.modifier, t.operatorKeyword, t.controlKeyword, t.definitionKeyword, t.moduleKeyword],
      color: 'var(--syn-keyword)',
    },
    { tag: [t.bool, t.null, t.atom, t.self], color: 'var(--syn-keyword)' },
    { tag: [t.string, t.special(t.string), t.character, t.regexp], color: 'var(--syn-string)' },
    { tag: t.escape, color: 'var(--syn-escape)' },
    { tag: [t.number, t.integer, t.float], color: 'var(--syn-number)' },
    { tag: [t.comment, t.lineComment, t.blockComment], color: 'var(--syn-comment)', fontStyle: 'italic' },
    { tag: t.docComment, color: 'var(--syn-doc)', fontStyle: 'italic' },
    {
      tag: [t.function(t.definition(t.variableName)), t.function(t.definition(t.propertyName))],
      color: 'var(--syn-function-def)',
    },
    { tag: [t.function(t.variableName), t.function(t.propertyName)], color: 'var(--syn-function)' },
    { tag: [t.typeName, t.className, t.namespace], color: 'var(--syn-type)' },
    { tag: [t.propertyName, t.attributeName], color: 'var(--syn-property)' },
    { tag: [t.constant(t.variableName), t.standard(t.variableName)], color: 'var(--syn-constant)' },
    { tag: [t.meta, t.annotation, t.processingInstruction], color: 'var(--syn-meta)' },
    { tag: [t.tagName], color: 'var(--syn-tag)' },
    { tag: [t.heading], color: 'var(--syn-keyword)', fontWeight: 'bold' },
    { tag: t.strong, fontWeight: 'bold' },
    { tag: t.emphasis, fontStyle: 'italic' },
    { tag: t.strikethrough, textDecoration: 'line-through' },
    { tag: [t.link, t.url], color: 'var(--syn-link)', textDecoration: 'underline' },
    { tag: t.invalid, color: 'var(--syn-invalid)' },
  ]),
)

// followScheme tells CodeMirror whether the page is dark, as the person's
// theme has it and as it changes, so its own parts -- tooltips, the
// completion list, panels -- take their dark looks with it.
const scheme = new Compartment()
export const followScheme: Extension = [
  scheme.of(EditorView.darkTheme.of(isDark())),
  ViewPlugin.define((view) => {
    const off = onDarkChange((dark) => view.dispatch({ effects: scheme.reconfigure(EditorView.darkTheme.of(dark)) }))
    return { destroy: off }
  }),
]

// editorSetup is CodeMirror's basic setup, less its own undo history: a
// shared document's undo is Yjs's, which undoes only this window's edits;
// and with IntelliJ's find bar in place of CodeMirror's search panel.
export const editorSetup: Extension = [
  lineNumbers(),
  highlightActiveLineGutter(),
  highlightSpecialChars(),
  foldGutter(),
  drawSelection(),
  dropCursor(),
  EditorState.allowMultipleSelections.of(true),
  indentOnInput(),
  codeHighlight,
  followScheme,
  bracketMatching(),
  closeBrackets(),
  autocompletion({ activateOnTyping: false }),
  rectangularSelection(),
  crosshairCursor(),
  highlightActiveLine(),
  highlightSelectionMatches(),
  findBar,
  keymap.of([...closeBracketsKeymap, ...defaultKeymap, ...foldKeymap, indentWithTab]),
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
  '.cm-panels': { backgroundColor: 'var(--surface)', color: 'var(--text)', fontFamily: 'var(--font)' },
  '.cm-panels.cm-panels-top': { borderBottom: '1px solid var(--border)' },
  '.cm-panels.cm-panels-bottom': { borderTop: '1px solid var(--border)' },
  // Matches of the find bar, and the one selected, as IntelliJ marks them.
  '.cm-searchMatch': { backgroundColor: 'var(--code-find-hit)', outline: 'none', borderRadius: '2px' },
  '.cm-searchMatch.cm-searchMatch-selected': { backgroundColor: 'var(--code-find-on)' },
  // A language server's message is a banner across the top of the editor,
  // as IntelliJ's editor notifications are.
  '.cm-panel.cm-dialog': {
    display: 'flex',
    alignItems: 'center',
    gap: '10px',
    padding: '6px 8px 6px 12px',
    fontSize: '12.5px',
    backgroundColor: 'var(--accent-soft)',
  },
  '.cm-panel.cm-lsp-message-warning': { backgroundColor: 'var(--warn-bg)' },
  '.cm-panel.cm-lsp-message-error': { backgroundColor: 'var(--danger-bg)' },
  '.cm-dialog form': { flex: '1', display: 'flex', alignItems: 'center', gap: '12px', minWidth: '0' },
  '.cm-dialog .cm-button': {
    flex: 'none',
    marginLeft: 'auto',
    padding: '2px 10px',
    border: '1px solid var(--border-strong)',
    borderRadius: '5px',
    background: 'var(--surface)',
    backgroundImage: 'none',
    color: 'var(--text)',
    fontFamily: 'inherit',
    fontSize: '12px',
    cursor: 'pointer',
  },
  '.cm-dialog .cm-button:hover': { borderColor: 'var(--accent)' },
  '.cm-dialog .cm-dialog-close': {
    position: 'static',
    width: '22px',
    height: '22px',
    border: 'none',
    borderRadius: '5px',
    background: 'none',
    color: 'var(--muted)',
    fontSize: '16px',
    lineHeight: '1',
    cursor: 'pointer',
  },
  '.cm-dialog .cm-dialog-close:hover': { backgroundColor: 'var(--sidebar-hover)', color: 'var(--text)' },
  '.cm-blame-gutter .cm-gutterElement': {
    boxSizing: 'border-box',
    width: 'var(--blame-width, 170px)',
    padding: '0 10px 0 6px',
    overflow: 'hidden',
    textOverflow: 'ellipsis',
    whiteSpace: 'nowrap',
    color: 'var(--muted)',
    fontFamily: 'var(--font)',
    fontSize: '11.5px',
    cursor: 'default',
  },
  '.cm-blame-gutter .cm-blame-fresh': { color: 'var(--accent)' },
  '.cm-blame-gutter .cm-blame-commit': { cursor: 'pointer' },
  '.cm-blame-gutter .cm-blame-commit:hover': { color: 'var(--accent)', textDecoration: 'underline' },
  // Its right edge drags its width.
  '.cm-blame-gutter': { position: 'relative', borderRight: '1px solid var(--border)' },
  '.cm-blame-gutter::after': {
    content: '""',
    position: 'absolute',
    top: '0',
    bottom: '0',
    right: '-1px',
    width: '6px',
    cursor: 'col-resize',
  },
  '.cm-blame-gutter:hover::after': { borderRight: '2px solid var(--accent)' },
  '.cm-tooltip': {
    backgroundColor: 'var(--surface) !important',
    color: 'var(--text) !important',
    border: '1px solid var(--border-strong) !important',
    borderRadius: '6px',
    boxShadow: 'var(--shadow-pop)',
  },
  '.cm-tooltip code, .cm-tooltip pre': {
    backgroundColor: 'var(--surface-2) !important',
    color: 'var(--text) !important',
    fontFamily: 'var(--code-font)',
  },
  '.cm-tooltip a': { color: 'var(--accent)' },
  '.cm-tooltip-autocomplete ul li[aria-selected]': {
    backgroundColor: 'var(--accent) !important',
    color: 'var(--accent-text) !important',
  },
  '.cm-completionDetail, .cm-completionInfo': { color: 'var(--muted)' },
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

// languageName is the name of the language a file's name says it is
// written in.
function languageName(name: string): string | null {
  return LanguageDescription.matchFilename(languages, name)?.name ?? null
}

// caret is where the main caret is, as a line and a column (a tab counted
// to its stop), and how much is selected.
function caret(state: EditorState) {
  const r = state.selection.main
  const line = state.doc.lineAt(r.head)
  return {
    line: line.number,
    col: countColumn(line.text.slice(0, r.head - line.from), state.tabSize) + 1,
    selected: state.selection.ranges.reduce((n, x) => n + x.to - x.from, 0),
    selectedLines: r.empty ? 0 : state.doc.lineAt(r.to).number - state.doc.lineAt(r.from).number + 1,
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
    { key: 'Mod-g', mac: 'Mod-Alt-g', run: openGotoLine, preventDefault: true },
  ]),
)

const [undoKey, , redoKey] = yUndoManagerKeymap
const selectedText = (view: EditorView) =>
  view.state.selection.ranges.map((r) => view.state.sliceDoc(r.from, r.to)).join('\n')

// The jumps whose annotations have been shown.
const blamed = new WeakSet<object>()

// CodeEditor edits one shared file. Everyone with it open sees the others'
// carets and selections, named. Beside the line numbers, a gutter marks
// what differs from git's index; a click on a mark shows what was there,
// with the change's rollback and staging.
export function CodeEditor({
  file,
  user,
  client,
  path,
  jump,
  onReveal,
  onShowCommit,
}: {
  file: SharedFile
  user: { name: string; color: string }
  client: CodeClient
  path: string
  // jump is a place to put the caret, each time it changes.
  jump?: (Jump & { n: number }) | null
  onReveal: (path: string) => void
  // onShowCommit shows a commit in the Git Log: one clicked in the blame.
  onShowCommit: (hash: string, path: string) => void
}) {
  const host = useRef<HTMLDivElement>(null)
  const viewRef = useRef<EditorView | null>(null)
  const [ready, setReady] = useState(false)
  const jumpRef = useRef(jump)
  jumpRef.current = jump
  // Git blame in the gutter, while asked for.
  const [annotating, setAnnotating] = useState(false)
  const onShowCommitRef = useRef(onShowCommit)
  onShowCommitRef.current = onShowCommit
  const blameSlot = useRef(new Compartment())
  const languages = useContext(LanguagesContext)
  const [server, setServer] = useState<ServerState | null>(null)
  // What is shown over the editor: Go to Line, or a symbol being renamed.
  const [popup, setPopup] = useState<
    { kind: 'goto'; view: EditorView } | { kind: 'rename'; view: EditorView; from: number; to: number } | null
  >(null)
  // The status bar is told where the caret is, the language, and the server.
  const status = useContext(EditorStatusContext)
  // A file the user may not write -- a library's, reached by going to a
  // definition -- is shown, not edited.
  const readOnly = !file.writable
  const outside = path.startsWith('/')
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
    // open runs what puts the keyboard somewhere of its own: the find bar,
    // or a popup.
    const open = (cmd: Command) => () => cmd(view)
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
      { label: 'Find…', shortcut: shortcut('Mod+F'), onSelect: open(openFind) },
      { label: 'Replace…', shortcut: shortcut('Mod+R'), disabled: readOnly, onSelect: open(openReplace) },
      { label: 'Go to Line…', shortcut: shortcut(isMac ? 'Mod+Alt+G' : 'Mod+G'), onSelect: open(openGotoLine) },
      'separator',
      { label: empty ? 'Duplicate Line' : 'Duplicate Selection', shortcut: shortcut('Mod+D'), onSelect: run(duplicate) },
      { label: 'Move Line Up', shortcut: shortcut('Alt+Shift+↑'), onSelect: run(moveLineUp) },
      { label: 'Move Line Down', shortcut: shortcut('Alt+Shift+↓'), onSelect: run(moveLineDown) },
      { label: 'Delete Line', shortcut: shortcut('Mod+Y'), onSelect: run(deleteLine) },
      'separator',
      {
        label: annotating ? 'Close Annotations' : 'Annotate with Git Blame',
        onSelect: () => setAnnotating((a) => !a),
      },
      'separator',
      { label: 'Copy Path', onSelect: () => copyText(`${client.root}/${path}`) },
      { label: 'Copy Reference', onSelect: () => copyText(`${path}:${line.number}`) },
      { label: 'Reveal in Project', onSelect: () => onReveal(path) },
    ]
    if (LSPPlugin.get(view) && languages) {
      const { goToDefinition, findUsages } = languages.commands
      items.splice(
        0,
        0,
        { label: 'Go to Definition', shortcut: shortcut('Mod+B'), onSelect: run(goToDefinition) },
        { label: 'Find Usages', shortcut: shortcut('Alt+F7'), onSelect: run(findUsages) },
        { label: 'Rename…', shortcut: shortcut('Shift+F6'), disabled: readOnly, onSelect: open(startRename) },
        {
          label: 'Reformat Code',
          shortcut: shortcut('Mod+Alt+L'),
          disabled: readOnly,
          onSelect: run(formatDocument),
        },
        'separator',
      )
    }
    if (h) {
      items.push(
        'separator',
        { label: 'Rollback Change', onSelect: () => rollback(view, h) },
        { label: 'Stage Change', onSelect: () => stage(view, h) },
      )
    }
    menu.open(e, items)
  }
  // gutterMenu is a right-click in the gutter: what concerns the line
  // there, as IntelliJ's gutter offers it.
  const gutterMenu = (view: EditorView, e: MouseEvent) => {
    const block = view.lineBlockAtHeight(e.clientY - view.documentTop)
    const line = view.state.doc.lineAt(block.from)
    const h = hunkAt(view.state, line.from)
    const items: MenuItem[] = [
      {
        label: annotating ? 'Close Annotations' : 'Annotate with Git Blame',
        onSelect: () => setAnnotating((a) => !a),
      },
      'separator',
      { label: `Copy Reference (${path.split('/').pop()}:${line.number})`, onSelect: () => copyText(`${path}:${line.number}`) },
      {
        label: 'Go to Line…',
        shortcut: shortcut(isMac ? 'Mod+Alt+G' : 'Mod+G'),
        onSelect: () => openGotoLine(view),
      },
    ]
    if (h) {
      items.push(
        'separator',
        { label: 'Show Change', onSelect: () => setHunk({ view, h, x: e.clientX, y: e.clientY }) },
        { label: 'Rollback Change', disabled: readOnly, onSelect: () => rollback(view, h) },
        { label: 'Stage Change', onSelect: () => stage(view, h) },
      )
    }
    menu.open(e, items)
  }
  // The editor, made once per file, reaches the latest menus through these.
  const gutterMenuRef = useRef(gutterMenu)
  useEffect(() => {
    gutterMenuRef.current = gutterMenu
  })
  const openMenuRef = useRef(openMenu)
  useEffect(() => {
    openMenuRef.current = openMenu
  })

  useEffect(() => {
    const el = host.current
    if (!el) return
    file.awareness.setLocalStateField('user', { name: user.name, color: user.color, colorLight: user.color + '33' })
    const language = new Compartment()
    const lsp = new Compartment()
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
          lsp.of([]),
          blameField,
          blameSlot.current.of([]),
          EditorState.readOnly.of(readOnly),
          yCollab(file.text, file.awareness, { undoManager: undo }),
          gitGutter((v, h, e) => setHunk({ view: v, h, x: e.clientX, y: e.clientY })),
          EditorView.updateListener.of((u) => {
            if (u.selectionSet || u.docChanged) status?.set(path, caret(u.state))
          }),
          EditorView.domEventHandlers({
            contextmenu: (e, v) => {
              openMenuRef.current(v, e)
              return true
            },
          }),
        ],
      }),
    })
    registerPopups(view, {
      gotoLine: () => setPopup({ kind: 'goto', view }),
      rename: (from, to) => setPopup({ kind: 'rename', view, from, to }),
    })
    // A right-click in the gutters -- the line numbers, the changes, the
    // annotations -- is the gutter's menu, not the browser's.
    view.dom.addEventListener('contextmenu', (e) => {
      if (!(e.target as HTMLElement).closest('.cm-gutters')) return
      gutterMenuRef.current(view, e)
    })
    status?.set(path, {
      ...caret(view.state),
      language: languageName(path.split('/').pop() ?? ''),
      readOnly,
      goto: () => openGotoLine(view),
    })
    let disposed = false
    languageFor(file.path.split('/').pop() ?? '').then((ext) => {
      if (!disposed) view.dispatch({ effects: language.reconfigure(ext) })
    })

    // The file's language server, if its language has one: attached once
    // it is running, and again whenever the servers start over (the
    // connection dropped, or a server stopped).
    const id = languageId(path)
    const abs = outside ? path : `${client.root}/${path}`
    const attach = () => {
      const p = id ? languages?.server(path) : null
      if (!p || !id) {
        setServer(null)
        return
      }
      setServer({ state: 'starting', language: id })
      p.then((s) => {
        if (disposed) return
        setServer(s)
        view.dispatch({ effects: lsp.reconfigure(Languages.extension(s, abs, id)) })
      })
    }
    attach()
    const offLanguages = languages?.onChange(() => {
      if (disposed) return
      try {
        view.dispatch({ effects: lsp.reconfigure([]) })
      } catch {
        // The old server's client is gone with its connection.
      }
      attach()
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

    // A file opened by stepping through a list leaves the keyboard there.
    const j = jumpRef.current
    if (!(j && j.path === path && j.focus === false)) view.focus()
    viewRef.current = view
    setReady(true)
    return () => {
      disposed = true
      offLanguages?.()
      setReady(false)
      viewRef.current = null
      clearTimeout(timer)
      offGit()
      view.destroy()
      undo.destroy()
      status?.set(path, null)
    }
  }, [file, user.name, user.color, client, path, languages, outside, readOnly, status])

  useEffect(() => {
    status?.set(path, { server })
  }, [status, path, server])

  // Annotations: loaded when asked for, again as git's state moves (a
  // commit), and gone when closed.
  useEffect(() => {
    const view = viewRef.current
    if (!ready || !view) return
    if (!annotating) {
      view.dispatch({ effects: [blameSlot.current.reconfigure([]), setBlame.of(null)] })
      return
    }
    let live = true
    const load = () =>
      client.blame(path).then(
        (b) => live && view.dispatch({ effects: setBlame.of(blameMarkers(view.state.doc, b)) }),
        () => live && setAnnotating(false),
      )
    view.dispatch({
      effects: blameSlot.current.reconfigure(Prec.highest(blameGutter((hash) => onShowCommitRef.current(hash, path)))),
    })
    load()
    let timer: ReturnType<typeof setTimeout> | undefined
    const off = client.on('git.changed', () => {
      clearTimeout(timer)
      timer = setTimeout(load, 300)
    })
    return () => {
      live = false
      clearTimeout(timer)
      off()
    }
  }, [ready, annotating, client, path])

  // A jump that asks for the annotations shows them, once: the editor
  // opened again later is as it was left.
  useEffect(() => {
    if (!jump?.blame || jump.path !== path || blamed.has(jump)) return
    blamed.add(jump)
    setAnnotating(true)
  }, [jump, path])

  // A jump puts the caret on its line, selecting the text it names there,
  // with the line in the middle of the view.
  useEffect(() => {
    const view = viewRef.current
    if (!ready || !view || !jump || jump.path !== path || jump.line < 1) return
    const doc = view.state.doc
    const line = doc.line(Math.min(jump.line, doc.lines))
    let from = line.from + Math.min(Math.max((jump.col ?? 1) - 1, 0), line.length)
    let to = from
    if (jump.find) {
      const at = line.text.indexOf(jump.find)
      const i = at >= 0 ? at : line.text.toLowerCase().indexOf(jump.find.toLowerCase())
      if (i >= 0) {
        from = line.from + i
        to = from + jump.find.length
      }
    }
    view.dispatch({ selection: { anchor: from, head: to }, effects: EditorView.scrollIntoView(from, { y: 'center' }) })
    if (jump.focus !== false) view.focus()
  }, [ready, jump, path])

  return (
    <>
      <div ref={host} className="code-editor" />
      {menu.menu}
      {popup?.kind === 'goto' && <GotoLine view={popup.view} onClose={() => setPopup(null)} />}
      {popup?.kind === 'rename' && (
        <RenamePopup view={popup.view} from={popup.from} to={popup.to} onClose={() => setPopup(null)} />
      )}
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

