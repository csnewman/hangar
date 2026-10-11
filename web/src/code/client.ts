// The browser end of the Code tab's service in the environment (editor/code):
// requests and events on one WebSocket, and shared documents over Yjs.
//
// The WebSocket carries the service's frames, one per message: a channel
// byte, 'r' for JSON, 'y' for a document's Yjs message (its number as a
// big-endian u32, then the y-protocols message), or 'l' for a language
// server's message (the server's number, then the message's JSON).
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
const CHANNEL_LSP = 0x6c // 'l'
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
  // The name the file had in the commit compared with, when renamed since.
  from?: string
  head: string
  working: string
  // Bytes, not text: head and working are empty, and the view reads the
  // two sides with blob and bytes.
  binary?: boolean
}

// What a file is: text, or bytes that are not, and how large; whether
// the user may write it, and its version, which any write changes.
export interface Probe {
  size: number
  binary: boolean
  writable: boolean
  version: string
}

// Part of a file, base64, the whole file's size, and the version read.
export interface Bytes {
  size: number
  offset: number
  data: string
  version: string
}

// A ref names a commit: the branch checked out (head), a local or remote
// branch, or a tag.
export interface Ref {
  name: string
  kind: 'head' | 'branch' | 'remote' | 'tag'
}

export interface LogCommit {
  hash: string
  parents: string[]
  author: string
  email: string
  time: number
  subject: string
  refs: Ref[]
}

export interface Log {
  commits: LogCommit[]
  more: boolean
}

export interface CommitInfo {
  hash: string
  parents: string[]
  author: string
  email: string
  time: number
  committer: string
  committerTime: number
  message: string
  files: { path: string; status: string; from?: string }[]
}

export interface Blame {
  commits: Record<string, { author: string; time: number; summary: string }>
  lines: string[]
}

export interface BranchChanges {
  base: string
  mergeBase: string
  commits: { hash: string; subject: string; author: string; time: number }[]
  changes: { path: string; status: string; from?: string }[]
}

export interface Branch {
  name: string
  upstream?: string
  ahead?: number
  behind?: number
}

export interface Branches {
  current?: string
  local: Branch[]
  remote: string[]
}

export interface Match {
  path: string
  line: number
  text: string
  ranges: [number, number][]
}

export interface TextQuery {
  query: string
  regex?: boolean
  caseSensitive?: boolean
  wholeWord?: boolean
  mask?: string
}

export interface Container {
  id: string
  name: string
  image: string
  state: string
  status: string
  ports: string[]
  created: number
  command: string
  project?: string
  service?: string
  workingDir?: string
  configFiles?: string[]
}

export interface DockerImage {
  id: string
  tags: string[]
  size: number
  created: number
  containers: number
}

export interface DockerOverview {
  available: boolean
  why?: string
  containers: Container[]
  images: DockerImage[]
}

export type DockerAction = 'start' | 'stop' | 'restart' | 'pause' | 'unpause' | 'remove'

export type ConnectionState = 'connecting' | 'live' | 'reconnecting'

type Events = {
  state: (s: ConnectionState, why: string) => void
  'fs.changed': (p: { dirs: string[] }) => void
  'git.changed': () => void
  // An open file was deleted from disk. Its path is absolute.
  'doc.deleted': (p: { path: string }) => void
  // Another tab changed the settings.
  'settings.changed': (s: Record<string, unknown>) => void
  // A language server this tab started stopped.
  'lsp.exited': (p: { server: number; why: string }) => void
  // Something happened to a container or image.
  'docker.changed': () => void
  // Output from a container whose logs are followed, and the end of it.
  'docker.log': (p: { stream: number; text: string }) => void
  'docker.logEnd': (p: { stream: number; why: string }) => void
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
  // writable is whether the environment's user may write the file.
  writable = true
  private syncedWaiters: (() => void)[] = []
  refs = 0
  readonly path: string
  private client: CodeClient

  constructor(path: string, client: CodeClient) {
    this.path = path
    this.client = client
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
    'settings.changed': new Set(),
    'lsp.exited': new Set(),
    'docker.changed': new Set(),
    'docker.log': new Set(),
    'docker.logEnd': new Set(),
  }
  private lspHandlers = new Map<number, Set<(json: string) => void>>()
  private files = new Map<string, SharedFile>()
  private byNumber = new Map<number, SharedFile>()
  private retry: ReturnType<typeof setTimeout> | undefined
  private backoff = 500
  private closed = false
  private ready: Promise<void>
  private markReady!: () => void
  state: ConnectionState = 'connecting'
  // root is the folder this tab works in; home is the environment's own,
  // which every tab starts in.
  root = ''
  home = ''
  private env: string
  private want: string

  // want is a folder to work in other than the environment's own, or ''.
  constructor(env: string, want = '') {
    this.env = env
    this.want = want
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
        const hello = (await this.call('hello', { root: this.want })) as { root: string; home: string }
        this.root = hello.root
        this.home = hello.home
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
      // Language servers end with the connection.
      this.lspHandlers.clear()
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
      } else if (m.event === 'settings.changed') {
        this.emit('settings.changed', m.params)
      } else if (m.event === 'docker.changed') {
        this.emit('docker.changed')
      } else if (m.event === 'docker.log') {
        this.emit('docker.log', m.params)
      } else if (m.event === 'docker.logEnd') {
        this.emit('docker.logEnd', m.params)
      } else if (m.event === 'lsp.exited') {
        this.lspHandlers.delete(m.params.server)
        this.emit('lsp.exited', m.params)
      }
    } else if (msg[0] === CHANNEL_LSP && msg.length >= 5) {
      const n = new DataView(msg.buffer, msg.byteOffset + 1, 4).getUint32(0)
      const json = new TextDecoder().decode(msg.subarray(5))
      for (const h of this.lspHandlers.get(n) ?? []) h(json)
    } else if (msg[0] === CHANNEL_YJS && msg.length >= 5) {
      const n = new DataView(msg.buffer, msg.byteOffset + 1, 4).getUint32(0)
      this.byNumber.get(n)?.receive(msg.subarray(5))
    }
  }

  private write(frame: Uint8Array) {
    if (this.ws?.readyState === WebSocket.OPEN) this.ws.send(frame as Uint8Array<ArrayBuffer>)
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

  // connected waits until the connection is first made.
  connected(): Promise<void> {
    return this.ready
  }

  // request waits for the connection, then makes a request.
  async request<T>(method: string, params: Record<string, unknown> = {}): Promise<T> {
    await this.ready
    return (await this.call(method, params)) as T
  }

  list(path: string) {
    return this.request<Entry[]>('fs.list', { path })
  }
  // read is a file's text, or null for one too large or not text.
  async read(path: string) {
    return (await this.request<{ text: string | null }>('fs.read', { path })).text
  }
  // probe says whether a file is text or bytes, for choosing its view.
  probe(path: string) {
    return this.request<Probe>('fs.probe', { path })
  }
  // bytes is length bytes of a file from offset, base64.
  bytes(path: string, offset: number, length: number) {
    return this.request<Bytes>('fs.bytes', { path, offset, length })
  }
  // patch writes runs of bytes over a file's own, if it is still version.
  patch(path: string, version: string, runs: { offset: number; data: string }[]) {
    return this.request<{ version: string }>('fs.patch', { path, version, runs })
  }
  dockerOverview() {
    return this.request<DockerOverview>('docker.overview')
  }
  dockerAct(id: string, action: DockerAction) {
    return this.request<void>('docker.act', { id, action })
  }
  // dockerProject does to every container of a Compose project, or takes
  // it down.
  dockerProject(project: string, action: 'start' | 'stop' | 'restart' | 'down') {
    return this.request<void>('docker.project', { project, action })
  }
  dockerRemoveImage(id: string) {
    return this.request<void>('docker.removeImage', { id })
  }
  // dockerLogs follows a container's output, from its last tail lines.
  dockerLogs(id: string, tail = 1000) {
    return this.request<{ stream: number }>('docker.logs', { id, tail })
  }
  dockerLogsStop(stream: number) {
    return this.request<void>('docker.logsStop', { stream })
  }

  // startLanguageServer starts a server for an LSP language ID in this
  // tab's folder, or says why there is none.
  startLanguageServer(language: string) {
    return this.request<{ server: number; root: string; command: string } | { missing: string }>('lsp.start', {
      language,
    })
  }
  stopLanguageServer(server: number) {
    return this.request<void>('lsp.stop', { server })
  }
  // onLanguageServer subscribes to a server's messages.
  onLanguageServer(server: number, fn: (json: string) => void): () => void {
    let set = this.lspHandlers.get(server)
    if (!set) this.lspHandlers.set(server, (set = new Set()))
    set.add(fn)
    return () => set.delete(fn)
  }
  sendLanguageServer(server: number, json: string) {
    const body = new TextEncoder().encode(json)
    const frame = new Uint8Array(5 + body.length)
    frame[0] = CHANNEL_LSP
    new DataView(frame.buffer).setUint32(1, server)
    frame.set(body, 5)
    this.write(frame)
  }

  // settings are the Code tab's, kept in the user's home (shared by their
  // profile by default).
  settings() {
    return this.request<Record<string, unknown>>('settings.get')
  }
  saveSettings(settings: Record<string, unknown>) {
    return this.request<void>('settings.set', { settings })
  }
  searchFiles() {
    return this.request<{ files: string[]; truncated: boolean }>('search.files')
  }
  searchText(q: TextQuery) {
    return this.request<{ matches: Match[]; truncated: boolean }>('search.text', { ...q })
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
  // diff is a file as HEAD has it, or another commit (against), under the
  // name it had there (from), beside the working tree's or a later
  // commit's (to).
  diff(path: string, against?: string, from?: string, to?: string) {
    return this.request<Diff>('git.diff', { path, against, from, to })
  }
  // log is a page of the history: every branch's, or ref's.
  log(ref: string | undefined, skip: number, max: number) {
    return this.request<Log>('git.log', { ref, skip, max })
  }
  // blob is a file as a commit has it, base64; empty when it has none.
  blob(rev: string, path: string) {
    return this.request<{ size: number; data: string }>('git.blob', { rev, path })
  }
  commitInfo(hash: string) {
    return this.request<CommitInfo>('git.commitInfo', { hash })
  }
  // blame is who last changed each line of a file, as it is on disk.
  blame(path: string) {
    return this.request<Blame>('git.blame', { path })
  }
  // branchChanges is what the branch has changed since it left base, the
  // repository's main branch when none is given.
  branchChanges(base?: string) {
    return this.request<BranchChanges>('git.branchChanges', { base })
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
  commit(message: string, amend = false) {
    return this.request<void>('git.commit', { message, amend })
  }
  lastMessage() {
    return this.request<string>('git.lastMessage')
  }
  // show is a file as HEAD or the index has it; null where it has none.
  async show(path: string, from: 'HEAD' | 'index') {
    return (await this.request<{ text: string | null }>('git.show', { path, from })).text
  }
  // setIndex stages exactly this content for a file.
  setIndex(path: string, content: string) {
    return this.request<void>('git.setIndex', { path, content })
  }
  branches() {
    return this.request<Branches>('git.branches')
  }
  checkout(name: string) {
    return this.request<void>('git.checkout', { name })
  }
  createBranch(name: string) {
    return this.request<void>('git.createBranch', { name })
  }
  push() {
    return this.request<void>('git.push')
  }
  pull() {
    return this.request<void>('git.pull')
  }
  fetch() {
    return this.request<void>('git.fetch')
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
    const { doc, writable } = (await this.call('doc.open', { path: f.path })) as { doc: number; writable?: boolean }
    f.writable = writable !== false
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
