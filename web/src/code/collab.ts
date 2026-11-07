// A file open for editing as a collab document: the Rust Code service's
// (crates/agent), which orders every change through one authority, as
// @codemirror/collab has it.
//
// The file is one collab client per browser tab, however many editors show
// it: a state of its own, with no view, holds the synced version and the
// changes not yet confirmed, and every editor of the file is kept equal to
// it. An editor's own changes go to the others as they are made, and to the
// service; what the service sends is applied to every editor as remote,
// outside its undo history, so each editor undoes its own edits only.
//
// A dropped connection is carried on from where it was: the document is
// opened again with its epoch, the client and the synced version, and the
// service answers with what it missed. When it cannot (the service started
// over, or forgot that far back), the text is taken afresh and what was
// typed meanwhile is rebased onto it.

import { collab, getSyncedVersion, receiveUpdates, sendableUpdates } from '@codemirror/collab'
import { history, historyKeymap, redo as redoEdit, undo as undoEdit } from '@codemirror/commands'
import {
  Annotation,
  ChangeSet,
  EditorState,
  StateEffect,
  StateField,
  Text,
  Transaction,
  type Extension,
} from '@codemirror/state'
import { Decoration, EditorView, keymap, ViewPlugin, WidgetType, type DecorationSet } from '@codemirror/view'

import type { DocTransport, FileUser, SharedFile } from './shared'

// What doc.open answers.
export type Opened = {
  doc: number
  writable?: boolean
  epoch: string
  version: number
  client: string
  text?: string
  updates?: Update[]
  presence?: { client: string; version: number; state: Presence | null }[]
}

type Update = { changes: unknown; client: string }

// Presence is where a tab's carets and selections are in the document, at
// a version, and whose they are.
type Presence = { name: string; color: string; ranges: { anchor: number; head: number }[] }

// A tab's presence in this tab's text.
type Remote = { name: string; color: string; ranges: { anchor: number; head: number }[] }

// How many received changes are kept, to bring a presence made at an
// earlier version up to date.
const RECENT = 200

// Changes this file makes to its editors, which they do not hand back.
const fromFile = Annotation.define<boolean>()

const setRemotes = StateEffect.define<Map<string, Remote>>()

export class CollabFile implements SharedFile {
  readonly path: string
  writable = true
  refs = 0
  n = 0
  private transport: DocTransport
  // The file's own state: the text, the synced version and the changes
  // still unconfirmed. Null until first opened.
  private state: EditorState | null = null
  // The text at the synced version.
  private base = Text.empty
  private epoch = ''
  private client = ''
  private views = new Set<EditorView>()
  private listeners = new Set<() => void>()
  private pushing = false
  private pushAgain = false
  private retry: ReturnType<typeof setTimeout> | undefined
  // The changes received, each with the version it applies to.
  private recent: { version: number; changes: ChangeSet }[] = []
  private remotes = new Map<string, Remote>()
  // Presences made at a version not yet received.
  private early = new Map<string, { version: number; state: Presence }>()
  private local: Presence | null = null
  private presenceTimer: ReturnType<typeof setTimeout> | undefined
  private syncedWaiters: (() => void)[] = []

  constructor(path: string, transport: DocTransport) {
    this.path = path
    this.transport = transport
  }

  whenSynced(): Promise<void> {
    if (this.state) return Promise.resolve()
    return new Promise((r) => this.syncedWaiters.push(r))
  }

  text(): string {
    return this.state?.doc.toString() ?? ''
  }

  onChange(fn: () => void): () => void {
    this.listeners.add(fn)
    return () => this.listeners.delete(fn)
  }

  // openParams is what doc.open is told, to carry on where the file was.
  openParams(): Record<string, unknown> {
    if (!this.state) return {}
    return { epoch: this.epoch, client: this.client, since: getSyncedVersion(this.state) }
  }

  // start takes the service's answer to doc.open, on the current connection.
  start(r: Opened) {
    this.n = r.doc
    this.writable = r.writable !== false
    if (this.state && r.updates && r.epoch === this.epoch && r.client === this.client) {
      this.receive(r.version, r.updates)
    } else if (this.state) {
      this.rebase(r)
    } else {
      this.base = Text.of((r.text ?? '').split('\n'))
      this.state = fresh(this.base, r.version, r.client)
    }
    this.epoch = r.epoch
    this.client = r.client
    this.remotes.clear()
    this.early.clear()
    for (const p of r.presence ?? []) this.presence(p.client, p.version, p.state)
    this.showRemotes()
    for (const w of this.syncedWaiters.splice(0)) w()
    this.push()
    if (this.local) this.sendPresence()
  }

  // detach is the connection dropping: nothing is sent until it is back.
  detach() {
    this.n = 0
    this.pushing = false
    this.pushAgain = false
    clearTimeout(this.retry)
    clearTimeout(this.presenceTimer)
    this.presenceTimer = undefined
  }

  destroy() {
    this.detach()
    this.listeners.clear()
  }

  // event takes the service's doc.changes and doc.presence.
  event(name: string, p: { version: number; updates?: Update[]; client?: string; state?: Presence | null }) {
    if (name === 'doc.changes' && p.updates) {
      this.receive(p.version, p.updates)
    } else if (name === 'doc.presence' && p.client) {
      this.presence(p.client, p.version, p.state ?? null)
      this.showRemotes()
    }
  }

  binding(user: FileUser): Extension {
    return [
      history(),
      keymap.of(historyKeymap),
      remotesField.init(() => decorate(this.remotes, this.state?.doc.length ?? 0)),
      ViewPlugin.define((view) => {
        this.views.add(view)
        return {
          update: (u) => {
            for (const tr of u.transactions) {
              if (tr.docChanged && !tr.annotation(fromFile)) this.edited(tr.changes, view)
            }
            if (u.view.hasFocus && (u.selectionSet || u.docChanged || u.focusChanged)) {
              this.setLocal(user, u.state)
            }
          },
          destroy: () => {
            this.views.delete(view)
          },
        }
      }),
    ]
  }

  undo = undoEdit
  redo = redoEdit

  // edited takes a change one of the editors made.
  private edited(changes: ChangeSet, from: EditorView) {
    if (!this.state) return
    this.state = this.state.update({ changes }).state
    this.mapRemotes(changes)
    for (const v of this.views) {
      if (v !== from) v.dispatch({ changes, annotations: [fromFile.of(true), Transaction.addToHistory.of(false)] })
    }
    this.changed()
    this.push()
  }

  // receive takes the authority's updates from `version` on, one at a
  // time, as the authority rebases what it is pushed.
  private receive(version: number, updates: Update[]) {
    if (!this.state) return
    const synced = getSyncedVersion(this.state)
    if (version > synced) return
    let moved = false
    updates.slice(synced - version).forEach((u, i) => {
      const changes = ChangeSet.fromJSON(u.changes)
      this.recent.push({ version: synced + i, changes })
      this.base = changes.apply(this.base)
      const tr = receiveUpdates(this.state!, [{ changes, clientID: u.client }])
      this.state = tr.state
      if (!tr.changes.empty) {
        moved = true
        this.mapRemotes(tr.changes)
        this.toViews(tr.changes)
      }
    })
    if (this.recent.length > RECENT) this.recent.splice(0, this.recent.length - RECENT)
    if (this.early.size) {
      for (const [client, p] of this.early) this.presence(client, p.version, p.state)
      this.showRemotes()
    }
    if (moved) this.changed()
  }

  // rebase takes the text afresh, keeping what was typed and not yet
  // confirmed: the difference between the text this tab last had confirmed
  // and the new one is applied as another's change.
  private rebase(r: Opened) {
    const old = this.state!
    const next = Text.of((r.text ?? '').split('\n'))
    let mine = ChangeSet.empty(this.base.length)
    for (const u of sendableUpdates(old)) mine = mine.compose(u.changes)
    this.recent = []
    if (old.doc.eq(next)) {
      // Everything typed was taken before the connection dropped.
      this.base = next
      this.state = fresh(next, r.version, r.client)
      return
    }
    const theirs = difference(this.base, next)
    const toLocal = theirs.map(mine, true)
    this.base = next
    this.state = fresh(next, r.version, r.client).update({ changes: mine.map(theirs) }).state
    this.toViews(toLocal)
    this.changed()
  }

  private toViews(changes: ChangeSet) {
    for (const v of this.views) {
      v.dispatch({
        changes,
        annotations: [fromFile.of(true), Transaction.addToHistory.of(false), Transaction.remote.of(true)],
      })
    }
  }

  private changed() {
    for (const fn of this.listeners) fn()
  }

  // push sends the unconfirmed changes, one request at a time: the
  // authority drops those it already has, so sending all of them again is
  // safe, and sending while a push is out would only repeat it.
  private push() {
    if (!this.state || !this.n || !this.writable) return
    if (this.pushing) {
      this.pushAgain = true
      return
    }
    const pending = sendableUpdates(this.state)
    if (pending.length === 0) return
    this.pushing = true
    const n = this.n
    this.transport
      .call('doc.push', {
        doc: n,
        version: getSyncedVersion(this.state),
        changes: pending.map((u) => u.changes.toJSON()),
      })
      .then(
        () => {
          if (this.n !== n) return
          this.pushing = false
          if (this.pushAgain) {
            this.pushAgain = false
            this.push()
          }
        },
        () => {
          if (this.n !== n) return
          this.pushing = false
          this.pushAgain = false
          clearTimeout(this.retry)
          this.retry = setTimeout(() => this.push(), 1000)
        },
      )
  }

  // setLocal is where the focused editor's selection is, sent shortly.
  private setLocal(user: FileUser, state: EditorState) {
    this.local = {
      name: user.name,
      color: user.color,
      ranges: state.selection.ranges.map((r) => ({ anchor: r.anchor, head: r.head })),
    }
    if (this.presenceTimer) return
    this.presenceTimer = setTimeout(() => {
      this.presenceTimer = undefined
      this.sendPresence()
    }, 80)
  }

  // sendPresence sends the local selection, at the synced version: its
  // positions are moved back over the unconfirmed changes, which the others
  // have not seen.
  private sendPresence() {
    if (!this.state || !this.n || !this.local) return
    const back = this.unconfirmed().invert(this.base)
    const at = (pos: number) => back.mapPos(pos, -1)
    this.transport
      .call('doc.presence', {
        doc: this.n,
        version: getSyncedVersion(this.state),
        state: { ...this.local, ranges: this.local.ranges.map((r) => ({ anchor: at(r.anchor), head: at(r.head) })) },
      })
      .catch(() => {})
  }

  // The unconfirmed changes as one, from the synced text to this tab's.
  private unconfirmed(): ChangeSet {
    let c = ChangeSet.empty(this.base.length)
    for (const u of sendableUpdates(this.state!)) c = c.compose(u.changes)
    return c
  }

  // presence places another tab's selection, made at `version`, in this
  // tab's text: forward over what it had not seen, then over what this tab
  // has not had confirmed.
  private presence(client: string, version: number, state: Presence | null) {
    this.early.delete(client)
    if (!state) {
      this.remotes.delete(client)
      return
    }
    if (!this.state) return
    const synced = getSyncedVersion(this.state)
    if (version > synced) {
      this.early.set(client, { version, state })
      return
    }
    const over = this.recent.filter((r) => r.version >= version).map((r) => r.changes)
    const length = this.state.doc.length
    if (over.length < synced - version) {
      // Made too long ago to place; shown where it said, until it moves.
      this.remotes.set(client, { ...state, ranges: state.ranges.map((r) => clamp(r, length)) })
      return
    }
    over.push(this.unconfirmed())
    const place = (pos: number) => over.reduce((p, c) => c.mapPos(p, 1), pos)
    this.remotes.set(client, {
      ...state,
      ranges: state.ranges.map((r) => clamp({ anchor: place(r.anchor), head: place(r.head) }, length)),
    })
  }

  private mapRemotes(changes: ChangeSet) {
    for (const r of this.remotes.values()) {
      r.ranges = r.ranges.map((x) => ({ anchor: changes.mapPos(x.anchor, 1), head: changes.mapPos(x.head, 1) }))
    }
  }

  private showRemotes() {
    const remotes = new Map(this.remotes)
    for (const v of this.views) v.dispatch({ effects: setRemotes.of(remotes) })
  }
}

function fresh(doc: Text, version: number, client: string): EditorState {
  return EditorState.create({ doc, extensions: collab({ startVersion: version, clientID: client }) })
}

function clamp(r: { anchor: number; head: number }, length: number) {
  return { anchor: Math.min(r.anchor, length), head: Math.min(r.head, length) }
}

// difference is a change from one text to another: what lies between their
// common start and end, replaced.
function difference(a: Text, b: Text): ChangeSet {
  const x = a.toString()
  const y = b.toString()
  let start = 0
  const most = Math.min(x.length, y.length)
  while (start < most && x.charCodeAt(start) === y.charCodeAt(start)) start++
  let end = 0
  while (end < most - start && x.charCodeAt(x.length - 1 - end) === y.charCodeAt(y.length - 1 - end)) end++
  return ChangeSet.of({ from: start, to: x.length - end, insert: y.slice(start, y.length - end) }, x.length)
}

// The other tabs' carets and selections, each in its tab's colour, its
// name shown over the caret.
const remotesField = StateField.define<DecorationSet>({
  create: () => Decoration.none,
  update(set, tr) {
    for (const e of tr.effects) if (e.is(setRemotes)) return decorate(e.value, tr.state.doc.length)
    return set.map(tr.changes)
  },
  provide: (f) => EditorView.decorations.from(f),
})

function decorate(remotes: Map<string, Remote>, length: number): DecorationSet {
  const marks = []
  for (const r of remotes.values()) {
    for (const range of r.ranges) {
      const from = Math.min(range.anchor, range.head, length)
      const to = Math.min(Math.max(range.anchor, range.head), length)
      if (from < to) {
        marks.push(
          Decoration.mark({ class: 'cm-remoteSelection', attributes: { style: `background-color: ${r.color}33` } }).range(
            from,
            to,
          ),
        )
      }
      marks.push(Decoration.widget({ widget: new Caret(r.name, r.color), side: 1 }).range(Math.min(range.head, length)))
    }
  }
  return Decoration.set(marks, true)
}

class Caret extends WidgetType {
  readonly name: string
  readonly color: string

  constructor(name: string, color: string) {
    super()
    this.name = name
    this.color = color
  }

  eq(other: Caret) {
    return other.name === this.name && other.color === this.color
  }

  toDOM() {
    const caret = document.createElement('span')
    caret.className = 'cm-remoteCaret'
    caret.style.borderColor = this.color
    const name = document.createElement('span')
    name.className = 'cm-remoteCaretName'
    name.style.backgroundColor = this.color
    name.textContent = this.name
    caret.append(name)
    return caret
  }

  ignoreEvent() {
    return true
  }
}
