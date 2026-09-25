import './code.css'

import { FolderTree, GitCompare, SquareTerminal } from 'lucide-react'
import { useCallback, useEffect, useMemo, useRef, useState, type ReactNode } from 'react'
import {
  Group,
  Panel,
  Separator,
  useDefaultLayout,
  usePanelRef,
  type PanelImperativeHandle,
} from 'react-resizable-panels'

import { useEnv } from '../pages/Environment'
import { useMe } from '../session'
import { ChangesView } from './ChangesView'
import { CodeClient, type ConnectionState, type GitStatus } from './client'
import { CodeTerminal } from './CodeTerminal'
import { EditorArea, tabKey, type Tab } from './EditorArea'
import { ProjectView } from './ProjectView'

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
function Connected({ env }: { env: string }) {
  const [client, setClient] = useState<CodeClient | null>(null)
  useEffect(() => {
    const c = new CodeClient(env)
    setClient(c)
    return () => c.close()
  }, [env])
  if (!client) return <div className="code-page" />
  return <CodeView env={env} client={client} />
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

function CodeView({ env, client }: { env: string; client: CodeClient }) {
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

  // Tool windows.
  const project = usePanelRef()
  const changes = usePanelRef()
  const terminal = usePanelRef()

  // Open tabs, remembered per environment.
  const storeKey = `hangar.code.${env}.tabs`
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
  const open = (tab: Tab) =>
    setStore((s) => ({
      tabs: s.tabs.some((t) => tabKey(t) === tabKey(tab)) ? s.tabs : [...s.tabs, tab],
      active: tabKey(tab),
    }))
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
    project.current?.expand()
    setReveal((r) => ({ path, n: (r?.n ?? 0) + 1 }))
  }

  // Open in Terminal: a new session in a folder.
  const [termIn, setTermIn] = useState<{ dir: string; n: number } | null>(null)
  const openTerminal = (dir: string) => {
    terminal.current?.expand()
    setTermIn((t) => ({ dir, n: (t?.n ?? 0) + 1 }))
  }

  // Keys for tabs, chosen from those a browser leaves to the page: Alt+W
  // closes, Alt+Shift+T reopens, Alt+[ and Alt+] step through them. They
  // work wherever focus is while the Code tab shows -- it is on the page
  // itself between one editor closing and the next opening -- except in the
  // terminal, which keeps its own keys.
  const onKey = (e: KeyboardEvent) => {
    if (!e.altKey || e.ctrlKey || e.metaKey) return
    if ((e.target as HTMLElement).closest('.code-term')) return
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

  const [shown, setShown] = useState({ project: true, changes: true, terminal: true })
  const track = (name: keyof typeof shown) => (size: { asPercentage: number }) =>
    setShown((s) => (s[name] === size.asPercentage > 0 ? s : { ...s, [name]: size.asPercentage > 0 }))
  const toggle = (ref: React.RefObject<PanelImperativeHandle | null>) => {
    const p = ref.current
    if (!p) return
    if (p.isCollapsed()) p.expand()
    else p.collapse()
  }
  const outer = useDefaultLayout({ id: 'hangar.code.v', storage: localStorage })
  const inner = useDefaultLayout({ id: 'hangar.code.h', storage: localStorage })

  return (
    <div className="code-page">
      <div className="code-stripe">
        <StripeButton on={shown.project} title="Project" onClick={() => toggle(project)}>
          <FolderTree size={16} />
        </StripeButton>
        <span className="code-tool-spacer" />
        <StripeButton on={shown.terminal} title="Terminal" onClick={() => toggle(terminal)}>
          <SquareTerminal size={16} />
        </StripeButton>
      </div>
      <div className="code-main">
        <Group orientation="vertical" defaultLayout={outer.defaultLayout} onLayoutChanged={outer.onLayoutChanged}>
          <Panel id="top" minSize="30%">
            <Group orientation="horizontal" defaultLayout={inner.defaultLayout} onLayoutChanged={inner.onLayoutChanged}>
              <Panel
                id="project"
                panelRef={project}
                defaultSize="22%"
                minSize="140px"
                collapsible
                collapsedSize={0}
                onResize={track('project')}
              >
                <ProjectView
                  client={client}
                  env={env}
                  status={status}
                  selected={activeTab?.path ?? null}
                  reveal={reveal}
                  onOpen={(path) => open({ kind: 'file', path })}
                  onDiff={(path) => open({ kind: 'diff', path })}
                  onTerminal={openTerminal}
                  onHide={() => project.current?.collapse()}
                />
              </Panel>
              <Separator className="code-split code-split-v" />
              <Panel id="editors" minSize="25%">
                <EditorArea
                  client={client}
                  tabs={store.tabs}
                  active={store.active}
                  user={user}
                  status={status}
                  canReopen={canReopen}
                  onActivate={(key) => setStore((s) => ({ ...s, active: key }))}
                  onOpen={open}
                  onClose={close}
                  onCloseMany={closeTabs}
                  onReopen={reopen}
                  onReveal={revealInProject}
                />
              </Panel>
              <Separator className="code-split code-split-v" />
              <Panel
                id="changes"
                panelRef={changes}
                defaultSize="24%"
                minSize="180px"
                collapsible
                collapsedSize={0}
                onResize={track('changes')}
              >
                <ChangesView
                  client={client}
                  status={status}
                  reload={reloadStatus}
                  onDiff={(path) => open({ kind: 'diff', path })}
                  onOpen={(path) => open({ kind: 'file', path })}
                  onHide={() => changes.current?.collapse()}
                />
              </Panel>
            </Group>
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
            <CodeTerminal env={env} openIn={termIn} onHide={() => terminal.current?.collapse()} />
          </Panel>
        </Group>
        {state !== 'live' && (
          <div className="code-status">
            {state === 'connecting' ? 'Connecting to the environment…' : `Reconnecting…${why ? ` (${why})` : ''}`}
          </div>
        )}
      </div>
      <div className="code-stripe">
        <StripeButton on={shown.changes} title="Changes" onClick={() => toggle(changes)}>
          <GitCompare size={16} />
          {!!status?.changes.length && <span className="code-stripe-count">{status.changes.length}</span>}
        </StripeButton>
      </div>
    </div>
  )
}

function StripeButton({ on, title, onClick, children }: { on: boolean; title: string; onClick: () => void; children: ReactNode }) {
  return (
    <button
      type="button"
      className={on ? 'code-stripe-btn code-stripe-btn-on' : 'code-stripe-btn'}
      title={title}
      aria-label={title}
      aria-pressed={on}
      onClick={onClick}
    >
      {children}
    </button>
  )
}
