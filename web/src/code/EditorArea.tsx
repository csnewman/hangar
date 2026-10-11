import { File, FileText, GitCompare, Monitor, X } from 'lucide-react'
import { useEffect, useState } from 'react'

import type { CodeClient, GitStatus, SharedFile } from './client'
import { CodeEditor } from './CodeEditor'
import { copyText, shortcut, useContextMenu, type MenuItem } from './ContextMenu'
import { DesktopTab } from '../desktop/DesktopTab'
import { DiffView } from './DiffView'
import { DockerLogs } from './DockerView'
import type { Jump } from './Search'

// A tab is a file, its changes, a container's logs (path is then the
// container's ID, and title its name), or the environment's desktop, which
// is connected only while its tab is the one shown. A diff is against HEAD, or against
// base (a commit, labelled baseLabel), where the file was named from.
export type Tab = {
  kind: 'file' | 'diff' | 'logs' | 'desktop'
  path: string
  title?: string
  base?: string
  baseLabel?: string
  from?: string
}

export const tabKey = (t: Tab) => (t.base ? `diff@${t.base}:${t.path}` : `${t.kind}:${t.path}`)

// EditorArea is the Code tab's centre: a strip of tabs and the chosen one, a
// shared file or a diff.
export function EditorArea({
  client,
  tabs,
  active,
  user,
  status,
  canReopen,
  onActivate,
  onOpen,
  onClose,
  onCloseMany,
  onReopen,
  onReveal,
  jump,
}: {
  client: CodeClient
  tabs: Tab[]
  active: string | null
  jump: (Jump & { n: number }) | null
  user: { name: string; color: string }
  status: GitStatus | null
  canReopen: boolean
  onActivate: (key: string) => void
  onOpen: (tab: Tab) => void
  onClose: (key: string) => void
  onCloseMany: (keys: string[]) => void
  onReopen: () => void
  onReveal: (path: string) => void
}) {
  const current = tabs.find((t) => tabKey(t) === active) ?? null
  const menu = useContextMenu()

  // tabMenu is IntelliJ's and VS Code's tab menu, less what an editor
  // that saves as it goes has no use for.
  const tabMenu = (t: Tab): MenuItem[] => {
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
        <div className="code-tabs" role="tablist">
          {tabs.map((t) => {
            const key = tabKey(t)
            const name = t.title ?? t.path.split('/').pop() ?? t.path
            return (
              <div
                key={key}
                role="tab"
                aria-selected={key === active}
                className={key === active ? 'code-tab code-tab-on' : 'code-tab'}
                title={
                  t.kind === 'diff'
                    ? `Changes to ${t.path}${t.base ? ` since ${t.baseLabel ?? t.base}` : ''}`
                    : t.kind === 'logs'
                      ? `Logs of ${name}`
                      : t.path
                }
                onClick={() => onActivate(key)}
                onContextMenu={(e) => menu.open(e, tabMenu(t))}
                onAuxClick={(e) => {
                  if (e.button === 1) onClose(key)
                }}
              >
                {t.kind === 'diff' ? (
                  <GitCompare size={13} className="code-icon-diff" />
                ) : t.kind === 'logs' ? (
                  <FileText size={13} className="code-icon-file" />
                ) : t.kind === 'desktop' ? (
                  <Monitor size={13} className="code-icon-file" />
                ) : (
                  <File size={13} className="code-icon-file" />
                )}
                <span>{name}</span>
                <button
                  type="button"
                  className="code-tab-close"
                  aria-label={`Close ${name}`}
                  onClick={(e) => {
                    e.stopPropagation()
                    onClose(key)
                  }}
                >
                  <X size={12} />
                </button>
              </div>
            )
          })}
        </div>
      )}
      <div className="code-editor-body">
        {current?.kind === 'file' && (
          <FileTab key={tabKey(current)} client={client} path={current.path} user={user} jump={jump} onReveal={onReveal} />
        )}
        {current?.kind === 'diff' && (
          <DiffView
            key={tabKey(current)}
            client={client}
            path={current.path}
            base={current.base}
            baseLabel={current.baseLabel}
            from={current.from}
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

function FileTab({
  client,
  path,
  user,
  jump,
  onReveal,
}: {
  client: CodeClient
  path: string
  user: { name: string; color: string }
  jump: (Jump & { n: number }) | null
  onReveal: (path: string) => void
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
  return <CodeEditor file={file} user={user} client={client} path={path} jump={jump} onReveal={onReveal} />
}
