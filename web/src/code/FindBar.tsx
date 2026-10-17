import {
  closeSearchPanel,
  findNext,
  findPrevious,
  getSearchQuery,
  openSearchPanel,
  replaceAll,
  replaceNext,
  search,
  SearchQuery,
  selectMatches,
  selectSelectionMatches,
  setSearchQuery,
} from '@codemirror/search'
import { StateEffect, StateField, type EditorState, type Extension } from '@codemirror/state'
import { EditorView, keymap, runScopeHandlers, type Command, type Panel } from '@codemirror/view'
import { ArrowDown, ArrowUp, CaseSensitive, ChevronRight, Regex, TextSelect, WholeWord, X } from 'lucide-react'
import { useState, type ReactNode } from 'react'
import { flushSync } from 'react-dom'
import { createRoot } from 'react-dom/client'

import { isMac, shortcut } from './ContextMenu'

// Whether the find bar shows its Replace row.
const showReplace = StateEffect.define<boolean>()
const replaceShown = StateField.define<boolean>({
  create: () => false,
  update: (shown, tr) => {
    for (const e of tr.effects) if (e.is(showReplace)) shown = e.value
    return shown
  },
})

// openFind shows the find bar, without its Replace row, as Ctrl/Cmd+F does
// in IntelliJ; openReplace shows both rows (Ctrl/Cmd+R).
export const openFind: Command = (view) => {
  if (view.state.field(replaceShown)) view.dispatch({ effects: showReplace.of(false) })
  return openSearchPanel(view)
}

export const openReplace: Command = (view) => {
  if (!view.state.field(replaceShown)) view.dispatch({ effects: showReplace.of(true) })
  return openSearchPanel(view)
}

// The most matches counted; past it the bar says there are more.
const maxCount = 9999

type Spec = {
  search: string
  caseSensitive: boolean
  regexp: boolean
  wholeWord: boolean
  replace: string
}

const specOf = (q: SearchQuery): Spec => ({
  search: q.search,
  caseSensitive: q.caseSensitive,
  regexp: q.regexp,
  wholeWord: q.wholeWord,
  replace: q.replace,
})

// count is how many matches there are, and which of them is selected.
function count(state: EditorState, q: SearchQuery): { total: number; current: number; more: boolean } | null {
  if (!q.search || !q.valid) return null
  const sel = state.selection.main
  const cursor = q.getCursor(state)
  let total = 0
  let current = 0
  for (let m = cursor.next(); !m.done; m = cursor.next()) {
    total++
    if (m.value.from === sel.from && m.value.to === sel.to) current = total
    if (total >= maxCount) return { total, current, more: true }
  }
  return { total, current, more: false }
}

// seek selects the first match of q from the caret on, round to the start,
// as IntelliJ does while the text to find is typed.
function seek(view: EditorView, q: SearchQuery) {
  if (!q.search || !q.valid) {
    view.dispatch({ effects: setSearchQuery.of(q) })
    return
  }
  const from = view.state.selection.main.from
  let m = q.getCursor(view.state, from).next()
  if (m.done) m = q.getCursor(view.state).next()
  if (m.done) {
    view.dispatch({ effects: setSearchQuery.of(q) })
    return
  }
  view.dispatch({
    selection: { anchor: m.value.from, head: m.value.to },
    effects: [setSearchQuery.of(q), EditorView.scrollIntoView(m.value.from, { y: 'center' })],
    userEvent: 'select.search',
  })
}

// FindBar is the editor's Find and Replace as IntelliJ draws it, across the
// top of the editor: the text to find with its match case, words and regex
// toggles inside the field, which of how many matches is selected, the
// previous and next, selecting all of them, and below, when shown, the
// replacement and its buttons.
function FindBar({ view, state }: { view: EditorView; state: EditorState }) {
  const query = getSearchQuery(state)
  const replacing = state.field(replaceShown)
  // The fields keep what is typed, and take a query set from elsewhere: Find
  // opened again with other text selected.
  const [text, setText] = useState(query.search)
  const [replace, setReplace] = useState(query.replace)
  const [seen, setSeen] = useState(query)
  if (query !== seen) {
    setSeen(query)
    if (query.search !== seen.search) setText(query.search)
    if (query.replace !== seen.replace) setReplace(query.replace)
  }
  const n = count(state, query)
  const set = (patch: Partial<Spec>, move = false) => {
    const q = new SearchQuery({ ...specOf(query), ...patch })
    if (move) seek(view, q)
    else view.dispatch({ effects: setSearchQuery.of(q) })
  }
  const toggle = (key: 'caseSensitive' | 'wholeWord' | 'regexp', label: string, icon: ReactNode) => (
    <button
      type="button"
      className={query[key] ? 'code-findbar-opt code-findbar-opt-on' : 'code-findbar-opt'}
      tabIndex={-1}
      title={label}
      aria-label={label}
      aria-pressed={query[key]}
      onClick={() => set({ [key]: !query[key] }, true)}
    >
      {icon}
    </button>
  )
  const none = !!query.search && (!query.valid || n?.total === 0)

  return (
    <div
      className="code-findbar"
      onKeyDown={(e) => {
        if (runScopeHandlers(view, e.nativeEvent, 'search-panel')) e.preventDefault()
      }}
    >
      <div className="code-findbar-row">
        <button
          type="button"
          className={
            replacing
              ? 'code-findbar-btn code-findbar-expand code-findbar-expand-on'
              : 'code-findbar-btn code-findbar-expand'
          }
          tabIndex={-1}
          title={`Replace (${shortcut('Mod+R')})`}
          aria-label="Show Replace"
          aria-pressed={replacing}
          onClick={() => view.dispatch({ effects: showReplace.of(!replacing) })}
        >
          <ChevronRight size={14} />
        </button>
        <div className={none ? 'code-findbar-field code-findbar-field-none' : 'code-findbar-field'}>
          <input
            {...{ 'main-field': 'true' }}
            aria-label="Find"
            placeholder="Find"
            spellCheck={false}
            value={text}
            onChange={(e) => {
              setText(e.target.value)
              set({ search: e.target.value }, true)
            }}
            onKeyDown={(e) => {
              if (e.key !== 'Enter') return
              e.preventDefault()
              if (e.shiftKey) findPrevious(view)
              else findNext(view)
              // CodeMirror selects the text to find after moving; IntelliJ
              // leaves the caret where it was, to type on.
              const end = e.currentTarget.value.length
              e.currentTarget.setSelectionRange(end, end)
            }}
          />
          {toggle('caseSensitive', 'Match Case', <CaseSensitive size={15} />)}
          {toggle('wholeWord', 'Words', <WholeWord size={15} />)}
          {toggle('regexp', 'Regex', <Regex size={15} />)}
        </div>
        <span className={none ? 'code-findbar-count code-findbar-count-none' : 'code-findbar-count'}>
          {!query.search
            ? ''
            : !query.valid
              ? 'Bad regex'
              : n?.total === 0
                ? '0 results'
                : n && n.current > 0
                  ? `${n.current}/${n.total}${n.more ? '+' : ''}`
                  : `${n?.total}${n?.more ? '+' : ''} results`}
        </span>
        <button
          type="button"
          className="code-findbar-btn"
          tabIndex={-1}
          title={`Previous Occurrence (${shortcut('Shift+Enter')})`}
          aria-label="Previous Occurrence"
          disabled={!n?.total}
          onClick={() => findPrevious(view)}
        >
          <ArrowUp size={14} />
        </button>
        <button
          type="button"
          className="code-findbar-btn"
          tabIndex={-1}
          title="Next Occurrence (Enter)"
          aria-label="Next Occurrence"
          disabled={!n?.total}
          onClick={() => findNext(view)}
        >
          <ArrowDown size={14} />
        </button>
        <button
          type="button"
          className="code-findbar-btn"
          tabIndex={-1}
          title={`Select All Occurrences (${shortcut(isMac ? 'Ctrl+Mod+G' : 'Ctrl+Alt+Shift+J')})`}
          aria-label="Select All Occurrences"
          disabled={!n?.total}
          onClick={() => {
            selectMatches(view)
            view.focus()
          }}
        >
          <TextSelect size={14} />
        </button>
        <span className="code-tool-spacer" />
        <button
          type="button"
          className="code-findbar-btn"
          tabIndex={-1}
          title="Close (Escape)"
          aria-label="Close"
          onClick={() => closeSearchPanel(view)}
        >
          <X size={14} />
        </button>
      </div>
      {replacing && (
        <div className="code-findbar-row">
          <span className="code-findbar-indent" />
          <div className="code-findbar-field">
            <input
              aria-label="Replace"
              placeholder={query.regexp ? 'Replace ($1 for a group)' : 'Replace'}
              spellCheck={false}
              value={replace}
              readOnly={state.readOnly}
              onChange={(e) => {
                setReplace(e.target.value)
                set({ replace: e.target.value })
              }}
              onKeyDown={(e) => {
                if (e.key !== 'Enter') return
                e.preventDefault()
                replaceNext(view)
              }}
            />
          </div>
          <button
            type="button"
            className="btn btn-sm"
            disabled={!n?.total || state.readOnly}
            onClick={() => replaceNext(view)}
          >
            Replace
          </button>
          <button
            type="button"
            className="btn btn-sm"
            disabled={!n?.total || state.readOnly}
            onClick={() => replaceAll(view)}
          >
            Replace All
          </button>
        </div>
      )}
    </div>
  )
}

// findPanel is the find bar as a CodeMirror panel. It is drawn on opening,
// at once, so that the text to find can be selected, and again with each
// change to the editor, which it counts the matches in.
function findPanel(view: EditorView): Panel {
  const dom = document.createElement('div')
  const root = createRoot(dom)
  return {
    dom,
    top: true,
    mount() {
      flushSync(() => root.render(<FindBar view={view} state={view.state} />))
      const input = dom.querySelector<HTMLInputElement>('[main-field]')
      input?.focus()
      input?.select()
    },
    update(u) {
      if (
        u.docChanged ||
        u.selectionSet ||
        u.transactions.some((t) => t.effects.length > 0) ||
        u.startState.readOnly !== u.state.readOnly
      )
        root.render(<FindBar view={view} state={u.state} />)
    },
    destroy() {
      // React does not unmount a root while it is rendering one.
      setTimeout(() => root.unmount(), 0)
    },
  }
}

// findBar is the editor's Find and Replace and its keys, as IntelliJ's:
// Ctrl/Cmd+F finds, Ctrl/Cmd+R replaces, F3 (Cmd+G on a Mac) and with
// Shift go to the next and previous match, Escape closes the bar, and
// Ctrl+Alt+Shift+J (Ctrl+Cmd+G) selects every match of the selection.
export const findBar: Extension = [
  search({ top: true, createPanel: findPanel }),
  replaceShown,
  keymap.of([
    { key: 'Mod-f', run: openFind, scope: 'editor search-panel', preventDefault: true },
    { key: 'Mod-r', run: openReplace, scope: 'editor search-panel', preventDefault: true },
    { key: 'F3', run: findNext, shift: findPrevious, scope: 'editor search-panel', preventDefault: true },
    { mac: 'Mod-g', run: findNext, shift: findPrevious, scope: 'editor search-panel', preventDefault: true },
    { key: 'Escape', run: closeSearchPanel, scope: 'editor search-panel' },
    { key: 'Ctrl-Alt-Shift-j', mac: 'Ctrl-Mod-g', run: selectSelectionMatches, preventDefault: true },
  ]),
]
