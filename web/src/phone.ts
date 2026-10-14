import { useSyncExternalStore } from 'react'

// A window this narrow is a phone's, and gets the phone's layout: a bar of
// tabs at the foot in place of the sidebar, lists in place of tables, and an
// environment as one screen leading to each way into it. The stylesheet's
// breakpoint is the same.
const narrow = window.matchMedia('(max-width: 760px)')

function subscribe(fn: () => void) {
  narrow.addEventListener('change', fn)
  return () => narrow.removeEventListener('change', fn)
}

// usePhone is whether the window is a phone's.
export function usePhone(): boolean {
  return useSyncExternalStore(subscribe, () => narrow.matches)
}

// installable is whether this is Safari on an iPhone or iPad with Hangar not
// yet on the home screen: there, it is added from the Share menu, which no
// page can open, so the page says how.
export function installable(): boolean {
  const nav = navigator as Navigator & { standalone?: boolean }
  return nav.standalone === false && !window.matchMedia('(display-mode: standalone)').matches
}
