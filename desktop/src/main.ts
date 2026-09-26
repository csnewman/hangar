// Hangar for the desktop.
//
// Each window is one server: its control panel in the first tab and its
// environments in tabs of their own. Another server is another window, and
// ⌘N picks or adds one; a tab dragged out of a window makes another window
// of its server, and one dragged onto another window of it moves there.
//
// The app is two things. Its own parts -- the tab bar, the environment
// switcher, the server picker, the menu bar icon, notifications, opening VS
// Code or a terminal -- are built into the app and talk to each server
// through the client API (/api/v1), which is versioned so an app and a
// server of different releases still agree. What the tabs show is the
// server's own web UI, so it is always the panel that server ships.

import {
  app,
  BaseWindow,
  BrowserWindow,
  clipboard,
  shell,
  screen,
  dialog,
  ipcMain,
  Menu,
  nativeImage,
  Notification,
  Tray,
  type IpcMainEvent,
  type IpcMainInvokeEvent,
  type WebContents,
} from 'electron'
import { join } from 'node:path'

import * as api from './api'
import { openInVSCode, openTerminal } from './launch'
import * as store from './store'
import type { Server } from './store'
import { watch, type ServerState } from './watch'
import { barHeight, roleFor, ServerWindow, type Tab } from './window'

// windows are the open server windows, the most recently in front first.
let windows: ServerWindow[] = []
let picker: BrowserWindow | null = null
let tray: Tray | null = null
let quitting = false

// ---- What the app's own pages are shown ----

function serverInfo(st: ServerState) {
  return {
    id: st.server.id,
    name: st.server.name,
    url: st.server.url,
    compatibility: st.compatibility,
    signedIn: st.signedIn,
    username: st.me?.display_name || st.me?.username,
    ssh: !!st.me?.ssh,
    error: st.error,
    open: windows.some((w) => w.server.id === st.server.id),
    environments: st.environments.map((e) => ({ ...e, own: e.owner_id === st.me?.id })),
  }
}

// summary is what one of the app's pages is shown: every server, and for a
// window's bar, which server is its and its tabs.
function summary(w?: ServerWindow) {
  return {
    window: w?.id ?? null,
    server: w?.server.id ?? null,
    tabs: w?.info() ?? [],
    servers: watch.all().map(serverInfo),
  }
}

let pending: NodeJS.Timeout | undefined
// changed tells the app's pages and the menu bar icon, once per burst.
function changed() {
  clearTimeout(pending)
  pending = setTimeout(() => {
    for (const w of windows) w.bar.webContents.send('app:state', summary(w))
    picker?.webContents.send('app:state', summary())
    updateTray()
  }, 30)
}

// ---- Windows ----

// newWindow opens a window of a server: restoring one, or a fresh one
// where bounds say.
function newWindow(s: Server, opts: ConstructorParameters<typeof ServerWindow>[2] = {}): ServerWindow {
  const w = new ServerWindow(s, changed, opts)
  windows.unshift(w)
  w.win.on('focus', () => {
    windows = [w, ...windows.filter((x) => x !== w)]
  })
  w.win.on('close', () => {
    // A server's last window is kept when closed, to open again as it was;
    // one of several is forgotten.
    const others = windows.some((x) => x !== w && x.server.id === s.id)
    if (quitting) w.save(true)
    else if (others) store.forgetWindow(w.id)
    else w.save(false)
  })
  w.win.on('closed', () => {
    windows = windows.filter((x) => x !== w)
    changed()
  })
  changed()
  return w
}

// openServer brings a server's window forward: the one last in front, or
// the one it was last left with, or a new one.
function openServer(s: Server): ServerWindow {
  const w = windows.find((x) => x.server.id === s.id) ?? newWindow(s, { saved: store.closedWindow(s.id) })
  w.win.show()
  w.win.focus()
  return w
}

// detach moves a tab to a window of its own, placed at a point on the
// screen, or beside its window.
function detach(from: ServerWindow, tab: Tab, at?: { x: number; y: number }) {
  if (tab === from.panel) return
  const [width, height] = from.win.getSize()
  const [fx, fy] = from.win.getPosition()
  const bounds = at ? { x: at.x - 120, y: at.y - 16, width, height } : { x: fx + 40, y: fy + 40, width, height }
  const w = newWindow(from.server, { bounds })
  w.adopt(tab)
  w.win.focus()
  closeIfEmptied(from)
}

// showPicker opens the window that picks or adds a server.
function showPicker() {
  if (picker) {
    picker.show()
    picker.focus()
    return
  }
  picker = new BrowserWindow({
    width: 560,
    height: 600,
    minWidth: 420,
    minHeight: 400,
    title: 'Hangar',
    backgroundColor: '#0f1419',
    webPreferences: { preload: join(__dirname, 'preload-app.js'), contextIsolation: true, sandbox: true },
  })
  picker.loadURL(`file://${join(__dirname, 'ui', 'home.html')}`)
  picker.on('closed', () => {
    picker = null
  })
}

// focused is the server window the menus act on: the one in front, or the
// one last in front while the app has no window focused.
function focused(): ServerWindow | undefined {
  const bw = BaseWindow.getFocusedWindow()
  return bw ? windows.find((w) => w.win === bw) : windows[0]
}

// windowFor is the server window a page belongs to: its bar or a tab.
function windowFor(wc: WebContents): ServerWindow | undefined {
  return windows.find((w) => w.bar.webContents === wc || w.tabFor(wc))
}

// ---- Calls from the app's own pages ----

// own refuses an app call from anything but the app's own pages.
function own(e: IpcMainInvokeEvent) {
  const w = windowFor(e.sender)
  if (!(w && w.bar.webContents === e.sender) && e.sender !== picker?.webContents) throw new Error('not allowed')
  return w
}

function env(serverId: string, envId: string) {
  const st = watch.all().find((s) => s.server.id === serverId)
  const e = st?.environments.find((x) => x.id === envId)
  if (!st || !e) throw new Error('no such environment')
  return { st, e }
}

ipcMain.handle('app:get', (e) => summary(own(e)))
ipcMain.handle('tab:activate', (e, id: number) => {
  const w = own(e)
  const t = w?.tabs.find((x) => x.id === id)
  if (t) w!.activate(t)
})
ipcMain.handle('tab:close', (e, id: number) => {
  const w = own(e)
  const t = w?.tabs.find((x) => x.id === id)
  if (t) w!.close(t)
})
ipcMain.handle('tab:move', (e, id: number, to: number) => own(e)?.move(id, to))
ipcMain.handle('tab:menu', (e, id: number) => {
  const w = own(e)
  const t = w?.tab(id)
  if (w && t) Menu.buildFromTemplate(tabMenu(w, t)).popup({ window: w.win })
})
// A tab dragged off its bar tears off into a window of its own at once,
// and the window follows the pointer until it is let go: over another
// window's bar of the same server the tab joins that window, and anywhere
// else its new window stays where it was dropped. The bar that started the
// drag keeps the pointer throughout, and says when it is let go.
let tearing: { tab: Tab; win: ServerWindow; from: ServerWindow; timer: NodeJS.Timeout } | null = null

ipcMain.handle('tab:tear', (e, id: number) => {
  const w = own(e)
  const t = w?.tab(id)
  if (!w || !t || t === w.panel || tearing) return
  const at = screen.getCursorScreenPoint()
  const [width, height] = w.win.getSize()
  // The tab sits under the pointer in its new window's bar.
  const dx = 150
  const dy = barHeight / 2
  const win = newWindow(w.server, { bounds: { x: at.x - dx, y: at.y - dy, width, height } })
  win.adopt(t)
  const timer = setInterval(() => {
    const p = screen.getCursorScreenPoint()
    win.win.setPosition(Math.round(p.x - dx), Math.round(p.y - dy))
  }, 16)
  tearing = { tab: t, win, from: w, timer }
})

ipcMain.handle('tab:release', (e) => {
  own(e)
  if (!tearing) return
  const { tab, win, from, timer } = tearing
  tearing = null
  clearInterval(timer)
  const at = screen.getCursorScreenPoint()
  const onBar = (x: ServerWindow) => {
    const b = x.win.getContentBounds()
    return at.x >= b.x && at.x < b.x + b.width && at.y >= b.y - 8 && at.y < b.y + barHeight + 16
  }
  const into = windows.find((x) => x !== win && x.server.id === win.server.id && onBar(x))
  if (into) {
    into.adopt(tab)
    into.win.focus()
  } else {
    win.win.focus()
  }
  closeIfEmptied(win)
  closeIfEmptied(from)
})

// closeIfEmptied closes a window whose environment tabs have all gone
// elsewhere, when its server has another window: it has nothing left the
// other does not.
function closeIfEmptied(w: ServerWindow) {
  if (w.tabs.length === 1 && windows.some((x) => x !== w && x.server.id === w.server.id)) w.win.close()
}

ipcMain.handle('overlay', (e, open: boolean) => own(e)?.setOverlay(open))
ipcMain.handle('server:add', async (e, input: string) => {
  own(e)
  let url: string
  try {
    url = new URL(/^https?:\/\//i.test(input.trim()) ? input.trim() : `https://${input.trim()}`).origin
  } catch {
    return { error: 'That is not an address.' }
  }
  const c = await api.checkServer(url)
  if (!c.ok && c.update !== 'server') return { error: `${new URL(url).host} ${c.why}` }
  const s = store.addServer(url, new URL(url).host)
  await watch.refresh()
  openServer(s)
  picker?.close()
  return { ok: true, warning: c.ok ? undefined : `${new URL(url).host} ${c.why}` }
})
ipcMain.handle('server:remove', (e, id: string) => {
  own(e)
  for (const w of windows.filter((x) => x.server.id === id)) w.win.close()
  store.removeServer(id)
  watch.refresh()
})
ipcMain.handle('server:open', (e, id: string) => {
  own(e)
  const s = store.server(id)
  if (!s) return
  openServer(s)
  if (e.sender === picker?.webContents) picker.close()
})
ipcMain.handle('env:open', (e, serverId: string, envId: string) => {
  own(e)
  const { st } = env(serverId, envId)
  openServer(st.server).openEnvironment(envId)
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
// The tab's server is the one a page speaks for, never one the page names.

ipcMain.on('page:role', (e: IpcMainEvent) => {
  const w = windowFor(e.sender)
  e.returnValue = w?.tabFor(e.sender)?.role ?? null
})
ipcMain.handle('page:route', (e, url: string) => {
  const w = windowFor(e.sender)
  if (w && roleFor(w.server, url)) w.route(url)
})

function pageEnv(e: IpcMainInvokeEvent, envId: string) {
  const w = windowFor(e.sender)
  if (!w?.tabFor(e.sender)) throw new Error('not allowed')
  return env(w.server.id, envId)
}

ipcMain.handle('page:vscode', async (e, envId: string) => {
  const { st, e: x } = pageEnv(e, envId)
  await openInVSCode(st.server, x)
})
ipcMain.handle('page:terminal', (e, envId: string) => {
  const { st, e: x } = pageEnv(e, envId)
  return st.me ? openTerminal(st.me, x) : 'not signed in'
})

// ---- A tab's menu ----

function tabMenu(w: ServerWindow, tab: Tab): Electron.MenuItemConstructorOptions[] {
  const i = w.tabs.indexOf(tab)
  const url = tab.view.webContents.getURL()
  const common: Electron.MenuItemConstructorOptions[] = [
    { label: 'Reload Tab', click: () => tab.view.webContents.reload() },
    { label: 'Copy Link', click: () => clipboard.writeText(url) },
    { label: 'Open in Browser', click: () => shell.openExternal(url) },
  ]
  const closing: Electron.MenuItemConstructorOptions[] = [
    { label: 'Close Other Tabs', enabled: w.tabs.length > 2 || (tab === w.panel && w.tabs.length > 1), click: () => w.closeMany(tab, 'others') },
    { label: 'Close Tabs to the Left', enabled: i > 1, click: () => w.closeMany(tab, 'left') },
    { label: 'Close Tabs to the Right', enabled: i < w.tabs.length - 1, click: () => w.closeMany(tab, 'right') },
    { type: 'separator' },
    { label: 'Reopen Closed Tab', click: () => w.reopen() },
  ]
  if (tab === w.panel) {
    return [
      { label: 'New Window', click: () => newWindow(w.server).win.focus() },
      { type: 'separator' },
      ...common,
      { type: 'separator' },
      ...closing,
    ]
  }
  const st = watch.all().find((s) => s.server.id === w.server.id)
  const env = tab.role.kind === 'env' ? st?.environments.find((e) => tab.role.kind === 'env' && e.id === tab.role.env) : undefined
  const running = env?.phase === 'running'
  return [
    { label: 'Duplicate Tab', click: () => w.duplicate(tab) },
    { label: 'Move Tab to New Window', click: () => detach(w, tab) },
    { type: 'separator' },
    { label: 'Open in VS Code', enabled: !!env && running, click: () => env && openInVSCode(w.server, env) },
    {
      label: 'Open Terminal',
      enabled: !!env && running && !!st?.me?.ssh,
      click: () => env && st?.me && openTerminal(st.me, env),
    },
    { type: 'separator' },
    ...common,
    { type: 'separator' },
    { label: 'Close Tab', click: () => w.close(tab) },
    ...closing,
  ]
}

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
  n?.on('click', () => openServer(st.server).openEnvironment(e.id))
  n?.show()
})

watch.on('change', () => {
  // A deleted environment's tab goes with it.
  for (const st of watch.all()) {
    if (!st.signedIn || st.error) continue
    const ids = new Set(st.environments.map((e) => e.id))
    for (const w of windows.filter((x) => x.server.id === st.server.id)) w.prune((env) => ids.has(env))
  }
  changed()
})

// ---- The menu bar icon ----

function trayIcon() {
  const file = process.platform === 'darwin' ? 'trayTemplate.png' : 'tray.png'
  const img = nativeImage.createFromPath(join(__dirname, 'assets', file))
  if (process.platform === 'darwin') img.setTemplateImage(true)
  return img
}

function updateTray() {
  if (!tray) return
  const items: Electron.MenuItemConstructorOptions[] = []
  for (const st of watch.all()) {
    items.push({ label: st.server.name, click: () => openServer(st.server) })
    if (st.compatibility && !st.compatibility.ok) {
      items.push({ label: `  ${st.compatibility.why}`, enabled: false }, { type: 'separator' })
      continue
    }
    if (!st.signedIn) {
      items.push({ label: '  Sign in…', click: () => openServer(st.server) }, { type: 'separator' })
      continue
    }
    const mine = st.environments.filter((e) => e.owner_id === st.me?.id)
    if (mine.length === 0) items.push({ label: '  No environments', enabled: false })
    for (const e of mine) {
      const running = e.phase === 'running'
      const busy = !['running', 'stopped', 'suspended', 'failed'].includes(e.phase)
      items.push({
        label: `  ${e.name} — ${e.phase}`,
        submenu: [
          { label: 'Open', click: () => openServer(st.server).openEnvironment(e.id) },
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
    items.push({ type: 'separator' })
  }
  items.push({ label: 'Open a Server…', click: showPicker }, { type: 'separator' }, { label: 'Quit Hangar', role: 'quit' })
  tray.setContextMenu(Menu.buildFromTemplate(items))
}

// ---- The application menu ----

function menu() {
  const mac = process.platform === 'darwin'
  const template: Electron.MenuItemConstructorOptions[] = [
    ...(mac ? [{ role: 'appMenu' as const }] : []),
    {
      label: 'File',
      submenu: [
        { label: 'New Window…', accelerator: 'CmdOrCtrl+N', click: showPicker },
        { label: 'New Tab…', accelerator: 'CmdOrCtrl+T', click: () => focused()?.setOverlay(true, true) },
        { label: 'Go to Environment…', accelerator: 'CmdOrCtrl+L', click: () => focused()?.setOverlay(true, true) },
        { label: 'Reopen Closed Tab', accelerator: 'CmdOrCtrl+Shift+T', click: () => focused()?.reopen() },
        {
          label: 'Duplicate Tab',
          click: () => {
            const w = focused()
            if (w) w.duplicate(w.active)
          },
        },
        {
          label: 'Move Tab to New Window',
          click: () => {
            const w = focused()
            if (w && w.active !== w.panel) detach(w, w.active)
          },
        },
        { type: 'separator' },
        {
          label: 'Close Tab',
          accelerator: 'CmdOrCtrl+W',
          click: () => {
            const w = focused()
            // The control panel does not close; ⌘W on it closes the window.
            if (w && w.active !== w.panel) w.close(w.active)
            else BaseWindow.getFocusedWindow()?.close()
          },
        },
        { label: 'Close Window', accelerator: 'CmdOrCtrl+Shift+W', click: () => BaseWindow.getFocusedWindow()?.close() },
        ...(mac ? [] : [{ type: 'separator' as const }, { role: 'quit' as const }]),
      ],
    },
    { role: 'editMenu' },
    {
      label: 'View',
      submenu: [
        { label: 'Reload Tab', accelerator: 'CmdOrCtrl+R', click: () => focused()?.active.view.webContents.reload() },
        {
          label: 'Developer Tools for Tab',
          accelerator: mac ? 'Alt+Cmd+I' : 'Ctrl+Shift+I',
          click: () => focused()?.active.view.webContents.toggleDevTools(),
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
        { label: 'Next Tab', accelerator: 'Ctrl+Tab', click: () => focused()?.cycle(1) },
        { label: 'Previous Tab', accelerator: 'Ctrl+Shift+Tab', click: () => focused()?.cycle(-1) },
        ...Array.from({ length: 9 }, (_, i) => ({
          label: `Tab ${i + 1}`,
          accelerator: `CmdOrCtrl+${i + 1}`,
          visible: false,
          click: () => {
            const w = focused()
            const t = i === 8 ? w?.tabs.at(-1) : w?.tabs[i]
            if (t) w!.activate(t)
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
  const w = openServer(s)
  if (path !== '/') w.route(s.url + (path.startsWith('/') ? path : '/' + path))
}

// ---- Start ----

if (!app.requestSingleInstanceLock()) {
  app.quit()
} else {
  // Only an installed app claims hangar:// links: one run from a checkout
  // would register the bare Electron binary as their handler.
  if (app.isPackaged) app.setAsDefaultProtocolClient('hangar')
  app.on('second-instance', (_, argv) => {
    const link = argv.find((a) => a.startsWith('hangar://'))
    if (link) openLink(link)
    else if (windows.length === 0) showPicker()
    else windows[0].win.focus()
  })
  app.on('open-url', (e, link) => {
    e.preventDefault()
    if (app.isReady()) openLink(link)
    else app.once('ready', () => openLink(link))
  })
  app.on('before-quit', () => {
    quitting = true
  })
  app.on('window-all-closed', () => {
    // On macOS the app stays in the menu bar with no window open, as Mail
    // and Slack do; quitting is ⌘Q.
    if (process.platform !== 'darwin') app.quit()
  })
  app.on('activate', () => {
    if (windows.length === 0 && !picker) showPicker()
  })

  app.whenReady().then(() => {
    store.load()
    menu()
    for (const saved of store.reopenable()) {
      const s = store.server(saved.server)
      if (s) newWindow(s, { saved })
    }
    if (windows.length === 0) showPicker()
    tray = new Tray(trayIcon())
    tray.setToolTip('Hangar')
    updateTray()
    watch.start()
    const link = process.argv.find((a) => a.startsWith('hangar://'))
    if (link) openLink(link)
  })
}
