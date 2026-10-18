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
// merges it when the documents sync. A connection that stops answering is
// taken as dropped (Heartbeat), and one dropped is tried again at once when
// the page is shown again or the network comes back.

import * as decoding from 'lib0/decoding'
import * as encoding from 'lib0/encoding'
import * as awarenessProtocol from 'y-protocols/awareness'
import * as syncProtocol from 'y-protocols/sync'
import * as Y from 'yjs'

import { Heartbeat, onReturn } from '../heartbeat'

const CHANNEL_JSON = 0x72 // 'r'
const CHANNEL_YJS = 0x79 // 'y'
const CHANNEL_LSP = 0x6c // 'l'
const MESSAGE_SYNC = 0
const MESSAGE_AWARENESS = 1

// An entry in a folder. Of a file: whether it is a link, runs and may not
// be written; of either, whether git ignores it.
export interface Entry {
  name: string
  dir: boolean
  size: number
  link?: boolean
  exec?: boolean
  readonly?: boolean
  ignored?: boolean
}

// What a copy or move does with a name the folder has: report it, or
// replace it, skip it, or give the new one a free name.
export type Clash = 'ask' | 'replace' | 'skip' | 'rename'

// A path in the trash, by its id there, to restore it by.
export type Trashed = { path: string; id: string }

export type Encoding = 'UTF-8' | 'UTF-16LE' | 'UTF-16BE' | 'ISO-8859-1'
export type Separator = 'LF' | 'CRLF' | 'CR'

// TextProps are what a file's bytes say of its text, and its mode.
export type TextProps = {
  encoding: Encoding | null
  bom: boolean
  separator: Separator | 'mixed' | null
  mode: number
  size: number
  modified: number
}

export interface Change {
  path: string
  index: string
  worktree: string
  from?: string
  // A submodule whose commit has moved, for its parent to record.
  submodule?: boolean
}

// A Repo is a git repository in the folder: the one the folder is in
// (path ''), a submodule another names, or a clone of its own. Its
// changes' paths are the folder's.
export interface Repo {
  path: string
  name: string
  kind: 'folder' | 'submodule' | 'clone'
  // The repository naming a submodule.
  parent?: string
  // A submodule named but not cloned.
  uninitialised?: boolean
  // Why git cannot read it.
  error?: string
  branch?: string
  ahead?: number
  behind?: number
  changes: Change[]
}

// GitStatus is every repository in the folder, and all their changes.
export interface GitStatus {
  repo: boolean
  changes: Change[]
  repos: Repo[]
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

// MissingServer is why a language has no server, and the servers that
// would serve it, with how each is installed when Hangar knows.
export interface MissingServer {
  missing: string
  install: { command: string; install?: string }[]
}

// A message between a language server and the service, for debugging.
export interface Traced {
  t: number
  dir: 'to' | 'from'
  kind: 'request' | 'notification' | 'response'
  method?: string
  id?: string
  ms?: number
  error?: string
}

// ServerInfo is a language server running for the folder, one that is
// installed but not running, or why there is none.
export type ServerInfo =
  | {
      running: true
      language: string
      command: string
      path: string
      args: string[]
      pid?: number
      started: number
      root: string
      serverInfo?: { name?: string; version?: string }
      tabs: number
      documents: number
      log: string
      trace: Traced[]
    }
  | { running: false; command: string; path: string }
  | ({ running: false } & MissingServer)

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
  // A folder to search in, from the root.
  dir?: string
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
  'doc.restored': (p: { path: string }) => void
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
    'doc.restored': new Set(),
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
  private beat: Heartbeat | undefined
  private offReturn: () => void
  private closed = false
  // ready is settled while the connection is live, and pending while it
  // is being made or made again.
  private ready: Promise<void>
  private markReady!: () => void
  state: ConnectionState = 'connecting'
  // root is the folder this tab works in; home is the environment's own,
  // which every tab starts in.
  root = ''
  home = ''
  // settingsPath is the Code tab's settings file, in the user's home.
  settingsPath = ''
  private env: string
  private want: string

  // want is a folder to work in other than the environment's own, or ''.
  constructor(env: string, want = '') {
    this.env = env
    this.want = want
    this.ready = new Promise((r) => (this.markReady = r))
    this.offReturn = onReturn(() => {
      if (this.retry === undefined) return
      clearTimeout(this.retry)
      this.retry = undefined
      this.backoff = 500
      this.connect()
    })
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
        const hello = (await this.call('hello', { root: this.want })) as {
          root: string
          home: string
          settings?: string
        }
        this.root = hello.root
        this.home = hello.home
        this.settingsPath = hello.settings ?? ''
        this.backoff = 500
        for (const f of this.files.values()) await this.reopen(f)
        this.setState('live')
        this.markReady()
      } catch (e) {
        ws.close(4000, e instanceof Error ? e.message : String(e))
      }
    }
    const beat = new Heartbeat(ws, (why) => {
      this.dropped(ws, why)
      ws.close()
    })
    this.beat = beat
    ws.onmessage = (e) => {
      if (typeof e.data === 'string') beat.answered(e.data)
      else this.receive(new Uint8Array(e.data as ArrayBuffer))
    }
    ws.onclose = (e) => this.dropped(ws, e.reason)
  }

  // dropped is what follows a connection ending, or being taken as gone:
  // what waits on it fails or waits for the next, and the next is made.
  private dropped(ws: WebSocket, why: string) {
    if (this.ws !== ws) return
    this.ws = null
    this.beat?.stop()
    for (const w of this.waiting.values()) w.reject(new Error('the connection dropped'))
    this.waiting.clear()
    this.byNumber.clear()
    // Language servers end with the connection.
    this.lspHandlers.clear()
    if (this.closed) return
    // Requests wait for the connection to be back, not fail at once.
    if (this.state === 'live') this.ready = new Promise((r) => (this.markReady = r))
    this.setState('reconnecting', why)
    this.retry = setTimeout(() => {
      this.retry = undefined
      this.connect()
    }, this.backoff)
    this.backoff = Math.min(this.backoff * 2, 8000)
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
      } else if (m.event === 'doc.restored') {
        this.emit('doc.restored', m.params)
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

  // ping is how long, in milliseconds, a request takes to reach the service
  // and be answered: through Hangar's server and the worker's tunnel to the
  // environment, and back. A service too old to know the request answers
  // that it does not, which takes as long.
  async ping(): Promise<number> {
    await this.ready
    const start = performance.now()
    try {
      await this.call('ping')
    } catch (e) {
      if (!(e instanceof Error && e.message.startsWith('no such method'))) throw e
    }
    return performance.now() - start
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
    return this.request<{ server: number; root: string; command: string } | MissingServer>('lsp.start', {
      language,
    })
  }
  // languageServerInfo is what this folder's server for a language is
  // doing, or why there is none.
  languageServerInfo(language: string) {
    return this.request<ServerInfo>('lsp.info', { language })
  }
  // restartLanguageServer stops the server for every tab; each starts it
  // again.
  restartLanguageServer(language: string) {
    return this.request<null>('lsp.restart', { language })
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
  // previewServer starts the service's server of files for an HTML
  // file's preview, if it is not running, and says its port and the secret
  // its paths begin with.
  previewServer() {
    return this.request<{ port: number; secret: string }>('preview.start')
  }
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
  // transfer copies or moves paths into a folder, under their own names or,
  // for one, name. With clash 'ask', names the folder has are reported and
  // nothing is done.
  transfer(op: 'copy' | 'move', from: string[], to: string, clash: Clash, name?: string) {
    return this.request<{ done: { from: string; to: string }[]; clashes: string[] }>('fs.transfer', {
      op,
      from,
      to,
      clash,
      name,
    })
  }
  // trash puts paths in the trash; restore puts them back.
  trash(paths: string[]) {
    return this.request<Trashed[]>('fs.trash', { paths })
  }
  restore(items: Trashed[]) {
    return this.request<void>('fs.restore', { items })
  }
  uploadStart(path: string) {
    return this.request<string>('fs.uploadStart', { path })
  }
  uploadWrite(token: string, data: string) {
    return this.request<void>('fs.uploadWrite', { token, data })
  }
  uploadEnd(token: string, replace: boolean) {
    return this.request<string>('fs.uploadEnd', { token, replace })
  }
  uploadAbort(token: string) {
    return this.request<void>('fs.uploadAbort', { token })
  }
  // zip packs paths into a zip to download, read with bytes, then removed
  // with unzip.
  zip(paths: string[]) {
    return this.request<{ path: string; size: number }>('fs.zip', { paths })
  }
  unzip(path: string) {
    return this.request<void>('fs.unzip', { path })
  }
  setMode(path: string, change: { exec?: boolean; writable?: boolean }) {
    return this.request<number>('fs.setMode', { path, ...change })
  }
  textProps(path: string) {
    return this.request<TextProps>('fs.textProps', { path })
  }
  convert(path: string, to: { separator?: Separator; encoding?: Encoding; bom?: boolean }) {
    return this.request<TextProps>('fs.convert', { path, ...to })
  }
  // replace replaces a query's every match, in the files given or all it
  // matches in.
  replace(q: TextQuery, replacement: string, paths?: string[]) {
    return this.request<{ files: string[]; replaced: number }>('search.replace', { ...q, replacement, paths })
  }
  // ignore adds a path to the repository's .gitignore.
  ignore(path: string, dir: boolean) {
    return this.request<void>('git.ignore', { path, dir })
  }
  async status(): Promise<GitStatus> {
    const s = await this.request<GitStatus & { branch?: string; ahead?: number; behind?: number }>('git.status')
    if (s.repos) return s
    // The editor disk of an environment is built apart from this page, and
    // one whose service lists no repositories reports the folder's own,
    // its fields beside the changes.
    const { branch, ahead, behind, ...rest } = s
    const name = this.root.split('/').pop() || this.root
    return {
      ...rest,
      repos: s.repo ? [{ path: '', name, kind: 'folder', branch, ahead, behind, changes: s.changes }] : [],
    }
  }
  // diff is a file as HEAD has it, or another commit (against), under the
  // name it had there (from), beside the working tree's or a later
  // commit's (to).
  diff(path: string, against?: string, from?: string, to?: string) {
    return this.request<Diff>('git.diff', { path, against, from, to })
  }
  // log is a page of the history: every branch's, or ref's; of a path,
  // the commits that changed it.
  //
  // The calls that act on a whole repository take repo, a path in it,
  // the folder's own repository when none is given; the others act on
  // the repository their path is in.
  log(ref: string | undefined, skip: number, max: number, path?: string, repo?: string) {
    return this.request<Log>('git.log', { ref, skip, max, path, repo })
  }
  // blob is a file as a commit has it, base64; empty when it has none.
  blob(rev: string, path: string) {
    return this.request<{ size: number; data: string }>('git.blob', { rev, path })
  }
  commitInfo(hash: string, repo?: string) {
    return this.request<CommitInfo>('git.commitInfo', { hash, repo })
  }
  // blame is who last changed each line of a file, as it is on disk.
  blame(path: string) {
    return this.request<Blame>('git.blame', { path })
  }
  // branchChanges is what the branch has changed since it left base, the
  // repository's main branch when none is given.
  branchChanges(base?: string, repo?: string) {
    return this.request<BranchChanges>('git.branchChanges', { base, repo })
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
  commit(message: string, amend = false, repo?: string) {
    return this.request<void>('git.commit', { message, amend, repo })
  }
  lastMessage(repo?: string) {
    return this.request<string>('git.lastMessage', { repo })
  }
  // show is a file as HEAD or the index has it; null where it has none.
  async show(path: string, from: 'HEAD' | 'index') {
    return (await this.request<{ text: string | null }>('git.show', { path, from })).text
  }
  // setIndex stages exactly this content for a file.
  setIndex(path: string, content: string) {
    return this.request<void>('git.setIndex', { path, content })
  }
  branches(repo?: string) {
    return this.request<Branches>('git.branches', { repo })
  }
  checkout(name: string, repo?: string) {
    return this.request<void>('git.checkout', { name, repo })
  }
  createBranch(name: string, repo?: string) {
    return this.request<void>('git.createBranch', { name, repo })
  }
  push(repo?: string) {
    return this.request<void>('git.push', { repo })
  }
  pull(repo?: string) {
    return this.request<void>('git.pull', { repo })
  }
  fetch(repo?: string) {
    return this.request<void>('git.fetch', { repo })
  }
  // initSubmodule clones a submodule its repository names, and its own.
  initSubmodule(path: string) {
    return this.request<void>('git.initSubmodule', { path })
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
    this.beat?.stop()
    this.offReturn()
    this.ws?.close()
  }
}

// A live connection keeps the code it was made with, which a hot update
// in development would leave stale under the page: a change to this file
// reloads the page instead.
if (import.meta.hot) import.meta.hot.accept(() => window.location.reload())
