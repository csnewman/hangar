import type { EditorView } from '@codemirror/view'
import { createContext, useSyncExternalStore } from 'react'

import type { Indent } from './indent'
import type { ServerState } from './lsp'

// EditorStatus is what the status bar says of the file being edited: where
// the caret is and how much is selected, the file's language, what knows
// its code, and whether it may be written.
export type EditorStatus = {
  path: string
  // root is the folder of the project the file is in: its connection's.
  root: string
  line: number
  col: number
  // Characters selected, and over how many lines.
  selected: number
  selectedLines: number
  language: string | null
  server: ServerState | null
  readOnly: boolean
  // goto asks for a line to go to.
  goto: () => void
  // view is the editor itself, for the Structure window; edits counts its
  // changes, so what follows the file knows to read it again.
  view: EditorView | null
  edits: number
  // indent is how the file is indented, and where that was learnt.
  indent: Indent | null
}

// EditorStatusStore holds the status of the editor shown. The editor writes
// it as the caret moves, and only what reads it is drawn again, not the
// workspace around it.
export class EditorStatusStore {
  private status: EditorStatus | null = null
  private listeners = new Set<() => void>()

  get = () => this.status

  subscribe = (fn: () => void) => {
    this.listeners.add(fn)
    return () => {
      this.listeners.delete(fn)
    }
  }

  // set changes what the editor of path says, or with null says it has
  // gone. An editor that has gone says nothing of the one after it.
  set(path: string, patch: Partial<EditorStatus> | null) {
    const was = this.status
    if (patch === null) {
      if (was?.path !== path) return
      this.status = null
    } else {
      this.status = {
        ...(was?.path === path
          ? was
          : {
              path,
              root: '',
              line: 1,
              col: 1,
              selected: 0,
              selectedLines: 0,
              language: null,
              server: null,
              readOnly: false,
              goto: () => {},
              view: null,
              edits: 0,
              indent: null,
            }),
        ...patch,
      }
    }
    for (const fn of this.listeners) fn()
  }
}

export const EditorStatusContext = createContext<EditorStatusStore | null>(null)

export function useEditorStatus(store: EditorStatusStore): EditorStatus | null {
  return useSyncExternalStore(store.subscribe, store.get)
}
