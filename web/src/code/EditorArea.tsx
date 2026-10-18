import { EditorView } from '@codemirror/view'
import { Table2 } from 'lucide-react'
import { useEffect, useState } from 'react'

import type { CodeClient, GitStatus, SharedFile } from './client'
import { LanguagesContext, type Languages } from './lsp'
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
import { useTabDrag, type DropTarget } from './tabDrag'
import { openWith, opensAs, type FileView } from './views'

export { tabKey, type Tab } from './TabBar'

// EditorArea is the Code tab's centre: a strip of tabs and the chosen one, a
// shared file or a diff.
export function EditorArea({
  clientFor,
  languagesFor,
  statusOf,
  projectLabel,
  onOutside,
  tabs,
  active,
  opened,
  user,
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
  group = 0,
  onFocus,
  onSplit,
  onUnsplit,
  onDropTab,
}: {
  // clientFor, languagesFor and statusOf are a tab's project's: its
  // connection, its language servers, and what git says of it.
  clientFor: (root?: string) => CodeClient
  languagesFor: (root?: string) => Languages
  statusOf: (root?: string) => GitStatus | null
  // projectLabel names the project a tab's file is in, where there are
  // several.
  projectLabel?: (t: Tab) => string | undefined
  // onOutside offers what to do about a file outside every project.
  onOutside?: { reveal: (abs: string) => void; add?: (dir: string) => void }
  tabs: Tab[]
  active: string | null
  // Counts the tabs opened, the active one again included.
  opened: number
  jump: (Jump & { n: number }) | null
  user: { name: string; color: string }
  canReopen: boolean
  onActivate: (key: string) => void
  onMove: (key: string, to: number) => void
  onOpen: (tab: Tab) => void
  onClose: (key: string) => void
  onCloseMany: (keys: string[]) => void
  onReopen: () => void
  onReveal: (path: string, root?: string) => void
  onShowCommit: (hash: string, path: string, root?: string) => void
  // split says how many groups of editors there are, and whether this one
  // is the one in use beside another.
  split?: { groups: number; focused: boolean }
  // onFocus says this group was used: clicked in.
  onFocus?: () => void
  // onSplit shows a tab in the other group, or moves it there.
  onSplit?: (tab: Tab, move: boolean) => void
  // onUnsplit gathers both groups' tabs into one.
  onUnsplit?: () => void
  // group is which group of editors this is.
  group?: number
  // onDropTab puts a tab dragged out of this group's row where it was
  // dropped: another group, or a split.
  onDropTab?: (key: string, target: DropTarget) => void
}) {
  const env = useEnv()
  const current = tabs.find((t) => tabKey(t) === active) ?? null
  // A tab dragged here from another group, or to split this one, shows
  // where it would land.
  const dragging = useTabDrag()
  const zone = !dragging?.target
    ? null
    : 'split' in dragging.target
      ? dragging.from === group && 'right'
      : dragging.from !== group && dragging.target.group === group && dragging.target.index === undefined && 'all'
  const menu = useContextMenu()

  // tabMenu is IntelliJ's and VS Code's tab menu, less what an editor
  // that saves as it goes has no use for.
  const tabMenu = (t: Tab, x: number, y: number): MenuItem[] => {
    const key = tabKey(t)
    const i = tabs.findIndex((o) => tabKey(o) === key)
    const keys = tabs.map(tabKey)
    const changed = statusOf(t.root)?.changes.some((c) => c.path === t.path) ?? false
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
      {
        label: 'Copy Path',
        onSelect: () => copyText(t.path.startsWith('/') ? t.path : `${clientFor(t.root).root}/${t.path}`),
      },
      { label: 'Copy Relative Path', onSelect: () => copyText(t.path) },
      'separator',
      { label: 'Reveal in Project', onSelect: () => onReveal(t.path, t.root) },
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
  // outside is whether a tab's file is in no project: a path from / that
  // no project's folder holds.
  const outside = (t: Tab) => t.path.startsWith('/') && !t.root
  const fileTab = (t: Tab) => (
    <FileTab
      key={tabKey(t)}
      client={clientFor(t.root)}
      path={t.path}
      view={t.view}
      user={user}
      jump={jump && (jump.root ?? undefined) === t.root ? jump : null}
      onReveal={(path) => onReveal(path, t.root)}
      onShowCommit={(hash, path) => onShowCommit(hash, path, t.root)}
      onOpenFile={(path) => onOpen({ kind: 'file', path, root: path.startsWith('/') ? undefined : t.root })}
    />
  )
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
      data-editor-group={group}
    >
      {tabs.length > 0 && (
        <TabBar
          tabs={tabs}
          active={active}
          opened={opened}
          onActivate={onActivate}
          onClose={onClose}
          onMove={onMove}
          projectLabel={projectLabel}
          group={group}
          groups={split?.groups ?? 1}
          onDropOut={onDropTab}
          onMenu={(e, t) => menu.open(e, tabMenu(t, e.clientX, e.clientY))}
        />
      )}
      <div className="code-editor-body">
        {zone && <div className={`code-drop-zone code-drop-zone-${zone}`} />}
        {current?.kind === 'file' && (
          <LanguagesContext.Provider value={languagesFor(current.root)}>
            {outside(current) ? (
              <div className="code-outside">
                <div className="code-outside-bar">
                  <span>
                    <b>{current.path}</b> is outside the workspace.
                  </span>
                  <span className="code-tool-spacer" />
                  {onOutside && (
                    <button type="button" className="code-tool-btn" onClick={() => onOutside.reveal(current.path)}>
                      Reveal on Computer
                    </button>
                  )}
                  {onOutside?.add && (
                    <button
                      type="button"
                      className="code-tool-btn"
                      onClick={() => onOutside.add?.(current.path.slice(0, current.path.lastIndexOf('/')) || '/')}
                    >
                      Add {current.path.slice(0, current.path.lastIndexOf('/')) || '/'} to Workspace
                    </button>
                  )}
                </div>
                <div className="code-outside-body">{fileTab(current)}</div>
              </div>
            ) : (
              fileTab(current)
            )}
          </LanguagesContext.Provider>
        )}
        {current?.kind === 'diff' && (
          <DiffView
            key={tabKey(current)}
            client={clientFor(current.root)}
            path={current.path}
            base={current.base}
            baseLabel={current.baseLabel}
            from={current.from}
            to={current.to}
            toLabel={current.toLabel}
          />
        )}
        {current?.kind === 'compare' && current.other && (
          <CompareView
            key={tabKey(current)}
            client={clientFor(current.root)}
            left={current.other}
            right={current.path}
          />
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
          <DockerLogs
            key={tabKey(current)}
            client={clientFor()}
            id={current.path}
            name={current.title ?? current.path.slice(0, 12)}
          />
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
