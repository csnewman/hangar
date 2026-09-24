// The browser end of the Code tab's service in the environment (editor/code):
// requests and events on one WebSocket, and shared documents over Yjs.
//
// The WebSocket carries the service's frames, one per message: a channel
// byte, 'r' for JSON or 'y' for a document's Yjs message (its number as a
// big-endian u32, then the y-protocols message).
//
// A dropped connection is reconnected. Open documents keep their Y.Doc
// across it and are opened again, so nothing typed meanwhile is lost: Yjs
// merges it when the documents sync.

import * as decoding from 'lib0/decoding'
import * as encoding from 'lib0/encoding'
import * as awarenessProtocol from 'y-protocols/awareness'
import * as syncProtocol from 'y-protocols/sync'
import * as Y from 'yjs'

const CHANNEL_JSON = 0x72 // 'r'
const CHANNEL_YJS = 0x79 // 'y'
const MESSAGE_SYNC = 0
const MESSAGE_AWARENESS = 1

export interface Entry {
  name: string
  dir: boolean
  size: number
}

export interface Change {
  path: string
  index: string
  worktree: string
  from?: string
}

export interface GitStatus {
  repo: boolean
  branch?: string
  ahead?: number
  behind?: number
  changes: Change[]
}

export interface Diff {
  path: string
  head: string
  working: string
}

export type ConnectionState = 'connecting' | 'live' | 'reconnecting'

type Events = {
  state: (s: ConnectionState, why: string) => void
  'fs.changed': (p: { dirs: string[] }) => void
  'git.changed': () => void
  // An open file was deleted from disk. Its path is absolute.
  'doc.deleted': (p: { path: string }) => void
}

// SharedFile is one file open for editing, shared with everyone who has it
// open. Its text is the document's 'text'.
export class SharedFile {
  readonly doc = new Y.Doc()
  readonly text = this.doc.getText('text')
  readonly awareness = new awarenessProtocol.Awareness(this.doc)
  // n is the service's number for the document on the current connection.
  n = 0
  // synced is set once the document has caught up with the service's.
  synced = false
  private syncedWaiters: (() => void)[] = []
  refs = 0

  constructor(
    readonly path: string,
    private client: CodeClient,
  ) {
    this.doc.on('update', (update: Uint8Array, origin: unknown) => {
      if (origin === this.client) return
      const enc = encoding.createEncoder()
      encoding.writeVarUint(enc, MESSAGE_SYNC)
      syncProtocol.writeUpdate(enc, update)
      this.send(encoding.toUint8Array(enc))
    })
    this.awareness.on('update', ({ added, updated, removed }: AwarenessChange, origin: unknown) => {
      if (origin === this.client) return
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

  // start begins syncing on a newly opened document number.
  start(n: number) {
    this.n = n
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

  receive(msg: Uint8Array) {
    const dec = decoding.createDecoder(msg)
    const type = decoding.readVarUint(dec)
    if (type === MESSAGE_SYNC) {
      const enc = encoding.createEncoder()
      encoding.writeVarUint(enc, MESSAGE_SYNC)
      const kind = syncProtocol.readSyncMessage(dec, enc, this.doc, this.client)
      if (encoding.length(enc) > 1) this.send(encoding.toUint8Array(enc))
      if (kind === syncProtocol.messageYjsSyncStep2 && !this.synced) {
        this.synced = true
        for (const w of this.syncedWaiters.splice(0)) w()
      }
    } else if (type === MESSAGE_AWARENESS) {
      awarenessProtocol.applyAwarenessUpdate(this.awareness, decoding.readVarUint8Array(dec), this.client)
    }
  }

  private send(msg: Uint8Array) {
    if (this.n) this.client.sendYjs(this.n, msg)
  }
}

type AwarenessChange = { added: number[]; updated: number[]; removed: number[] }

export class CodeClient {
  private ws: WebSocket | null = null
  private nextID = 1
  private waiting = new Map<number, { resolve: (v: unknown) => void; reject: (e: Error) => void }>()
  private listeners: { [K in keyof Events]: Set<Events[K]> } = {
    state: new Set(),
    'fs.changed': new Set(),
    'git.changed': new Set(),
    'doc.deleted': new Set(),
  }
  private files = new Map<string, SharedFile>()
  private byNumber = new Map<number, SharedFile>()
  private retry: ReturnType<typeof setTimeout> | undefined
  private backoff = 500
  private closed = false
  private ready: Promise<void>
  private markReady!: () => void
  state: ConnectionState = 'connecting'
  root = ''

  constructor(private env: string) {
    this.ready = new Promise((r) => (this.markReady = r))
    this.connect()
  }

  on<K extends keyof Events>(event: K, fn: Events[K]): () => void {
    this.listeners[event].add(fn)
    return () => this.listeners[event].delete(fn)
  }

  private emit<K extends keyof Events>(event: K, ...args: Parameters<Events[K]>) {
    for (const fn of this.listeners[event]) (fn as (...a: Parameters<Events[K]>) => void)(...args)
  }

  private setState(s: ConnectionState, why = '') {
    this.state = s
    this.emit('state', s, why)
  }

  private connect() {
    const scheme = location.protocol === 'https:' ? 'wss' : 'ws'
    const ws = new WebSocket(`${scheme}://${location.host}/api/frontend/environments/${this.env}/code`)
    ws.binaryType = 'arraybuffer'
    this.ws = ws
    ws.onopen = async () => {
      try {
        const hello = (await this.call('hello')) as { root: string }
        this.root = hello.root
        this.backoff = 500
        for (const f of this.files.values()) await this.reopen(f)
        this.setState('live')
        this.markReady()
      } catch (e) {
        ws.close(4000, e instanceof Error ? e.message : String(e))
      }
    }
    ws.onmessage = (e) => this.receive(new Uint8Array(e.data as ArrayBuffer))
    ws.onclose = (e) => {
      if (this.ws !== ws) return
      this.ws = null
      for (const w of this.waiting.values()) w.reject(new Error('the connection dropped'))
      this.waiting.clear()
      this.byNumber.clear()
      if (this.closed) return
      this.setState('reconnecting', e.reason)
      this.retry = setTimeout(() => this.connect(), this.backoff)
      this.backoff = Math.min(this.backoff * 2, 8000)
    }
  }

  private receive(msg: Uint8Array) {
    if (msg.length === 0) return
    if (msg[0] === CHANNEL_JSON) {
      const m = JSON.parse(new TextDecoder().decode(msg.subarray(1)))
      if (m.id) {
        const w = this.waiting.get(m.id)
        this.waiting.delete(m.id)
        if (m.error) w?.reject(new Error(m.error))
        else w?.resolve(m.result)
      } else if (m.event === 'fs.changed') {
        this.emit('fs.changed', m.params)
      } else if (m.event === 'git.changed') {
        this.emit('git.changed')
      } else if (m.event === 'doc.deleted') {
        this.emit('doc.deleted', m.params)
      }
    } else if (msg[0] === CHANNEL_YJS && msg.length >= 5) {
      const n = new DataView(msg.buffer, msg.byteOffset + 1, 4).getUint32(0)
      this.byNumber.get(n)?.receive(msg.subarray(5))
    }
  }

  private write(frame: Uint8Array) {
    if (this.ws?.readyState === WebSocket.OPEN) this.ws.send(frame)
  }

  sendYjs(n: number, msg: Uint8Array) {
    const frame = new Uint8Array(5 + msg.length)
    frame[0] = CHANNEL_YJS
    new DataView(frame.buffer).setUint32(1, n)
    frame.set(msg, 5)
    this.write(frame)
  }

  private call(method: string, params: Record<string, unknown> = {}): Promise<unknown> {
    const id = this.nextID++
    const body = new TextEncoder().encode(JSON.stringify({ id, method, params }))
    const frame = new Uint8Array(1 + body.length)
    frame[0] = CHANNEL_JSON
    frame.set(body, 1)
    return new Promise((resolve, reject) => {
      if (this.ws?.readyState !== WebSocket.OPEN) {
        reject(new Error('not connected'))
        return
      }
      this.waiting.set(id, { resolve, reject })
      this.write(frame)
    })
  }

  // request waits for the connection, then makes a request.
  async request<T>(method: string, params: Record<string, unknown> = {}): Promise<T> {
    await this.ready
    return (await this.call(method, params)) as T
  }

  list(path: string) {
    return this.request<Entry[]>('fs.list', { path })
  }
  create(path: string) {
    return this.request<void>('fs.create', { path })
  }
  mkdir(path: string) {
    return this.request<void>('fs.mkdir', { path })
  }
  rename(from: string, to: string) {
    return this.request<void>('fs.rename', { from, to })
  }
  remove(path: string) {
    return this.request<void>('fs.delete', { path })
  }
  status() {
    return this.request<GitStatus>('git.status')
  }
  diff(path: string) {
    return this.request<Diff>('git.diff', { path })
  }
  stage(path: string) {
    return this.request<void>('git.stage', { path })
  }
  unstage(path: string) {
    return this.request<void>('git.unstage', { path })
  }
  rollback(path: string) {
    return this.request<void>('git.rollback', { path })
  }
  commit(message: string) {
    return this.request<void>('git.commit', { message })
  }

  // open returns the shared document for a file, opening it on the service
  // the first time. Each open is matched by a release.
  async open(path: string): Promise<SharedFile> {
    let f = this.files.get(path)
    if (!f) {
      f = new SharedFile(path, this)
      this.files.set(path, f)
      try {
        await this.ready
        await this.reopen(f)
      } catch (e) {
        this.files.delete(path)
        throw e
      }
    }
    f.refs++
    return f
  }

  private async reopen(f: SharedFile) {
    const { doc } = (await this.call('doc.open', { path: f.path })) as { doc: number }
    this.byNumber.set(doc, f)
    f.start(doc)
  }

  release(f: SharedFile) {
    if (--f.refs > 0) return
    this.files.delete(f.path)
    if (f.n) {
      this.byNumber.delete(f.n)
      this.call('doc.close', { doc: f.n }).catch(() => {})
    }
    awarenessProtocol.removeAwarenessStates(f.awareness, [f.doc.clientID], 'local')
    f.awareness.destroy()
    f.doc.destroy()
  }

  close() {
    this.closed = true
    clearTimeout(this.retry)
    this.ws?.close()
  }
}
