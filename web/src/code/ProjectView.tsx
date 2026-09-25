import { ChevronRight, File, FilePlus, Folder, FolderOpen, FolderPlus, ListCollapse, RefreshCw } from 'lucide-react'
import { useCallback, useEffect, useRef, useState } from 'react'

import type { CodeClient, Entry, GitStatus } from './client'
import { copyText, useContextMenu, type MenuItem } from './ContextMenu'
import { ToolWindow } from './ToolWindow'

// Where a pending new entry or rename is typed.
type Editing = { kind: 'file' | 'dir'; parent: string } | { kind: 'rename'; path: string } | null

// ProjectView is the Project tool window: the files under the root, loaded a
// folder at a time as they are opened, kept current as they change on disk,
// and coloured by what git says of them.
export function ProjectView({
  client,
  env,
  status,
  selected,
  reveal,
  onOpen,
  onDiff,
  onTerminal,
  onHide,
}: {
  client: CodeClient
  env: string
  status: GitStatus | null
  selected: string | null
  // reveal asks for a path to be shown: its folders opened, it selected
  // and scrolled to. n changes with each ask.
  reveal: { path: string; n: number } | null
  onOpen: (path: string) => void
  onDiff: (path: string) => void
  onTerminal: (dir: string) => void
  onHide: () => void
}) {
  const storeKey = `hangar.code.${env}.expanded`
  const [root, setRoot] = useState(client.root)
  const [children, setChildren] = useState<Record<string, Entry[]>>({})
  const [expanded, setExpanded] = useState<Set<string>>(() => {
    try {
      return new Set(JSON.parse(localStorage.getItem(storeKey) ?? '[]'))
    } catch {
      return new Set()
    }
  })
  const [editing, setEditing] = useState<Editing>(null)
  const menu = useContextMenu()
  // The revealed path is highlighted until another tab is chosen.
  const [revealed, setRevealed] = useState<{ path: string; over: string | null } | null>(null)
  const highlighted = revealed && revealed.over === selected ? revealed.path : selected
  const tree = useRef<HTMLDivElement>(null)
  const [error, setError] = useState('')
  const expandedRef = useRef(expanded)
  expandedRef.current = expanded
  const selectedRef = useRef(selected)
  selectedRef.current = selected

  const load = useCallback(
    async (dir: string) => {
      try {
        const list = await client.list(dir)
        setChildren((c) => ({ ...c, [dir]: list }))
      } catch {
        setChildren((c) => {
          const next = { ...c }
          delete next[dir]
          return next
        })
      }
    },
    [client],
  )

  // The root, and every folder left open last time.
  useEffect(() => {
    let live = true
    const start = async () => {
      await client.request('hello')
      if (!live) return
      setRoot(client.root)
      load('')
      for (const d of expandedRef.current) load(d)
    }
    start().catch(() => {})
    const offState = client.on('state', (s) => {
      if (s === 'live') start().catch(() => {})
    })
    const offFs = client.on('fs.changed', ({ dirs }) => {
      for (const abs of dirs) {
        const rel = abs === client.root ? '' : abs.startsWith(client.root + '/') ? abs.slice(client.root.length + 1) : null
        if (rel === null) continue
        if (rel === '' || expandedRef.current.has(rel)) load(rel)
      }
    })
    return () => {
      live = false
      offState()
      offFs()
    }
  }, [client, load])

  useEffect(() => {
    try {
      localStorage.setItem(storeKey, JSON.stringify([...expanded]))
    } catch {
      // Remembering open folders is a convenience.
    }
  }, [expanded, storeKey])

  // Reveal: open each folder above the path, loading it, then scroll to
  // the path once it is drawn.
  useEffect(() => {
    if (!reveal) return
    let live = true
    const parts = reveal.path.split('/')
    const dirs = parts.slice(0, -1).map((_, i) => parts.slice(0, i + 1).join('/'))
    ;(async () => {
      for (const d of dirs) await load(d)
      if (!live) return
      setExpanded((e) => new Set([...e, ...dirs]))
      setRevealed({ path: reveal.path, over: selectedRef.current })
      requestAnimationFrame(() =>
        tree.current
          ?.querySelector(`[data-path="${CSS.escape(reveal.path)}"]`)
          ?.scrollIntoView({ block: 'center' }),
      )
    })()
    return () => {
      live = false
    }
  }, [reveal, load])

  const toggle = (dir: string) => {
    setExpanded((e) => {
      const next = new Set(e)
      if (next.has(dir)) next.delete(dir)
      else {
        next.add(dir)
        load(dir)
      }
      return next
    })
  }

  // The git colour for a path: its own change, or a folder holding changes.
  const tone = (path: string, dir: boolean): string => {
    if (!status?.repo) return ''
    for (const c of status.changes) {
      if (dir ? c.path.startsWith(path + '/') : c.path === path) {
        if (dir) return 'code-git-modified'
        if (c.index === '?') return 'code-git-unversioned'
        if (c.index === 'A' || c.worktree === 'A') return 'code-git-added'
        if (c.index === 'U') return 'code-git-conflict'
        return 'code-git-modified'
      }
    }
    return ''
  }

  const act = async (fn: () => Promise<void>) => {
    setError('')
    try {
      await fn()
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e))
    }
  }

  const submit = (name: string) => {
    const e = editing
    setEditing(null)
    name = name.trim()
    if (!e || !name) return
    if (e.kind === 'rename') {
      const parent = e.path.includes('/') ? e.path.slice(0, e.path.lastIndexOf('/')) : ''
      const to = parent ? `${parent}/${name}` : name
      if (to !== e.path) act(() => client.rename(e.path, to))
      return
    }
    const path = e.parent ? `${e.parent}/${name}` : name
    act(async () => {
      if (e.kind === 'dir') await client.mkdir(path)
      else {
        await client.create(path)
        onOpen(path)
      }
    })
  }

  const startNew = (kind: 'file' | 'dir', parent: string) => {
    if (parent && !expanded.has(parent)) toggle(parent)
    setEditing({ kind, parent })
  }

  // entryMenu is what a right-click on an entry offers; path '' is the
  // root, from a click on the tree's empty space.
  const entryMenu = (path: string, dir: boolean): MenuItem[] => {
    const name = path.split('/').pop() || root.split('/').pop() || root
    const abs = path ? `${root}/${path}` : root
    const folder = dir ? path : path.includes('/') ? path.slice(0, path.lastIndexOf('/')) : ''
    const change = !dir && status?.changes.find((c) => c.path === path)
    const items: MenuItem[] = []
    if (!dir) {
      items.push({ label: 'Open', onSelect: () => onOpen(path) })
      if (change) items.push({ label: 'Show Changes', onSelect: () => onDiff(path) })
      items.push('separator')
    }
    items.push(
      { label: 'New File…', onSelect: () => startNew('file', folder) },
      { label: 'New Folder…', onSelect: () => startNew('dir', folder) },
      'separator',
      { label: 'Copy Path', onSelect: () => copyText(abs) },
      { label: 'Copy Relative Path', disabled: !path, onSelect: () => copyText(path) },
      { label: 'Copy Name', onSelect: () => copyText(name) },
      'separator',
      { label: 'Open in Terminal', onSelect: () => onTerminal(folder ? `${root}/${folder}` : root) },
    )
    if (change) {
      items.push(
        'separator',
        { label: 'Stage', disabled: change.worktree === '.', onSelect: () => act(() => client.stage(path)) },
        {
          label: change.index === '?' ? 'Delete Unversioned File' : 'Rollback',
          confirm: change.index === '?' ? `Delete ${name}?` : `Discard all changes to ${name}?`,
          danger: true,
          onSelect: () => act(() => client.rollback(path)),
        },
      )
    }
    if (path) {
      items.push(
        'separator',
        { label: 'Rename…', onSelect: () => setEditing({ kind: 'rename', path }) },
        { label: 'Delete', danger: true, confirm: `Delete ${name}?`, onSelect: () => act(() => client.remove(path)) },
      )
    }
    return items
  }

  const renderDir = (dir: string, depth: number): React.ReactNode => {
    const list = children[dir]
    const pending = editing && editing.kind !== 'rename' && editing.parent === dir
    return (
      <>
        {pending && (
          <NameInput depth={depth} dir={editing.kind === 'dir'} initial="" onDone={submit} />
        )}
        {list?.map((e) => {
          const path = dir ? `${dir}/${e.name}` : e.name
          const open = e.dir && expanded.has(path)
          if (editing?.kind === 'rename' && editing.path === path) {
            return <NameInput key={path} depth={depth} dir={e.dir} initial={e.name} onDone={submit} />
          }
          return (
            <div key={path}>
              <div
                className={`code-tree-row ${highlighted === path ? 'code-tree-row-on' : ''}`}
                data-path={path}
                style={{ paddingLeft: 6 + depth * 14 }}
                onClick={() => {
                  setRevealed(null)
                  if (e.dir) toggle(path)
                  else onOpen(path)
                }}
                onContextMenu={(ev) => menu.open(ev, entryMenu(path, e.dir))}
                title={path}
              >
                {e.dir ? (
                  <ChevronRight size={13} className={open ? 'code-chev code-chev-open' : 'code-chev'} />
                ) : (
                  <span className="code-chev-space" />
                )}
                {e.dir ? (
                  open ? <FolderOpen size={14} className="code-icon-dir" /> : <Folder size={14} className="code-icon-dir" />
                ) : (
                  <File size={14} className="code-icon-file" />
                )}
                <span className={`code-tree-name ${tone(path, e.dir)}`}>{e.name}</span>
              </div>
              {open && renderDir(path, depth + 1)}
            </div>
          )
        })}
      </>
    )
  }

  const rootName = root ? root.split('/').pop() || root : 'Project'
  return (
    <ToolWindow
      title="Project"
      onHide={onHide}
      actions={
        <>
          <button type="button" className="code-tool-btn" title="New file" onClick={() => startNew('file', '')}>
            <FilePlus size={14} />
          </button>
          <button type="button" className="code-tool-btn" title="New folder" onClick={() => startNew('dir', '')}>
            <FolderPlus size={14} />
          </button>
          <button type="button" className="code-tool-btn" title="Collapse all" onClick={() => setExpanded(new Set())}>
            <ListCollapse size={14} />
          </button>
          <button
            type="button"
            className="code-tool-btn"
            title="Reload from disk"
            onClick={() => {
              load('')
              for (const d of expanded) load(d)
            }}
          >
            <RefreshCw size={13} />
          </button>
        </>
      }
    >
      <div
        className="code-tree"
        ref={tree}
        onContextMenu={(ev) => {
          if (ev.target === ev.currentTarget || (ev.target as HTMLElement).closest('.code-tree-root')) {
            menu.open(ev, entryMenu('', true))
          }
        }}
      >
        <div className="code-tree-root" title={root}>
          <FolderOpen size={14} className="code-icon-dir" />
          <span className="strong">{rootName}</span>
          <span className="muted small code-tree-rootpath">{root}</span>
        </div>
        {renderDir('', 1)}
        {error && <div className="code-note code-note-error">{error}</div>}
      </div>
      {menu.menu}
    </ToolWindow>
  )
}

function NameInput({
  depth,
  dir,
  initial,
  onDone,
}: {
  depth: number
  dir: boolean
  initial: string
  onDone: (name: string) => void
}) {
  const ref = useRef<HTMLInputElement>(null)
  useEffect(() => {
    const el = ref.current
    if (!el) return
    el.focus()
    const dot = initial.lastIndexOf('.')
    el.setSelectionRange(0, dot > 0 ? dot : initial.length)
  }, [initial])
  return (
    <div className="code-tree-row" style={{ paddingLeft: 6 + depth * 14 }}>
      <span className="code-chev-space" />
      {dir ? <Folder size={14} className="code-icon-dir" /> : <File size={14} className="code-icon-file" />}
      <input
        ref={ref}
        className="code-tree-input"
        defaultValue={initial}
        placeholder={dir ? 'folder name' : 'file name'}
        onKeyDown={(e) => {
          if (e.key === 'Enter') onDone(e.currentTarget.value)
          if (e.key === 'Escape') onDone('')
        }}
        onBlur={(e) => onDone(e.currentTarget.value)}
      />
    </div>
  )
}
