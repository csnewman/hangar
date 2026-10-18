import './code.css'

import {
  Activity,
  CircleAlert,
  Container,
  FolderTree,
  GitBranch,
  GitCompare,
  GitGraph,
  HardDrive,
  ListTree,
  Monitor,
  ScrollText,
  SignalHigh,
  SignalLow,
  SignalMedium,
  SquareTerminal,
  TableOfContents,
  TextSearch,
} from 'lucide-react'
import { createContext, useContext, useEffect, useMemo, useRef, useState, type ReactNode } from 'react'
import { Group, Panel, Separator, useDefaultLayout, usePanelRef } from 'react-resizable-panels'
import { useSearchParams } from 'react-router'

import type { Environment, EnvironmentLogName } from '../api'
import { useEnv } from '../pages/Environment'
import { usePhone } from '../phone'
import { useMe } from '../session'
import { StartProgress } from '../components/StartProgress'
import { environmentTab, nameTab, useWorkspaceTab, type WorkspaceShow } from '../workspaceLinks'
import { ChangesView } from './ChangesView'
import { ComputerView } from './ComputerView'
import { WorkspaceView } from './WorkspaceView'
import { CodeClient, type ConnectionState, type GitStatus } from './client'
import { CodeTerminal } from './CodeTerminal'
import { isMac, shortcut, useContextMenu, type MenuItem } from './ContextMenu'
import { containerShell, DockerView } from './DockerView'
import { EditorArea, tabKey, type Tab } from './EditorArea'
import { EditorStatusContext, EditorStatusStore } from './editorStatus'
import { EnvironmentView } from './EnvironmentView'
import { PerformanceView } from './PerformanceView'
import { ProjectView } from './ProjectView'
import { Languages, LanguagesContext, type Usages } from './lsp'
import { GitLog } from './GitLog'
import { useLatency } from './latency'
import { LogsView } from './LogsView'
import { Maximise } from './maximise'
import { PhoneCode } from './PhoneCode'
import { counts, ProblemsView, useProblems } from './ProblemsView'
import { repoFor } from './repos'
import { FindInFiles, GotoFile, GotoSymbol, OpenFolder, type Jump } from './Search'
import { EditorItems, StatusBar } from './StatusBar'
import { StructureView } from './StructureView'
import { UsagesView } from './UsagesView'
import { ProjectGit } from './useGitStatus'
import { locate, useProjects, useWorkspace, type Project } from './projects'

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

// Workspace is an environment's page on a computer: its files, terminals,
// desktop and logs as the tabs and tool windows of one workspace, with a
// status bar along its foot. While the environment is not running there
// are no files or terminals to show, and the workspace offers to start it,
// beside its details and its logs.
export function Workspace({ env }: { env: Environment }) {
  const [params, setParams] = useSearchParams()
  const [ask, setAsk] = useState<WorkspaceAsk | null>(() =>
    params.get('show') === 'environment' ? { show: 'environment', n: 1 } : null,
  )
  useEffect(() => {
    nameTab(environmentTab(env.id))
  }, [env.id])
  // The ask in the address is taken once, not again at each reload.
  useEffect(() => {
    if (params.has('show')) setParams({}, { replace: true })
  }, [params, setParams])
  useWorkspaceTab(env.id, (show) => setAsk((a) => ({ show, n: (a?.n ?? 0) + 1 })))
  return (
    <AskContext.Provider value={ask}>
      {env.phase !== 'running' ? <Idle env={env} /> : <Connected key={env.id} env={env.id} />}
    </AskContext.Provider>
  )
}

// WorkspaceAsk is what the workspace has been asked to show, by its address
// or by another tab; n counts the asks, so asking twice shows twice.
type WorkspaceAsk = { show: WorkspaceShow; n: number }

const AskContext = createContext<WorkspaceAsk | null>(null)

// Idle is the workspace of an environment that is not running: its own
// page, which says what it is doing and starts it, and its logs, which its
// worker keeps whatever the machine is doing.
function Idle({ env }: { env: Environment }) {
  const logsPanel = usePanelRef()
  const [logsShown, setLogsShown] = useState(true)
  const starting = env.phase === 'starting' || env.phase === 'pending'
  // A start shows its console; otherwise the worker's account of the last.
  const [show, setShow] = useState<{ log: EnvironmentLogName; n: number }>({
    log: starting ? 'console' : 'worker',
    n: 0,
  })
  const [wasStarting, setWasStarting] = useState(starting)
  if (starting !== wasStarting) {
    setWasStarting(starting)
    if (starting) setShow((s) => ({ log: 'console', n: s.n + 1 }))
  }
  const showLogs = () => {
    logsPanel.current?.expand()
    setLogsShown(true)
  }
  const layout = useDefaultLayout({ id: 'hangar.idle.v', storage: localStorage })

  return (
    <div className="workspace">
      <div className="workspace-body">
        <Group orientation="vertical" defaultLayout={layout.defaultLayout} onLayoutChanged={layout.onLayoutChanged}>
          <Panel id="idle" minSize="30%">
            <EnvironmentView env={env} lead={<IdleLead env={env} onLogs={showLogs} />} />
          </Panel>
          <Separator className="code-split code-split-h" />
          <Panel
            id="logs"
            panelRef={logsPanel}
            defaultSize="35%"
            minSize="90px"
            collapsible
            collapsedSize={0}
            onResize={(size) => setLogsShown(size.asPercentage > 0)}
          >
            <LogsView env={env.id} show={show} onHide={() => logsPanel.current?.collapse()} />
          </Panel>
        </Group>
      </div>
      <StatusBar
        env={env}
        right={
          !logsShown && (
            <button type="button" className="code-statusbar-item" onClick={showLogs}>
              <ScrollText size={12} />
              Logs
            </button>
          )
        }
      />
    </div>
  )
}

// IdleLead says how a start, stop or deletion is going, or why a start
// failed; a stopped or suspended environment's page needs nothing above it.
function IdleLead({ env, onLogs }: { env: Environment; onLogs: () => void }) {
  const phase = env.phase
  if (env.desired === 'deleted' || phase === 'deleting') {
    return (
      <section className="envtab-lead">
        <h3>Deleting {env.name}</h3>
      </section>
    )
  }
  if (phase === 'starting' || phase === 'pending') {
    return (
      <section className="envtab-lead">
        <h3>Starting {env.name}</h3>
        <StartProgress env={env} onLogs={onLogs} />
        <p className="muted small">Your tabs and tool windows come back once its editor service answers.</p>
      </section>
    )
  }
  if (phase === 'stopping' || phase === 'suspending') {
    return (
      <section className="envtab-lead">
        <h3>
          {phase === 'stopping' ? 'Stopping' : 'Suspending'} {env.name}
        </h3>
        {env.reason && <p className="muted">{env.reason}</p>}
      </section>
    )
  }
  if (phase === 'failed') {
    return (
      <section className="envtab-lead">
        <h3>{env.name} did not start</h3>
        <p>{env.reason ?? 'Its logs say why.'}</p>
        <div>
          <button type="button" className="btn btn-ghost btn-sm" onClick={onLogs}>
            Show its logs
          </button>
        </div>
      </section>
    )
  }
  return null
}

// Connected holds the tab's connection to the environment. It is made in an
// effect, not during render, so React's development double mount makes and
// closes a spare one rather than closing the one in use.
//
// The connection works in the environment's folder, unless this browser tab
// has opened another, which it remembers for itself alone. On a phone it
// shows PhoneCode.
function Connected({ env }: { env: string }) {
  const phone = usePhone()
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
  if (phone) return <PhoneCode client={client} />
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

// The editors: one group of tabs, or two side by side, and which of them
// is in use, where what is opened goes.
type TabGroup = { tabs: Tab[]; active: string | null }
type Store = { groups: TabGroup[]; focus: number }

const noGroup: TabGroup = { tabs: [], active: null }

// readStore is the tabs remembered, of either shape: one group, as they
// were once kept, or the groups.
function readStore(raw: string | null): Store {
  try {
    const s = JSON.parse(raw ?? '')
    if (Array.isArray(s.groups) && s.groups.length && s.groups.every((g: TabGroup) => Array.isArray(g?.tabs))) {
      const groups = (s.groups as TabGroup[]).slice(0, 2)
      return { groups, focus: s.focus === 1 && groups.length > 1 ? 1 : 0 }
    }
    if (Array.isArray(s.tabs)) return { groups: [{ tabs: s.tabs, active: s.active ?? null }], focus: 0 }
  } catch {
    // Nothing remembered.
  }
  return { groups: [noGroup], focus: 0 }
}

// withGroup changes one group of tabs. A second group left with none is
// gone, the other taking the whole width.
function withGroup(s: Store, g: number, fn: (x: TabGroup) => TabGroup): Store {
  const groups = s.groups.map((x, i) => (i === g ? fn(x) : x))
  if (groups.length > 1 && groups.some((x) => x.tabs.length === 0)) {
    const kept = groups.filter((x) => x.tabs.length > 0)
    return { groups: kept.length ? kept : [noGroup], focus: 0 }
  }
  return groups.every((x, i) => x === s.groups[i]) ? s : { groups, focus: s.focus }
}

// without closes tabs in a group. When the active one goes, the nearest
// survivor to its right, else its left, takes its place.
function without(g: TabGroup, gone: Set<string>): TabGroup {
  const tabs = g.tabs.filter((t) => !gone.has(tabKey(t)))
  if (tabs.length === g.tabs.length) return g
  let active = g.active
  if (active && gone.has(active)) {
    const i = g.tabs.findIndex((t) => tabKey(t) === active)
    const next =
      g.tabs.slice(i + 1).find((t) => !gone.has(tabKey(t))) ??
      g.tabs
        .slice(0, i)
        .reverse()
        .find((t) => !gone.has(tabKey(t)))
    active = next ? tabKey(next) : null
  }
  return { tabs, active }
}

// withTab opens a tab in a group, or shows it if it is open there.
const withTab = (g: TabGroup, tab: Tab): TabGroup => ({
  tabs: g.tabs.some((t) => tabKey(t) === tabKey(tab)) ? g.tabs : [...g.tabs, tab],
  active: tabKey(tab),
})

// A tool window that moves between the sides, and where it is docked: a
// side, and the top or the bottom of it. A side shows one window of each
// half at a time, one over the other when both are open, as IntelliJ's
// sides do.
type ToolName = 'project' | 'structure' | 'computer' | 'changes' | 'docker' | 'performance'
type Bottom = 'terminal' | 'find' | 'problems' | 'log' | 'logs'
type Side = 'left' | 'right'
type Slot = 'top' | 'bottom'
type Dock = { side: Side; slot: Slot }
type Docks = Record<ToolName, Dock>
const defaultDocks: Docks = {
  project: { side: 'left', slot: 'top' },
  structure: { side: 'left', slot: 'top' },
  computer: { side: 'left', slot: 'bottom' },
  changes: { side: 'right', slot: 'top' },
  docker: { side: 'right', slot: 'top' },
  performance: { side: 'right', slot: 'top' },
}
const toolNames: ToolName[] = ['project', 'structure', 'computer', 'changes', 'docker', 'performance']
const toolTitles: Record<ToolName, string> = {
  project: 'Workspace',
  structure: 'Structure',
  computer: 'Computer',
  changes: 'Changes',
  docker: 'Docker',
  performance: 'Performance',
}
const toolIcons: Record<ToolName, ReactNode> = {
  project: <FolderTree size={16} />,
  structure: <TableOfContents size={16} />,
  computer: <HardDrive size={16} />,
  changes: <GitCompare size={16} />,
  docker: <Container size={16} />,
  performance: <Activity size={16} />,
}

// docksFrom reads where each window is docked from the settings: a side,
// with -bottom for the bottom half.
function docksFrom(settings: Record<string, unknown>): Docks {
  const tw = settings.toolWindows as Partial<Record<ToolName, unknown>> | undefined
  const out = { ...defaultDocks }
  for (const n of toolNames) {
    const m = typeof tw?.[n] === 'string' ? /^(left|right)(-bottom)?$/.exec(tw[n] as string) : null
    if (m) out[n] = { side: m[1] as Side, slot: m[2] ? 'bottom' : 'top' }
  }
  return out
}

const dockName = (d: Dock) => (d.slot === 'bottom' ? `${d.side}-bottom` : d.side)

type Popup = 'goto' | 'everywhere' | 'find' | 'folder' | 'symbol' | null

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
  const environment = useEnv()
  const hasDesktop = environment.spec.display !== 'none'
  // What this tab remembers -- open tabs, open folders -- is kept per
  // folder.
  const scope = folder ? `${env}:${folder}` : env
  const me = useMe()
  const user = useMemo(() => ({ name: me.display_name || me.username, color: colourFor(me.username) }), [me])

  const [state, setState] = useState<ConnectionState>(client.state)
  const [why, setWhy] = useState('')
  useEffect(() => client.on('state', (s, w) => { setState(s); setWhy(w) }), [client])

  const latency = useLatency(client, state === 'live')
  // What the editor shown says of its file, for the status bar.
  const [editorStatus] = useState(() => new EditorStatusStore())

  // Each project's git status, reported by its ProjectGit as files or the
  // repositories change.
  const [gits, setGits] = useState<Record<string, { status: GitStatus | null; reload: () => void }>>({})

  // Tool windows: a panel on each side, showing one of the windows docked
  // there at a time, as IntelliJ's sides do, and the terminal below.
  const leftPanel = usePanelRef()
  const rightPanel = usePanelRef()
  const terminal = usePanelRef()
  // The panel above the bottom one, folded away when the bottom one is
  // maximised.
  const top = usePanelRef()
  const [maximised, setMaximised] = useState(false)
  const restore = () => {
    if (top.current?.isCollapsed()) top.current.expand()
  }

  // Open tabs, remembered per environment.
  const storeKey = `hangar.code.${scope}.tabs`
  const [store, setStore] = useState<Store>(() => {
    try {
      return readStore(localStorage.getItem(storeKey))
    } catch {
      return readStore(null)
    }
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
  // open opens a tab in the group in use.
  // place is where a path is: a path from / is in the project it is in,
  // if any, and the tab's own folder is named by no root.
  const place = (path: string, root?: string): { path: string; root?: string } => {
    let at: { path: string; root?: string } = { path, root }
    if (path.startsWith('/') && !root) at = locate(projectsRef.current, path)
    return at.root === client.root ? { path: at.path } : at
  }
  // open opens a tab in the group in use.
  const open = (tab: Tab) => {
    restore()
    const { root: _, ...rest } = tab
    const t: Tab = tab.kind === 'logs' || tab.kind === 'desktop' ? tab : { ...rest, ...place(tab.path, tab.root) }
    setStore((s) => withGroup(s, s.focus, (g) => withTab(g, t)))
    setOpened((n) => n + 1)
  }
  // Closed tabs, most recent last, for reopening.
  const closed = useRef<Tab[]>([])
  const [canReopen, setCanReopen] = useState(false)
  // closeTabs closes several tabs of a group at once, by default the
  // group in use.
  const closeTabs = (keys: string[], group?: number) => {
    const gone = new Set(keys)
    setStore((s) => {
      const g = group ?? s.focus
      const was = s.groups[g]
      if (!was) return s
      const next = withGroup(s, g, (x) => without(x, gone))
      if (next !== s) {
        closed.current.push(...was.tabs.filter((t) => gone.has(tabKey(t))).reverse())
        closed.current = closed.current.slice(-30)
      }
      return next
    })
    setCanReopen(true)
  }
  const close = (key: string, group?: number) => closeTabs([key], group)
  // split shows a tab in the other group, making it if there is one group,
  // and moves it there rather than copying it if asked.
  const split = (from: number, tab: Tab, move: boolean) =>
    setStore((s) => {
      const to = s.groups.length > 1 ? 1 - from : 1
      const groups = s.groups.length > 1 ? [...s.groups] : [...s.groups, noGroup]
      groups[to] = withTab(groups[to], tab)
      const next = { groups, focus: to }
      return move ? withGroup(next, from, (g) => without(g, new Set([tabKey(tab)]))) : next
    })
  // moveTab moves a tab dragged from one group into another, at an index
  // among its tabs or at its end; to a group past the last, it splits.
  const moveTab = (from: number, key: string, to: number, index?: number) =>
    setStore((s) => {
      const tab = s.groups[from]?.tabs.find((t) => tabKey(t) === key)
      if (!tab) return s
      const groups = to < s.groups.length ? [...s.groups] : [...s.groups, noGroup]
      const rest = groups[to].tabs.filter((t) => tabKey(t) !== key)
      const at = Math.min(index ?? rest.length, rest.length)
      groups[to] = { tabs: [...rest.slice(0, at), tab, ...rest.slice(at)], active: key }
      return withGroup({ groups, focus: to }, from, (g) => without(g, new Set([key])))
    })
  // unsplit gathers both groups' tabs into one.
  const unsplit = () =>
    setStore((s) => {
      if (s.groups.length < 2) return s
      const [a, b] = s.groups
      const keys = new Set(a.tabs.map(tabKey))
      return {
        groups: [{ tabs: [...a.tabs, ...b.tabs.filter((t) => !keys.has(tabKey(t)))], active: s.groups[s.focus].active }],
        focus: 0,
      }
    })
  const focusGroup = (g: number) => setStore((s) => (s.focus === g || g >= s.groups.length ? s : { ...s, focus: g }))
  const reopen = () => {
    const tab = closed.current.pop()
    setCanReopen(closed.current.length > 0)
    if (tab) open(tab)
  }
  const cycle = (by: number) =>
    setStore((s) =>
      withGroup(s, s.focus, (g) => {
        if (g.tabs.length === 0) return g
        const i = g.tabs.findIndex((t) => tabKey(t) === g.active)
        return { ...g, active: tabKey(g.tabs[(i + by + g.tabs.length) % g.tabs.length]) }
      }),
    )
  const focused = store.groups[store.focus] ?? store.groups[0]
  const activeTab = focused.tabs.find((t) => tabKey(t) === focused.active)

  // Reveal in Project: the tree opens the folders to a path and selects it.
  const [reveal, setReveal] = useState<{ path: string; root?: string; n: number } | null>(null)
  const revealInProject = (path: string, root?: string) => {
    const at = place(path, root)
    if (at.path.startsWith('/')) {
      revealOnComputer(at.path)
      return
    }
    showTool('project')
    setReveal((r) => ({ ...at, n: (r?.n ?? 0) + 1 }))
  }
  // Reveal on Computer: Computer shows the folder a path is in.
  const [computerAt, setComputerAt] = useState<{ path: string; n: number } | null>(null)
  const revealOnComputer = (abs: string) => {
    showTool('computer')
    setComputerAt((r) => ({ path: abs, n: (r?.n ?? 0) + 1 }))
  }

  // Open in Terminal: a new session in a folder.
  const [termIn, setTermIn] = useState<{ dir: string; run?: string; n: number } | null>(null)
  const openTerminal = (dir: string, run?: string) => {
    showBottom('terminal')
    setTermIn((t) => ({ dir, run, n: (t?.n ?? 0) + 1 }))
  }

  // Search: popups, and the place in a file one of them opens.
  const [popup, setPopup] = useState<Popup>(null)
  // Find in Files opened from the Project tree: in a folder, to replace.
  const [findIn, setFindIn] = useState<{ dir?: string; replace?: boolean; root?: string }>({})
  const findInFolder = (dir: string, replace: boolean, root?: string) => {
    setFindIn({ dir, replace, root })
    setPopup('find')
  }
  // A jump is for the editor of the group in use, not the other.
  const [jump, setJump] = useState<(Jump & { n: number; group: number }) | null>(null)
  const openAt = (j: Jump) => {
    const at = place(j.path, j.root)
    open({ kind: 'file', ...at })
    if (j.line > 0 || j.blame) setJump((x) => ({ ...j, ...at, root: at.root, n: (x?.n ?? 0) + 1, group: store.focus }))
  }
  // The bottom panel shows the terminal, the Find tool window or the Git
  // Log, keeping the others.
  const [bottom, setBottom] = useState<Bottom>('terminal')
  const showBottom = (w: Bottom) => {
    setBottom(w)
    fold('terminal', true)
  }
  // The Git Log reads the history once first shown, and is kept after. A
  // commit asked for elsewhere (the blame) is found and chosen in it.
  const [logSeen, setLogSeen] = useState(false)
  const [logFocus, setLogFocus] = useState<{ hash: string; repo?: string; n: number } | null>(null)
  const [logHistory, setLogHistory] = useState<{ path: string; n: number } | null>(null)
  // The project the Git Log shows, and Changes: chosen there, else the
  // one being worked in.
  const [logRoot, setLogRoot] = useState<string | null>(null)
  const [changesRoot, setChangesRoot] = useState<string | null>(null)
  const showHistory = (path: string, root?: string) => {
    setLogRoot(root ?? client.root)
    setLogHistory((h) => ({ path, n: (h?.n ?? 0) + 1 }))
    showBottom('log')
  }
  // moved has the tabs on paths that moved in a project follow them.
  const moved = (pairs: { from: string; to: string }[], root?: string) =>
    setStore((s) => {
      const r = root === client.root ? undefined : root
      const follow = (p: string) => {
        for (const { from, to } of pairs) {
          if (p === from) return to
          if (p.startsWith(from + '/')) return to + p.slice(from.length)
        }
        return p
      }
      const groups = s.groups.map((g) => {
        const tabs = g.tabs.map((t) =>
          t.root !== r ? t : { ...t, path: follow(t.path), ...(t.other ? { other: follow(t.other) } : {}) },
        )
        const was = g.tabs.findIndex((t) => tabKey(t) === g.active)
        return { tabs, active: was >= 0 ? tabKey(tabs[was]) : g.active }
      })
      return { ...s, groups }
    })
  // showCommit finds a commit of the repository a file is in.
  const showCommit = (hash: string, path: string, root?: string) => {
    const r = root ?? client.root
    setLogRoot(r)
    setLogFocus((f) => ({ hash, repo: repoFor(gits[r]?.status?.repos ?? [], path)?.path, n: (f?.n ?? 0) + 1 }))
    showBottom('log')
  }
  useEffect(() => {
    if (bottom === 'log') setLogSeen(true)
  }, [bottom])
  const toggleBottom = (w: Bottom) => {
    if (bottom === w && shown.terminal) fold('terminal', false)
    else showBottom(w)
  }
  const [usages, setUsages] = useState<Usages | null>(null)
  const maximise = useMemo(
    () => ({
      maximised,
      toggle: () => {
        if (maximised) {
          top.current?.expand()
          return
        }
        fold('terminal', true)
        top.current?.collapse()
      },
    }),
    // fold reads only refs.
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [maximised, top],
  )

  // Language servers, which open the files they point at as tabs, and list
  // usages in the Find tool window.
  const opener = useRef<{
    open: typeof open
    openAt: typeof openAt
    run: (command: string) => void
    showUsages: (u: Usages) => void
  } | null>(null)
  opener.current = {
    open,
    openAt,
    run: (command: string) => openTerminal(client.root, command),
    showUsages: (u: Usages) => {
      setUsages(u)
      showBottom('find')
    },
  }
  // Each project's language servers open what they point at in their
  // project, and list usages with it.
  const languagesFor = (c: CodeClient, root: string) => {
    const r = root === client.root ? undefined : root
    const inProject = (path: string) => (path.startsWith('/') ? undefined : r)
    return new Languages(c, {
      open: (path) => opener.current?.open({ kind: 'file', path, root: inProject(path) }),
      jump: (j) => opener.current?.openAt({ ...j, root: inProject(j.path) }),
      usages: (u) =>
        opener.current?.showUsages({ ...u, usages: u.usages.map((x) => ({ ...x, root: inProject(x.path) })) }),
      run: (command) => opener.current?.run(command),
    })
  }
  const languagesForRef = useRef(languagesFor)
  languagesForRef.current = languagesFor
  // The tab's own folder's servers, whether or not it is a project, for
  // files from / that are in none.
  const languages = useMemo(() => languagesForRef.current(client, client.root), [client])

  // The workspace's projects: the environment's, or this tab's folder
  // alone. Until the list is set it is the environment's folder and every
  // repository its template clones.
  const environmentRepos = environment.spec.repos.map((r) => r.path).filter(Boolean)
  const workspace = useWorkspace(client, folder !== '', environmentRepos)
  const projects = useProjects(client, env, workspace.present, (c, root) =>
    c === client ? languages : languagesForRef.current(c, root),
  )
  const projectsRef = useRef(projects)
  projectsRef.current = projects
  const projectOf = (root?: string) => projects.find((p) => p.root === (root ?? client.root))
  const clientFor = (root?: string) => projectOf(root)?.client ?? client
  const languagesOf = (root?: string) => projectOf(root)?.languages ?? languages
  const setFolders = workspace.set
  const addFolder = setFolders && ((dir: string) => setFolders([...workspace.folders, dir]).catch(() => {}))

  // The project being worked in: the one the active tab's file is in, else
  // the first.
  const activeRoot =
    activeTab && activeTab.kind !== 'logs' && !(activeTab.path.startsWith('/') && !activeTab.root)
      ? (activeTab.root ?? client.root)
      : undefined
  const currentProject = projectOf(activeRoot) ?? projects[0]
  const statusOf = (root?: string) => gits[root ?? client.root]?.status ?? null
  const changeCount = projects.reduce((n, p) => n + (gits[p.root]?.status?.changes.length ?? 0), 0)
  const changesProject = (changesRoot && projectOf(changesRoot)) || currentProject
  const logProject = (logRoot && projectOf(logRoot)) || currentProject
  const projectPicker = (value: string | undefined, onPick: (root: string) => void) =>
    projects.length > 1 && (
      <select
        className="code-project-picker"
        aria-label="Project"
        value={value}
        onChange={(e) => onPick(e.target.value)}
      >
        {projects.map((p) => (
          <option key={p.root} value={p.root}>
            {p.name}
          </option>
        ))}
      </select>
    )
  const rootOf = (p: Project) => (p.root === client.root ? undefined : p.root)
  // projectMenu is a project's own menu, at the top of its row's.
  const projectMenu = (p: Project, i: number): MenuItem[] => [
    {
      label: 'Move Up',
      disabled: !setFolders || i === 0,
      onSelect: () => {
        const f = [...workspace.folders]
        ;[f[i - 1], f[i]] = [f[i], f[i - 1]]
        setFolders?.(f).catch(() => {})
      },
    },
    {
      label: 'Move Down',
      disabled: !setFolders || i === projects.length - 1,
      onSelect: () => {
        const f = [...workspace.folders]
        ;[f[i + 1], f[i]] = [f[i], f[i + 1]]
        setFolders?.(f).catch(() => {})
      },
    },
    { label: 'Reveal on Computer', onSelect: () => revealOnComputer(p.root) },
    'separator',
    { label: 'Open Alone in This Tab', onSelect: () => onFolder(p.root === client.home ? '' : p.root) },
    {
      label: 'Remove from Workspace',
      detail: 'its files stay where they are',
      disabled: !setFolders || projects.length < 2,
      onSelect: () => setFolders?.(workspace.folders.filter((f) => f !== p.root)).catch(() => {}),
    },
  ]

  // projectLabel names the project a tab's file is in, where there are
  // several.
  const projectLabel = (t: Tab) =>
    projects.length > 1 && (t.kind === 'file' || t.kind === 'diff' || t.kind === 'compare') && !(t.path.startsWith('/') && !t.root)
      ? projectOf(t.root)?.name
      : undefined

  const problems = useProblems(projects.length ? projects.map((p) => p.languages) : [languages])
  const problemCount = counts(problems)

  const recent = useMemo(
    () => [
      ...new Set(
        store.groups
          .flatMap((g) => g.tabs)
          .reverse()
          .filter((t) => t.kind === 'file')
          .map((t) => JSON.stringify([t.path, t.root ?? ''])),
      ),
    ].map((k) => {
      const [path, root] = JSON.parse(k) as [string, string]
      return { path, root: root || undefined }
    }),
    [store.groups],
  )

  // Keys, chosen from those a browser leaves to the page: as IntelliJ,
  // Ctrl/Cmd+Shift+O goes to a file, Ctrl/Cmd+Alt+O to a symbol,
  // Ctrl/Cmd+Shift+F finds in files and
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
    if (mod && e.altKey && !e.shiftKey && e.code === 'KeyO') {
      e.preventDefault()
      e.stopPropagation()
      setPopup('symbol')
      return
    }
    if ((e.target as HTMLElement).closest('.code-term')) return
    if (!e.altKey || e.ctrlKey || e.metaKey || popup) return
    const act = {
      KeyW: !e.shiftKey && (() => focused.active && close(focused.active)),
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
  // The folders the person has pinned to Computer's places.
  const pinned = Array.isArray(settings.places)
    ? settings.places.filter((p): p is string => typeof p === 'string')
    : []
  const pin = (path: string, on: boolean) => {
    const places = on ? [...pinned.filter((p) => p !== path), path] : pinned.filter((p) => p !== path)
    const next = { ...settings, places }
    setSettings(next)
    client.saveSettings(next).catch(() => {})
  }
  const docks = docksFrom(settings)
  const sides = Object.fromEntries(toolNames.map((n) => [n, docks[n].side])) as Record<ToolName, Side>
  // The windows in the order their buttons are in, as dragged.
  const savedOrder = Array.isArray(settings.toolOrder)
    ? (settings.toolOrder as unknown[]).filter((n): n is ToolName => toolNames.includes(n as ToolName))
    : []
  const order = [...savedOrder, ...toolNames.filter((n) => !savedOrder.includes(n))]
  // moveTool docks a window, and puts its button before the one at index
  // among those docked there, or last.
  const moveTool = (name: ToolName, dock: Dock, index?: number) => {
    const here = order.filter((n) => n !== name && docks[n].side === dock.side && docks[n].slot === dock.slot)
    const before = index === undefined ? undefined : here[index]
    const rest = order.filter((n) => n !== name)
    const at = before ? rest.indexOf(before) : here.length ? rest.indexOf(here[here.length - 1]) + 1 : rest.length
    const toolOrder = [...rest.slice(0, at), name, ...rest.slice(at)]
    const toolWindows = Object.fromEntries(toolNames.map((n) => [n, dockName(n === name ? dock : docks[n])]))
    const next = { ...settings, toolWindows, toolOrder }
    setSettings(next)
    client.saveSettings(next).catch(() => {})
  }

  const [shown, setShown] = useState({ left: true, right: true, terminal: true })
  const track = (name: keyof typeof shown) => (size: { asPercentage: number }) =>
    setShown((s) => (s[name] === size.asPercentage > 0 ? s : { ...s, [name]: size.asPercentage > 0 }))
  // fold opens or closes a panel. The panels report only sizes dragged,
  // so whether it is open is read back once the change has taken effect.
  const fold = (name: keyof typeof shown, open: boolean) => {
    const ref = name === 'terminal' ? terminal : name === 'left' ? leftPanel : rightPanel
    if (open) ref.current?.expand()
    else ref.current?.collapse()
    setTimeout(() => {
      const isOpen = !(ref.current?.isCollapsed() ?? false)
      setShown((s) => (s[name] === isOpen ? s : { ...s, [name]: isOpen }))
    }, 0)
  }

  const outer = useDefaultLayout({ id: 'hangar.code.v', storage: localStorage })
  const sidePanel = (side: Side) => (side === 'left' ? leftPanel : rightPanel)

  // Which window each half of each side shows: the one last chosen there,
  // else the first docked there; and which halves are open. A side's panel
  // is folded away when neither is.
  const [chosen, setChosen] = useState<Record<string, ToolName | null>>({})
  const [halves, setHalves] = useState<Record<Side, Record<Slot, boolean>>>(() => {
    try {
      const h = JSON.parse(localStorage.getItem('hangar.code.halves') ?? '')
      if (h?.left && h?.right) return h
    } catch {
      // The halves open to begin with.
    }
    return { left: { top: true, bottom: false }, right: { top: true, bottom: false } }
  })
  useEffect(() => {
    try {
      localStorage.setItem('hangar.code.halves', JSON.stringify(halves))
    } catch {
      // Which halves are open is a convenience.
    }
  }, [halves])
  const docked = (side: Side, slot: Slot) => order.filter((n) => docks[n].side === side && docks[n].slot === slot)
  const showing = (side: Side, slot: Slot = 'top'): ToolName | null => {
    const c = chosen[`${side}-${slot}`]
    return c && docks[c].side === side && docks[c].slot === slot ? c : (docked(side, slot)[0] ?? null)
  }
  // halfOpen is whether a half of a side is shown: open, its side not
  // folded, and something docked there. A side shown with neither half
  // open shows its top, or its bottom if nothing is docked at the top.
  const halfOpen = (side: Side, slot: Slot) => {
    if (!shown[side] || docked(side, slot).length === 0) return false
    if (halves[side][slot]) return true
    const any = (['top', 'bottom'] as Slot[]).some((h) => halves[side][h] && docked(side, h).length > 0)
    return !any && (slot === 'top' || docked(side, 'top').length === 0)
  }
  const openOther = (side: Side, slot: Slot) => {
    const other: Slot = slot === 'top' ? 'bottom' : 'top'
    return halves[side][other] && docked(side, other).length > 0
  }
  const isShown = (name: ToolName) => halfOpen(docks[name].side, docks[name].slot) && showing(docks[name].side, docks[name].slot) === name
  const setHalf = (side: Side, slot: Slot, on: boolean) =>
    setHalves((h) => (h[side][slot] === on ? h : { ...h, [side]: { ...h[side], [slot]: on } }))
  const showTool = (name: ToolName) => {
    const { side, slot } = docks[name]
    setChosen((c) => ({ ...c, [`${side}-${slot}`]: name }))
    setHalf(side, slot, true)
    fold(side, true)
  }
  // hideTool closes a window's half, folding its side away if the other
  // half is closed too.
  const hideTool = (name: ToolName) => {
    const { side, slot } = docks[name]
    setHalf(side, slot, false)
    if (!openOther(side, slot)) fold(side, false)
  }
  // A window's button shows it, or hides it if it is what its half shows.
  const toggleTool = (name: ToolName) => {
    if (isShown(name)) hideTool(name)
    else showTool(name)
  }

  // The environment's own pages open as tabs.
  const showEnvironment = () => open({ kind: 'environment', path: 'environment', title: 'Environment' })
  const ask = useContext(AskContext)
  const showAsked = useRef(showEnvironment)
  useEffect(() => {
    showAsked.current = showEnvironment
  })
  useEffect(() => {
    if (ask?.show === 'environment') showAsked.current()
  }, [ask])
  const showPerformancePage = () => open({ kind: 'performance', path: 'performance', title: 'Performance' })

  const dropTool = (name: ToolName, to: Drop) => {
    const was = docks[name]
    moveTool(name, to, to.index)
    if (was.side === to.side && was.slot === to.slot) return
    // Moved, it is what its new half shows, as it was shown before.
    setChosen((c) => ({ ...c, [`${to.side}-${to.slot}`]: name }))
    if (isShown(name)) {
      setHalf(to.side, to.slot, true)
      fold(to.side, true)
    }
  }
  // A window's button's menu docks it elsewhere.
  const stripeMenu = useContextMenu()
  const dockMenu = (name: ToolName): MenuItem[] => {
    const { side, slot } = docks[name]
    const other: Side = side === 'left' ? 'right' : 'left'
    const dockTo = (d: Dock) => () => {
      moveTool(name, d)
      setChosen((c) => ({ ...c, [`${d.side}-${d.slot}`]: name }))
      setHalf(d.side, d.slot, true)
      fold(d.side, true)
    }
    return [
      { heading: toolTitles[name] },
      { label: 'Dock at the Top', checked: slot === 'top', onSelect: dockTo({ side, slot: 'top' }) },
      { label: 'Dock at the Bottom', checked: slot === 'bottom', onSelect: dockTo({ side, slot: 'bottom' }) },
      { label: other === 'left' ? 'Move to the Left' : 'Move to the Right', onSelect: dockTo({ side: other, slot }) },
      'separator',
      { label: isShown(name) ? 'Hide' : 'Show', onSelect: () => toggleTool(name) },
    ]
  }

  const [dragging, setDragging] = useState<Dragging | null>(null)
  const toolButton = (name: ToolName) => (
    <StripeButton
      key={name}
      on={isShown(name)}
      title={`${toolTitles[name]} (drag to the other side to move it there; right-click to dock it elsewhere)`}
      onClick={() => toggleTool(name)}
      onPointerDown={(e) => dragTool(name, e, setDragging, dropTool)}
      onContextMenu={(e) => stripeMenu.open(e, dockMenu(name))}
      tool={name}
    >
      {toolIcons[name]}
      {name === 'changes' && changeCount > 0 && <span className="code-stripe-count">{changeCount}</span>}
    </StripeButton>
  )

  // stripeZone is a half's buttons on its side's stripe: where a button
  // dragged is dropped to dock its window there, at the place marked.
  const stripeZone = (side: Side, slot: Slot) => {
    const over = dragging?.over && dragging.over.side === side && dragging.over.slot === slot ? dragging.over : null
    const names = docked(side, slot)
    // The button dragged keeps its place, faded, and the mark of where it
    // would go is drawn on its neighbour, taking no room: nothing moves
    // under the pointer as it goes.
    const others = names.filter((n) => n !== dragging?.tool)
    const before = over ? (others[over.index] ?? null) : undefined
    const after = before === null ? others[others.length - 1] : undefined
    return (
      <div
        className={['code-stripe-zone', over && 'code-stripe-zone-drop', dragging && 'code-stripe-zone-open']
          .filter(Boolean)
          .join(' ')}
        data-dock={`${side}:${slot}`}
      >
        {names.map((n) => (
          <span
            key={n}
            className={[
              'code-stripe-place',
              dragging?.tool === n && 'code-stripe-dragged',
              before === n && 'code-stripe-before',
              after === n && 'code-stripe-after',
            ]
              .filter(Boolean)
              .join(' ')}
          >
            {toolButton(n)}
          </span>
        ))}
      </div>
    )
  }

  const toolBody = (name: ToolName) =>
    name === 'project' ? (
      <WorkspaceView
        env={scope}
        projects={projects}
        missing={workspace.missing}
        onRemove={(root) => setFolders?.(workspace.folders.filter((f) => f !== root)).catch(() => {})}
        statuses={Object.fromEntries(projects.map((p) => [p.root, statusOf(p.root)]))}
        alone={folder !== ''}
        projectMenu={projectMenu}
        tree={(p, embedded) => {
          const r = rootOf(p)
          const mine = (activeTab?.root ?? undefined) === r && activeTab?.kind === 'file'
          return (
            <ProjectView
              key={p.root}
              client={p.client}
              env={r ? `${scope}:${p.root}` : scope}
              status={statusOf(p.root)}
              selected={mine ? (activeTab?.path ?? null) : null}
              reveal={reveal && reveal.root === r ? reveal : null}
              embedded={embedded}
              onOpen={(path, view, stay) => {
                open({ kind: 'file', path, view, root: r })
                if (stay)
                  setJump((x) => ({ path, root: r, line: 0, focus: false, n: (x?.n ?? 0) + 1, group: store.focus }))
              }}
              onTab={(tab) => open({ ...tab, root: r })}
              onDiff={(path) => open({ kind: 'diff', path, root: r })}
              onTerminal={openTerminal}
              onGotoFile={() => setPopup('goto')}
              onOpenFolder={() => setPopup('folder')}
              onFolderHere={(dir) => onFolder(dir === client.home ? '' : dir)}
              onFind={(dir, replace) => findInFolder(dir, replace, r)}
              onHistory={(path) => showHistory(path, p.root)}
              onAnnotate={(path) => openAt({ path, root: r, line: 0, blame: true })}
              onMoved={(pairs) => moved(pairs, r)}
              onHide={() => hideTool('project')}
            />
          )
        }}
        onAdd={() => setPopup('folder')}
        onGotoFile={() => setPopup('goto')}
        onLocate={() => activeTab && revealInProject(activeTab.path, activeTab.root)}
        onBack={() => onFolder('')}
        onHide={() => hideTool('project')}
      />
    ) : name === 'computer' ? (
      <ComputerView
        client={client}
        env={scope}
        projects={projects}
        pinned={pinned}
        onPin={pin}
        reveal={computerAt}
        onOpenFile={(abs) => open({ kind: 'file', path: abs })}
        onTerminal={(dir) => openTerminal(dir)}
        onAddToWorkspace={addFolder ?? undefined}
        onOpenAlone={(dir) => onFolder(dir === client.home ? '' : dir)}
        onHide={() => hideTool('computer')}
      />
    ) : name === 'structure' ? (
      <StructureView
        store={editorStatus}
        active={isShown('structure')}
        onJump={openAt}
        onHide={() => hideTool('structure')}
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
    ) : name === 'performance' ? (
      <PerformanceView
        env={environment}
        active={isShown('performance')}
        onMore={showPerformancePage}
        onHide={() => hideTool('performance')}
      />
    ) : changesProject ? (
      <ChangesView
        key={changesProject.root}
        client={changesProject.client}
        status={statusOf(changesProject.root)}
        reload={gits[changesProject.root]?.reload ?? (() => {})}
        projectPicker={projectPicker(changesProject.root, setChangesRoot)}
        onDiff={(path, against) => open({ kind: 'diff', path, ...against, root: rootOf(changesProject) })}
        onOpen={(path) => open({ kind: 'file', path, root: rootOf(changesProject) })}
        onHide={() => hideTool('changes')}
      />
    ) : null

  // A side's panel holds every window docked there, showing one of each
  // half, one over the other when both are open; the others keep their
  // state, hidden.
  const toolPanel = (side: Side) => {
    const top = halfOpen(side, 'top') ? showing(side, 'top') : null
    const bottom = halfOpen(side, 'bottom') ? showing(side, 'bottom') : null
    const slot = (names: ToolName[], current: ToolName | null) =>
      names.map((n) => (
        <div key={n} className="code-side-slot" hidden={n !== current}>
          {toolBody(n)}
        </div>
      ))
    const tops = docked(side, 'top')
    const bottoms = docked(side, 'bottom')
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
        {top && bottom ? (
          <SideHalves side={side} top={slot(tops, top)} bottom={slot(bottoms, bottom)} />
        ) : (
          slot([...tops, ...bottoms], top ?? bottom)
        )}
      </Panel>
    )
  }

  // editorGroup is one group of tabs and the editor of its chosen one.
  // Using either makes it the group in use.
  const editorGroup = (g: TabGroup, i: number) => (
    <EditorArea
      clientFor={clientFor}
      languagesFor={languagesOf}
      statusOf={statusOf}
      projectLabel={projectLabel}
      onOutside={{ reveal: revealOnComputer, add: addFolder ?? undefined }}
      tabs={g.tabs}
      active={g.active}
      opened={i === store.focus ? opened : 0}
      jump={jump?.group === i ? jump : null}
      user={user}
      canReopen={canReopen}
      split={{ groups: store.groups.length, focused: store.groups.length > 1 && i === store.focus }}
      onFocus={() => focusGroup(i)}
      onActivate={(key) => setStore((s) => ({ groups: s.groups.map((x, j) => (j === i ? { ...x, active: key } : x)), focus: i }))}
      onMove={(key, to) =>
        setStore((s) =>
          withGroup(s, i, (x) => {
            const tab = x.tabs.find((t) => tabKey(t) === key)
            if (!tab) return x
            const tabs = x.tabs.filter((t) => t !== tab)
            tabs.splice(to, 0, tab)
            return { ...x, tabs }
          }),
        )
      }
      onOpen={open}
      onClose={(key) => close(key, i)}
      onCloseMany={(keys) => closeTabs(keys, i)}
      onReopen={reopen}
      onReveal={revealInProject}
      onShowCommit={showCommit}
      onSplit={(tab, move) => split(i, tab, move)}
      onUnsplit={unsplit}
      group={i}
      onDropTab={(key, target) =>
        'split' in target ? moveTab(i, key, store.groups.length) : moveTab(i, key, target.group, target.index)
      }
    />
  )

  const left = toolNames.filter((n) => sides[n] === 'left')
  const right = toolNames.filter((n) => sides[n] === 'right')
  // The branch the folder's repository, or else its first, is on.
  const status = statusOf(currentProject?.root)
  const branch = status?.repos.find((r) => r.branch)
  // The row's sizes are remembered for each set of sides in use.
  const arrangement = `${left.length > 0 ? 'left' : ''}|${right.length > 0 ? 'right' : ''}`

  return (
    <LanguagesContext.Provider value={languages}>
      <div className="workspace">
        {projects.map((p) => (
          <ProjectGit
            key={p.root}
            client={p.client}
            onStatus={(st, reload) => setGits((g) => ({ ...g, [p.root]: { status: st, reload } }))}
          />
        ))}
        <div className="code-page">
          <Stripe side="left">
            {stripeZone('left', 'top')}
            <span className="code-stripe-gap" />
            {stripeZone('left', 'bottom')}
            <span className="code-tool-spacer" />
            {hasDesktop && (
              <StripeButton
                on={store.groups.some((g) => g.active === 'desktop:desktop')}
                title="Desktop (connected while its tab is shown)"
                onClick={() => open({ kind: 'desktop', path: 'desktop', title: 'Desktop' })}
              >
                <Monitor size={16} />
              </StripeButton>
            )}
            <StripeButton
              on={false}
              title={`Find in Files (${shortcut('Mod+Shift+F')})`}
              onClick={() => setPopup('find')}
            >
              <TextSearch size={16} />
            </StripeButton>
            <StripeButton on={shown.terminal && bottom === 'find'} title="Find" onClick={() => toggleBottom('find')}>
              <ListTree size={16} />
            </StripeButton>
            <StripeButton
              on={shown.terminal && bottom === 'problems'}
              title={`Problems: ${problemCount.errors} errors, ${problemCount.warnings} warnings`}
              onClick={() => toggleBottom('problems')}
            >
              <CircleAlert size={16} />
              {problemCount.errors + problemCount.warnings > 0 && (
                <span className={problemCount.errors ? 'code-stripe-count code-stripe-count-bad' : 'code-stripe-count'}>
                  {problemCount.errors || problemCount.warnings}
                </span>
              )}
            </StripeButton>
            <StripeButton on={shown.terminal && bottom === 'log'} title="Git Log" onClick={() => toggleBottom('log')}>
              <GitGraph size={16} />
            </StripeButton>
            <StripeButton on={shown.terminal && bottom === 'logs'} title="Logs" onClick={() => toggleBottom('logs')}>
              <ScrollText size={16} />
            </StripeButton>
            <StripeButton
              on={shown.terminal && bottom === 'terminal'}
              title="Terminal"
              onClick={() => toggleBottom('terminal')}
            >
              <SquareTerminal size={16} />
            </StripeButton>
          </Stripe>
          <div className="code-main">
            <Group orientation="vertical" defaultLayout={outer.defaultLayout} onLayoutChanged={outer.onLayoutChanged}>
              <Panel
                id="top"
                panelRef={top}
                minSize="30%"
                collapsible
                collapsedSize={0}
                onResize={(size) => setMaximised(size.asPercentage === 0)}
              >
                <ToolRow key={arrangement} arrangement={arrangement}>
                  {left.length > 0 && [
                    toolPanel('left'),
                    <Separator key="left-sep" className="code-split code-split-v" />,
                  ]}
                  <Panel id="editors" minSize="25%">
                    <EditorStatusContext.Provider value={editorStatus}>
                      {store.groups.length > 1 ? (
                        <EditorSplit>
                          {store.groups.map((g, i) => [
                            i > 0 && <Separator key="sep" className="code-split code-split-v" />,
                            <Panel key={i} id={`group-${i}`} minSize="15%">
                              {editorGroup(g, i)}
                            </Panel>,
                          ])}
                        </EditorSplit>
                      ) : (
                        editorGroup(store.groups[0], 0)
                      )}
                    </EditorStatusContext.Provider>
                  </Panel>
                  {right.length > 0 && [
                    <Separator key="right-sep" className="code-split code-split-v" />,
                    toolPanel('right'),
                  ]}
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
                <Maximise.Provider value={maximise}>
                  <div className="code-side-slot" hidden={bottom !== 'terminal'}>
                    <CodeTerminal env={env} openIn={termIn} onHide={() => fold('terminal', false)} />
                  </div>
                  <div className="code-side-slot" hidden={bottom !== 'find'}>
                    <UsagesView
                      clientFor={clientFor}
                      usages={usages}
                      onJump={openAt}
                      onHide={() => fold('terminal', false)}
                    />
                  </div>
                  <div className="code-side-slot" hidden={bottom !== 'problems'}>
                    <ProblemsView problems={problems} onJump={openAt} onHide={() => fold('terminal', false)} />
                  </div>
                  {logSeen && (
                    <div className="code-side-slot" hidden={bottom !== 'log'}>
                      {logProject && (
                        <GitLog
                          key={logProject.root}
                          client={logProject.client}
                          repos={statusOf(logProject.root)?.repos ?? []}
                          focus={logFocus}
                          history={logHistory}
                          projectPicker={projectPicker(logProject.root, setLogRoot)}
                          onDiff={(path, against) => open({ kind: 'diff', path, ...against, root: rootOf(logProject) })}
                          onHide={() => fold('terminal', false)}
                        />
                      )}
                    </div>
                  )}
                  {bottom === 'logs' && (
                    <div className="code-side-slot">
                      <LogsView env={env} show={null} onHide={() => fold('terminal', false)} />
                    </div>
                  )}
                </Maximise.Provider>
              </Panel>
            </Group>
          </div>
          <Stripe side="right">
            {stripeZone('right', 'top')}
            <span className="code-stripe-gap" />
            {stripeZone('right', 'bottom')}
          </Stripe>
          {stripeMenu.menu}
          {dragging && (
            <div className="code-drag-ghost" style={{ left: dragging.x + 12, top: dragging.y + 12 }}>
              {toolIcons[dragging.tool]}
              {toolTitles[dragging.tool]}
            </div>
          )}
          {(popup === 'goto' || popup === 'everywhere') && (
            <GotoFile
              client={client}
              projects={projects}
              title={popup === 'goto' ? 'Go to File' : 'Search Everywhere'}
              recent={recent}
              onOpen={openAt}
              onClose={() => setPopup(null)}
            />
          )}
          {popup === 'find' && (
            <FindInFiles
              client={clientFor(findIn.root)}
              projects={projects}
              root={findIn.root ?? (findIn.dir !== undefined ? client.root : undefined)}
              dir={findIn.dir}
              replace={findIn.replace}
              onOpen={openAt}
              onFindWindow={(u) => {
                setUsages(u)
                showBottom('find')
              }}
              onClose={() => {
                setPopup(null)
                setFindIn({})
              }}
            />
          )}
          {popup === 'symbol' && (
            <GotoSymbol
              languages={projects.length ? projects.map((p) => p.languages) : [languages]}
              place={(abs) => place(abs)}
              onOpen={openAt}
              onClose={() => setPopup(null)}
            />
          )}
          {popup === 'folder' && (
            <OpenFolder
              client={client}
              onChoose={onFolder}
              onAdd={addFolder ?? undefined}
              have={workspace.folders}
              onClose={() => setPopup(null)}
            />
          )}
        </div>
        <StatusBar
          env={environment}
          onEnvironment={showEnvironment}
          onPerformance={() => showTool('performance')}
          right={
            <>
              <EditorItems store={editorStatus} client={client} languages={languages} />
              {latency !== null && <Latency ms={latency} />}
            </>
          }
        >
          {branch && (
            <button
              type="button"
              className="code-statusbar-item"
              title={`${branch.name}: ${status?.repos.length === 1 ? 'its changes' : 'the repositories and their changes'}`}
              onClick={() => showTool('changes')}
            >
              <GitBranch size={12} />
              <span className="code-statusbar-branch">{branch.branch}</span>
              {!!branch.ahead && <span>↑{branch.ahead}</span>}
              {!!branch.behind && <span>↓{branch.behind}</span>}
              {(status?.repos.length ?? 0) > 1 && <span className="muted">+{(status?.repos.length ?? 1) - 1}</span>}
            </button>
          )}
          {state !== 'live' && (
            <span className="code-statusbar-warn" role="status">
              {state === 'connecting' ? 'Connecting to the environment…' : `Reconnecting…${why ? ` (${why})` : ''}`}
              {folder && state === 'reconnecting' && (
                <button type="button" className="code-statusbar-item" onClick={() => onFolder('')}>
                  Back to the environment&rsquo;s folder
                </button>
              )}
            </span>
          )}
        </StatusBar>
      </div>
    </LanguagesContext.Provider>
  )
}

// Latency is how long the way to the environment and back takes, coloured
// by how it will feel to type over it.
function Latency({ ms }: { ms: number }) {
  const [tone, Icon] = ms < 150 ? ['good', SignalHigh] : ms < 400 ? ['busy', SignalMedium] : ['bad', SignalLow]
  return (
    <span
      className={`code-statusbar-info tone-${tone}`}
      title="How long a request takes to reach the environment and come back, through Hangar's server and its worker"
    >
      <Icon size={13} />
      {Math.round(ms)} ms
    </span>
  )
}

// SideHalves lays a side's two open halves one over the other, the split
// between them remembered for each side.
function SideHalves({ side, top, bottom }: { side: Side; top: ReactNode; bottom: ReactNode }) {
  const layout = useDefaultLayout({ id: `hangar.code.halves.${side}`, storage: localStorage })
  return (
    <Group orientation="vertical" defaultLayout={layout.defaultLayout} onLayoutChanged={layout.onLayoutChanged}>
      <Panel id="top" minSize="80px">
        {top}
      </Panel>
      <Separator className="code-split code-split-h" />
      <Panel id="bottom" minSize="80px">
        {bottom}
      </Panel>
    </Group>
  )
}

// EditorSplit lays two groups of editors side by side, the split between
// them remembered.
function EditorSplit({ children }: { children: ReactNode }) {
  const layout = useDefaultLayout({ id: 'hangar.code.split', storage: localStorage })
  return (
    <Group orientation="horizontal" defaultLayout={layout.defaultLayout} onLayoutChanged={layout.onLayoutChanged}>
      {children}
    </Group>
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
function Stripe({ side, children }: { side: Side; children: ReactNode }) {
  return (
    <div className="code-stripe" data-side={side}>
      {children}
    </div>
  )
}

// Drop is where a tool window's button would be dropped: a half of a side,
// and the place among the buttons docked there.
type Drop = Dock & { index: number }

// Dragging is a tool window's button being dragged: where the pointer is,
// and where the button would be dropped.
type Dragging = { tool: ToolName; x: number; y: number; over: Drop | null }

// dropAt is where on the stripes a point is: the half of the side whose
// buttons it is among (above the line between them, the top; below, the
// bottom), and before which of them, other than the one dragged.
function dropAt(tool: ToolName, x: number, y: number): Drop | null {
  const stripe = document.elementFromPoint(x, y)?.closest<HTMLElement>('.code-stripe')
  if (!stripe) return null
  const zones = [...stripe.querySelectorAll<HTMLElement>('[data-dock]')]
  const zone =
    zones.find((z) => {
      const r = z.getBoundingClientRect()
      return y >= r.top && y <= r.bottom
    }) ?? (zones.length > 1 && y > zones[1].getBoundingClientRect().top - 6 ? zones[1] : zones[0])
  if (!zone) return null
  const [side, slot] = zone.dataset.dock!.split(':') as [Side, Slot]
  const buttons = [...zone.querySelectorAll<HTMLElement>('[data-tool]')].filter((b) => b.dataset.tool !== tool)
  const i = buttons.findIndex((b) => {
    const r = b.getBoundingClientRect()
    return y < r.top + r.height / 2
  })
  return { side, slot, index: i < 0 ? buttons.length : i }
}

// dragTool follows the pointer from a press on a tool window's button. Once
// it has moved a few pixels it is a drag, shown by onMove, and dropped on a
// half of a stripe by onDrop; a press that does not move is left to be a
// click.
function dragTool(
  tool: ToolName,
  e: React.PointerEvent,
  onMove: (d: Dragging | null) => void,
  onDrop: (tool: ToolName, to: Drop) => void,
) {
  if (e.button !== 0) return
  const x0 = e.clientX
  const y0 = e.clientY
  let dragging = false
  const move = (ev: PointerEvent) => {
    if (!dragging && Math.hypot(ev.clientX - x0, ev.clientY - y0) < 5) return
    dragging = true
    onMove({ tool, x: ev.clientX, y: ev.clientY, over: dropAt(tool, ev.clientX, ev.clientY) })
  }
  const up = (ev: PointerEvent) => {
    document.removeEventListener('pointermove', move, true)
    document.removeEventListener('pointerup', up, true)
    if (!dragging) return
    onMove(null)
    const to = dropAt(tool, ev.clientX, ev.clientY)
    if (to) onDrop(tool, to)
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
  onContextMenu,
  tool,
  children,
}: {
  // tool names the window a button is for, which can be dragged.
  tool?: ToolName
  on: boolean
  title: string
  onClick: () => void
  onPointerDown?: (e: React.PointerEvent) => void
  onContextMenu?: (e: React.MouseEvent) => void
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
      data-tool={tool}
      onPointerDown={onPointerDown}
      onContextMenu={
        onContextMenu &&
        ((e) => {
          e.preventDefault()
          onContextMenu(e)
        })
      }
      onClick={onClick}
    >
      {children}
    </button>
  )
}
