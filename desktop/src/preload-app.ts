// The bridge for the app's own pages -- a window's tab bar and the server
// picker -- to the main process. Nothing else is exposed to them.
import { contextBridge, ipcRenderer } from 'electron'

contextBridge.exposeInMainWorld('hangar', {
  platform: process.platform,
  get: () => ipcRenderer.invoke('app:get'),
  onState: (fn: (s: unknown) => void) => ipcRenderer.on('app:state', (_, s) => fn(s)),
  activate: (id: number) => ipcRenderer.invoke('tab:activate', id),
  close: (id: number) => ipcRenderer.invoke('tab:close', id),
  move: (id: number, to: number) => ipcRenderer.invoke('tab:move', id, to),
  overlay: (open: boolean) => ipcRenderer.invoke('overlay', open),
  onOverlay: (fn: () => void) => ipcRenderer.on('overlay:open', () => fn()),
  addServer: (url: string) => ipcRenderer.invoke('server:add', url),
  removeServer: (id: string) => ipcRenderer.invoke('server:remove', id),
  openServer: (id: string) => ipcRenderer.invoke('server:open', id),
  openEnv: (server: string, env: string) => ipcRenderer.invoke('env:open', server, env),
  act: (server: string, env: string, action: string) => ipcRenderer.invoke('env:act', server, env, action),
  vscode: (server: string, env: string) => ipcRenderer.invoke('env:vscode', server, env),
  terminal: (server: string, env: string) => ipcRenderer.invoke('env:terminal', server, env),
})
