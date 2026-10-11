// Code intelligence for the Code tab: language servers run in the
// environment by its Code service, one per language for each browser tab,
// spoken to through @codemirror/lsp-client over the tab's connection.

import {
  findReferences,
  formatDocument,
  jumpToDefinition,
  languageServerExtensions,
  LSPClient,
  LSPPlugin,
  renameSymbol,
  showSignatureHelp,
  Workspace,
  type Transport,
  type WorkspaceFile,
} from '@codemirror/lsp-client'
import { MapMode, Prec, Text, type Extension } from '@codemirror/state'
import { createContext } from 'react'
import { EditorView, keymap } from '@codemirror/view'

import type { CodeClient } from './client'
import { isMac } from './ContextMenu'

// languageId is the LSP language ID for a file, from its name, or null for
// one no server is known for.
export function languageId(path: string): string | null {
  const name = path.split('/').pop() ?? ''
  if (name === 'go.mod' || name === 'go.work') return 'go.mod'
  const ext = name.includes('.') ? name.slice(name.lastIndexOf('.') + 1).toLowerCase() : ''
  return (
    {
      go: 'go',
      ts: 'typescript',
      mts: 'typescript',
      cts: 'typescript',
      tsx: 'typescriptreact',
      js: 'javascript',
      mjs: 'javascript',
      cjs: 'javascript',
      jsx: 'javascriptreact',
      py: 'python',
      rs: 'rust',
      c: 'c',
      h: 'c',
      cc: 'cpp',
      cpp: 'cpp',
      cxx: 'cpp',
      hpp: 'cpp',
      sh: 'shellscript',
      bash: 'shellscript',
      yaml: 'yaml',
      yml: 'yaml',
      json: 'json',
      java: 'java',
      zig: 'zig',
    }[ext] ?? null
  )
}

// The server for each language that has another's: go.mod is gopls's.
const serverLanguage = (id: string) => (id === 'go.mod' ? 'go' : id)

// Position is LSP's place in a file: a 0-based line, and a character in it.
type Position = { line: number; character: number }

export const fileUri = (abs: string) => 'file://' + abs.split('/').map(encodeURIComponent).join('/')
const pathOf = (uri: string) => decodeURIComponent(new URL(uri).pathname)

// Opening files the server points at, such as a definition in another
// file: the Code tab opens it as a tab, and says when its editor is ready.
export interface FileOpener {
  open(path: string): void
}

class OpenFile implements WorkspaceFile {
  uri: string
  languageId: string
  version: number
  doc: Text
  view: EditorView | null

  constructor(uri: string, languageId: string, version: number, doc: Text, view: EditorView | null) {
    this.uri = uri
    this.languageId = languageId
    this.version = version
    this.doc = doc
    this.view = view
  }

  getView() {
    return this.view
  }
}

// HangarWorkspace is the files a server knows of: those open in an editor,
// one editor each, as the default workspace has them; and, for a server
// pointing at a file not open (a reference elsewhere), the file read from
// the environment, or opened as a tab to show it.
class HangarWorkspace extends Workspace {
  files: OpenFile[] = []
  // Files read for a server, not open: their text as read.
  read = new Map<string, Text>()
  private reading = new Map<string, Promise<Text | null>>()

  // fresh has files read again for the next request, which may follow
  // changes to them.
  fresh() {
    this.reading.clear()
  }
  private versions: Record<string, number> = Object.create(null)
  private waiting = new Map<string, ((v: EditorView | null) => void)[]>()
  private code: CodeClient
  private opener: FileOpener

  constructor(client: LSPClient, code: CodeClient, opener: FileOpener) {
    super(client)
    this.code = code
    this.opener = opener
  }

  private nextVersion(uri: string) {
    return (this.versions[uri] = (this.versions[uri] ?? -1) + 1)
  }

  syncFiles() {
    const out = []
    for (const file of this.files) {
      const plugin = file.view && LSPPlugin.get(file.view)
      if (!plugin) continue
      const changes = plugin.unsyncedChanges
      if (!changes.empty) {
        out.push({ changes, file, prevDoc: file.doc })
        file.doc = file.view!.state.doc
        file.version = this.nextVersion(file.uri)
        plugin.clear()
      }
    }
    return out
  }

  openFile(uri: string, languageId: string, view: EditorView) {
    const old = this.getFile(uri) as OpenFile | null
    if (old) {
      // A second editor on the file takes it over.
      this.files = this.files.filter((f) => f !== old)
      this.client.didClose(uri)
    }
    const file = new OpenFile(uri, languageId, this.nextVersion(uri), view.state.doc, view)
    this.files.push(file)
    this.client.didOpen(file)
    for (const w of this.waiting.get(uri) ?? []) w(view)
    this.waiting.delete(uri)
  }

  closeFile(uri: string, view: EditorView) {
    const file = this.getFile(uri) as OpenFile | null
    if (!file || file.view !== view) return
    this.files = this.files.filter((f) => f !== file)
    this.client.didClose(uri)
  }

  async requestFile(uri: string): Promise<WorkspaceFile | null> {
    const open = this.getFile(uri)
    if (open) return open
    // Read once, however many of a server's answers point into it.
    let reading = this.reading.get(uri)
    if (!reading) {
      reading = this.code.read(pathOf(uri)).then(
        (text) => (text === null ? null : Text.of(text.replace(/\r\n/g, '\n').split('\n'))),
        () => null,
      )
      this.reading.set(uri, reading)
    }
    const doc = await reading
    if (!doc) return null
    this.read.set(uri, doc)
    return new OpenFile(uri, languageId(pathOf(uri)) ?? 'plaintext', 0, doc, null)
  }

  displayFile(uri: string): Promise<EditorView | null> {
    const open = this.getFile(uri) as OpenFile | null
    if (open?.view?.dom.isConnected) return Promise.resolve(open.view)
    return new Promise((resolve) => {
      const list = this.waiting.get(uri) ?? []
      list.push(resolve)
      this.waiting.set(uri, list)
      const abs = pathOf(uri)
      const root = this.code.root
      this.opener.open(abs.startsWith(root + '/') ? abs.slice(root.length + 1) : abs)
      setTimeout(() => {
        const left = this.waiting.get(uri)
        if (left?.includes(resolve)) {
          this.waiting.set(
            uri,
            left.filter((w) => w !== resolve),
          )
          resolve(null)
        }
      }, 10_000)
    })
  }
}

const mod = (e: MouseEvent) => (isMac ? e.metaKey : e.ctrlKey)

// ideaKeys are IntelliJ's keys for what a server answers, beside the
// package's own (F12, Shift+F12, F2, Shift+Alt+F): Ctrl/Cmd+B and
// Ctrl/Cmd+click go to a definition, Alt+F7 finds usages, Shift+F6
// renames, Ctrl+Alt+L (Cmd+Alt+L) reformats, and Ctrl/Cmd+P shows a call's
// parameters.
const ideaKeys: Extension = [
  Prec.high(
    keymap.of([
      { key: 'Mod-b', run: jumpToDefinition, preventDefault: true },
      { key: 'Alt-F7', run: findReferences, preventDefault: true },
      { key: 'Shift-F6', run: renameSymbol, preventDefault: true },
      { key: 'Mod-Alt-l', run: formatDocument, preventDefault: true },
      { key: 'Mod-p', run: showSignatureHelp, preventDefault: true },
    ]),
  ),
  EditorView.domEventHandlers({
    mousedown(e, view) {
      if (e.button !== 0 || !mod(e) || e.shiftKey || e.altKey) return false
      const pos = view.posAtCoords({ x: e.clientX, y: e.clientY })
      if (pos == null) return false
      e.preventDefault()
      view.dispatch({ selection: { anchor: pos } })
      return jumpToDefinition(view)
    },
  }),
  EditorView.theme({
    '.cm-lsp-documentation': { fontFamily: 'var(--font)', fontSize: '12.5px' },
    '.cm-tooltip.cm-tooltip-hover, .cm-tooltip.cm-lsp-signature-tooltip': {
      maxWidth: 'min(640px, 80vw)',
      maxHeight: '320px',
      overflow: 'auto',
    },
  }),
]

// A language's server, as far as this tab knows: starting, ready with its
// client, or missing, with why.
export type ServerState =
  | { state: 'starting'; language: string }
  | { state: 'ready'; language: string; command: string; client: LSPClient }
  | { state: 'missing'; language: string; why: string }

// Languages starts, keeps and forgets a tab's language servers. A server
// is started the first time a file of its language is opened, and all of
// them end with the connection; editors are told, and attach again.
export class Languages {
  private servers = new Map<string, Promise<ServerState>>()
  private numbers = new Map<number, string>()
  private listeners = new Set<() => void>()
  private code: CodeClient
  private opener: FileOpener

  constructor(code: CodeClient, opener: FileOpener) {
    this.code = code
    this.opener = opener
    code.on('state', (s) => {
      if (s === 'reconnecting') this.reset()
    })
    code.on('lsp.exited', ({ server, why }) => {
      const lang = this.numbers.get(server)
      if (lang === undefined) return
      this.numbers.delete(server)
      this.servers.set(lang, Promise.resolve({ state: 'missing', language: lang, why: `its server stopped: ${why}` }))
      this.changed()
    })
  }

  private reset() {
    this.servers.clear()
    this.numbers.clear()
    this.changed()
  }

  private changed() {
    for (const fn of this.listeners) fn()
  }

  // onChange calls fn when servers stop, for editors to attach again.
  onChange(fn: () => void): () => void {
    this.listeners.add(fn)
    return () => this.listeners.delete(fn)
  }

  // server is the server for a file's language, started if need be.
  server(path: string): Promise<ServerState> | null {
    const id = languageId(path)
    if (!id) return null
    const lang = serverLanguage(id)
    let p = this.servers.get(lang)
    if (!p) {
      p = this.start(lang)
      this.servers.set(lang, p)
    }
    return p
  }

  private async start(language: string): Promise<ServerState> {
    let res
    try {
      res = await this.code.startLanguageServer(language)
    } catch (e) {
      // The connection dropped; the next attach starts again.
      this.servers.delete(language)
      return { state: 'missing', language, why: e instanceof Error ? e.message : String(e) }
    }
    if ('missing' in res) return { state: 'missing', language, why: res.missing }
    const n = res.server
    this.numbers.set(n, language)
    const offs = new Map<(v: string) => void, () => void>()
    const transport: Transport = {
      send: (m) => this.code.sendLanguageServer(n, m),
      subscribe: (h) => offs.set(h, this.code.onLanguageServer(n, h)),
      unsubscribe: (h) => {
        offs.get(h)?.()
        offs.delete(h)
      },
    }
    let workspace: HangarWorkspace | null = null
    const client = new LSPClient({
      rootUri: fileUri(res.root),
      timeout: 15_000,
      workspace: (c) => (workspace = new HangarWorkspace(c, this.code, this.opener)),
      extensions: [...languageServerExtensions(), ideaKeys],
    }).connect(transport)
    // A mapping places a server's positions in files as they were when it
    // was made, and knows only the files open then; a reference in a file
    // not open is placed in its text as read for the list of them.
    const mapping = client.workspaceMapping.bind(client)
    client.workspaceMapping = () => {
      workspace?.fresh()
      const m = mapping()
      const mapPosition = m.mapPosition.bind(m) as (
        uri: string,
        pos: Position,
        assoc?: number,
        mode?: MapMode,
      ) => number
      m.mapPosition = ((uri: string, pos: Position, assoc?: number, mode?: MapMode) => {
        try {
          return mapPosition(uri, pos, assoc, mode)
        } catch (e) {
          const doc = workspace?.read.get(uri)
          if (!doc) throw e
          const line = doc.line(Math.min(pos.line + 1, doc.lines))
          return line.from + Math.min(pos.character, line.length)
        }
      }) as typeof m.mapPosition
      return m
    }
    return { state: 'ready', language, command: res.command, client }
  }

  // extension is what an editor of a file adds to talk to its server.
  static extension(s: ServerState, abs: string, id: string): Extension {
    return s.state === 'ready' ? s.client.plugin(fileUri(abs), id) : []
  }
}

// LanguagesContext gives editors their tab's language servers.
export const LanguagesContext = createContext<Languages | null>(null)
