// Code intelligence for the Code tab: language servers run in the
// environment by its Code service, one per language for each browser tab,
// spoken to through @codemirror/lsp-client over the tab's connection.

import {
  formatDocument,
  languageServerExtensions,
  LSPClient,
  LSPPlugin,
  showSignatureHelp,
  Workspace,
  type Transport,
  type WorkspaceFile,
} from '@codemirror/lsp-client'
import { MapMode, Prec, Text, type Extension } from '@codemirror/state'
import { createContext } from 'react'
import { EditorView, keymap, type Command } from '@codemirror/view'

import type { CodeClient, MissingServer } from './client'
import { isMac } from './ContextMenu'
import { startRename } from './editorPopups'
import type { Jump } from './Search'

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
  // open opens a file as a tab.
  open(path: string): void
  // jump opens a file at a place, the place in the middle of the view.
  jump(j: Jump): void
  // usages shows a symbol's usages, or that they are being found.
  usages(u: Usages): void
  // run runs a command in a new terminal session: installing a server.
  run(command: string): void
}

// A usage of a symbol: where it is, in the Code tab's terms (a path from
// the folder, or absolute outside it), and the line it is on.
export type Usage = { path: string; line: number; col: number; length: number; text: string }

// Usages are what the Find window lists: a symbol's usages, or the
// matches of a Find in Files moved there (kind 'text', symbol being what
// was searched for, more saying when they were cut short).
export type Usages = {
  kind?: 'usages' | 'text'
  symbol: string
  state: 'finding' | 'found' | 'failed'
  usages: Usage[]
  why?: string
  more?: boolean
}

type Location = { uri: string; range: Range }
type LocationLink = { targetUri: string; targetSelectionRange: Range }
type Range = { start: Position; end: Position }

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

// The Code tab's own going to a definition and finding of usages, in place
// of the package's: a place in another file opens through the Code tab,
// in the middle of the view, and usages are listed in its Find tool window,
// which stays as files are opened from it.
function commands(code: CodeClient, opener: FileOpener) {
  const pathOfUri = (uri: string) => {
    const abs = pathOf(uri)
    return abs.startsWith(code.root + '/') ? abs.slice(code.root.length + 1) : abs
  }

  const goToDefinition: Command = (view) => {
    const plugin = LSPPlugin.get(view)
    if (!plugin || (plugin.client.serverCapabilities && !plugin.client.serverCapabilities.definitionProvider))
      return false
    plugin.client.sync()
    const at = view.state.selection.main.head
    plugin.client
      .request<unknown, Location | Location[] | LocationLink[] | null>('textDocument/definition', {
        textDocument: { uri: plugin.uri },
        position: plugin.toPosition(at),
      })
      .then(
        (res) => {
          const first = Array.isArray(res) ? res[0] : res
          if (!first) return
          const uri = 'targetUri' in first ? first.targetUri : first.uri
          const range = 'targetUri' in first ? first.targetSelectionRange : first.range
          if (uri === plugin.uri && view.dom.isConnected) {
            const pos = plugin.fromPosition(range.start)
            view.dispatch({ selection: { anchor: pos }, effects: EditorView.scrollIntoView(pos, { y: 'center' }) })
            view.focus()
          } else opener.jump({ path: pathOfUri(uri), line: range.start.line + 1, col: range.start.character + 1 })
        },
        (e) => plugin.reportError('Going to the definition failed', e),
      )
    return true
  }

  const findUsages: Command = (view) => {
    const plugin = LSPPlugin.get(view)
    if (!plugin || (plugin.client.serverCapabilities && !plugin.client.serverCapabilities.referencesProvider))
      return false
    plugin.client.sync()
    const at = view.state.selection.main.head
    const word = view.state.wordAt(at)
    const symbol = word ? view.state.sliceDoc(word.from, word.to) : 'the symbol'
    opener.usages({ symbol, state: 'finding', usages: [] })
    const workspace = plugin.client.workspace
    plugin.client
      .request<unknown, Location[] | null>('textDocument/references', {
        textDocument: { uri: plugin.uri },
        position: plugin.toPosition(at),
        context: { includeDeclaration: true },
      })
      .then(async (res) => {
        const usages: Usage[] = []
        for (const loc of res ?? []) {
          const file = await workspace.requestFile(loc.uri)
          const n = loc.range.start.line + 1
          const line = file && n <= file.doc.lines ? file.doc.line(n).text : ''
          const length =
            loc.range.end.line === loc.range.start.line ? loc.range.end.character - loc.range.start.character : 0
          usages.push({ path: pathOfUri(loc.uri), line: n, col: loc.range.start.character + 1, length, text: line })
        }
        usages.sort((a, b) => a.path.localeCompare(b.path) || a.line - b.line || a.col - b.col)
        opener.usages({ symbol, state: 'found', usages })
      })
      .catch((e: unknown) =>
        opener.usages({ symbol, state: 'failed', usages: [], why: e instanceof Error ? e.message : String(e) }),
      )
    return true
  }

  return { goToDefinition, findUsages }
}

const mod = (e: MouseEvent) => (isMac ? e.metaKey : e.ctrlKey)

// ideaKeys are IntelliJ's keys for what a server answers: Ctrl/Cmd+B,
// Ctrl/Cmd+click and F12 go to a definition, Alt+F7 and Shift+F12 find
// usages, Shift+F6 (and F2) renames, Ctrl+Alt+L (Cmd+Alt+L) reformats, and
// Ctrl/Cmd+P shows a call's parameters.
function ideaKeys(c: ReturnType<typeof commands>): Extension {
  return [
    Prec.highest(
      keymap.of([
        { key: 'Mod-b', run: c.goToDefinition, preventDefault: true },
        { key: 'F12', run: c.goToDefinition, preventDefault: true },
        { key: 'Alt-F7', run: c.findUsages, preventDefault: true },
        { key: 'Shift-F12', run: c.findUsages, preventDefault: true },
        { key: 'Shift-F6', run: startRename, preventDefault: true },
        { key: 'F2', run: startRename, preventDefault: true },
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
        return c.goToDefinition(view)
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
}

// A problem a language server has found in a file: where it is (lines and
// columns from 1, the file as the Code tab names it), how bad (LSP's
// severity: 1 an error, 2 a warning, 3 information, 4 a hint), and what it
// says.
export type Problem = {
  path: string
  line: number
  col: number
  severity: number
  message: string
  source?: string
  code?: string
}

type Diagnostic = {
  range: Range
  severity?: number
  message: string
  source?: string
  code?: string | number
}

// A language's server, as far as this tab knows: starting, ready with its
// client, or missing, with why.
export type ServerState =
  | { state: 'starting'; language: string }
  | { state: 'ready'; language: string; command: string; client: LSPClient }
  | { state: 'missing'; language: string; why: string; install: MissingServer['install']; stopped?: boolean }

// Languages starts, keeps and forgets a tab's language servers. A server
// is started the first time a file of its language is opened, and all of
// them end with the connection; editors are told, and attach again.
export class Languages {
  private servers = new Map<string, Promise<ServerState>>()
  private numbers = new Map<number, string>()
  private listeners = new Set<() => void>()
  // ready are the clients of the servers running, by language.
  private ready = new Map<string, LSPClient>()
  // problems are what each server last said of each file, by language and
  // then file.
  private problems = new Map<string, Map<string, Problem[]>>()
  private problemListeners = new Set<() => void>()
  private code: CodeClient
  private opener: FileOpener
  // commands are going to a definition and finding usages, for menus.
  readonly commands: ReturnType<typeof commands>

  constructor(code: CodeClient, opener: FileOpener) {
    this.code = code
    this.opener = opener
    this.commands = commands(code, opener)
    code.on('state', (s) => {
      if (s === 'reconnecting') this.reset()
    })
    code.on('lsp.exited', ({ server, why }) => {
      const lang = this.numbers.get(server)
      if (lang === undefined) return
      this.numbers.delete(server)
      this.ready.delete(lang)
      this.forget(lang)
      // Restarted, by anyone: started again here too.
      if (why === 'restarted') this.servers.delete(lang)
      else {
        this.servers.set(
          lang,
          Promise.resolve({ state: 'missing', language: lang, why: `its server stopped: ${why}`, install: [], stopped: true }),
        )
      }
      this.changed()
    })
  }

  private reset() {
    this.servers.clear()
    this.numbers.clear()
    this.ready.clear()
    for (const lang of [...this.problems.keys()]) this.forget(lang)
    this.changed()
  }

  // clients are the servers running, for asking all of them: Go to Symbol.
  clients(): LSPClient[] {
    return [...this.ready.values()]
  }

  // allProblems are what every server running has found, by file.
  allProblems(): Problem[] {
    return [...this.problems.values()].flatMap((files) => [...files.values()].flat())
  }

  // onProblems calls fn whenever a server says what it has found.
  onProblems(fn: () => void): () => void {
    this.problemListeners.add(fn)
    return () => this.problemListeners.delete(fn)
  }

  private forget(lang: string) {
    if (!this.problems.delete(lang)) return
    for (const fn of this.problemListeners) fn()
  }

  private publish(lang: string, uri: string, diagnostics: Diagnostic[]) {
    const abs = pathOf(uri)
    const path = abs.startsWith(this.code.root + '/') ? abs.slice(this.code.root.length + 1) : abs
    let files = this.problems.get(lang)
    if (!files) this.problems.set(lang, (files = new Map()))
    if (!diagnostics.length) files.delete(uri)
    else {
      files.set(
        uri,
        diagnostics.map((d) => ({
          path,
          line: d.range.start.line + 1,
          col: d.range.start.character + 1,
          severity: d.severity ?? 1,
          message: d.message,
          source: d.source,
          code: d.code === undefined ? undefined : String(d.code),
        })),
      )
    }
    for (const fn of this.problemListeners) fn()
  }

  private changed() {
    for (const fn of this.listeners) fn()
  }

  // restart restarts a language's server, for every tab.
  async restart(language: string) {
    await this.code.restartLanguageServer(language)
  }

  // retry forgets that a language had no server, or that it stopped, and
  // has editors start it again: after installing one, say.
  retry(language: string) {
    this.servers.delete(language)
    this.changed()
  }

  info(language: string) {
    return this.code.languageServerInfo(language)
  }

  // openFile and run reach the Code tab, for a server's panel: its
  // settings, and installing one.
  openFile(path: string) {
    this.opener.open(path)
  }

  run(command: string) {
    this.opener.run(command)
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
    } catch {
      // The connection dropped, as it does while the environment restarts:
      // once it is back, editors attach again and the server is started.
      this.servers.delete(language)
      this.code.connected().then(
        () => this.changed(),
        () => {},
      )
      return { state: 'starting', language }
    }
    if ('missing' in res) return { state: 'missing', language, why: res.missing, install: res.install ?? [] }
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
    let last = { message: '', at: 0 }
    const client = new LSPClient({
      rootUri: fileUri(res.root),
      timeout: 15_000,
      // A server's information ("Finished loading packages") is not shown;
      // its warnings and errors are, each once: gopls sends the same one
      // for each folder it loads, moments apart.
      notificationHandlers: {
        'window/showMessage': (_c, p: { type?: number; message?: string }) => {
          if ((p?.type ?? 1) >= 3) return true
          const now = Date.now()
          if (p.message === last.message && now - last.at < 10_000) return true
          last = { message: p.message ?? '', at: now }
          return false
        },
        // Every file's problems are kept for the Problems window; an open
        // file's are shown in its editor as well, by the package.
        'textDocument/publishDiagnostics': (_c, p: { uri: string; diagnostics: Diagnostic[] }) => {
          this.publish(language, p.uri, p.diagnostics ?? [])
          return false
        },
      },
      workspace: (c) => (workspace = new HangarWorkspace(c, this.code, this.opener)),
      extensions: [
        ...languageServerExtensions(),
        ideaKeys(this.commands),
        // The Structure window shows symbols inside those that hold them,
        // and Go to Symbol searches the workspace.
        {
          clientCapabilities: {
            textDocument: { documentSymbol: { hierarchicalDocumentSymbolSupport: true } },
            workspace: { symbol: {} },
          },
        },
      ],
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
    this.ready.set(language, client)
    return { state: 'ready', language, command: res.command, client }
  }

  // extension is what an editor of a file adds to talk to its server.
  static extension(s: ServerState, abs: string, id: string): Extension {
    return s.state === 'ready' ? s.client.plugin(fileUri(abs), id) : []
  }
}

// LanguagesContext gives editors their tab's language servers.
export const LanguagesContext = createContext<Languages | null>(null)
