// The Hangar desktop app shows this UI in its tabs and adds a bridge for
// what a browser cannot do: open an environment in desktop VS Code or in a
// terminal. In a browser there is no bridge and none of it shows.

interface DesktopBridge {
  version: number
  // tab is what the page's tab is for, where the app says: the server's
  // control panel, or one environment.
  tab?: 'panel' | 'environment'
  openInVSCode(env: string): Promise<void>
  openTerminal(env: string): Promise<string | null>
}

declare global {
  interface Window {
    hangarDesktop?: DesktopBridge
  }
}

// desktop is the bridge, when this page is in the desktop app and speaks a
// version of it this UI knows.
export const desktop: DesktopBridge | null =
  typeof window !== 'undefined' && window.hangarDesktop && window.hangarDesktop.version >= 1
    ? window.hangarDesktop
    : null
