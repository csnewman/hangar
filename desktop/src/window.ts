// A window of one server: a bar of tabs across the top, and under it the
// tab in hand. The first tab is the server's control panel and stays; every
// other tab is an environment. A server may have several windows, and an
// environment several tabs.
//
// Every tab is a view of its own, kept alive while another is shown. An
// environment's tab keeps all of it -- VS Code's state, terminal sessions,
// the desktop, scroll positions -- however often the user moves between
// tabs, and when it is dragged to another window of its server: the view
// moves, the page in it does not reload. A server's tabs share its
// session, so one sign-in covers them all.
//
// Pages are routed to the tab they belong in: an environment's pages to its
// tab, everything else to the control panel. A link is caught in the page
// before the web UI follows it (see preload-page.ts); a move the web UI
// makes by itself, such as to a new environment it has just created, is
// caught here once made, and taken back where it happened.

import { BaseWindow, clipboard, Menu, WebContentsView, shell, type WebContents } from 'electron'
import { randomUUID } from 'node:crypto'
import { join } from 'node:path'

import { partition } from './api'
import * as store from './store'
import type { Bounds, SavedWindow, Server } from './store'

// The bar's height, in the page's CSS pixels.
export const barHeight = 40

// Role is what a tab is for: the control panel, or one environment.
export type Role = { kind: 'panel' } | { kind: 'env'; env: string }

// roleFor is the tab a page of this server belongs in: its environment's,
// for /environments/<id> and anything under it, and the control panel for
// everything else.
export function roleFor(server: Server, url: string): Role | null {
  let u: URL
  try {
    u = new URL(url)
  } catch {
    return null
  }
  if (u.origin !== server.url) return null
  const m = /^\/environments\/([^/]+)/.exec(u.pathname)
  if (m && m[1] !== 'new') return { kind: 'env', env: decodeURIComponent(m[1]) }
  return { kind: 'panel' }
}

const same = (a: Role, b: Role | null) =>
  !!b && a.kind === b.kind && (a.kind === 'panel' || (b.kind === 'env' && a.env === b.env))

export interface Tab {
  id: number
  role: Role
  view: WebContentsView
  // owner is the window the tab is in; a tab dragged to another window
  // changes it.
  owner: ServerWindow
  title: string
  favicon?: string
  loading: boolean
}

export interface TabInfo {
  id: number
  panel: boolean
  env?: string
  title: string
  favicon?: string
  loading: boolean
  active: boolean
}

const ui = (page: string) => `file://${join(__dirname, 'ui', page)}`

let nextTab = 1

export interface WindowOptions {
  // saved is the window to restore: its tabs and place.
  saved?: SavedWindow
  // bounds is where to put a new window, such as under a tab dragged out.
  bounds?: Partial<Bounds>
}

export class ServerWindow {
  readonly id: string
  readonly server: Server
  readonly win: BaseWindow
  // bar is the tab strip, and the switcher when it is open over the tabs.
  readonly bar: WebContentsView
  readonly panel: Tab
  tabs: Tab[] = []
  active: Tab
  private closed: string[] = []
  private overlay = false
  private onChange: () => void

  constructor(server: Server, onChange: () => void, opts: WindowOptions = {}) {
    this.id = opts.saved?.id ?? randomUUID()
    this.server = server
    this.onChange = onChange
    const b = { ...opts.saved?.bounds, ...opts.bounds }
    this.win = new BaseWindow({
      width: b.width ?? 1400,
      height: b.height ?? 900,
      x: b.x,
      y: b.y,
      minWidth: 640,
      minHeight: 400,
      title: server.name,
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
    // Closing a window leaves the pages in its views running; they are
    // ended with it. A tab moved to another window is not among them.
    this.win.on('closed', () => {
      for (const wc of [this.bar.webContents, ...this.tabs.map((t) => t.view.webContents)]) {
        if (!wc.isDestroyed()) wc.close()
      }
    })

    this.panel = this.make({ kind: 'panel' }, server.url)
    this.tabs.push(this.panel)
    this.active = this.panel
    for (const url of opts.saved?.tabs ?? []) {
      const role = roleFor(server, url)
      if (role?.kind === 'env') this.tabs.push(this.make(role, url))
    }
    this.activate(this.tabs[Math.min(Math.max(opts.saved?.active ?? 0, 0), this.tabs.length - 1)])
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
  // its own. switcher opens the switcher in it, as ⌘T and ⌘L do.
  setOverlay(open: boolean, switcher = false) {
    this.overlay = open
    this.win.contentView.addChildView(this.bar)
    this.layout()
    if (open) {
      if (switcher) this.bar.webContents.send('overlay:open')
      this.bar.webContents.focus()
    } else this.active.view.webContents.focus()
  }

  info(): TabInfo[] {
    return this.tabs.map((t) => ({
      id: t.id,
      panel: t.role.kind === 'panel',
      env: t.role.kind === 'env' ? t.role.env : undefined,
      title: t.title,
      favicon: t.favicon,
      loading: t.loading,
      active: t === this.active,
    }))
  }

  tabFor(wc: WebContents): Tab | undefined {
    return this.tabs.find((t) => t.view.webContents === wc)
  }

  tab(id: number): Tab | undefined {
    return this.tabs.find((t) => t.id === id)
  }

  // envTab is the tab a link to an environment goes to: the one in hand if
  // it shows that environment, else the first that does.
  envTab(env: string): Tab | undefined {
    const shows = (t: Tab) => t.role.kind === 'env' && t.role.env === env
    return shows(this.active) ? this.active : this.tabs.find(shows)
  }

  // make builds a tab's view and wires its events; the caller places it.
  // The events find the tab's window through the tab, which a drag to
  // another window changes.
  private make(role: Role, url: string): Tab {
    const view = new WebContentsView({
      webPreferences: {
        partition: partition(this.server),
        preload: join(__dirname, 'preload-page.js'),
        contextIsolation: true,
        sandbox: true,
        // A tab out of sight keeps its timers, so its sockets keep their
        // heartbeats and it is as it was when shown again.
        backgroundThrottling: false,
      },
    })
    const tab: Tab = {
      id: nextTab++,
      role,
      view,
      owner: this,
      title: role.kind === 'panel' ? this.server.name : '…',
      loading: true,
    }
    const server = this.server
    const wc = view.webContents
    const changed = () => tab.owner.onChange()
    wc.on('page-title-updated', (_, title) => {
      // The web UI ends every title with "· Hangar", which the app says
      // already.
      tab.title = title.replace(/ · Hangar$/, '')
      changed()
    })
    wc.on('page-favicon-updated', (_, icons) => {
      tab.favicon = icons[0]
      changed()
    })
    wc.on('did-start-loading', () => {
      tab.loading = true
      changed()
    })
    wc.on('did-stop-loading', () => {
      tab.loading = false
      changed()
    })
    wc.setWindowOpenHandler(({ url: target, disposition }) => {
      // A link opened as a new tab or window -- a middle-click, a ⌘-click
      // -- goes to its tab here; the rest of the web to the browser.
      if (roleFor(server, target)) tab.owner.route(target, { activate: disposition !== 'background-tab' })
      else if (/^https?:/.test(target)) shell.openExternal(target)
      return { action: 'deny' }
    })
    wc.on('will-navigate', (e, target) => {
      const to = roleFor(server, target)
      if (!to) {
        e.preventDefault()
        if (/^https?:/.test(target)) shell.openExternal(target)
      } else if (!same(tab.role, to)) {
        e.preventDefault()
        tab.owner.route(target)
      }
    })
    wc.on('did-navigate-in-page', (_, target, isMainFrame) => {
      // The web UI moved by itself to a page that belongs in another tab:
      // it goes there, and this tab goes back to where it was.
      if (!isMainFrame) return
      const to = roleFor(server, target)
      if (to && !same(tab.role, to)) {
        tab.owner.route(target)
        if (wc.navigationHistory.canGoBack()) wc.navigationHistory.goBack()
      }
    })
    wc.on('context-menu', (_, p) => {
      // Pages with a menu of their own show it; this is the plain one for
      // text, which Electron leaves out.
      const items: Electron.MenuItemConstructorOptions[] = []
      if (p.isEditable) items.push({ role: 'cut' }, { role: 'copy' }, { role: 'paste' }, { role: 'selectAll' })
      else if (p.selectionText) items.push({ role: 'copy' })
      if (p.linkURL) items.push({ label: 'Copy Link', click: () => clipboard.writeText(p.linkURL) })
      if (items.length) Menu.buildFromTemplate(items).popup()
    })
    this.win.contentView.addChildView(view)
    this.win.contentView.addChildView(this.bar)
    wc.loadURL(url)
    return tab
  }

  // place puts a tab after the one in hand, never before the control panel.
  private place(tab: Tab, at?: number) {
    const i = at ?? this.tabs.indexOf(this.active) + 1
    this.tabs.splice(Math.max(1, Math.min(i, this.tabs.length)), 0, tab)
  }

  // route shows a page of this server in the tab it belongs in: its
  // environment's -- made if there is none -- or the control panel.
  route(url: string, opts: { activate?: boolean } = {}) {
    const role = roleFor(this.server, url)
    if (!role) return
    let tab = role.kind === 'panel' ? this.panel : this.envTab(role.env)
    if (!tab) {
      tab = this.make(role, url)
      this.place(tab)
    } else {
      this.go(tab, url)
    }
    if (opts.activate !== false) this.activate(tab)
    else this.layout()
    this.onChange()
  }

  // go moves a tab to another of its pages without loading it afresh: the
  // web UI's router follows the history it is given, so the page keeps
  // everything it holds.
  private go(tab: Tab, url: string) {
    const u = new URL(url)
    let current: URL
    try {
      current = new URL(tab.view.webContents.getURL())
    } catch {
      return
    }
    if (u.pathname === current.pathname && u.search === current.search) return
    const path = JSON.stringify(u.pathname + u.search + u.hash)
    tab.view.webContents
      .executeJavaScript(
        `history.pushState({ usr: null, key: 'desktop', idx: (history.state?.idx ?? 0) + 1 }, '', ${path});` +
          `dispatchEvent(new PopStateEvent('popstate', { state: history.state }))`,
      )
      .catch(() => {})
  }

  activate(tab: Tab) {
    this.active = tab
    this.layout()
    tab.view.webContents.focus()
    this.onChange()
  }

  // duplicate opens another tab on what a tab shows, loaded afresh: a page
  // cannot be copied with what it holds.
  duplicate(tab: Tab) {
    if (tab === this.panel) return
    const copy = this.make(tab.role, tab.view.webContents.getURL())
    this.place(copy, this.tabs.indexOf(tab) + 1)
    this.activate(copy)
  }

  // close closes an environment's tab. The control panel stays.
  close(tab: Tab) {
    if (tab === this.panel) return
    if (!this.release(tab)) return
    this.closed.push(tab.view.webContents.getURL())
    this.closed = this.closed.slice(-20)
    tab.view.webContents.close()
  }

  // closeMany closes environment tabs by where they are: every other one,
  // or those left or right of a tab.
  closeMany(tab: Tab, which: 'others' | 'left' | 'right') {
    const i = this.tabs.indexOf(tab)
    const doomed = this.tabs.filter((t, j) => {
      if (t === this.panel || t === tab) return false
      return which === 'others' || (which === 'left' ? j < i : j > i)
    })
    for (const t of doomed) this.close(t)
    if (!this.tabs.includes(this.active)) this.activate(tab)
  }

  // release takes a tab out of the window, alive, for closing or for
  // another window to adopt.
  release(tab: Tab): boolean {
    const i = this.tabs.indexOf(tab)
    if (i < 1) return false
    this.tabs.splice(i, 1)
    this.win.contentView.removeChildView(tab.view)
    if (this.active === tab) this.activate(this.tabs[Math.min(i, this.tabs.length - 1)])
    this.onChange()
    return true
  }

  // adopt takes a tab from another window of the same server, at index.
  // The page in it carries on as it was.
  adopt(tab: Tab, at?: number) {
    if (tab.owner === this) {
      if (at !== undefined) this.move(tab.id, at)
      return
    }
    if (!tab.owner.release(tab)) return
    tab.owner = this
    this.place(tab, at)
    this.win.contentView.addChildView(tab.view)
    this.win.contentView.addChildView(this.bar)
    this.activate(tab)
  }

  reopen() {
    const url = this.closed.pop()
    if (url) this.route(url)
  }

  cycle(by: number) {
    const i = this.tabs.indexOf(this.active)
    this.activate(this.tabs[(i + by + this.tabs.length) % this.tabs.length])
  }

  // move reorders an environment's tab; the control panel stays first.
  move(id: number, to: number) {
    const i = this.tabs.findIndex((t) => t.id === id)
    if (i < 1) return
    const [t] = this.tabs.splice(i, 1)
    this.tabs.splice(Math.max(1, Math.min(to, this.tabs.length)), 0, t)
    this.onChange()
  }

  // openEnvironment shows an environment in its tab, at section -- a tab of
  // its page, such as "terminal" -- when given.
  openEnvironment(id: string, section = '') {
    const tab = this.envTab(id)
    if (tab && !section) return this.activate(tab)
    this.route(`${this.server.url}/environments/${encodeURIComponent(id)}${section ? '/' + section : ''}`)
  }

  // prune closes the tabs of environments that are gone.
  prune(exists: (env: string) => boolean) {
    for (const t of [...this.tabs]) {
      if (t.role.kind === 'env' && !exists(t.role.env)) this.close(t)
    }
  }

  saved(): SavedWindow {
    const [x, y] = this.win.getPosition()
    const [width, height] = this.win.getSize()
    return {
      id: this.id,
      server: this.server.id,
      tabs: this.tabs.filter((t) => t !== this.panel).map((t) => t.view.webContents.getURL()),
      active: this.tabs.indexOf(this.active),
      bounds: { x, y, width, height },
    }
  }

  save(open: boolean) {
    store.saveWindow(this.saved(), open)
  }
}
