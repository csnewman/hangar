import {
  ChevronDown,
  ChevronRight,
  Eye,
  File,
  Folder,
  FolderOpen,
  HardDrive,
  House,
  Lock,
  Pin,
  RefreshCw,
} from 'lucide-react'
import { useCallback, useEffect, useRef, useState, type KeyboardEvent } from 'react'

import { formatBytes } from '../components/format'
import type { CodeClient, Entry } from './client'
import { copyText, useContextMenu, type MenuItem } from './ContextMenu'
import { ToolWindow } from './ToolWindow'

// A place the Filesystem window offers to go to: the home folder, the
// root, each project, and what the person has pinned.
type Place = { name: string; path: string; kind: 'home' | 'root' | 'project' | 'pinned' }

const parentOf = (p: string) => (p.lastIndexOf('/') > 0 ? p.slice(0, p.lastIndexOf('/')) : '/')
const join = (dir: string, name: string) => (dir === '/' ? `/${name}` : `${dir}/${name}`)
const nameOf = (p: string) => (p === '/' ? '/' : p.slice(p.lastIndexOf('/') + 1))

// FilesystemView is the Filesystem tool window: the whole machine, from any
// folder. A path is typed (Tab completes it) or a place chosen, and the
// folder there is a tree, its folders opened in place. A file opens as a
// tab, in its project if it is in one; a folder can be opened in a
// terminal, added to the workspace or pinned. Nothing here is watched: a
// folder is read again as it is opened, and all of them with Reload.
export function FilesystemView({
  client,
  env,
  projects,
  pinned,
  onPin,
  onOpenFile,
  onTerminal,
  onAddToWorkspace,
  onOpenAlone,
  onHide,
  reveal,
}: {
  client: CodeClient
  env: string
  // projects are the workspace's folders, offered as places.
  projects: { name: string; root: string }[]
  // pinned are the folders the person has pinned, and onPin changes them.
  pinned: string[]
  onPin: (path: string, pin: boolean) => void
  onOpenFile: (abs: string) => void
  onTerminal: (dir: string) => void
  // onAddToWorkspace is absent where the workspace cannot change: a tab
  // working in one folder alone.
  onAddToWorkspace?: (dir: string) => void
  onOpenAlone: (dir: string) => void
  onHide: () => void
  // reveal shows the folder a path is in, choosing the path, each time it
  // changes.
  reveal?: { path: string; n: number } | null
}) {
  const atKey = `hangar.code.${env}.filesystem`
  const [at, setAtState] = useState(() => {
    try {
      return localStorage.getItem(atKey) ?? ''
    } catch {
      return ''
    }
  })
  const [typed, setTyped] = useState(at)
  const [children, setChildren] = useState<Record<string, Entry[] | string>>({})
  const [expanded, setExpanded] = useState<Set<string>>(new Set())
  const [chosen, setChosen] = useState<string | null>(null)
  const [hidden, setHidden] = useState(false)
  const menu = useContextMenu()
  const input = useRef<HTMLInputElement>(null)

  const setAt = (dir: string) => {
    setAtState(dir)
    setTyped(dir)
    setExpanded(new Set())
    setChosen(null)
    try {
      localStorage.setItem(atKey, dir)
    } catch {
      // Where the window was is a convenience.
    }
  }

  useEffect(() => {
    if (!reveal) return
    client.list(reveal.path).then(
      () => setAt(reveal.path),
      () => {
        setAt(parentOf(reveal.path))
        setChosen(reveal.path)
      },
    )
    // setAt only writes state.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [reveal, client])

  // The home folder to begin with, once the connection says where it is.
  useEffect(() => {
    if (at) return
    client.connected().then(
      () => setAt(client.home || '/'),
      () => {},
    )
    // setAt only writes state.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [client, at])

  const load = useCallback(
    (dir: string) =>
      client.list(dir).then(
        (list) => setChildren((c) => ({ ...c, [dir]: list })),
        (e: Error) => setChildren((c) => ({ ...c, [dir]: e.message })),
      ),
    [client],
  )
  useEffect(() => {
    if (at) load(at)
  }, [at, load])

  const reload = () => {
    if (at) load(at)
    for (const d of expanded) load(d)
  }

  const toggle = (dir: string) =>
    setExpanded((e) => {
      const next = new Set(e)
      if (next.has(dir)) next.delete(dir)
      else {
        next.add(dir)
        load(dir)
      }
      return next
    })

  // go goes to a typed path: a folder is shown, a file opened beside its
  // folder.
  const go = async (raw: string) => {
    let p = raw.trim().replace(/\/+$/, '') || '/'
    if (p === '~' || p.startsWith('~/')) p = client.home + p.slice(1)
    if (!p.startsWith('/')) p = join(at || '/', p)
    try {
      await client.list(p)
      setAt(p)
    } catch {
      try {
        await client.probe(p)
        setAt(parentOf(p))
        setChosen(p)
        onOpenFile(p)
      } catch {
        setTyped(raw)
        setChildren((c) => ({ ...c, [p]: `${p} does not exist.` }))
        setAtState(p)
      }
    }
  }

  // complete fills in the rest of the name being typed, as far as the
  // folders it could be agree.
  const complete = async () => {
    const t = typed.startsWith('~') ? client.home + typed.slice(1) : typed
    const dir = t.endsWith('/') ? t.replace(/\/+$/, '') || '/' : parentOf(t)
    const part = t.endsWith('/') ? '' : nameOf(t)
    try {
      const names = (await client.list(dir))
        .filter((e) => e.dir && e.name.startsWith(part) && (part.startsWith('.') || !e.name.startsWith('.')))
        .map((e) => e.name)
      if (!names.length) return
      let common = names[0]
      for (const n of names) while (!n.startsWith(common)) common = common.slice(0, -1)
      setTyped(join(dir, common) + (names.length === 1 ? '/' : ''))
    } catch {
      // Nothing to complete.
    }
  }

  const places: Place[] = [
    { name: 'Home', path: client.home, kind: 'home' } as Place,
    { name: '/', path: '/', kind: 'root' } as Place,
    ...projects.map((p): Place => ({ name: p.name, path: p.root, kind: 'project' })),
    ...pinned.map((p): Place => ({ name: nameOf(p), path: p, kind: 'pinned' })),
  ].filter((p, i, all) => p.path && all.findIndex((o) => o.path === p.path) === i)

  const folderMenu = (dir: string): MenuItem[] => {
    const isPinned = pinned.includes(dir)
    return [
      { label: 'Show Here', onSelect: () => setAt(dir) },
      { label: 'Open in Terminal', onSelect: () => onTerminal(dir) },
      'separator',
      {
        label: 'Add to Workspace',
        disabled: !onAddToWorkspace || projects.some((p) => p.root === dir),
        onSelect: () => onAddToWorkspace?.(dir),
      },
      { label: 'Open Alone in This Tab', onSelect: () => onOpenAlone(dir) },
      { label: isPinned ? 'Unpin from Places' : 'Pin to Places', onSelect: () => onPin(dir, !isPinned) },
      'separator',
      { label: 'Copy Path', onSelect: () => copyText(dir) },
    ]
  }
  const fileMenu = (path: string): MenuItem[] => [
    { label: 'Open', onSelect: () => onOpenFile(path) },
    { label: 'Open Folder in Terminal', onSelect: () => onTerminal(parentOf(path)) },
    'separator',
    { label: 'Copy Path', onSelect: () => copyText(path) },
  ]

  const rows = (dir: string, depth: number): React.ReactNode => {
    const list = children[dir]
    if (list === undefined)
      return (
        <div className="code-filesystem-note" style={{ paddingLeft: 8 + depth * 14 }}>
          Reading…
        </div>
      )
    if (typeof list === 'string') {
      return (
        <div className="code-filesystem-note code-note-error" style={{ paddingLeft: 8 + depth * 14 }}>
          {list}
        </div>
      )
    }
    const shown = list.filter((e) => hidden || !e.name.startsWith('.'))
    if (!shown.length)
      return (
        <div className="code-filesystem-note" style={{ paddingLeft: 8 + depth * 14 }}>
          Empty
        </div>
      )
    return shown.map((e) => {
      const path = join(dir, e.name)
      const open = e.dir && expanded.has(path)
      return (
        <div key={path}>
          <div
            className={chosen === path ? 'code-tree-row code-tree-row-on' : 'code-tree-row'}
            style={{ paddingLeft: 6 + depth * 14 }}
            title={path}
            onClick={() => {
              setChosen(path)
              if (e.dir) toggle(path)
              else onOpenFile(path)
            }}
            onDoubleClick={() => e.dir && setAt(path)}
            onContextMenu={(ev) => {
              setChosen(path)
              menu.open(ev, e.dir ? folderMenu(path) : fileMenu(path))
            }}
          >
            {e.dir ? (
              open ? (
                <ChevronDown size={12} className="code-chev" />
              ) : (
                <ChevronRight size={12} className="code-chev" />
              )
            ) : (
              <span className="code-chev-space" />
            )}
            {e.dir ? (
              open ? (
                <FolderOpen size={14} className="code-icon-dir" />
              ) : (
                <Folder size={14} className="code-icon-dir" />
              )
            ) : (
              <File size={14} className="code-icon-file" />
            )}
            <span className="code-tree-name">{e.name}</span>
            {e.readonly && <Lock size={11} className="code-filesystem-lock" aria-label="Read-only" />}
            {!e.dir && <span className="code-filesystem-size">{formatBytes(e.size)}</span>}
          </div>
          {open && rows(path, depth + 1)}
        </div>
      )
    })
  }

  const placeIcon = (p: Place) =>
    p.kind === 'home' ? (
      <House size={11} />
    ) : p.kind === 'root' ? (
      <HardDrive size={11} />
    ) : p.kind === 'pinned' ? (
      <Pin size={11} />
    ) : (
      <Folder size={11} />
    )

  return (
    <ToolWindow
      title="Filesystem"
      onHide={onHide}
      actions={
        <>
          <button
            type="button"
            className={hidden ? 'code-tool-btn code-tool-btn-on' : 'code-tool-btn'}
            title="Show hidden files"
            aria-pressed={hidden}
            onClick={() => setHidden((h) => !h)}
          >
            <Eye size={13} />
          </button>
          <button type="button" className="code-tool-btn" title="Reload" onClick={reload}>
            <RefreshCw size={13} />
          </button>
        </>
      }
    >
      <div className="code-filesystem">
        <label className="code-filesystem-path">
          <span className="code-sr-only">Folder to show</span>
          <Folder size={13} className="code-icon-dir" />
          <input
            ref={input}
            value={typed}
            spellCheck={false}
            placeholder="/ or ~/ and a path"
            onChange={(e) => setTyped(e.target.value)}
            onKeyDown={(e: KeyboardEvent) => {
              if (e.key === 'Tab' && !e.shiftKey) {
                e.preventDefault()
                complete()
              } else if (e.key === 'Enter') go(typed)
              else if (e.key === 'Escape') setTyped(at)
            }}
            onBlur={() => setTyped(at)}
          />
        </label>
        <div className="code-filesystem-places" role="list" aria-label="Places">
          {places.map((p) => (
            <button
              key={p.path}
              type="button"
              role="listitem"
              className={p.path === at ? 'code-filesystem-place code-filesystem-place-on' : 'code-filesystem-place'}
              title={p.path}
              onClick={() => setAt(p.path)}
              onContextMenu={(ev) => menu.open(ev, folderMenu(p.path))}
            >
              {placeIcon(p)}
              {p.name}
            </button>
          ))}
        </div>
        <div className="code-filesystem-tree">
          {at && (
            <div
              className="code-tree-root"
              title={at}
              onContextMenu={(ev) => menu.open(ev, folderMenu(at))}
              onDoubleClick={() => at !== '/' && setAt(parentOf(at))}
            >
              <FolderOpen size={14} className="code-icon-dir" />
              <span className="strong">{nameOf(at)}</span>
              {at !== '/' && (
                <button type="button" className="code-tool-btn" title="Up a folder" onClick={() => setAt(parentOf(at))}>
                  ..
                </button>
              )}
            </div>
          )}
          {at && rows(at, 1)}
        </div>
      </div>
      {menu.menu}
    </ToolWindow>
  )
}
