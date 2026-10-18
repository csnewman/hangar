import { EditorView } from '@codemirror/view'
import { Table2 } from 'lucide-react'
import { useEffect, useState } from 'react'

import type { CodeClient, GitStatus, SharedFile } from './client'
import { CodeEditor } from './CodeEditor'
import { copyText, shortcut, useContextMenu, type MenuItem } from './ContextMenu'
import { DesktopTab } from '../desktop/DesktopTab'
import { useEnv } from '../pages/Environment'
import { PerformanceTab } from '../pages/Performance'
import { CompareView, DiffView } from './DiffView'
import { DockerLogs } from './DockerView'
import { EnvironmentView } from './EnvironmentView'
import { HexView } from './HexView'
import { ImageView } from './ImageView'
import { isMarkdown, MarkdownPreview } from './MarkdownPreview'
import { PreviewSplit, useYText } from './PreviewSplit'
import { isTable, TableView } from './TableView'
import { HtmlPreview, isHtml } from './HtmlPreview'
import { MediaView } from './MediaView'
import type { Jump } from './Search'
import { TabBar, tabKey, type Tab } from './TabBar'
import { openWith, opensAs, type FileView } from './views'

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
  split,
  onFocus,
  onSplit,
  onUnsplit,
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
  onShowCommit: (hash: string, path: string) => void
  // split says how many groups of editors there are, and whether this one
  // is the one in use beside another.
  split?: { groups: number; focused: boolean }
  // onFocus says this group was used: clicked in.
  onFocus?: () => void
  // onSplit shows a tab in the other group, or moves it there.
  onSplit?: (tab: Tab, move: boolean) => void
  // onUnsplit gathers both groups' tabs into one.
  onUnsplit?: () => void
}) {
  const env = useEnv()
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
    const groups = split?.groups ?? 1
    const splitting: MenuItem[] = onSplit
      ? [
          'separator',
          {
            label: groups > 1 ? 'Open on the Other Side' : 'Split Right',
            disabled: t.kind !== 'file' && t.kind !== 'diff' && t.kind !== 'compare',
            onSelect: () => onSplit(t, false),
          },
          {
            label: groups > 1 ? 'Move to the Other Side' : 'Split and Move Right',
            disabled: groups === 1 && tabs.length < 2,
            onSelect: () => onSplit(t, true),
          },
          ...(groups > 1 && onUnsplit ? [{ label: 'Unsplit', onSelect: onUnsplit }] : []),
        ]
      : []
    if (t.kind !== 'file' && t.kind !== 'diff' && t.kind !== 'compare') return [...closing, ...splitting]
    return [
      ...closing,
      ...splitting,
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
    <div
      className={[
        'code-editors',
        (split?.groups ?? 1) > 1 && 'code-editors-split',
        split?.focused && 'code-editors-focused',
      ]
        .filter(Boolean)
        .join(' ')}
      // A press in the group, not in a menu it opened over the page, makes
      // it the group in use.
      onPointerDownCapture={(e) => {
        if (e.currentTarget.contains(e.target as Node)) onFocus?.()
      }}
    >
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
            onOpenFile={(path) => onOpen({ kind: 'file', path })}
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
        {current?.kind === 'compare' && current.other && (
          <CompareView key={tabKey(current)} client={client} left={current.other} right={current.path} />
        )}
        {current?.kind === 'desktop' && (
          <div className="code-desktop">
            <DesktopTab />
          </div>
        )}
        {current?.kind === 'environment' && (
          <EnvironmentView env={env} onDesktop={() => onOpen({ kind: 'desktop', path: 'desktop', title: 'Desktop' })} />
        )}
        {current?.kind === 'performance' && (
          <div className="code-page-tab">
            <PerformanceTab />
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
// calls for: an image as an image, a PDF, sound or video played, other bytes
// as hex, text to edit.
function FileTab({
  client,
  path,
  view,
  user,
  jump,
  onReveal,
  onShowCommit,
  onOpenFile,
}: {
  client: CodeClient
  path: string
  view?: FileView
  user: { name: string; color: string }
  jump: (Jump & { n: number }) | null
  onReveal: (path: string) => void
  onShowCommit: (hash: string, path: string) => void
  onOpenFile: (path: string) => void
}) {
  const [shown, setShown] = useState<FileView | null>(view ?? opensAs(path))
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
  if (shown === 'pdf' || shown === 'media') return <MediaView client={client} path={path} kind={shown} />
  if (!shown) return <div className="code-empty"><p className="muted">Opening {path}…</p></div>
  return (
    <TextTab
      client={client}
      path={path}
      user={user}
      jump={jump}
      onReveal={onReveal}
      onShowCommit={onShowCommit}
      onOpenFile={onOpenFile}
    />
  )
}

// TextTab is a text file, shared with everyone who has it open, and
// Markdown beside its preview.
function TextTab({
  client,
  path,
  user,
  jump,
  onReveal,
  onShowCommit,
  onOpenFile,
}: {
  client: CodeClient
  path: string
  user: { name: string; color: string }
  jump: (Jump & { n: number }) | null
  onReveal: (path: string) => void
  onShowCommit: (hash: string, path: string) => void
  onOpenFile: (path: string) => void
}) {
  const [file, setFile] = useState<SharedFile | null>(null)
  const [error, setError] = useState('')

  // A file deleted while open is shown as deleted rather than edited, which
  // would bring it back, until it is back: restored from the trash.
  useEffect(() => {
    const ours = (p: string) => p === `${client.root}/${path}` || p === path
    const offGone = client.on('doc.deleted', ({ path: gone }) => {
      if (ours(gone)) setError(`${path.split('/').pop()} was deleted.`)
    })
    const offBack = client.on('doc.restored', ({ path: back }) => {
      if (ours(back)) setError('')
    })
    return () => {
      offGone()
      offBack()
    }
  }, [client, path])

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
  if (isHtml(path)) {
    return (
      <PreviewSplit
        kind="html"
        initial="editor"
        names={['Editor Only', 'Editor and Preview', 'Preview Only']}
        editor={
          <CodeEditor
            file={file}
            user={user}
            client={client}
            path={path}
            jump={jump}
            onReveal={onReveal}
            onShowCommit={onShowCommit}
          />
        }
        preview={<HtmlPreview client={client} path={path} />}
      />
    )
  }
  if (isTable(path)) {
    return (
      <TableTab
        client={client}
        file={file}
        path={path}
        user={user}
        jump={jump}
        onReveal={onReveal}
        onShowCommit={onShowCommit}
      />
    )
  }
  if (isMarkdown(path)) {
    return (
      <MarkdownTab
        client={client}
        file={file}
        path={path}
        user={user}
        jump={jump}
        onReveal={onReveal}
        onShowCommit={onShowCommit}
        onOpenFile={onOpenFile}
      />
    )
  }
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

// MarkdownTab is a Markdown file's editor and its preview, kept level.
function MarkdownTab({
  client,
  file,
  path,
  user,
  jump,
  onReveal,
  onShowCommit,
  onOpenFile,
}: {
  client: CodeClient
  file: SharedFile
  path: string
  user: { name: string; color: string }
  jump: (Jump & { n: number }) | null
  onReveal: (path: string) => void
  onShowCommit: (hash: string, path: string) => void
  onOpenFile: (path: string) => void
}) {
  const text = useYText(file.text)
  const [view, setView] = useState<EditorView | null>(null)
  return (
    <PreviewSplit
      kind="markdown"
      names={['Editor Only', 'Editor and Preview', 'Preview Only']}
      editor={
        <CodeEditor
          file={file}
          user={user}
          client={client}
          path={path}
          jump={jump}
          onReveal={onReveal}
          onShowCommit={onShowCommit}
          onView={setView}
        />
      }
      preview={<MarkdownPreview client={client} path={path} text={text} editor={view} onOpen={onOpenFile} />}
    />
  )
}

// TableTab is a CSV or TSV file's text beside it as a table, the table
// alone to begin with.
function TableTab({
  client,
  file,
  path,
  user,
  jump,
  onReveal,
  onShowCommit,
}: {
  client: CodeClient
  file: SharedFile
  path: string
  user: { name: string; color: string }
  jump: (Jump & { n: number }) | null
  onReveal: (path: string) => void
  onShowCommit: (hash: string, path: string) => void
}) {
  const text = useYText(file.text)
  const [view, setView] = useState<EditorView | null>(null)
  const toLine = (n: number) => {
    if (!view) return
    const line = view.state.doc.line(Math.min(n, view.state.doc.lines))
    view.dispatch({ selection: { anchor: line.from }, effects: EditorView.scrollIntoView(line.from, { y: 'center' }) })
  }
  return (
    <PreviewSplit
      kind="table"
      initial="preview"
      names={['Text Only', 'Text and Table', 'Table Only']}
      previewIcon={<Table2 size={14} />}
      editor={
        <CodeEditor
          file={file}
          user={user}
          client={client}
          path={path}
          jump={jump}
          onReveal={onReveal}
          onShowCommit={onShowCommit}
          onView={setView}
        />
      }
      preview={<TableView path={path} text={text} onLine={toLine} />}
    />
  )
}
