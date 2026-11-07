// What every kind of shared document is to the editors that show it. The
// Code service says which kind it holds files as (hello's docs): collab
// documents (collab.ts) from the Rust service, Yjs ones (yjs.ts) from the
// Node service on the editor disk.

import type { Extension } from '@codemirror/state'
import type { Command } from '@codemirror/view'
import { useEffect, useState } from 'react'

export type FileUser = { name: string; color: string }

// DocTransport is how a document reaches the service: requests, and the
// Yjs channel's messages.
export interface DocTransport {
  call(method: string, params: Record<string, unknown>): Promise<unknown>
  sendYjs(n: number, msg: Uint8Array): void
}

// SharedFile is one file open for editing, shared with everyone who has it
// open.
export interface SharedFile {
  readonly path: string
  // writable is whether the environment's user may write the file.
  readonly writable: boolean
  whenSynced(): Promise<void>
  // text is what the file holds now, as this tab has it.
  text(): string
  // onChange is told whenever the text changes, from anywhere.
  onChange(fn: () => void): () => void
  // binding is what an editor of the file needs: its text kept with
  // everyone's, its own undo, and the others' carets and selections.
  binding(user: FileUser): Extension
  undo: Command
  redo: Command
}

// useSharedText is a shared file's text, kept up with its edits, at most
// every tenth of a second.
export function useSharedText(file: SharedFile): string {
  const [value, setValue] = useState(() => file.text())
  useEffect(() => {
    let timer: ReturnType<typeof setTimeout> | undefined
    setValue(file.text())
    const off = file.onChange(() => {
      if (timer) return
      timer = setTimeout(() => {
        timer = undefined
        setValue(file.text())
      }, 100)
    })
    return () => {
      clearTimeout(timer)
      off()
    }
  }, [file])
  return value
}
