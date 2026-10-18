import { syntaxTree } from '@codemirror/language'
import { Facet, RangeSetBuilder, StateEffect, type Extension } from '@codemirror/state'
import { Decoration, EditorView, ViewPlugin, type DecorationSet, type ViewUpdate } from '@codemirror/view'

import type { CodeClient } from './client'
import type { SpellReply, SpellRequest } from './spellWorker'

// Spelling is checked as IntelliJ checks it: in comments, in strings that
// read as prose, and through the whole of a Markdown or text file but its
// code, links and identifiers. A word is known if English (Hunspell's
// en_US) or software (cspell's lists for its languages and tools) has it,
// or the person has saved it. A word not known is underlined, and the
// editor's menu offers the dictionary's guesses for it and saving it to
// the person's own dictionary, which their settings hold and so follows
// them to each environment.

let worker: Worker | null = null
let nextID = 0
const waiting = new Map<number, (r: SpellReply) => void>()

const ask = (req: Omit<SpellRequest, 'id'>): Promise<SpellReply> => {
  if (!worker) {
    worker = new Worker(new URL('./spellWorker.ts', import.meta.url), { type: 'module' })
    worker.onmessage = (e: MessageEvent<SpellReply>) => {
      waiting.get(e.data.id)?.(e.data)
      waiting.delete(e.data.id)
    }
  }
  const id = ++nextID
  return new Promise((resolve) => {
    waiting.set(id, resolve)
    worker!.postMessage({ ...req, id })
  })
}

// verdicts holds each word asked about: true if it is spelt right, false if
// not, and absent while the worker has not said.
const verdicts = new Map<string, boolean>()
const asked = new Set<string>()

// personal is the person's own dictionary, lower-cased.
let personal = new Set<string>()
const listeners = new Set<() => void>()
const changed = () => listeners.forEach((fn) => fn())

// suggestions are the dictionary's guesses at what a word should be.
export async function suggestions(word: string): Promise<string[]> {
  return (await ask({ suggest: word })).suggestions ?? []
}

// followSettings keeps the person's own dictionary, and whether spelling
// is checked at all, from their settings, until the function it returns is
// called.
export function followSettings(client: CodeClient): () => void {
  const take = (s: Record<string, unknown>) => {
    const words = Array.isArray(s.dictionary) ? s.dictionary.filter((w): w is string => typeof w === 'string') : []
    const next = new Set(words.map((w) => w.toLowerCase()))
    const on = s.spellcheck !== false
    if (on === enabled && next.size === personal.size && [...next].every((w) => personal.has(w))) return
    personal = next
    enabled = on
    changed()
  }
  client.settings().then(take, () => {})
  return client.on('settings.changed', take)
}

let enabled = true

// spellingEnabled is whether spelling is checked, as the person's settings
// say.
export const spellingEnabled = () => enabled

// setSpelling turns spelling on or off in the person's settings.
export async function setSpelling(client: CodeClient, on: boolean) {
  const s = await client.settings()
  enabled = on
  changed()
  await client.saveSettings({ ...s, spellcheck: on })
}

// saveWord adds a word to the person's own dictionary.
export async function saveWord(client: CodeClient, word: string) {
  const s = await client.settings()
  const words = Array.isArray(s.dictionary) ? s.dictionary.filter((w): w is string => typeof w === 'string') : []
  if (words.some((w) => w.toLowerCase() === word.toLowerCase())) return
  personal.add(word.toLowerCase())
  changed()
  await client.saveSettings({ ...s, dictionary: [...words, word].sort((a, b) => a.localeCompare(b)) })
}

// prose is whether a file is written to be read, all of it checked rather
// than its comments: Markdown and text, by name.
export const prose = (path: string) =>
  /\.(md|markdown|mdown|mkd|mdx|txt|text|rst|adoc|asciidoc)$/i.test(path) || /(^|\/)COMMIT_EDITMSG$/.test(path)

// Markdown's nodes that are not prose: code, links' addresses, and HTML.
const notProse =
  /^(FencedCode|CodeBlock|InlineCode|CodeText|URL|Autolink|HTMLBlock|HTMLTag|LinkLabel|ProcessingInstructionBlock|CommentBlock|Comment)$/

// A chunk of text (between spaces) that is an address, a path, an email,
// an assignment or an escape is not words, and nor is code written in
// prose: an identifier in camelCase or snake_case, a call, or letters run
// into digits (eth0, utf8).
const notWords = /:\/\/|[@/\\=%{}<>$`_]|^[-+]|^\w+\.\w+|[a-z][A-Z]|\w\(|[A-Za-z]\d|\d[A-Za-z]/

// typo is a word not known, at its place.
type Typo = { from: number; to: number; word: string }

// words are the words of a run of text, one in capitals (HTTPServer)
// split into its own.
function* words(text: string, at: number): Generator<Typo> {
  for (const chunk of text.matchAll(/\S+/g)) {
    if (notWords.test(chunk[0])) continue
    for (const run of chunk[0].matchAll(/[A-Za-z][A-Za-z'’]*/g)) {
      const base = at + chunk.index + run.index
      for (const part of run[0].matchAll(/[A-Z]+(?![a-z])|[A-Z]?[a-z'’]+/g)) {
        let w = part[0].replace(/’/g, "'")
        let from = base + part.index
        // A quotation's marks and a possessive are not the word's.
        const lead = w.match(/^'+/)?.[0].length ?? 0
        w = w.slice(lead).replace(/'+$/, '').replace(/'s$/i, '')
        from += lead
        // Short words, and acronyms, are left be.
        if (w.length < 3 || w === w.toUpperCase()) continue
        yield { from, to: from + w.length, word: w }
      }
    }
  }
}

// checked are the runs of a view's visible text to check.
function checked(view: EditorView, path: string): { from: number; to: number }[] {
  const out: { from: number; to: number }[] = []
  const tree = syntaxTree(view.state)
  const isProse = prose(path)
  for (const { from, to } of view.visibleRanges) {
    if (isProse) {
      let pos = from
      tree.iterate({
        from,
        to,
        enter: (n) => {
          if (!notProse.test(n.name)) return
          if (n.from > pos) out.push({ from: pos, to: n.from })
          pos = Math.max(pos, n.to)
          return false
        },
      })
      if (pos < to) out.push({ from: pos, to })
      continue
    }
    tree.iterate({
      from,
      to,
      enter: (n) => {
        const name = n.name
        if (/comment/i.test(name)) {
          out.push({ from: Math.max(n.from, from), to: Math.min(n.to, to) })
          return false
        }
        // A string that reads as prose, with spaces in it, not an import's
        // path or a key.
        if (/string/i.test(name) && !/^(Special|Escape)/.test(name)) {
          const s = view.state.sliceDoc(n.from, n.to)
          if (/[A-Za-z]{2,}\s+[A-Za-z]{2,}/.test(s)) out.push({ from: Math.max(n.from, from), to: Math.min(n.to, to) })
          return false
        }
      },
    })
  }
  return out
}

const typoMark = Decoration.mark({ class: 'cm-typo' })
// recheck asks for the words to be checked again: the dictionary or the
// settings changed. redraw shows the marks found.
const recheck = StateEffect.define<null>()
const redraw = StateEffect.define<null>()

// fileName is the path of the file being checked.
const fileName = Facet.define<string, string>({ combine: (v) => v[0] ?? '' })

// typoAt is the word not known at a place in a view, if one is there.
export function typoAt(view: EditorView, pos: number): Typo | null {
  const plugin = view.plugin(spellPlugin)
  if (!plugin) return null
  let found: Typo | null = null
  plugin.marks.between(pos, pos, (from, to) => {
    found = { from, to, word: view.state.sliceDoc(from, to) }
    return false
  })
  return found
}

const known = (w: string) => verdicts.get(w) !== false || personal.has(w.toLowerCase())

const spellPlugin = ViewPlugin.fromClass(
  class {
    marks: DecorationSet = Decoration.none
    timer: ReturnType<typeof setTimeout> | undefined
    off: () => void
    view: EditorView

    constructor(view: EditorView) {
      this.view = view
      const again = () => view.dispatch({ effects: recheck.of(null) })
      listeners.add(again)
      this.off = () => listeners.delete(again)
      this.schedule()
    }

    update(u: ViewUpdate) {
      if (u.docChanged) this.marks = this.marks.map(u.changes)
      if (u.docChanged || u.viewportChanged || u.transactions.some((t) => t.effects.some((e) => e.is(recheck)))) {
        this.schedule()
      }
    }

    schedule() {
      clearTimeout(this.timer)
      this.timer = setTimeout(() => this.run(), 300)
    }

    run() {
      const view = this.view
      if (!enabled) {
        if (this.marks.size) {
          this.marks = Decoration.none
          view.dispatch({ effects: redraw.of(null) })
        }
        return
      }
      const found: Typo[] = []
      const fresh = new Set<string>()
      for (const r of checked(view, view.state.facet(fileName))) {
        for (const w of words(view.state.sliceDoc(r.from, r.to), r.from)) {
          found.push(w)
          if (!verdicts.has(w.word) && !asked.has(w.word)) fresh.add(w.word)
        }
      }
      if (fresh.size) {
        const list = [...fresh]
        list.forEach((w) => asked.add(w))
        ask({ check: list }).then((r) => {
          const bad = new Set(r.bad)
          for (const w of list) {
            verdicts.set(w, !bad.has(w))
            asked.delete(w)
          }
          if (bad.size) this.schedule()
        })
      }
      const b = new RangeSetBuilder<Decoration>()
      let last = -1
      for (const t of found.sort((a, c) => a.from - c.from)) {
        if (t.from < last || known(t.word)) continue
        b.add(t.from, t.to, typoMark)
        last = t.to
      }
      const next = b.finish()
      if (next.size === 0 && this.marks.size === 0) return
      this.marks = next
      view.dispatch({ effects: redraw.of(null) })
    }

    destroy() {
      clearTimeout(this.timer)
      this.off()
    }
  },
  { decorations: (p) => p.marks },
)

// spelling checks a file's spelling, as its name says it is to be read.
export function spelling(path: string): Extension {
  return [
    fileName.of(path),
    spellPlugin,
    EditorView.baseTheme({
      '.cm-typo': {
        textDecoration: 'underline wavy var(--code-typo, #3a9d3a)',
        textDecorationSkipInk: 'none',
        textUnderlineOffset: '3px',
      },
    }),
  ]
}
