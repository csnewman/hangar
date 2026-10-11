import { useEffect, useState } from 'react'

import type { CodeClient, GitStatus, SharedFile } from './client'
import { CodeEditor } from './CodeEditor'
import { copyText, shortcut, useContextMenu, type MenuItem } from './ContextMenu'
import { DesktopTab } from '../desktop/DesktopTab'
import { DiffView } from './DiffView'
import { DockerLogs } from './DockerView'
import { HexView } from './HexView'
import { ImageView } from './ImageView'
import type { Jump } from './Search'
import { TabBar, tabKey, type Tab } from './TabBar'
import { openWith, opensAsImage, type FileView } from './views'

export { tabKey, type Tab } from './TabBar'

// EditorArea is the Code tab's centre: a strip of tabs and the chosen one, a
// shared file or a diff.
export function EditorArea({
  client,
  tabs,
  active,
  opened,
  user,
  status,
  canReopen,
  onActivate,
  onMove,
  onOpen,
  onClose,
  onCloseMany,
  onReopen,
  onReveal,
  onShowCommit,
  jump,
}: {
  client: CodeClient
  tabs: Tab[]
  active: string | null
  // Counts the tabs opened, the active one again included.
  opened: number
  jump: (Jump & { n: number }) | null
  user: { name: string; color: string }
  status: GitStatus | null
  canReopen: boolean
  onActivate: (key: string) => void
  onMove: (key: string, to: number) => void
  onOpen: (tab: Tab) => void
  onClose: (key: string) => void
  onCloseMany: (keys: string[]) => void
  onReopen: () => void
  onReveal: (path: string) => void
  onShowCommit: (hash: string) => void
}) {
  const current = tabs.find((t) => tabKey(t) === active) ?? null
  const menu = useContextMenu()

  // tabMenu is IntelliJ's and VS Code's tab menu, less what an editor
  // that saves as it goes has no use for.
  const tabMenu = (t: Tab, x: number, y: number): MenuItem[] => {
    const key = tabKey(t)
    const i = tabs.findIndex((o) => tabKey(o) === key)
    const keys = tabs.map(tabKey)
    const changed = status?.changes.some((c) => c.path === t.path) ?? false
    const closing: MenuItem[] = [
      { label: 'Close', shortcut: shortcut('Alt+W'), onSelect: () => onClose(key) },
      { label: 'Close Others', disabled: tabs.length < 2, onSelect: () => onCloseMany(keys.filter((k) => k !== key)) },
      { label: 'Close Tabs to the Left', disabled: i === 0, onSelect: () => onCloseMany(keys.slice(0, i)) },
      {
        label: 'Close Tabs to the Right',
        disabled: i === tabs.length - 1,
        onSelect: () => onCloseMany(keys.slice(i + 1)),
      },
      { label: 'Close All', onSelect: () => onCloseMany(keys) },
    ]
    if (t.kind === 'logs' || t.kind === 'desktop') return closing
    return [
      ...closing,
      'separator',
      { label: 'Copy Path', onSelect: () => copyText(`${client.root}/${t.path}`) },
      { label: 'Copy Relative Path', onSelect: () => copyText(t.path) },
      'separator',
      { label: 'Reveal in Project', onSelect: () => onReveal(t.path) },
      {
        label: 'Open With…',
        onSelect: () => menu.openAt(x, y, openWith(t.path, (view) => onOpen({ kind: 'file', path: t.path, view }))),
      },
      t.kind === 'file'
        ? { label: 'Show Changes', disabled: !changed, onSelect: () => onOpen({ kind: 'diff', path: t.path }) }
        : { label: 'Open File', onSelect: () => onOpen({ kind: 'file', path: t.path }) },
      'separator',
      { label: 'Reopen Closed Tab', shortcut: shortcut('Alt+Shift+T'), disabled: !canReopen, onSelect: onReopen },
    ]
  }
  return (
    <div className="code-editors">
      {tabs.length > 0 && (
        <TabBar
          tabs={tabs}
          active={active}
          opened={opened}
          onActivate={onActivate}
          onClose={onClose}
          onMove={onMove}
          onMenu={(e, t) => menu.open(e, tabMenu(t, e.clientX, e.clientY))}
        />
      )}
      <div className="code-editor-body">
        {current?.kind === 'file' && (
          <FileTab
            key={tabKey(current)}
            client={client}
            path={current.path}
            view={current.view}
            user={user}
            jump={jump}
            onReveal={onReveal}
            onShowCommit={onShowCommit}
          />
        )}
        {current?.kind === 'diff' && (
          <DiffView
            key={tabKey(current)}
            client={client}
            path={current.path}
            base={current.base}
            baseLabel={current.baseLabel}
            from={current.from}
            to={current.to}
            toLabel={current.toLabel}
          />
        )}
        {current?.kind === 'desktop' && (
          <div className="code-desktop">
            <DesktopTab />
          </div>
        )}
        {current?.kind === 'logs' && (
          <DockerLogs key={tabKey(current)} client={client} id={current.path} name={current.title ?? current.path.slice(0, 12)} />
        )}
        {!current && (
          <div className="code-empty">
            <p>Open a file from Project, or a change from Changes.</p>
            <p className="muted small">
              {shortcut('Mod+Shift+O')} or Shift twice goes to a file; {shortcut('Mod+Shift+F')} finds text in files.
            </p>
            <p className="muted small">Everyone with a file open edits it together; changes are saved as you type.</p>
          </div>
        )}
      </div>
      {menu.menu}
    </div>
  )
}

// FileTab shows a file in the view chosen for it, else the one its content
// calls for: an image as an image, other bytes as hex, text to edit.
function FileTab({
  client,
  path,
  view,
  user,
  jump,
  onReveal,
  onShowCommit,
}: {
  client: CodeClient
  path: string
  view?: FileView
  user: { name: string; color: string }
  jump: (Jump & { n: number }) | null
  onReveal: (path: string) => void
  onShowCommit: (hash: string) => void
}) {
  const [shown, setShown] = useState<FileView | null>(view ?? (opensAsImage(path) ? 'image' : null))
  useEffect(() => {
    if (shown) return
    client.probe(path).then(
      (p) => setShown(p.binary ? 'hex' : 'text'),
      // The editor says what is wrong with it.
      () => setShown('text'),
    )
  }, [client, path, shown])
  if (shown === 'image') return <ImageView client={client} path={path} />
  if (shown === 'hex') return <HexView client={client} path={path} />
  if (!shown) return <div className="code-empty"><p className="muted">Opening {path}…</p></div>
  return (
    <TextTab client={client} path={path} user={user} jump={jump} onReveal={onReveal} onShowCommit={onShowCommit} />
  )
}

// TextTab is a text file, shared with everyone who has it open.
function TextTab({
  client,
  path,
  user,
  jump,
  onReveal,
  onShowCommit,
}: {
  client: CodeClient
  path: string
  user: { name: string; color: string }
  jump: (Jump & { n: number }) | null
  onReveal: (path: string) => void
  onShowCommit: (hash: string) => void
}) {
  const [file, setFile] = useState<SharedFile | null>(null)
  const [error, setError] = useState('')

  // A file deleted while open is shown as deleted rather than edited, which
  // would bring it back.
  useEffect(
    () =>
      client.on('doc.deleted', ({ path: gone }) => {
        if (gone === `${client.root}/${path}` || gone === path) setError(`${path.split('/').pop()} was deleted.`)
      }),
    [client, path],
  )

  useEffect(() => {
    let f: SharedFile | null = null
    let live = true
    setFile(null)
    setError('')
    client.open(path).then(
      async (opened) => {
        f = opened
        if (!live) {
          client.release(opened)
          return
        }
        await opened.whenSynced()
        if (live) setFile(opened)
      },
      (e: Error) => live && setError(e.message),
    )
    return () => {
      live = false
      if (f) client.release(f)
    }
  }, [client, path])

  if (error) return <div className="code-empty"><p className="code-note-error">{error}</p></div>
  if (!file) return <div className="code-empty"><p className="muted">Opening {path}…</p></div>
  return (
    <CodeEditor
      file={file}
      user={user}
      client={client}
      path={path}
      jump={jump}
      onReveal={onReveal}
      onShowCommit={onShowCommit}
    />
  )
}
