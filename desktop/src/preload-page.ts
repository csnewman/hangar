// What the app adds to a server's own web UI, shown in a tab.
//
// It routes links: a click on a link to a page that belongs in another tab
// -- an environment from the control panel, another environment, or the
// control panel from an environment -- is caught before the web UI follows
// it, and the app shows that page in its own tab. A link to this tab's own
// pages is left to the web UI.
//
// And it offers the web UI a bridge: opening one of this server's
// environments in desktop VS Code or a terminal. The app decides which
// server by the tab, never by what the page says.
import { contextBridge, ipcRenderer } from 'electron'

// role is what this tab is for: the control panel, or one environment.
const role = ipcRenderer.sendSync('page:role') as { kind: 'panel' } | { kind: 'env'; env: string } | null

// tabOf is the environment a path belongs to, or null for the control panel.
function tabOf(path: string): string | null {
  const m = /^\/environments\/([^/]+)/.exec(path)
  return m && m[1] !== 'new' ? decodeURIComponent(m[1]) : null
}

window.addEventListener(
  'click',
  (e) => {
    // A click that opens elsewhere -- a new tab or window -- is the app's to
    // route already; one the page has handled is the page's.
    if (!role || e.defaultPrevented || e.button !== 0 || e.metaKey || e.ctrlKey || e.shiftKey || e.altKey) return
    const a = (e.target as Element | null)?.closest?.('a[href]') as HTMLAnchorElement | null
    if (!a || (a.target && a.target !== '_self') || a.hasAttribute('download')) return
    const url = new URL(a.href, location.href)
    if (url.origin !== location.origin) return
    const here = role.kind === 'env' ? role.env : null
    if (tabOf(url.pathname) === here) return
    e.preventDefault()
    e.stopPropagation()
    ipcRenderer.invoke('page:route', url.href)
  },
  true,
)

contextBridge.exposeInMainWorld('hangarDesktop', {
  // version is this bridge's: a page checks it before using what it adds.
  version: 1,
  // tab is what the page's tab is for: the server's control panel, or one
  // environment, whose tab has no use for the control panel's navigation.
  tab: role?.kind === 'env' ? 'environment' : 'panel',
  openInVSCode: (env: string) => ipcRenderer.invoke('page:vscode', env),
  openTerminal: (env: string) => ipcRenderer.invoke('page:terminal', env) as Promise<string | null>,
})
