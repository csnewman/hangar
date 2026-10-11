import './code.css'

import { Container, FolderTree, GitCompare, ListTree, Monitor, SquareTerminal, TextSearch } from 'lucide-react'
import { useCallback, useEffect, useMemo, useRef, useState, type ReactNode } from 'react'
import {
  Group,
  Panel,
  Separator,
  useDefaultLayout,
  usePanelRef,
} from 'react-resizable-panels'

import { useEnv } from '../pages/Environment'
import { useMe } from '../session'
import { ChangesView } from './ChangesView'
import { CodeClient, type ConnectionState, type GitStatus } from './client'
import { CodeTerminal } from './CodeTerminal'
import { isMac, shortcut } from './ContextMenu'
import { containerShell, DockerView } from './DockerView'
import { EditorArea, tabKey, type Tab } from './EditorArea'
import { ProjectView } from './ProjectView'
import { Languages, LanguagesContext, type Usages } from './lsp'
import { FindInFiles, GotoFile, OpenFolder, type Jump } from './Search'
import { UsagesView } from './UsagesView'

// CodeTab is Hangar's own editor for an environment, laid out as IntelliJ
// lays out its tool windows: Project on the left, Changes on the right,
// Terminal along the bottom, each on a stripe button that shows and hides it,
// around the editor. Files are edited together by everyone who has them
// open, and saved as they are typed.
export function CodeTab() {
  const env = useEnv()
  if (env.phase !== 'running') {
    return (
      <div className="console">
        <div className="console-empty">
          <h2>Code</h2>
          <p>Start {env.name} to edit its files.</p>
        </div>
      </div>
    )
  }
  return <Connected key={env.id} env={env.id} />
}

// Connected holds the tab's connection to the environment. It is made in an
// effect, not during render, so React's development double mount makes and
// closes a spare one rather than closing the one in use.
//
// The connection works in the environment's folder, unless this browser tab
// has opened another, which it remembers for itself alone.
function Connected({ env }: { env: string }) {
  const folderKey = `hangar.code.${env}.folder`
  const [folder, setFolder] = useState(() => {
    try {
      return sessionStorage.getItem(folderKey) ?? ''
    } catch {
      return ''
    }
  })
  const choose = (dir: string) => {
    try {
      if (dir) sessionStorage.setItem(folderKey, dir)
      else sessionStorage.removeItem(folderKey)
    } catch {
      // The folder is kept for as long as the page is open regardless.
    }
    setFolder(dir)
  }
  const [client, setClient] = useState<CodeClient | null>(null)
  useEffect(() => {
    const c = new CodeClient(env, folder)
    setClient(c)
    return () => c.close()
  }, [env, folder])
  if (!client) return <div className="code-page" />
  return <CodeView key={folder} env={env} client={client} folder={folder} onFolder={choose} />
}

// A colour per user, so the same person's caret is the same colour
// everywhere.
const palette = ['#2a78d6', '#d95926', '#1f9d55', '#8250df', '#c9302c', '#0f8fa3', '#b7791f', '#d6336c']
function colourFor(name: string) {
  let h = 0
  for (const c of name) h = (h * 31 + c.charCodeAt(0)) >>> 0
  return palette[h % palette.length]
}

type Store = { tabs: Tab[]; active: string | null }

// A tool window that moves between the sides, and the side it is on.
type ToolName = 'project' | 'changes' | 'docker'
type Side = 'left' | 'right'
type Sides = Record<ToolName, Side>
const defaultSides: Sides = { project: 'left', changes: 'right', docker: 'right' }
const toolNames: ToolName[] = ['project', 'changes', 'docker']
const toolTitles: Record<ToolName, string> = { project: 'Project', changes: 'Changes', docker: 'Docker' }
const toolIcons: Record<ToolName, ReactNode> = {
  project: <FolderTree size={16} />,
  changes: <GitCompare size={16} />,
  docker: <Container size={16} />,
}

function sidesFrom(settings: Record<string, unknown>): Sides {
  const tw = settings.toolWindows as Partial<Record<ToolName, unknown>> | undefined
  const out = { ...defaultSides }
  for (const n of toolNames) {
    const v = tw?.[n]
    if (v === 'left' || v === 'right') out[n] = v
  }
  return out
}

type Popup = 'goto' | 'everywhere' | 'find' | 'folder' | null

function CodeView({
  env,
  client,
  folder,
  onFolder,
}: {
  env: string
  client: CodeClient
  folder: string
  onFolder: (dir: string) => void
}) {
  const hasDesktop = useEnv().spec.display !== 'none'
  // What this tab remembers -- open tabs, open folders -- is kept per
  // folder.
  const scope = folder ? `${env}:${folder}` : env
  const me = useMe()
  const user = useMemo(() => ({ name: me.display_name || me.username, color: colourFor(me.username) }), [me])

  const [state, setState] = useState<ConnectionState>(client.state)
  const [why, setWhy] = useState('')
  useEffect(() => client.on('state', (s, w) => { setState(s); setWhy(w) }), [client])

  // Git status, reloaded whenever files or the repository change.
  const [status, setStatus] = useState<GitStatus | null>(null)
  const reloadStatus = useCallback(() => {
    client.status().then(setStatus, () => {})
  }, [client])
  useEffect(() => {
    reloadStatus()
    let timer: ReturnType<typeof setTimeout> | undefined
    const later = () => {
      clearTimeout(timer)
      timer = setTimeout(reloadStatus, 300)
    }
    const offs = [client.on('fs.changed', later), client.on('git.changed', later), client.on('state', (s) => s === 'live' && later())]
    return () => {
      clearTimeout(timer)
      offs.forEach((off) => off())
    }
  }, [client, reloadStatus])

  // Tool windows: a panel on each side, showing one of the windows docked
  // there at a time, as IntelliJ's sides do, and the terminal below.
  const leftPanel = usePanelRef()
  const rightPanel = usePanelRef()
  const terminal = usePanelRef()

  // Open tabs, remembered per environment.
  const storeKey = `hangar.code.${scope}.tabs`
  const [store, setStore] = useState<Store>(() => {
    try {
      const s = JSON.parse(localStorage.getItem(storeKey) ?? '')
      if (Array.isArray(s.tabs)) return s
    } catch {
      // Nothing remembered.
    }
    return { tabs: [], active: null }
  })
  useEffect(() => {
    try {
      localStorage.setItem(storeKey, JSON.stringify(store))
    } catch {
      // Remembering tabs is a convenience.
    }
  }, [store, storeKey])
  // opened counts the tabs opened, so the tab bar shows each one, the
  // active one opened again included.
  const [opened, setOpened] = useState(0)
  const open = (tab: Tab) => {
    setStore((s) => ({
      tabs: s.tabs.some((t) => tabKey(t) === tabKey(tab)) ? s.tabs : [...s.tabs, tab],
      active: tabKey(tab),
    }))
    setOpened((n) => n + 1)
  }
  // Closed tabs, most recent last, for reopening.
  const closed = useRef<Tab[]>([])
  const [canReopen, setCanReopen] = useState(false)
  // closeTabs closes several tabs at once. When the active one goes, the
  // nearest survivor to its right, else its left, takes its place.
  const closeTabs = (keys: string[]) => {
    const gone = new Set(keys)
    setStore((s) => {
      const tabs = s.tabs.filter((t) => !gone.has(tabKey(t)))
      if (tabs.length === s.tabs.length) return s
      closed.current.push(...s.tabs.filter((t) => gone.has(tabKey(t))).reverse())
      closed.current = closed.current.slice(-30)
      let active = s.active
      if (active && gone.has(active)) {
        const i = s.tabs.findIndex((t) => tabKey(t) === active)
        const next = s.tabs.slice(i + 1).find((t) => !gone.has(tabKey(t))) ?? s.tabs.slice(0, i).reverse().find((t) => !gone.has(tabKey(t)))
        active = next ? tabKey(next) : null
      }
      return { tabs, active }
    })
    setCanReopen(true)
  }
  const close = (key: string) => closeTabs([key])
  const reopen = () => {
    const tab = closed.current.pop()
    setCanReopen(closed.current.length > 0)
    if (tab) open(tab)
  }
  const cycle = (by: number) =>
    setStore((s) => {
      if (s.tabs.length === 0) return s
      const i = s.tabs.findIndex((t) => tabKey(t) === s.active)
      return { ...s, active: tabKey(s.tabs[(i + by + s.tabs.length) % s.tabs.length]) }
    })
  const activeTab = store.tabs.find((t) => tabKey(t) === store.active)

  // Reveal in Project: the tree opens the folders to a path and selects it.
  const [reveal, setReveal] = useState<{ path: string; n: number } | null>(null)
  const revealInProject = (path: string) => {
    showTool('project')
    setReveal((r) => ({ path, n: (r?.n ?? 0) + 1 }))
  }

  // Open in Terminal: a new session in a folder.
  const [termIn, setTermIn] = useState<{ dir: string; run?: string; n: number } | null>(null)
  const openTerminal = (dir: string, run?: string) => {
    showBottom('terminal')
    setTermIn((t) => ({ dir, run, n: (t?.n ?? 0) + 1 }))
  }

  // Search: popups, and the place in a file one of them opens.
  const [popup, setPopup] = useState<Popup>(null)
  const [jump, setJump] = useState<(Jump & { n: number }) | null>(null)
  const openAt = (j: Jump) => {
    open({ kind: 'file', path: j.path })
    if (j.line > 0) setJump((x) => ({ ...j, n: (x?.n ?? 0) + 1 }))
  }
  // The bottom panel shows the terminal or the Find tool window, keeping
  // the other.
  const [bottom, setBottom] = useState<'terminal' | 'find'>('terminal')
  const showBottom = (w: 'terminal' | 'find') => {
    setBottom(w)
    terminal.current?.expand()
  }
  const toggleBottom = (w: 'terminal' | 'find') => {
    if (bottom === w && shown.terminal) terminal.current?.collapse()
    else showBottom(w)
  }
  const [usages, setUsages] = useState<Usages | null>(null)

  // Language servers, which open the files they point at as tabs, and list
  // usages in the Find tool window.
  const opener = useRef<{ open: typeof open; openAt: typeof openAt; showUsages: (u: Usages) => void } | null>(null)
  opener.current = {
    open,
    openAt,
    showUsages: (u: Usages) => {
      setUsages(u)
      showBottom('find')
    },
  }
  const languages = useMemo(
    () =>
      new Languages(client, {
        open: (path) => opener.current?.open({ kind: 'file', path }),
        jump: (j) => opener.current?.openAt(j),
        usages: (u) => opener.current?.showUsages(u),
      }),
    [client],
  )

  const recent = useMemo(
    () => [...store.tabs].reverse().filter((t) => t.kind === 'file').map((t) => t.path),
    [store.tabs],
  )

  // Keys, chosen from those a browser leaves to the page: as IntelliJ,
  // Ctrl/Cmd+Shift+O goes to a file, Ctrl/Cmd+Shift+F finds in files and
  // Shift twice searches everywhere, wherever focus is; Alt+W closes a tab,
  // Alt+Shift+T reopens one, and Alt+[ and Alt+] step through them, except
  // in the terminal, which keeps its own Alt keys. They work while the Code
  // tab shows, focus being on the page itself between one editor closing
  // and the next opening.
  const lastShift = useRef(0)
  const onKey = (e: KeyboardEvent) => {
    if (e.key === 'Shift') {
      if (e.repeat) return
      const now = Date.now()
      if (now - lastShift.current < 400 && !popup) {
        lastShift.current = 0
        setPopup('everywhere')
      } else lastShift.current = now
      return
    }
    lastShift.current = 0
    const mod = isMac ? e.metaKey && !e.ctrlKey : e.ctrlKey && !e.metaKey
    if (mod && e.shiftKey && !e.altKey && (e.code === 'KeyO' || e.code === 'KeyF')) {
      e.preventDefault()
      e.stopPropagation()
      setPopup(e.code === 'KeyO' ? 'goto' : 'find')
      return
    }
    if ((e.target as HTMLElement).closest('.code-term')) return
    if (!e.altKey || e.ctrlKey || e.metaKey || popup) return
    const act = {
      KeyW: !e.shiftKey && (() => store.active && close(store.active)),
      KeyT: e.shiftKey && reopen,
      BracketLeft: !e.shiftKey && (() => cycle(-1)),
      BracketRight: !e.shiftKey && (() => cycle(1)),
    }[e.code]
    if (!act) return
    e.preventDefault()
    e.stopPropagation()
    act()
  }
  const onKeyRef = useRef(onKey)
  useEffect(() => {
    onKeyRef.current = onKey
  })
  useEffect(() => {
    const listen = (e: KeyboardEvent) => onKeyRef.current(e)
    document.addEventListener('keydown', listen, true)
    return () => document.removeEventListener('keydown', listen, true)
  }, [])

  // Settings, which say which side each tool window is on. They are the
  // user's, in a file their profile shares, so they follow them to every
  // environment.
  const [settings, setSettings] = useState<Record<string, unknown>>({})
  useEffect(() => {
    client.settings().then(setSettings, () => {})
    return client.on('settings.changed', setSettings)
  }, [client])
  const sides = sidesFrom(settings)
  const moveTool = (name: ToolName, side: Side) => {
    if (sides[name] === side) return
    const next = { ...settings, toolWindows: { ...sides, [name]: side } }
    setSettings(next)
    client.saveSettings(next).catch(() => {})
  }

  const [shown, setShown] = useState({ left: true, right: true, terminal: true })
  const track = (name: keyof typeof shown) => (size: { asPercentage: number }) =>
    setShown((s) => (s[name] === size.asPercentage > 0 ? s : { ...s, [name]: size.asPercentage > 0 }))

  const outer = useDefaultLayout({ id: 'hangar.code.v', storage: localStorage })
  const sidePanel = (side: Side) => (side === 'left' ? leftPanel : rightPanel)

  // Which window each side shows: the one last chosen there, else the
  // first docked there.
  const [chosen, setChosen] = useState<Record<Side, ToolName | null>>({ left: null, right: null })
  const showing = (side: Side): ToolName | null => {
    const c = chosen[side]
    return c && sides[c] === side ? c : (toolNames.find((n) => sides[n] === side) ?? null)
  }
  const showTool = (name: ToolName) => {
    const side = sides[name]
    setChosen((c) => ({ ...c, [side]: name }))
    sidePanel(side).current?.expand()
  }
  // A window's button shows it, or hides it if it is what its side shows.
  const toggleTool = (name: ToolName) => {
    const side = sides[name]
    if (showing(side) === name && shown[side]) sidePanel(side).current?.collapse()
    else showTool(name)
  }
  const hideTool = (name: ToolName) => sidePanel(sides[name]).current?.collapse()
  const dropTool = (name: ToolName, side: Side) => {
    moveTool(name, side)
    setChosen((c) => ({ ...c, [side]: name }))
  }

  const [dragging, setDragging] = useState<Dragging | null>(null)
  const toolButton = (name: ToolName) => (
    <StripeButton
      key={name}
      on={shown[sides[name]] && showing(sides[name]) === name}
      title={`${toolTitles[name]} (drag to the other side to move it there)`}
      onClick={() => toggleTool(name)}
      onPointerDown={(e) => dragTool(name, e, setDragging, dropTool)}
    >
      {toolIcons[name]}
      {name === 'changes' && !!status?.changes.length && <span className="code-stripe-count">{status.changes.length}</span>}
    </StripeButton>
  )

  const toolBody = (name: ToolName) =>
    name === 'project' ? (
      <ProjectView
        client={client}
        env={scope}
        status={status}
        selected={activeTab?.path ?? null}
        reveal={reveal}
        onOpen={(path) => open({ kind: 'file', path })}
        onDiff={(path) => open({ kind: 'diff', path })}
        onTerminal={openTerminal}
        onGotoFile={() => setPopup('goto')}
        onOpenFolder={() => setPopup('folder')}
        onFolderHere={(dir) => onFolder(dir === client.home ? '' : dir)}
        onHide={() => hideTool('project')}
      />
    ) : name === 'docker' ? (
      <DockerView
        client={client}
        onLogs={(c) => open({ kind: 'logs', path: c.id, title: c.name })}
        onShell={(c) => openTerminal(client.root, containerShell(c.name))}
        onOpenFile={(abs) =>
          open({ kind: 'file', path: abs.startsWith(client.root + '/') ? abs.slice(client.root.length + 1) : abs })
        }
        onHide={() => hideTool('docker')}
      />
    ) : (
      <ChangesView
        client={client}
        status={status}
        reload={reloadStatus}
        onDiff={(path, against) => open({ kind: 'diff', path, ...against })}
        onOpen={(path) => open({ kind: 'file', path })}
        onHide={() => hideTool('changes')}
      />
    )

  // A side's panel holds every window docked there, showing one; the
  // others keep their state, hidden.
  const toolPanel = (side: Side) => {
    const names = toolNames.filter((n) => sides[n] === side)
    const current = showing(side)
    return (
      <Panel
        key={side}
        id={side}
        panelRef={sidePanel(side)}
        defaultSize={side === 'left' ? '22%' : '24%'}
        minSize="160px"
        collapsible
        collapsedSize={0}
        onResize={track(side)}
      >
        {names.map((n) => (
          <div key={n} className="code-side-slot" hidden={n !== current}>
            {toolBody(n)}
          </div>
        ))}
      </Panel>
    )
  }

  const left = toolNames.filter((n) => sides[n] === 'left')
  const right = toolNames.filter((n) => sides[n] === 'right')
  // The row's sizes are remembered for each set of sides in use.
  const arrangement = `${left.length > 0 ? 'left' : ''}|${right.length > 0 ? 'right' : ''}`

  return (
    <LanguagesContext.Provider value={languages}>
      <div className="code-page">
        <Stripe side="left" over={dragging?.over === 'left'}>
          {left.map(toolButton)}
          <span className="code-tool-spacer" />
          {hasDesktop && (
            <StripeButton
              on={store.active === 'desktop:desktop'}
              title="Desktop (connected while its tab is shown)"
              onClick={() => open({ kind: 'desktop', path: 'desktop', title: 'Desktop' })}
            >
              <Monitor size={16} />
            </StripeButton>
          )}
          <StripeButton on={false} title={`Find in Files (${shortcut('Mod+Shift+F')})`} onClick={() => setPopup('find')}>
            <TextSearch size={16} />
          </StripeButton>
          <StripeButton on={shown.terminal && bottom === 'find'} title="Find" onClick={() => toggleBottom('find')}>
            <ListTree size={16} />
          </StripeButton>
          <StripeButton on={shown.terminal && bottom === 'terminal'} title="Terminal" onClick={() => toggleBottom('terminal')}>
            <SquareTerminal size={16} />
          </StripeButton>
        </Stripe>
        <div className="code-main">
          <Group orientation="vertical" defaultLayout={outer.defaultLayout} onLayoutChanged={outer.onLayoutChanged}>
            <Panel id="top" minSize="30%">
              <ToolRow key={arrangement} arrangement={arrangement}>
                {left.length > 0 && [toolPanel('left'), <Separator key="left-sep" className="code-split code-split-v" />]}
                <Panel id="editors" minSize="25%">
                  <EditorArea
                    client={client}
                    tabs={store.tabs}
                    active={store.active}
                    opened={opened}
                    jump={jump}
                    user={user}
                    status={status}
                    canReopen={canReopen}
                    onActivate={(key) => setStore((s) => ({ ...s, active: key }))}
                    onMove={(key, to) =>
                      setStore((s) => {
                        const tab = s.tabs.find((t) => tabKey(t) === key)
                        if (!tab) return s
                        const tabs = s.tabs.filter((t) => t !== tab)
                        tabs.splice(to, 0, tab)
                        return { ...s, tabs }
                      })
                    }
                    onOpen={open}
                    onClose={close}
                    onCloseMany={closeTabs}
                    onReopen={reopen}
                    onReveal={revealInProject}
                  />
                </Panel>
                {right.length > 0 && [<Separator key="right-sep" className="code-split code-split-v" />, toolPanel('right')]}
              </ToolRow>
            </Panel>
            <Separator className="code-split code-split-h" />
            <Panel
              id="terminal"
              panelRef={terminal}
              defaultSize="30%"
              minSize="90px"
              collapsible
              collapsedSize={0}
              onResize={track('terminal')}
            >
              <div className="code-side-slot" hidden={bottom !== 'terminal'}>
                <CodeTerminal env={env} openIn={termIn} onHide={() => terminal.current?.collapse()} />
              </div>
              <div className="code-side-slot" hidden={bottom !== 'find'}>
                <UsagesView usages={usages} onJump={openAt} onHide={() => terminal.current?.collapse()} />
              </div>
            </Panel>
          </Group>
          {state !== 'live' && (
            <div className="code-status">
              {state === 'connecting' ? 'Connecting to the environment…' : `Reconnecting…${why ? ` (${why})` : ''}`}
              {folder && state === 'reconnecting' && (
                <button type="button" className="btn btn-ghost" onClick={() => onFolder('')}>
                  Back to the environment&rsquo;s folder
                </button>
              )}
            </div>
          )}
        </div>
        <Stripe side="right" over={dragging?.over === 'right'}>
          {right.map(toolButton)}
        </Stripe>
        {dragging && (
          <div className="code-drag-ghost" style={{ left: dragging.x + 12, top: dragging.y + 12 }}>
            {toolIcons[dragging.tool]}
            {toolTitles[dragging.tool]}
          </div>
        )}
        {(popup === 'goto' || popup === 'everywhere') && (
          <GotoFile
            client={client}
            title={popup === 'goto' ? 'Go to File' : 'Search Everywhere'}
            recent={recent}
            onOpen={openAt}
            onClose={() => setPopup(null)}
          />
        )}
        {popup === 'find' && <FindInFiles client={client} onOpen={openAt} onClose={() => setPopup(null)} />}
        {popup === 'folder' && <OpenFolder client={client} onChoose={onFolder} onClose={() => setPopup(null)} />}
      </div>
    </LanguagesContext.Provider>
  )
}

// ToolRow is the row of side panels and editors, its sizes remembered for
// each arrangement: a row with a panel on each side is not one with one.
function ToolRow({ arrangement, children }: { arrangement: string; children: ReactNode }) {
  const layout = useDefaultLayout({ id: `hangar.code.h.${arrangement}`, storage: localStorage })
  return (
    <Group orientation="horizontal" defaultLayout={layout.defaultLayout} onLayoutChanged={layout.onLayoutChanged}>
      {children}
    </Group>
  )
}

// Stripe is a column of buttons at the side of the Code tab, which a tool
// window's button being dragged can be dropped on.
function Stripe({ side, over, children }: { side: Side; over: boolean; children: ReactNode }) {
  return (
    <div className={over ? 'code-stripe code-stripe-drop' : 'code-stripe'} data-side={side}>
      {children}
    </div>
  )
}

// Dragging is a tool window's button being dragged to the other side: where
// the pointer is, and the stripe it is over.
type Dragging = { tool: ToolName; x: number; y: number; over: Side | null }

// dragTool follows the pointer from a press on a tool window's button. Once
// it has moved a few pixels it is a drag, shown by onMove, and dropped on a
// stripe by onDrop; a press that does not move is left to be a click.
function dragTool(
  tool: ToolName,
  e: React.PointerEvent,
  onMove: (d: Dragging | null) => void,
  onDrop: (tool: ToolName, side: Side) => void,
) {
  if (e.button !== 0) return
  const x0 = e.clientX
  const y0 = e.clientY
  let dragging = false
  const sideAt = (x: number, y: number) =>
    ((document.elementFromPoint(x, y)?.closest('.code-stripe') as HTMLElement | null)?.dataset.side as Side | undefined) ??
    null
  const move = (ev: PointerEvent) => {
    if (!dragging && Math.hypot(ev.clientX - x0, ev.clientY - y0) < 5) return
    dragging = true
    onMove({ tool, x: ev.clientX, y: ev.clientY, over: sideAt(ev.clientX, ev.clientY) })
  }
  const up = (ev: PointerEvent) => {
    document.removeEventListener('pointermove', move, true)
    document.removeEventListener('pointerup', up, true)
    if (!dragging) return
    onMove(null)
    const side = sideAt(ev.clientX, ev.clientY)
    if (side) onDrop(tool, side)
    // The click that ends a drag is not a click on the button.
    const swallow = (c: MouseEvent) => {
      c.stopPropagation()
      c.preventDefault()
    }
    document.addEventListener('click', swallow, { capture: true, once: true })
    setTimeout(() => document.removeEventListener('click', swallow, true), 0)
  }
  document.addEventListener('pointermove', move, true)
  document.addEventListener('pointerup', up, true)
}

function StripeButton({
  on,
  title,
  onClick,
  onPointerDown,
  children,
}: {
  on: boolean
  title: string
  onClick: () => void
  onPointerDown?: (e: React.PointerEvent) => void
  children: ReactNode
}) {
  return (
    <button
      type="button"
      className={on ? 'code-stripe-btn code-stripe-btn-on' : 'code-stripe-btn'}
      title={title}
      aria-label={title}
      aria-pressed={on}
      data-movable={onPointerDown ? '' : undefined}
      onPointerDown={onPointerDown}
      onClick={onClick}
    >
      {children}
    </button>
  )
}
