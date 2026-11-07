// A file open for editing as a Yjs document: the Node Code service's, which
// an environment started by an older worker runs until it restarts, synced
// over the WebSocket's 'y' channel with y-protocols' sync and awareness
// messages. A dropped connection keeps the Y.Doc, and Yjs merges what was
// typed meanwhile when the document syncs again.

import type { Extension } from '@codemirror/state'
import { keymap, ViewPlugin } from '@codemirror/view'
import * as decoding from 'lib0/decoding'
import * as encoding from 'lib0/encoding'
import { yCollab, yUndoManagerKeymap } from 'y-codemirror.next'
import * as awarenessProtocol from 'y-protocols/awareness'
import * as syncProtocol from 'y-protocols/sync'
import * as Y from 'yjs'

import type { DocTransport, FileUser, SharedFile } from './shared'

const MESSAGE_SYNC = 0
const MESSAGE_AWARENESS = 1

type AwarenessChange = { added: number[]; updated: number[]; removed: number[] }

const [undoKey, , redoKey] = yUndoManagerKeymap

// YjsFile's text is the document's 'text'.
export class YjsFile implements SharedFile {
  readonly doc = new Y.Doc()
  readonly ytext = this.doc.getText('text')
  readonly awareness = new awarenessProtocol.Awareness(this.doc)
  readonly path: string
  writable = true
  refs = 0
  // n is the service's number for the document on the current connection.
  n = 0
  // synced is set once the document has caught up with the service's.
  private synced = false
  private syncedWaiters: (() => void)[] = []
  private transport: DocTransport

  constructor(path: string, transport: DocTransport) {
    this.path = path
    this.transport = transport
    this.doc.on('update', (update: Uint8Array, origin: unknown) => {
      if (origin === this.transport) return
      const enc = encoding.createEncoder()
      encoding.writeVarUint(enc, MESSAGE_SYNC)
      syncProtocol.writeUpdate(enc, update)
      this.send(encoding.toUint8Array(enc))
    })
    this.awareness.on('update', ({ added, updated, removed }: AwarenessChange, origin: unknown) => {
      if (origin === this.transport) return
      const enc = encoding.createEncoder()
      encoding.writeVarUint(enc, MESSAGE_AWARENESS)
      encoding.writeVarUint8Array(
        enc,
        awarenessProtocol.encodeAwarenessUpdate(this.awareness, [...added, ...updated, ...removed]),
      )
      this.send(encoding.toUint8Array(enc))
    })
  }

  whenSynced(): Promise<void> {
    if (this.synced) return Promise.resolve()
    return new Promise((r) => this.syncedWaiters.push(r))
  }

  text(): string {
    return this.ytext.toString()
  }

  onChange(fn: () => void): () => void {
    this.ytext.observe(fn)
    return () => this.ytext.unobserve(fn)
  }

  binding(user: FileUser): Extension {
    this.awareness.setLocalStateField('user', { name: user.name, color: user.color, colorLight: user.color + '33' })
    // Each editor undoes its own edits.
    const undo = new Y.UndoManager(this.ytext)
    return [
      keymap.of(yUndoManagerKeymap),
      yCollab(this.ytext, this.awareness, { undoManager: undo }),
      ViewPlugin.define(() => ({ destroy: () => undo.destroy() })),
    ]
  }

  undo = undoKey.run!
  redo = redoKey.run!

  openParams(): Record<string, unknown> {
    return {}
  }

  // start begins syncing on a newly opened document number.
  start(r: { doc: number; writable?: boolean }) {
    this.n = r.doc
    this.writable = r.writable !== false
    const enc = encoding.createEncoder()
    encoding.writeVarUint(enc, MESSAGE_SYNC)
    syncProtocol.writeSyncStep1(enc, this.doc)
    this.send(encoding.toUint8Array(enc))
    if (this.awareness.getLocalState() !== null) {
      const aw = encoding.createEncoder()
      encoding.writeVarUint(aw, MESSAGE_AWARENESS)
      encoding.writeVarUint8Array(aw, awarenessProtocol.encodeAwarenessUpdate(this.awareness, [this.doc.clientID]))
      this.send(encoding.toUint8Array(aw))
    }
  }

  detach() {
    this.n = 0
  }

  receive(msg: Uint8Array) {
    const dec = decoding.createDecoder(msg)
    const type = decoding.readVarUint(dec)
    if (type === MESSAGE_SYNC) {
      const enc = encoding.createEncoder()
      encoding.writeVarUint(enc, MESSAGE_SYNC)
      const kind = syncProtocol.readSyncMessage(dec, enc, this.doc, this.transport)
      if (encoding.length(enc) > 1) this.send(encoding.toUint8Array(enc))
      if (kind === syncProtocol.messageYjsSyncStep2 && !this.synced) {
        this.synced = true
        for (const w of this.syncedWaiters.splice(0)) w()
      }
    } else if (type === MESSAGE_AWARENESS) {
      awarenessProtocol.applyAwarenessUpdate(this.awareness, decoding.readVarUint8Array(dec), this.transport)
    }
  }

  destroy() {
    awarenessProtocol.removeAwarenessStates(this.awareness, [this.doc.clientID], 'local')
    this.awareness.destroy()
    this.doc.destroy()
  }

  private send(msg: Uint8Array) {
    if (this.n) this.transport.sendYjs(this.n, msg)
  }
}
