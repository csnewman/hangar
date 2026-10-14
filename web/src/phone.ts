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

// On a phone the on-screen keyboard covers the bottom of the page rather
// than shrinking it, and Safari scrolls the page to keep the field typed in
// in sight. The phone's layout is fitted to what the keyboard leaves
// instead, the visual viewport, as --phone-top and --phone-height, and
// data-keyboard is set on the page while the keyboard is up.
const vv = window.visualViewport
const keyboardWatchers = new Set<() => void>()
if (vv) {
  const fit = () => {
    // Zoomed in, the page is as it was.
    if (Math.abs(vv.scale - 1) > 0.01) return
    const root = document.documentElement.style
    root.setProperty('--phone-top', `${vv.offsetTop}px`)
    root.setProperty('--phone-height', `${vv.height}px`)
    // Far shorter than the window: the keyboard, not a browser's bars.
    const up = window.innerHeight - vv.height > 150
    if (up === keyboardUp()) return
    if (up) document.documentElement.dataset.keyboard = ''
    else delete document.documentElement.dataset.keyboard
    keyboardWatchers.forEach((fn) => fn())
  }
  vv.addEventListener('resize', fit)
  vv.addEventListener('scroll', fit)
  fit()
}

function keyboardUp(): boolean {
  return document.documentElement.dataset.keyboard !== undefined
}

// useKeyboard is whether the phone's on-screen keyboard is up.
export function useKeyboard(): boolean {
  return useSyncExternalStore((fn) => {
    keyboardWatchers.add(fn)
    return () => {
      keyboardWatchers.delete(fn)
    }
  }, keyboardUp)
}
