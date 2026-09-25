// The app's window: a bar of tabs across the top, and under it the tab in
// hand.
//
// Every tab is a view of its own, kept alive while another is shown. A tab
// holding VS Code, a terminal or a desktop keeps all of it -- the editor's
// state, the sessions, the scroll position -- however often the user moves
// between tabs; nothing reloads. A server's tabs share its session, so one
// sign-in covers them all.

import { BaseWindow, clipboard, Menu, WebContentsView, shell, type WebContents } from 'electron'
import { join } from 'node:path'

import { partition } from './api'
import * as store from './store'
import type { Server } from './store'

// The bar's height, in the page's CSS pixels.
const barHeight = 40

export interface Tab {
  id: number
  // server is the server the tab shows, or null for the app's home page.
  server: Server | null
  view: WebContentsView
  title: string
  favicon?: string
  loading: boolean
}

export interface TabInfo {
  id: number
  server: string | null
  serverName: string | null
  title: string
  favicon?: string
  loading: boolean
  active: boolean
}

const ui = (page: string) => `file://${join(__dirname, 'ui', page)}`
export const homeURL = ui('home.html')

let nextTab = 1

export class AppWindow {
  readonly win: BaseWindow
  // bar is the tab strip, and the switcher when it is open over the tabs.
  readonly bar: WebContentsView
  tabs: Tab[] = []
  active: Tab | null = null
  private closed: { server: string | null; url: string }[] = []
  private overlay = false
  private onChange: () => void

  constructor(onChange: () => void) {
    this.onChange = onChange
    const b = store.bounds()
    this.win = new BaseWindow({
      width: b?.width ?? 1400,
      height: b?.height ?? 900,
      x: b?.x,
      y: b?.y,
      minWidth: 640,
      minHeight: 400,
      title: 'Hangar',
      titleBarStyle: process.platform === 'darwin' ? 'hiddenInset' : 'default',
      backgroundColor: '#0f1419',
    })
    this.bar = new WebContentsView({
      webPreferences: { preload: join(__dirname, 'preload-app.js'), contextIsolation: true, sandbox: true },
    })
    this.bar.setBackgroundColor('#00000000')
    this.bar.webContents.loadURL(ui('tabs.html'))
    this.win.contentView.addChildView(this.bar)
    this.win.on('resize', () => this.layout())
    this.win.on('close', () => {
      const [x, y] = this.win.getPosition()
      const [width, height] = this.win.getSize()
      store.saveBounds({ x, y, width, height })
      this.save()
    })
    this.layout()
  }

  // layout puts the bar across the top and the tab in hand under it. With
  // the switcher open the bar covers the whole window, over the tab.
  layout() {
    const { width, height } = this.win.getContentBounds()
    this.bar.setBounds({ x: 0, y: 0, width, height: this.overlay ? height : barHeight })
    for (const t of this.tabs) {
      const shown = t === this.active
      t.view.setVisible(shown)
      if (shown) t.view.setBounds({ x: 0, y: barHeight, width, height: Math.max(0, height - barHeight) })
    }
  }

  // setOverlay lets the bar cover the window, for the switcher or a menu of
  // its own. switcher opens the switcher in it, as ⌘L does.
  setOverlay(open: boolean, switcher = false) {
    this.overlay = open
    // The bar goes on top of the tabs while it covers them.
    this.win.contentView.addChildView(this.bar)
    this.layout()
    if (open) {
      if (switcher) this.bar.webContents.send('overlay:open')
      this.bar.webContents.focus()
    } else this.active?.view.webContents.focus()
  }

  info(): TabInfo[] {
    return this.tabs.map((t) => ({
      id: t.id,
      server: t.server?.id ?? null,
      serverName: t.server?.name ?? null,
      title: t.title,
      favicon: t.favicon,
      loading: t.loading,
      active: t === this.active,
    }))
  }

  isOwn(wc: WebContents): boolean {
    return wc === this.bar.webContents || this.tabs.some((t) => t.server === null && t.view.webContents === wc)
  }

  tabFor(wc: WebContents): Tab | undefined {
    return this.tabs.find((t) => t.view.webContents === wc)
  }

  // open makes a tab showing url -- the home page when server is null --
  // after the tab in hand, or after at when given.
  open(server: Server | null, url: string, opts: { activate?: boolean; at?: number } = {}): Tab {
    const view = new WebContentsView({
      webPreferences: server
        ? {
            partition: partition(server),
            preload: join(__dirname, 'preload-page.js'),
            contextIsolation: true,
            sandbox: true,
            // A tab out of sight keeps its timers, so its sockets keep
            // their heartbeats and it is as it was when shown again.
            backgroundThrottling: false,
          }
        : { preload: join(__dirname, 'preload-app.js'), contextIsolation: true, sandbox: true },
    })
    const tab: Tab = { id: nextTab++, server, view, title: server?.name ?? 'Home', loading: true }
    const wc = view.webContents
    wc.on('page-title-updated', (_, title) => {
      // The web UI ends every title with "· Hangar", which the app says
      // already.
      tab.title = title.replace(/ · Hangar$/, '')
      this.onChange()
    })
    wc.on('page-favicon-updated', (_, icons) => {
      tab.favicon = icons[0]
      this.onChange()
    })
    wc.on('did-start-loading', () => {
      tab.loading = true
      this.onChange()
    })
    wc.on('did-stop-loading', () => {
      tab.loading = false
      this.onChange()
    })
    wc.setWindowOpenHandler(({ url: target, disposition }) => {
      // A link opened as a new tab or window on the same server -- a
      // middle-click, a ⌘-click -- becomes a tab; anything else goes to
      // the browser.
      const to = store.serverFor(target)
      if (server && to?.id === server.id) {
        this.open(server, target, { activate: disposition !== 'background-tab', at: this.tabs.indexOf(tab) })
      } else if (/^https?:/.test(target)) {
        shell.openExternal(target)
      }
      return { action: 'deny' }
    })
    wc.on('will-navigate', (e, target) => {
      // A tab stays on its server; the rest of the web opens in the browser.
      if (!server || store.serverFor(target)?.id !== server.id) {
        if (server || !target.startsWith('file:')) {
          e.preventDefault()
          if (/^https?:/.test(target)) shell.openExternal(target)
        }
      }
    })
    wc.on('context-menu', (_, p) => {
      // Pages with a menu of their own show it; this is the plain one
      // for text, which Electron leaves out.
      const items: Electron.MenuItemConstructorOptions[] = []
      if (p.isEditable) items.push({ role: 'cut' }, { role: 'copy' }, { role: 'paste' }, { role: 'selectAll' })
      else if (p.selectionText) items.push({ role: 'copy' })
      if (p.linkURL) items.push({ label: 'Copy Link', click: () => clipboard.writeText(p.linkURL) })
      if (items.length) Menu.buildFromTemplate(items).popup()
    })
    const at = opts.at ?? (this.active ? this.tabs.indexOf(this.active) : this.tabs.length - 1)
    this.tabs.splice(at + 1, 0, tab)
    this.win.contentView.addChildView(view)
    this.win.contentView.addChildView(this.bar)
    wc.loadURL(url)
    if (opts.activate !== false || !this.active) this.activate(tab)
    else this.layout()
    this.onChange()
    return tab
  }

  activate(tab: Tab) {
    this.active = tab
    this.layout()
    tab.view.webContents.focus()
    this.onChange()
  }

  close(tab: Tab) {
    const i = this.tabs.indexOf(tab)
    if (i < 0) return
    this.closed.push({ server: tab.server?.id ?? null, url: tab.view.webContents.getURL() })
    this.closed = this.closed.slice(-20)
    this.tabs.splice(i, 1)
    this.win.contentView.removeChildView(tab.view)
    tab.view.webContents.close()
    if (this.active === tab) {
      this.active = null
      const next = this.tabs[Math.min(i, this.tabs.length - 1)]
      if (next) this.activate(next)
    }
    if (this.tabs.length === 0) this.open(null, homeURL)
    this.onChange()
  }

  reopen() {
    const last = this.closed.pop()
    if (!last) return
    const server = store.server(last.server) ?? null
    this.open(server, server || last.url.startsWith('file:') ? last.url : homeURL)
  }

  cycle(by: number) {
    if (!this.active || this.tabs.length < 2) return
    const i = this.tabs.indexOf(this.active)
    this.activate(this.tabs[(i + by + this.tabs.length) % this.tabs.length])
  }

  move(id: number, to: number) {
    const i = this.tabs.findIndex((t) => t.id === id)
    if (i < 0) return
    const [t] = this.tabs.splice(i, 1)
    this.tabs.splice(Math.max(0, Math.min(to, this.tabs.length)), 0, t)
    this.onChange()
  }

  // openEnvironment shows an environment: the tab already on it, or a new
  // one.
  openEnvironment(server: Server, id: string, section = '') {
    const path = `/environments/${id}`
    const existing = this.tabs.find((t) => {
      if (t.server?.id !== server.id) return false
      try {
        return new URL(t.view.webContents.getURL()).pathname.startsWith(path)
      } catch {
        return false
      }
    })
    if (existing && !section) return this.activate(existing)
    this.open(server, `${server.url}${path}${section ? '/' + section : ''}`)
  }

  save() {
    store.saveTabs(
      this.tabs.map((t) => ({ server: t.server?.id ?? null, url: t.view.webContents.getURL() })),
      this.active ? this.tabs.indexOf(this.active) : 0,
    )
  }

  restore() {
    const { tabs, active } = store.savedTabs()
    for (const t of tabs) {
      const server = t.server ? store.server(t.server) : null
      if (t.server && !server) continue
      this.open(server ?? null, server ? t.url : homeURL, { activate: false, at: this.tabs.length - 1 })
    }
    if (this.tabs.length === 0) this.open(null, homeURL)
    this.activate(this.tabs[Math.min(active, this.tabs.length - 1)] ?? this.tabs[0])
  }
}
