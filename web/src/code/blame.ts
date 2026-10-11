// Git blame in an editor's gutter, as IntelliJ's Annotate: beside the first
// line of each run changed by one commit, its date and author; the commit
// itself on hovering, and in the Git Log on a click. The annotations are
// anchored to their lines, so they move with them as the file is edited,
// until they are loaded again. The gutter is as wide as it is dragged to
// be, from its right edge, and keeps that width.

import { RangeSet, RangeSetBuilder, StateEffect, StateField, type Extension, type Text } from '@codemirror/state'
import { EditorView, gutter, GutterMarker } from '@codemirror/view'

import type { Blame } from './client'

const uncommitted = '0'.repeat(40)

class BlameMarker extends GutterMarker {
  text: string
  tip: string
  hash: string

  constructor(text: string, tip: string, hash: string) {
    super()
    this.text = text
    this.tip = tip
    this.hash = hash
  }

  eq(o: BlameMarker) {
    return o.text === this.text && o.tip === this.tip && o.hash === this.hash
  }

  toDOM() {
    const el = document.createElement('span')
    el.textContent = this.text
    el.title = this.hash === uncommitted ? this.tip : `${this.tip}\nClick to show it in the Git Log.`
    el.className = this.hash === uncommitted ? 'cm-blame-text cm-blame-fresh' : 'cm-blame-text cm-blame-commit'
    return el
  }
}

// The same commit's later lines, so a run of them reads as one.
class ContinuedMarker extends GutterMarker {
  tip: string
  hash: string
  constructor(tip: string, hash: string) {
    super()
    this.tip = tip
    this.hash = hash
  }
  eq(o: ContinuedMarker) {
    return o.tip === this.tip && o.hash === this.hash
  }
  toDOM() {
    const el = document.createElement('span')
    el.className = 'cm-blame-continued'
    el.title = this.tip
    return el
  }
}

export const setBlame = StateEffect.define<RangeSet<GutterMarker> | null>()

// blameField holds an editor's annotations, null when not shown.
export const blameField = StateField.define<RangeSet<GutterMarker> | null>({
  create: () => null,
  update(v, tr) {
    for (const e of tr.effects) if (e.is(setBlame)) return e.value
    return v && tr.docChanged ? v.map(tr.changes) : v
  },
})

const widthKey = 'hangar.code.blameWidth'
const minWidth = 70
const maxWidth = 480

function savedWidth(): number {
  try {
    const n = Number(localStorage.getItem(widthKey))
    if (n >= minWidth && n <= maxWidth) return n
  } catch {
    // Remembering the width is a convenience.
  }
  return 170
}

// How near the gutter's right edge, in pixels, a press resizes it.
const edge = 6

// blameGutter is the gutter that shows them: a click on a commit's
// annotation calls onCommit with its hash, and a press at its right edge
// drags its width.
export function blameGutter(onCommit: (hash: string) => void): Extension {
  return [
    EditorView.editorAttributes.of({ style: `--blame-width: ${savedWidth()}px` }),
    gutter({
      class: 'cm-blame-gutter',
      markers: (v) => v.state.field(blameField) ?? RangeSet.empty,
      domEventHandlers: {
        mousedown(view, line, event) {
          const e = event as MouseEvent
          const g = (e.target as HTMLElement).closest('.cm-blame-gutter') as HTMLElement | null
          if (!g || e.button !== 0) return false
          const right = g.getBoundingClientRect().right
          if (right - e.clientX <= edge) {
            resize(view, g, e)
            return true
          }
          let hash: string | null = null
          view.state.field(blameField)?.between(line.from, line.from, (_from, _to, m) => {
            if (m instanceof BlameMarker || m instanceof ContinuedMarker) hash = m.hash
          })
          if (!hash || hash === uncommitted) return false
          onCommit(hash)
          return true
        },
      },
    }),
  ]
}

// resize follows the pointer from a press at the gutter's edge, setting
// its width as it goes and remembering it once let go.
function resize(view: EditorView, g: HTMLElement, e: MouseEvent) {
  e.preventDefault()
  const x0 = e.clientX
  const w0 = g.getBoundingClientRect().width
  let w = w0
  document.body.classList.add('cm-blame-resizing')
  const move = (ev: PointerEvent) => {
    w = Math.round(Math.max(minWidth, Math.min(maxWidth, w0 + ev.clientX - x0)))
    view.dom.style.setProperty('--blame-width', `${w}px`)
    view.requestMeasure()
  }
  const up = () => {
    document.removeEventListener('pointermove', move, true)
    document.removeEventListener('pointerup', up, true)
    document.body.classList.remove('cm-blame-resizing')
    try {
      localStorage.setItem(widthKey, String(w))
    } catch {
      // Remembering the width is a convenience.
    }
  }
  document.addEventListener('pointermove', move, true)
  document.addEventListener('pointerup', up, true)
}

const day = (t: number) => new Date(t * 1000).toISOString().slice(0, 10)

// blameMarkers places a blame's annotations in a document, line by line.
export function blameMarkers(doc: Text, blame: Blame): RangeSet<GutterMarker> {
  const b = new RangeSetBuilder<GutterMarker>()
  const n = Math.min(blame.lines.length, doc.lines)
  for (let i = 0; i < n; i++) {
    const hash = blame.lines[i]
    const c = blame.commits[hash]
    const fresh = hash === uncommitted
    const tip = fresh
      ? 'Not committed yet'
      : `${hash.slice(0, 8)} ${c?.author ?? ''}, ${c ? new Date(c.time * 1000).toLocaleString() : ''}\n${c?.summary ?? ''}`
    const from = doc.line(i + 1).from
    if (i > 0 && blame.lines[i - 1] === hash) b.add(from, from, new ContinuedMarker(tip, hash))
    else {
      b.add(
        from,
        from,
        new BlameMarker(fresh ? 'Not committed' : `${day(c?.time ?? 0)} ${c?.author ?? ''} · ${c?.summary ?? ''}`, tip, hash),
      )
    }
  }
  return b.finish()
}
