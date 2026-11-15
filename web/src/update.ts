import { useSyncExternalStore } from 'react'

// The UI's build, as the server that served this page names it in the page
// (an empty one is Vite's dev server, which serves the UI as it changes).
// Every API answer names the build the server serves; once one names
// another, this page is older than its server, and the files it would load
// next -- a part of the UI only loaded when first used -- may be gone.
const mine = document.querySelector<HTMLMetaElement>('meta[name="hangar-ui"]')?.content ?? ''
let newer = false
const listeners = new Set<() => void>()

// served notes the build an answer named.
export function served(build: string | null) {
  if (newer || !mine || !build || build === mine) return
  newer = true
  for (const l of listeners) l()
}

function subscribe(l: () => void) {
  listeners.add(l)
  return () => listeners.delete(l)
}

// useUpdated says whether the server serves a newer UI than this page.
export function useUpdated() {
  return useSyncExternalStore(
    subscribe,
    () => newer,
    () => false,
  )
}

const reloadedKey = 'hangar.reloaded-for-update'

// reloadForUpdate reloads the page when a part of the UI could not be
// loaded, which after an upgrade is its file gone from the server, unless it
// did so a moment ago: a part that cannot be loaded from a fresh page is not
// missing for that reason, and the notice says to reload instead.
export function reloadForUpdate() {
  let last = 0
  try {
    last = Number(sessionStorage.getItem(reloadedKey) ?? 0)
    sessionStorage.setItem(reloadedKey, String(Date.now()))
  } catch {
    // A browser that keeps nothing reloads each time.
  }
  if (Date.now() - last < 30_000) {
    newer = true
    for (const l of listeners) l()
    return
  }
  location.reload()
}
