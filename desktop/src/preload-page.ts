// What a server's own web UI, shown in a tab, may ask of the app: to open
// one of that server's environments in desktop VS Code or a terminal. The
// app decides which server by the tab, never by what the page says.
import { contextBridge, ipcRenderer } from 'electron'

contextBridge.exposeInMainWorld('hangarDesktop', {
  // version is this bridge's: a page checks it before using what it adds.
  version: 1,
  openInVSCode: (env: string) => ipcRenderer.invoke('page:vscode', env),
  openTerminal: (env: string) => ipcRenderer.invoke('page:terminal', env) as Promise<string | null>,
})
