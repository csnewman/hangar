import { LSPPlugin } from '@codemirror/lsp-client'
import type { Command, EditorView } from '@codemirror/view'

// Popups are what an editor shows over itself, as IntelliJ does, where
// CodeMirror would put a panel: Go to Line, and renaming a symbol. The
// editor registers how it opens them; the commands, bound to keys, open
// them.
type Popups = {
  gotoLine: () => void
  rename: (from: number, to: number) => void
}

const popups = new WeakMap<EditorView, Popups>()

export function registerPopups(view: EditorView, p: Popups) {
  popups.set(view, p)
}

export const openGotoLine: Command = (view) => {
  const p = popups.get(view)
  if (!p) return false
  p.gotoLine()
  return true
}

// startRename renames the symbol at the caret, if its language server can.
export const startRename: Command = (view) => {
  const word = view.state.wordAt(view.state.selection.main.head)
  const plugin = LSPPlugin.get(view)
  const p = popups.get(view)
  if (!word || !plugin || !p || plugin.client.serverCapabilities?.renameProvider === false) return false
  p.rename(word.from, word.to)
  return true
}

type TextEdit = {
  range: { start: { line: number; character: number }; end: { line: number; character: number } }
  newText: string
}
type WorkspaceEdit = {
  changes?: Record<string, TextEdit[]>
  documentChanges?: { textDocument?: { uri: string }; edits?: TextEdit[] }[]
}

// renameAt asks the language server to rename the symbol at pos, and makes
// the changes it answers with, in this file and any other it has open.
export function renameAt(view: EditorView, pos: number, newName: string): Promise<void> {
  const plugin = LSPPlugin.get(view)
  if (!plugin) return Promise.reject(new Error('No language server knows this file'))
  plugin.client.sync()
  return plugin.client.withMapping((mapping) =>
    plugin.client
      .request<unknown, WorkspaceEdit | null>('textDocument/rename', {
        newName,
        position: plugin.toPosition(pos),
        textDocument: { uri: plugin.uri },
      })
      .then((edit) => {
        if (!edit) throw new Error('The language server found nothing to rename here')
        const byFile = new Map<string, TextEdit[]>(Object.entries(edit.changes ?? {}))
        for (const d of edit.documentChanges ?? []) {
          if (d.textDocument && d.edits)
            byFile.set(d.textDocument.uri, [...(byFile.get(d.textDocument.uri) ?? []), ...d.edits])
        }
        for (const [uri, edits] of byFile) {
          if (!edits.length || !plugin.client.workspace.getFile(uri)) continue
          plugin.client.workspace.updateFile(uri, {
            changes: edits.map((e) => ({
              from: mapping.mapPosition(uri, e.range.start),
              to: mapping.mapPosition(uri, e.range.end),
              insert: e.newText,
            })),
            userEvent: 'rename',
          })
        }
      }),
  )
}
