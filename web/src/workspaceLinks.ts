import { desktop } from './desktop'
import { usePhone } from './phone'

// In a browser on a computer each environment's workspace is a browser tab
// of its own, and Hangar's own pages another, each named so that a link to
// one goes to the tab already showing it rather than opening a second. The
// desktop app and a phone keep their own ways of moving between them.
const separate = () => !desktop

export const homeTab = 'hangar-home'

export const environmentTab = (id: string) => `hangar-env-${id}`

// useEnvironmentTarget gives the target of a link to an environment's
// workspace: its own tab, where there is one.
export function useEnvironmentTarget(): (id: string) => string | undefined {
  const phone = usePhone()
  return (id) => (separate() && !phone ? environmentTab(id) : undefined)
}

// nameTab names this browser tab, so links to what it shows come back
// to it.
export function nameTab(name: string) {
  if (separate()) window.name = name
}

// otherTab is the target of a link from a workspace to another tab: the
// one named, or in the desktop app a new one, which the app moves to the
// tab already showing it.
export const otherTab = (name: string) => (separate() ? name : '_blank')
