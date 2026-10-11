// Git blame in an editor's gutter, as IntelliJ's Annotate: beside the first
// line of each run changed by one commit, its date and author; the commit
// itself on hovering. The annotations are anchored to their lines, so they
// move with them as the file is edited, until they are loaded again.

import { RangeSet, RangeSetBuilder, StateEffect, StateField, type Extension, type Text } from '@codemirror/state'
import { gutter, GutterMarker } from '@codemirror/view'

import type { Blame } from './client'

const uncommitted = '0'.repeat(40)

class BlameMarker extends GutterMarker {
  text: string
  tip: string
  fresh: boolean

  constructor(text: string, tip: string, fresh: boolean) {
    super()
    this.text = text
    this.tip = tip
    this.fresh = fresh
  }

  eq(o: BlameMarker) {
    return o.text === this.text && o.tip === this.tip
  }

  toDOM() {
    const el = document.createElement('span')
    el.textContent = this.text
    el.title = this.tip
    if (this.fresh) el.className = 'cm-blame-fresh'
    return el
  }
}

// The same commit's later lines, so a run of them reads as one.
class ContinuedMarker extends GutterMarker {
  tip: string
  constructor(tip: string) {
    super()
    this.tip = tip
  }
  eq(o: ContinuedMarker) {
    return o.tip === this.tip
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

// blameGutter is the gutter that shows them.
export const blameGutter: Extension = gutter({
  class: 'cm-blame-gutter',
  markers: (v) => v.state.field(blameField) ?? RangeSet.empty,
})

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
    if (i > 0 && blame.lines[i - 1] === hash) b.add(from, from, new ContinuedMarker(tip))
    else {
      const who = (c?.author ?? '').split(' ')[0].slice(0, 12)
      b.add(from, from, new BlameMarker(fresh ? 'Not committed' : `${day(c?.time ?? 0)} ${who}`, tip, fresh))
    }
  }
  return b.finish()
}
