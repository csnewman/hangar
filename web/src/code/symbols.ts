import { syntaxTree } from '@codemirror/language'
import { LSPPlugin, type LSPClient } from '@codemirror/lsp-client'
import type { EditorView } from '@codemirror/view'

// A symbol in a file, as the Structure window and Go to Symbol show it: its
// name, what the server adds (a signature, a type), its kind, where it
// is (lines from 1), and what is inside it.
export type Sym = {
  name: string
  detail?: string
  kind: number
  line: number
  col: number
  endLine: number
  children: Sym[]
}

// LSP's symbol kinds, by the badge IntelliJ shows for each: a letter and
// the colour of its kind of thing.
const kinds: Record<number, [string, string, string]> = {
  1: ['File', 'F', 'file'],
  2: ['Module', 'M', 'ns'],
  3: ['Namespace', 'N', 'ns'],
  4: ['Package', 'P', 'ns'],
  5: ['Class', 'C', 'type'],
  6: ['Method', 'm', 'fn'],
  7: ['Property', 'p', 'field'],
  8: ['Field', 'f', 'field'],
  9: ['Constructor', 'c', 'fn'],
  10: ['Enum', 'E', 'type'],
  11: ['Interface', 'I', 'type'],
  12: ['Function', 'f', 'fn'],
  13: ['Variable', 'v', 'var'],
  14: ['Constant', 'k', 'var'],
  15: ['String', 's', 'var'],
  16: ['Number', 'n', 'var'],
  17: ['Boolean', 'b', 'var'],
  18: ['Array', 'a', 'var'],
  19: ['Object', 'o', 'var'],
  20: ['Key', 'k', 'field'],
  21: ['Null', '∅', 'var'],
  22: ['Enum Member', 'e', 'field'],
  23: ['Struct', 'S', 'type'],
  24: ['Event', 'e', 'fn'],
  25: ['Operator', 'o', 'fn'],
  26: ['Type Parameter', 'T', 'type'],
}

// A Markdown heading, which is not one of LSP's kinds.
export const headingKind = 1000

export function kindOf(kind: number): { name: string; letter: string; tone: string } {
  if (kind === headingKind) return { name: 'Heading', letter: '#', tone: 'ns' }
  const [name, letter, tone] = kinds[kind] ?? ['Symbol', '?', 'var']
  return { name, letter, tone }
}

type Range = { start: { line: number; character: number }; end: { line: number; character: number } }
type DocumentSymbol = {
  name: string
  detail?: string
  kind: number
  range: Range
  selectionRange: Range
  children?: DocumentSymbol[]
}
type SymbolInformation = { name: string; kind: number; containerName?: string; location: { uri: string; range: Range } }

const fromDocumentSymbol = (s: DocumentSymbol): Sym => ({
  name: s.name,
  detail: s.detail || undefined,
  kind: s.kind,
  line: s.selectionRange.start.line + 1,
  col: s.selectionRange.start.character + 1,
  endLine: s.range.end.line + 1,
  children: (s.children ?? []).map(fromDocumentSymbol).sort((a, b) => a.line - b.line),
})

// nest makes a server's flat list of symbols a tree, each inside the
// innermost symbol whose range holds it.
function nest(list: SymbolInformation[]): Sym[] {
  const flat = list
    .map((s) => ({
      name: s.name,
      kind: s.kind,
      line: s.location.range.start.line + 1,
      col: s.location.range.start.character + 1,
      endLine: s.location.range.end.line + 1,
      children: [] as Sym[],
    }))
    .sort((a, b) => a.line - b.line || b.endLine - a.endLine)
  const roots: Sym[] = []
  const open: Sym[] = []
  for (const s of flat) {
    while (open.length && open[open.length - 1].endLine < s.line) open.pop()
    const parent = open[open.length - 1]
    if (parent && s.endLine <= parent.endLine && parent !== s) parent.children.push(s)
    else roots.push(s)
    open.push(s)
  }
  return roots
}

// headings are a Markdown file's outline: its headings, each holding those
// below it of a lower level.
function headings(view: EditorView): Sym[] {
  const roots: Sym[] = []
  const open: { level: number; sym: Sym }[] = []
  const doc = view.state.doc
  syntaxTree(view.state).iterate({
    enter: (n) => {
      const m = /^(?:ATXHeading|SetextHeading)(\d)$/.exec(n.name)
      if (!m) return
      const level = Number(m[1])
      const line = doc.lineAt(n.from)
      const name = view.state
        .sliceDoc(n.from, n.to)
        .replace(/^#+\s*/, '')
        .replace(/\s*#*\s*$/, '')
        .split('\n')[0]
      const sym: Sym = { name, kind: headingKind, line: line.number, col: 1, endLine: doc.lines, children: [] }
      while (open.length && open[open.length - 1].level >= level) {
        open.pop()!.sym.endLine = line.number - 1
      }
      if (open.length) open[open.length - 1].sym.children.push(sym)
      else roots.push(sym)
      open.push({ level, sym })
      return false
    },
  })
  return roots
}

// Outline is what a file's structure is read from: its server, or its
// headings, or nothing that can say.
export type Outline = { symbols: Sym[]; source: 'server' | 'headings' } | { symbols: null; why: string }

// outline reads the symbols of the file in a view: from its language
// server if it has one that lists them, else a Markdown file's headings.
export async function outline(view: EditorView, markdown: boolean): Promise<Outline> {
  const plugin = LSPPlugin.get(view)
  // A server just started says what it can do once it has initialised.
  if (plugin && !plugin.client.serverCapabilities) await plugin.client.initializing.catch(() => null)
  if (plugin?.client.serverCapabilities?.documentSymbolProvider) {
    plugin.client.sync()
    const res = await plugin.client.request<unknown, DocumentSymbol[] | SymbolInformation[] | null>(
      'textDocument/documentSymbol',
      { textDocument: { uri: plugin.uri } },
    )
    const list = res ?? []
    const symbols =
      list.length && 'location' in list[0]
        ? nest(list as SymbolInformation[])
        : (list as DocumentSymbol[]).map(fromDocumentSymbol).sort((a, b) => a.line - b.line)
    return { symbols, source: 'server' }
  }
  if (markdown) return { symbols: headings(view), source: 'headings' }
  return {
    symbols: null,
    why: plugin ? 'Its language server does not list symbols.' : 'No language server knows this file’s language.',
  }
}

// innermost is the deepest symbol holding a line, for marking where the
// caret is.
export function innermost(symbols: Sym[], line: number): Sym | null {
  for (const s of symbols) {
    if (line >= s.line && line <= s.endLine) return innermost(s.children, line) ?? s
  }
  return null
}

// A symbol found anywhere in the workspace: Go to Symbol's.
export type FoundSymbol = { name: string; kind: number; container?: string; uri: string; line: number; col: number }

type WorkspaceSymbol = {
  name: string
  kind: number
  containerName?: string
  location: { uri: string; range?: Range }
}

// workspaceSymbols asks each server that answers for the symbols whose
// names match a query.
export async function workspaceSymbols(clients: LSPClient[], query: string): Promise<FoundSymbol[]> {
  await Promise.all(clients.map((c) => c.initializing.catch(() => null)))
  const answers = await Promise.all(
    clients
      .filter((c) => c.serverCapabilities?.workspaceSymbolProvider)
      .map((c) =>
        c.request<unknown, WorkspaceSymbol[] | null>('workspace/symbol', { query }).then(
          (r) => r ?? [],
          () => [] as WorkspaceSymbol[],
        ),
      ),
  )
  return answers.flat().map((s) => ({
    name: s.name,
    kind: s.kind,
    container: s.containerName || undefined,
    uri: s.location.uri,
    line: (s.location.range?.start.line ?? 0) + 1,
    col: (s.location.range?.start.character ?? 0) + 1,
  }))
}
