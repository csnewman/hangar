import { useSyncExternalStore } from 'react'

// Where a tab dragged out of its own row of tabs would go: into a group of
// editors, at an index among its tabs or at its end, or into a group made
// to the right by splitting the editors.
export type DropTarget = { group: number; index?: number } | { split: true }

// TabDrag is a tab being dragged, the group it is from, and where it would
// go. Every group's tab bar and editors read it, to show where it would
// land.
export type TabDrag = { key: string; from: number; target: DropTarget | null }

let state: TabDrag | null = null
const listeners = new Set<() => void>()

export const tabDrag = {
  get: () => state,
  set(s: TabDrag | null) {
    if (s === state) return
    state = s
    for (const fn of listeners) fn()
  },
  subscribe(fn: () => void) {
    listeners.add(fn)
    return () => {
      listeners.delete(fn)
    }
  },
}

export function useTabDrag(): TabDrag | null {
  return useSyncExternalStore(tabDrag.subscribe, tabDrag.get)
}

// sameTarget says whether two places a tab would go are the same, so a
// pointer moving within one draws nothing again.
export function sameTarget(a: DropTarget | null, b: DropTarget | null): boolean {
  if (!a || !b) return a === b
  if ('split' in a || 'split' in b) return 'split' in a && 'split' in b
  return a.group === b.group && a.index === b.index
}
