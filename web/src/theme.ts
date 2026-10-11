// The UI's colours. A person chooses their system's, or always light, or
// always dark (kept on the server, so it follows them); the choice is
// resolved to light or dark and set on the page as data-theme, which the
// stylesheets follow, and told to the parts that colour themselves -- the
// editors and terminals. It is kept in this browser too, and set by
// index.html before anything is drawn, so a page opens in it rather than
// flashing the other.

export type Theme = 'system' | 'light' | 'dark'

const key = 'hangar.theme'

function stored(): Theme {
  try {
    const t = localStorage.getItem(key)
    if (t === 'light' || t === 'dark' || t === 'system') return t
  } catch {
    // Remembering it here is a convenience; the server has it.
  }
  return 'system'
}

let choice: Theme = stored()
const system = window.matchMedia('(prefers-color-scheme: dark)')
const listeners = new Set<(dark: boolean) => void>()
const choiceListeners = new Set<(t: Theme) => void>()

// isDark is whether the page is dark now.
export function isDark(): boolean {
  return choice === 'dark' || (choice === 'system' && system.matches)
}

let shown: boolean | null = null

function apply() {
  const dark = isDark()
  document.documentElement.dataset.theme = dark ? 'dark' : 'light'
  if (dark === shown) return
  shown = dark
  for (const fn of listeners) fn(dark)
}

// themeChoice is the person's choice, as it stands.
export function themeChoice(): Theme {
  return choice
}

// setTheme makes a choice the page's, and this browser's.
export function setTheme(t: Theme) {
  choice = t
  try {
    localStorage.setItem(key, t)
  } catch {
    // As above.
  }
  apply()
  for (const fn of choiceListeners) fn(t)
}

// onThemeChoice calls fn whenever the choice changes, here or in another
// tab.
export function onThemeChoice(fn: (t: Theme) => void): () => void {
  choiceListeners.add(fn)
  return () => choiceListeners.delete(fn)
}

// onDarkChange calls fn whenever the page turns light or dark: a new choice,
// or the system's changing while it is followed.
export function onDarkChange(fn: (dark: boolean) => void): () => void {
  listeners.add(fn)
  return () => listeners.delete(fn)
}

system.addEventListener('change', () => {
  if (choice === 'system') apply()
})
// A choice made in another tab of this browser is this one's at once.
window.addEventListener('storage', (e) => {
  if (e.key !== key) return
  choice = stored()
  apply()
  for (const fn of choiceListeners) fn(choice)
})
apply()
