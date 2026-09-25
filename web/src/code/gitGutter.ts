// The change gutter: a mark beside every line that differs from what git
// would stage, as VS Code's and IntelliJ's gutters mark them, and the
// commands that roll one change back or stage it alone.
//
// Changes are against the index, so a staged change stops being marked and
// "stage this change" means the same as it does in git.

import { Chunk } from '@codemirror/merge'
import { RangeSetBuilder, StateEffect, StateField, Text, type EditorState, type Extension } from '@codemirror/state'
import { EditorView, gutter, GutterMarker } from '@codemirror/view'

// setBase gives the editor the index's copy of its file, as LF-ended text.
// Null: the file is not in the index, and nothing is marked.
export const setBase = StateEffect.define<Text | null>()

type Changes = { base: Text | null; chunks: readonly Chunk[] }

const changes = StateField.define<Changes>({
  create: () => ({ base: null, chunks: [] }),
  update(value, tr) {
    for (const e of tr.effects) {
      if (e.is(setBase)) return { base: e.value, chunks: e.value ? Chunk.build(e.value, tr.state.doc) : [] }
    }
    if (tr.docChanged && value.base) {
      return { base: value.base, chunks: Chunk.updateB(value.chunks, value.base, tr.state.doc, tr.changes) }
    }
    return value
  },
})

type Kind = 'added' | 'modified' | 'deleted'

class ChangeMarker extends GutterMarker {
  readonly kind: Kind
  constructor(kind: Kind) {
    super()
    this.kind = kind
  }
  eq(other: ChangeMarker) {
    return other.kind === this.kind
  }
  toDOM() {
    const el = document.createElement('div')
    el.className = `cm-git-${this.kind}`
    return el
  }
}

const marks: Record<Kind, ChangeMarker> = {
  added: new ChangeMarker('added'),
  modified: new ChangeMarker('modified'),
  deleted: new ChangeMarker('deleted'),
}

const end = (to: number, doc: Text) => Math.min(to, doc.length)

// lines calls fn with the start of each line a chunk covers in the editor.
// A deletion covers none; it is marked on the line after it.
function lines(c: Chunk, doc: Text, fn: (from: number, kind: Kind) => void) {
  if (c.fromB === c.toB) {
    fn(doc.lineAt(end(c.fromB, doc)).from, 'deleted')
    return
  }
  const kind = c.fromA === c.toA ? 'added' : 'modified'
  for (let pos = c.fromB; pos <= end(c.endB, doc);) {
    const line = doc.lineAt(pos)
    fn(line.from, kind)
    pos = line.to + 1
  }
}

const cache = new WeakMap<Changes, ReturnType<RangeSetBuilder<GutterMarker>['finish']>>()
function markers(state: EditorState) {
  const value = state.field(changes)
  let set = cache.get(value)
  if (!set) {
    const b = new RangeSetBuilder<GutterMarker>()
    let last = -1
    for (const c of value.chunks) {
      lines(c, state.doc, (from, kind) => {
        if (from > last) b.add(from, from, marks[kind])
        last = from
      })
    }
    set = b.finish()
    cache.set(value, set)
  }
  return set
}

// Hunk is one change: where it is, and what the index has in its place.
export type Hunk = { chunk: Chunk; before: string }

// hunkAt is the change covering the line that starts at lineFrom.
export function hunkAt(state: EditorState, lineFrom: number): Hunk | null {
  const { base, chunks } = state.field(changes)
  if (!base) return null
  for (const c of chunks) {
    let hit = false
    lines(c, state.doc, (from) => {
      if (from === lineFrom) hit = true
    })
    if (hit) return { chunk: c, before: base.sliceString(c.fromA, end(c.toA, base)) }
  }
  return null
}

// rollback puts the index's lines back in place of a change.
export function rollback(view: EditorView, h: Hunk) {
  view.dispatch({ changes: { from: h.chunk.fromB, to: end(h.chunk.toB, view.state.doc), insert: h.before } })
}

// staged is what the index holds once a change alone is staged, LF-ended.
export function staged(state: EditorState, h: Hunk): string | null {
  const { base } = state.field(changes)
  if (!base) return null
  const c = h.chunk
  return (
    base.sliceString(0, c.fromA) +
    state.doc.sliceString(c.fromB, end(c.toB, state.doc)) +
    base.sliceString(end(c.toA, base))
  )
}

// gitGutter marks changed lines; a click on a mark calls onPick with its
// change and the click.
export function gitGutter(onPick: (view: EditorView, h: Hunk, e: MouseEvent) => void): Extension {
  return [
    changes,
    gutter({
      class: 'cm-git-gutter',
      markers: (view) => markers(view.state),
      domEventHandlers: {
        mousedown(view, line, e) {
          const h = hunkAt(view.state, line.from)
          if (!h) return false
          onPick(view, h, e as MouseEvent)
          return true
        },
      },
    }),
    EditorView.baseTheme({
      '.cm-git-gutter': { width: '6px', paddingLeft: '2px' },
      '.cm-git-gutter .cm-gutterElement': { cursor: 'pointer' },
      '.cm-git-added, .cm-git-modified': { width: '3px', height: '100%' },
      '.cm-git-added': { backgroundColor: 'var(--code-git-added)' },
      '.cm-git-modified': { backgroundColor: 'var(--code-git-modified)' },
      '.cm-git-deleted': {
        width: 0,
        height: 0,
        borderTop: '4px solid transparent',
        borderBottom: '4px solid transparent',
        borderLeft: '5px solid var(--code-git-conflict)',
        marginTop: '-4px',
      },
    }),
  ]
}

// baseText is the index's copy of a file as the editor holds text: LF-ended.
export function baseText(raw: string): Text {
  return Text.of(raw.replace(/\r\n/g, '\n').split('\n'))
}
