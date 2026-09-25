// Hangar for the desktop.
//
// The app is two things. Its own parts -- the tab bar, the home page across
// servers, the environment switcher, the menu bar icon, notifications,
// opening VS Code or a terminal -- are built into the app and talk to each
// server through the client API (/api/v1), which is versioned so an app
// and a server of different releases still agree. Each server's control
// panel is the server's own web UI, shown in a tab, so it is always the
// panel that server ships.

import { app, dialog, ipcMain, Menu, nativeImage, Notification, Tray, type IpcMainInvokeEvent } from 'electron'
import { join } from 'node:path'

import * as api from './api'
import { openInVSCode, openTerminal } from './launch'
import * as store from './store'
import { watch, type ServerState } from './watch'
import { AppWindow, homeURL } from './window'

let win: AppWindow | null = null
let tray: Tray | null = null
let quitting = false

// ---- What the app's own pages are shown ----

function summary() {
  return {
    tabs: win?.info() ?? [],
    servers: watch.all().map((st) => ({
      id: st.server.id,
      name: st.server.name,
      url: st.server.url,
      compatibility: st.compatibility,
      signedIn: st.signedIn,
      username: st.me?.display_name || st.me?.username,
      ssh: !!st.me?.ssh,
      error: st.error,
      environments: st.environments.map((e) => ({ ...e, own: e.owner_id === st.me?.id })),
    })),
  }
}

let pending: NodeJS.Timeout | undefined
// changed tells the app's pages and the menu bar icon, once per burst.
function changed() {
  clearTimeout(pending)
  pending = setTimeout(() => {
    const s = summary()
    win?.bar.webContents.send('app:state', s)
    for (const t of win?.tabs ?? []) {
      if (t.server === null) t.view.webContents.send('app:state', s)
    }
    updateTray()
  }, 30)
}

// ---- The window ----

function createWindow() {
  win = new AppWindow(changed)
  win.restore()
  win.win.on('closed', () => {
    win = null
  })
  win.win.on('close', (e) => {
    // On macOS closing the window leaves the app in the menu bar, as Mail
    // and Slack do; quitting is ⌘Q.
    if (process.platform === 'darwin' && !quitting) {
      e.preventDefault()
      win?.win.hide()
    }
  })
}

function showWindow() {
  if (!win) createWindow()
  win!.win.show()
  win!.win.focus()
  return win!
}

// ---- Calls from the app's own pages ----

// own refuses an app call from anything but the app's own pages.
function own(e: IpcMainInvokeEvent) {
  if (!win?.isOwn(e.sender)) throw new Error('not allowed')
}

function env(serverId: string, envId: string) {
  const st = watch.all().find((s) => s.server.id === serverId)
  const e = st?.environments.find((x) => x.id === envId)
  if (!st || !e) throw new Error('no such environment')
  return { st, e }
}

ipcMain.handle('app:get', (e) => {
  own(e)
  return summary()
})
ipcMain.handle('tab:activate', (e, id: number) => {
  own(e)
  const t = win?.tabs.find((x) => x.id === id)
  if (t) win!.activate(t)
})
ipcMain.handle('tab:close', (e, id: number) => {
  own(e)
  const t = win?.tabs.find((x) => x.id === id)
  if (t) win!.close(t)
})
ipcMain.handle('tab:move', (e, id: number, to: number) => {
  own(e)
  win?.move(id, to)
})
ipcMain.handle('tab:new', (e, serverId: string | null) => {
  own(e)
  const s = serverId ? store.server(serverId) : undefined
  win?.open(s ?? null, s ? s.url : homeURL)
})
ipcMain.handle('overlay', (e, open: boolean) => {
  own(e)
  win?.setOverlay(open)
})
ipcMain.handle('server:add', async (e, input: string) => {
  own(e)
  let url: string
  try {
    const u = new URL(/^https?:\/\//i.test(input.trim()) ? input.trim() : `https://${input.trim()}`)
    url = u.origin
  } catch {
    return { error: 'That is not an address.' }
  }
  const c = await api.checkServer(url)
  if (!c.ok && c.update !== 'server') return { error: `${new URL(url).host} ${c.why}` }
  const s = store.addServer(url, new URL(url).host)
  win?.open(s, s.url)
  await watch.refresh()
  return { ok: true, warning: c.ok ? undefined : `${new URL(url).host} ${c.why}` }
})
ipcMain.handle('server:remove', (e, id: string) => {
  own(e)
  for (const t of [...(win?.tabs ?? [])]) if (t.server?.id === id) win!.close(t)
  store.removeServer(id)
  watch.refresh()
})
ipcMain.handle('server:rename', (e, id: string, name: string) => {
  own(e)
  if (name.trim()) store.renameServer(id, name.trim())
  changed()
})
ipcMain.handle('server:open', (e, id: string) => {
  own(e)
  const s = store.server(id)
  if (s) win?.open(s, s.url)
})
ipcMain.handle('env:open', (e, serverId: string, envId: string) => {
  own(e)
  const { st } = env(serverId, envId)
  win?.openEnvironment(st.server, envId)
})
ipcMain.handle('env:act', async (e, serverId: string, envId: string, action: 'start' | 'stop' | 'suspend') => {
  own(e)
  const { st } = env(serverId, envId)
  await api.act(st.server, envId, action)
  await watch.refresh()
})
ipcMain.handle('env:vscode', (e, serverId: string, envId: string) => {
  own(e)
  const { st, e: x } = env(serverId, envId)
  return openInVSCode(st.server, x)
})
ipcMain.handle('env:terminal', (e, serverId: string, envId: string) => {
  own(e)
  const { st, e: x } = env(serverId, envId)
  return st.me ? openTerminal(st.me, x) : 'not signed in'
})

// ---- Calls from a server's own pages ----
//
// A server's web UI, shown in a tab, may ask the app to open one of that
// server's environments elsewhere. The server is the tab's, never one the
// page names.

function pageEnv(e: IpcMainInvokeEvent, envId: string) {
  const tab = win?.tabFor(e.sender)
  if (!tab?.server) throw new Error('not allowed')
  return env(tab.server.id, envId)
}

ipcMain.handle('page:vscode', async (e, envId: string) => {
  const { st, e: x } = pageEnv(e, envId)
  await openInVSCode(st.server, x)
})
ipcMain.handle('page:terminal', (e, envId: string) => {
  const { st, e: x } = pageEnv(e, envId)
  return st.me ? openTerminal(st.me, x) : 'not signed in'
})

// ---- Notifications ----

watch.on('phase', (st: ServerState, e: api.Environment, was: api.Environment) => {
  // Only the user's own environments: an administrator sees everyone's.
  if (e.owner_id !== st.me?.id || !Notification.isSupported()) return
  let n: Notification | null = null
  if (e.phase === 'running' && (was.phase === 'starting' || was.phase === 'pending')) {
    n = new Notification({ title: `${e.name} is ready`, body: `On ${st.server.name}` })
  } else if (e.phase === 'failed') {
    n = new Notification({ title: `${e.name} failed`, body: e.reason ?? `On ${st.server.name}` })
  }
  n?.on('click', () => showWindow().openEnvironment(st.server, e.id))
  n?.show()
})
watch.on('change', changed)

// ---- The menu bar icon ----

function trayIcon() {
  const file = process.platform === 'darwin' ? 'trayTemplate.png' : 'tray.png'
  const img = nativeImage.createFromPath(join(__dirname, 'assets', file))
  if (process.platform === 'darwin') img.setTemplateImage(true)
  return img
}

function updateTray() {
  if (!tray) return
  const items: Electron.MenuItemConstructorOptions[] = [{ label: 'Show Hangar', click: () => showWindow() }]
  for (const st of watch.all()) {
    items.push({ type: 'separator' }, { label: st.server.name, enabled: false })
    if (!st.compatibility?.ok && st.compatibility) {
      items.push({ label: `  ${st.compatibility.why}`, enabled: false })
      continue
    }
    if (!st.signedIn) {
      items.push({ label: '  Sign in…', click: () => showWindow().open(st.server, st.server.url) })
      continue
    }
    const mine = st.environments.filter((e) => e.owner_id === st.me?.id)
    if (mine.length === 0) items.push({ label: '  No environments', enabled: false })
    for (const e of mine) {
      const running = e.phase === 'running'
      const busy = !['running', 'stopped', 'suspended', 'failed'].includes(e.phase)
      items.push({
        label: `${e.name} — ${e.phase}`,
        submenu: [
          { label: 'Open', click: () => showWindow().openEnvironment(st.server, e.id) },
          { label: 'Open in VS Code', enabled: running, click: () => openInVSCode(st.server, e) },
          { label: 'Open Terminal', enabled: running && !!st.me?.ssh, click: () => openTerminal(st.me!, e) },
          { type: 'separator' },
          {
            label: e.phase === 'suspended' ? 'Resume' : 'Start',
            enabled: !running && !busy,
            click: () => api.act(st.server, e.id, 'start').then(() => watch.refresh()),
          },
          {
            label: 'Suspend',
            enabled: running,
            click: () => api.act(st.server, e.id, 'suspend').then(() => watch.refresh()),
          },
          {
            label: 'Stop',
            enabled: running || e.phase === 'suspended' || e.phase === 'starting',
            click: () => api.act(st.server, e.id, 'stop').then(() => watch.refresh()),
          },
        ],
      })
    }
  }
  items.push({ type: 'separator' }, { label: 'Quit Hangar', role: 'quit' })
  tray.setContextMenu(Menu.buildFromTemplate(items))
}

// ---- The application menu ----

function menu() {
  const mac = process.platform === 'darwin'
  const w = () => showWindow()
  const template: Electron.MenuItemConstructorOptions[] = [
    ...(mac ? [{ role: 'appMenu' as const }] : []),
    {
      label: 'File',
      submenu: [
        { label: 'New Tab', accelerator: 'CmdOrCtrl+T', click: () => w().open(null, homeURL) },
        { label: 'Go to Environment…', accelerator: 'CmdOrCtrl+L', click: () => w().setOverlay(true, true) },
        { label: 'Reopen Closed Tab', accelerator: 'CmdOrCtrl+Shift+T', click: () => w().reopen() },
        { type: 'separator' },
        { label: 'Close Tab', accelerator: 'CmdOrCtrl+W', click: () => win?.active && win.close(win.active) },
        ...(mac ? [] : [{ type: 'separator' as const }, { role: 'quit' as const }]),
      ],
    },
    { role: 'editMenu' },
    {
      label: 'View',
      submenu: [
        { label: 'Reload Tab', accelerator: 'CmdOrCtrl+R', click: () => win?.active?.view.webContents.reload() },
        {
          label: 'Developer Tools for Tab',
          accelerator: mac ? 'Alt+Cmd+I' : 'Ctrl+Shift+I',
          click: () => win?.active?.view.webContents.toggleDevTools(),
        },
        { type: 'separator' },
        { role: 'resetZoom' },
        { role: 'zoomIn' },
        { role: 'zoomOut' },
        { type: 'separator' },
        { role: 'togglefullscreen' },
      ],
    },
    {
      label: 'Window',
      submenu: [
        { label: 'Next Tab', accelerator: 'Ctrl+Tab', click: () => win?.cycle(1) },
        { label: 'Previous Tab', accelerator: 'Ctrl+Shift+Tab', click: () => win?.cycle(-1) },
        ...Array.from({ length: 9 }, (_, i) => ({
          label: `Tab ${i + 1}`,
          accelerator: `CmdOrCtrl+${i + 1}`,
          visible: false,
          click: () => {
            const t = i === 8 ? win?.tabs.at(-1) : win?.tabs[i]
            if (t) win!.activate(t)
          },
        })),
        { type: 'separator' },
        { role: 'minimize' },
        ...(mac ? [{ role: 'front' as const }] : []),
      ],
    },
  ]
  Menu.setApplicationMenu(Menu.buildFromTemplate(template))
}

// ---- Links: hangar://open?server=<origin>&path=/environments/<id> ----

async function openLink(link: string) {
  let u: URL
  try {
    u = new URL(link)
  } catch {
    return
  }
  if (u.protocol !== 'hangar:' || u.hostname !== 'open') return
  let origin: string
  try {
    origin = new URL(u.searchParams.get('server') ?? '').origin
  } catch {
    return
  }
  const path = u.searchParams.get('path') ?? '/'
  let s = store.serverFor(origin)
  if (!s) {
    const { response } = await dialog.showMessageBox({
      type: 'question',
      message: `Add ${new URL(origin).host} to Hangar?`,
      detail: 'A link asked to open this server. Add it only if you trust it.',
      buttons: ['Add', 'Cancel'],
      defaultId: 0,
      cancelId: 1,
    })
    if (response !== 0) return
    s = store.addServer(origin, new URL(origin).host)
    watch.refresh()
  }
  showWindow().open(s, s.url + (path.startsWith('/') ? path : '/'))
}

// ---- Start ----

if (!app.requestSingleInstanceLock()) {
  app.quit()
} else {
  app.setAsDefaultProtocolClient('hangar')
  app.on('second-instance', (_, argv) => {
    const link = argv.find((a) => a.startsWith('hangar://'))
    if (link) openLink(link)
    else showWindow()
  })
  app.on('open-url', (e, link) => {
    e.preventDefault()
    if (app.isReady()) openLink(link)
    else app.once('ready', () => openLink(link))
  })
  app.on('before-quit', () => {
    quitting = true
    win?.save()
  })
  app.on('window-all-closed', () => {
    if (process.platform !== 'darwin') app.quit()
  })
  app.on('activate', () => showWindow())

  app.whenReady().then(() => {
    store.load()
    menu()
    createWindow()
    tray = new Tray(trayIcon())
    tray.setToolTip('Hangar')
    updateTray()
    watch.start()
    const link = process.argv.find((a) => a.startsWith('hangar://'))
    if (link) openLink(link)
  })
}
