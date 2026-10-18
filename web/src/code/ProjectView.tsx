import {
  ChevronDown,
  ChevronRight,
  File,
  FilePlus,
  FileSearch,
  Folder,
  FolderInput,
  FolderOpen,
  FolderPlus,
  GitBranch,
  ListCollapse,
  LocateFixed,
  RefreshCw,
  Search,
  Settings2,
  Undo2,
} from 'lucide-react'
import { useCallback, useEffect, useRef, useState, useSyncExternalStore } from 'react'

import type { Clash, CodeClient, Encoding, Entry, GitStatus, Separator, TextProps } from './client'
import { copyText, isMac, shortcut, useContextMenu, type MenuItem } from './ContextMenu'
import { keepClipboard } from './DiffView'
import { marked, match } from './match'
import type { Tab } from './TabBar'
import { ToolWindow } from './ToolWindow'
import { ClashDialog, TransferDialog } from './TreeDialogs'
import {
  created,
  dirOf,
  download,
  fileClip,
  filesOf,
  join,
  nameOf,
  onFileClip,
  renamePath,
  setFileClip,
  transferPaths,
  trashPaths,
  uploadFiles,
  what,
  type Ask,
  type Moved,
  type Undo,
  type Upload,
} from './treeOps'
import { openWith, type FileView } from './views'

// Where a pending new entry or rename is typed.
type Editing = { kind: 'file' | 'dir'; parent: string } | { kind: 'rename'; path: string } | null

// A row of the tree as shown.
type Row = { path: string; name: string; dir: boolean; entry: Entry }

// The tree's options, as IntelliJ's Project view has them: whether it
// follows the file being edited, and whether it shows hidden files and
// those git ignores.
type Options = { follow: boolean; hidden: boolean; ignored: boolean }
const optionsKey = 'hangar.code.tree'
const defaultOptions: Options = { follow: false, hidden: true, ignored: true }

function loadOptions(): Options {
  try {
    return { ...defaultOptions, ...JSON.parse(localStorage.getItem(optionsKey) ?? '{}') }
  } catch {
    return defaultOptions
  }
}

// The options are every project's tree's, as one: a change shows in each.
let treeOptions: Options | null = null
const optionListeners = new Set<() => void>()
const treeOptionsStore = {
  get: () => (treeOptions ??= loadOptions()),
  subscribe: (fn: () => void) => {
    optionListeners.add(fn)
    return () => {
      optionListeners.delete(fn)
    }
  },
  set(o: Partial<Options>) {
    treeOptions = { ...treeOptionsStore.get(), ...o }
    try {
      localStorage.setItem(optionsKey, JSON.stringify(treeOptions))
    } catch {
      // Remembering them is a convenience.
    }
    for (const fn of optionListeners) fn()
  },
}

// treeOptionsMenu is the trees' options, as a menu.
export function treeOptionsMenu(): MenuItem[] {
  const o = treeOptionsStore.get()
  return [
    {
      label: 'Always Select Opened File',
      checked: o.follow,
      onSelect: () => treeOptionsStore.set({ follow: !o.follow }),
    },
    { label: 'Show Hidden Files', checked: o.hidden, onSelect: () => treeOptionsStore.set({ hidden: !o.hidden }) },
    { label: 'Show Ignored Files', checked: o.ignored, onSelect: () => treeOptionsStore.set({ ignored: !o.ignored }) },
  ]
}

// Embedded is a project's tree shown among others in the Workspace window:
// its row, which folds it and has the project's own menu, and no toolbar
// of its own.
export type Embedded = {
  branch?: string
  collapsed: boolean
  onCollapse: () => void
  menu: MenuItem[]
  // collapseAll closes every folder each time it changes.
  collapseAll: number
}

// A dialog the tree has open.
type Dialog =
  | { kind: 'transfer'; op: 'copy' | 'move'; paths: string[]; dir: string }
  | { kind: 'clash'; names: string[]; to: string; op: 'copy' | 'move' | 'upload'; answer: (c: Clash | null) => void }
  | null

// The key that deletes: Delete, or Backspace, as a Mac's keyboard has it.
const deleteKey = isMac ? '⌫' : 'Delete'

// ProjectView is the Project tool window: the files under the root, loaded a
// folder at a time as they are opened, kept current as they change on disk,
// and coloured by what git says of them. Rows are selected as a file
// manager's are -- one, a range with Shift, more with Ctrl or ⌘ -- and cut,
// copied, pasted, dragged, renamed, deleted, uploaded and downloaded, each
// undone with Undo. Focused, it is worked with the keyboard: the arrows
// move, open and close, Enter opens, and typing searches the rows shown.
export function ProjectView({
  client,
  env,
  status,
  selected,
  reveal,
  onOpen,
  onTab,
  onDiff,
  onTerminal,
  onGotoFile,
  onOpenFolder,
  onFolderHere,
  onFind,
  onHistory,
  onAnnotate,
  onMoved,
  onHide,
  embedded,
}: {
  client: CodeClient
  env: string
  status: GitStatus | null
  // selected is the file being edited.
  selected: string | null
  // reveal asks for a path to be shown: its folders opened, it selected
  // and scrolled to. n changes with each ask.
  reveal: { path: string; n: number } | null
  // onOpen opens a file, in the view chosen with Open With if any; stay
  // leaves the keyboard in the tree, as a click does.
  onOpen: (path: string, view?: FileView, stay?: boolean) => void
  // onTab opens any tab: a comparison, or a diff with a branch or commit.
  onTab: (tab: Tab) => void
  onDiff: (path: string) => void
  onTerminal: (dir: string) => void
  onGotoFile: () => void
  // onOpenFolder asks which folder to work in; onFolderHere works in one.
  onOpenFolder: () => void
  onFolderHere: (dir: string) => void
  // onFind opens Find in Files in a folder, to replace or not.
  onFind: (dir: string, replace: boolean) => void
  // onHistory shows a path's history in the Git Log; onAnnotate opens a
  // file with its annotations.
  onHistory: (path: string) => void
  onAnnotate: (path: string) => void
  // onMoved says where paths went, for the tabs open on them to follow.
  onMoved: (moved: Moved) => void
  onHide: () => void
  // embedded shows the tree among the Workspace window's others.
  embedded?: Embedded
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
  const tree = useRef<HTMLDivElement>(null)
  const [error, setError] = useState('')
  // What was last done, to undo with the button beside it or Ctrl/⌘+Z, and
  // what is under way: an upload or download.
  const undos = useRef<Undo[]>([])
  const [done, setDone] = useState<string>('')
  const [busy, setBusy] = useState('')
  const [dialog, setDialog] = useState<Dialog>(null)
  const options = useSyncExternalStore(treeOptionsStore.subscribe, treeOptionsStore.get)
  const expandedRef = useRef(expanded)
  expandedRef.current = expanded
  // The rows chosen, and the one a Shift range is from. They are kept in
  // refs as well as state, for a key pressed straight after a click to act
  // on what the click chose, before the tree draws it.
  const [chosen, setChosenState] = useState<string[]>([])
  const chosenRef = useRef<string[]>([])
  const setChosen = (v: string[] | ((c: string[]) => string[])) => {
    chosenRef.current = typeof v === 'function' ? v(chosenRef.current) : v
    setChosenState(chosenRef.current)
  }
  const anchor = useRef<string | null>(null)
  // The row the keyboard is on, and what is typed to find one (speed
  // search, as IntelliJ's): rows shown whose names match are marked, and
  // the cursor goes to the best.
  const [cursor, setCursorState] = useState<string | null>(null)
  const cursorRef = useRef<string | null>(null)
  const setCursor = (p: string | null) => {
    cursorRef.current = p
    setCursorState(p)
  }
  const [query, setQueryState] = useState('')
  // Keys come faster than renders: what is typed is kept here too, for
  // each key to add to.
  const typed = useRef('')
  const setQuery = (q: string) => {
    typed.current = q
    setQueryState(q)
  }
  // The folder a drag is over, '' for the top, and when it opens.
  const [dropAt, setDropAt] = useState<string | null>(null)
  const dropTimer = useRef<ReturnType<typeof setTimeout> | undefined>(undefined)
  const clip = useSyncExternalStore(onFileClip, fileClip)

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
      await client.connected()
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
        const rel =
          abs === client.root ? '' : abs.startsWith(client.root + '/') ? abs.slice(client.root.length + 1) : null
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

  // show opens each folder above a path, loading it, chooses the path and
  // scrolls to it once it is drawn.
  const show = useCallback(
    async (path: string, live: () => boolean = () => true) => {
      const parts = path.split('/')
      const dirs = parts.slice(0, -1).map((_, i) => parts.slice(0, i + 1).join('/'))
      for (const d of dirs) await load(d)
      if (!live()) return
      setExpanded((e) => new Set([...e, ...dirs]))
      setChosen([path])
      anchor.current = path
      setCursor(path)
      requestAnimationFrame(() =>
        tree.current?.querySelector(`[data-path="${CSS.escape(path)}"]`)?.scrollIntoView({ block: 'nearest' }),
      )
    },
    [load],
  )

  // Reveal in Project, and the file being edited while the tree follows it.
  useEffect(() => {
    if (!reveal) return
    let live = true
    show(reveal.path, () => live)
    return () => {
      live = false
    }
  }, [reveal, show])
  useEffect(() => {
    if (!options.follow || !selected || selected.startsWith('/')) return
    let live = true
    show(selected, () => live)
    return () => {
      live = false
    }
  }, [options.follow, selected, show])

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

  const visible = (e: Entry) => (options.hidden || !e.name.startsWith('.')) && (options.ignored || !e.ignored)

  // The rows shown, in order: the entries of every open folder.
  const rows = (): Row[] => {
    const out: Row[] = []
    const walk = (dir: string) => {
      for (const e of children[dir] ?? []) {
        if (!visible(e)) continue
        const path = join(dir, e.name)
        out.push({ path, name: e.name, dir: e.dir, entry: e })
        if (e.dir && expanded.has(path)) walk(path)
      }
    }
    walk('')
    return out
  }
  const entryOf = (path: string): Entry | undefined => children[dirOf(path)]?.find((e) => e.name === nameOf(path))
  const isDir = (path: string) => path === '' || !!entryOf(path)?.dir

  // act runs an operation, showing what went wrong, and keeps its undo.
  const act = async (fn: () => Promise<Undo | null | void>) => {
    setError('')
    try {
      const undo = await fn()
      if (undo) {
        undos.current = [...undos.current.slice(-49), undo]
        setDone(undo.label)
      }
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e))
    }
  }

  const undo = () =>
    act(async () => {
      const u = undos.current.pop()
      if (!u) return
      setDone('')
      await u.run()
      if (u.moved) onMoved(u.moved)
      setDone(`Undid: ${u.label.charAt(0).toLowerCase()}${u.label.slice(1)}`)
    })

  // ask asks about clashes in a dialog.
  const ask: Ask = (q) => new Promise((answer) => setDialog({ kind: 'clash', ...q, answer }))

  // folderFor is where something goes when put at a row: in it, if a
  // folder, else beside it.
  const folderFor = (path: string | null) => (path === null ? '' : isDir(path) ? path : dirOf(path))

  // targets is what an action works on: the rows chosen, or the cursor's.
  const targets = (): string[] =>
    chosenRef.current.length ? chosenRef.current : cursorRef.current ? [cursorRef.current] : []

  // showDone shows paths something put somewhere, and chooses them.
  const showDone = async (paths: string[]) => {
    if (!paths.length) return
    await show(paths[0])
    setChosen(paths)
    tree.current?.focus()
  }

  const transfer = (op: 'copy' | 'move', paths: string[], to: string, name?: string) =>
    act(async () => {
      if (to) await client.mkdir(to)
      const r = await transferPaths(client, op, paths, to, ask, name)
      if (!r) return null
      if (op === 'move') onMoved(r.done)
      await showDone(r.done.map((d) => d.to))
      return r.undo
    })

  const cut = (paths: string[]) => {
    if (!paths.length) return
    setFileClip({ op: 'cut', paths })
    copyText(paths.map((p) => `${root}/${p}`).join('\n'))
  }
  const copy = (paths: string[]) => {
    if (!paths.length) return
    setFileClip({ op: 'copy', paths })
    copyText(paths.map((p) => `${root}/${p}`).join('\n'))
  }
  const paste = (at: string | null) => {
    const c = fileClip()
    if (!c) return
    if (c.op === 'cut') setFileClip(null)
    transfer(c.op === 'cut' ? 'move' : 'copy', c.paths, folderFor(at))
  }
  const remove = (paths: string[]) => {
    if (!paths.length) return
    act(async () => {
      const u = await trashPaths(client, paths)
      setChosen([])
      return u
    })
  }

  const upload = (files: Upload[], to: string) => {
    if (!files.length) return
    act(async () => {
      const r = await uploadFiles(client, files, to, ask, setBusy)
      if (!r) return null
      await showDone(r.tops)
      return r.undo
    })
  }
  // pick asks the browser for files, or a folder, to upload.
  const pick = (to: string, folder: boolean) => {
    const input = document.createElement('input')
    input.type = 'file'
    input.multiple = true
    if (folder) input.webkitdirectory = true
    input.onchange = () =>
      upload(
        [...(input.files ?? [])].map((file) => ({ file, rel: file.webkitRelativePath || file.name })),
        to,
      )
    input.click()
  }
  const save = (paths: string[]) =>
    act(() =>
      download(
        client,
        paths.map((p) => ({ path: p, dir: isDir(p) })),
        nameOf(root) || 'files',
        setBusy,
      ),
    )

  const submit = (name: string) => {
    const e = editing
    setEditing(null)
    name = name.trim()
    if (!e || !name) return
    if (e.kind === 'rename') {
      if (name === nameOf(e.path)) return
      act(async () => {
        const r = await renamePath(client, e.path, name)
        onMoved(r.done)
        await showDone(r.done.map((d) => d.to))
        return r.undo
      })
      return
    }
    const path = join(e.parent, name)
    act(async () => {
      if (e.kind === 'dir') await client.mkdir(path)
      else await client.create(path)
      await showDone([path])
      if (e.kind === 'file') onOpen(path)
      return created(client, [path], `Made ${name}`)
    })
  }

  const startNew = (kind: 'file' | 'dir', parent: string) => {
    if (parent && !expanded.has(parent)) toggle(parent)
    setEditing({ kind, parent })
  }

  const newMenu = (folder: string): MenuItem[] => [
    { label: 'File…', onSelect: () => startNew('file', folder) },
    { label: 'Folder…', onSelect: () => startNew('dir', folder) },
  ]

  // gitMenu is IntelliJ's Git submenu for a path.
  const gitMenu = (path: string, dir: boolean, x: number, y: number): MenuItem[] => {
    const change = status?.changes.find((c) => c.path === path)
    const items: MenuItem[] = [{ label: 'Show History', onSelect: () => onHistory(path) }]
    if (!dir) {
      items.push(
        { label: 'Annotate with Git Blame', onSelect: () => onAnnotate(path) },
        {
          label: 'Compare with Branch…',
          onSelect: () =>
            client.branches(path).then((b) =>
              menu.openAt(x, y, [
                { heading: 'Compare with' },
                ...[...b.local.map((l) => l.name), ...b.remote]
                  .filter((name) => name !== b.current)
                  .slice(0, 40)
                  .map((name): MenuItem => ({
                    label: name,
                    onSelect: () => onTab({ kind: 'diff', path, base: name, baseLabel: name }),
                  })),
              ]),
            ),
        },
        {
          label: 'Compare with Revision…',
          onSelect: () =>
            client.log(undefined, 0, 30, path).then((l) =>
              menu.openAt(x, y, [
                { heading: 'Compare with' },
                ...l.commits.map((c): MenuItem => ({
                  label: c.subject || c.hash.slice(0, 8),
                  detail: `${c.hash.slice(0, 8)} · ${c.author}`,
                  onSelect: () => onTab({ kind: 'diff', path, base: c.hash, baseLabel: c.hash.slice(0, 8) }),
                })),
                ...(l.commits.length ? [] : [{ label: 'No commits changed it', disabled: true, onSelect: () => {} }]),
              ]),
            ),
        },
      )
    }
    items.push('separator', {
      label: 'Add to .gitignore',
      disabled: change ? change.index !== '?' : !dir,
      onSelect: () => act(() => client.ignore(path, dir)),
    })
    if (change) {
      items.push(
        'separator',
        { label: 'Stage', disabled: change.worktree === '.', onSelect: () => act(() => client.stage(path)) },
        // A submodule's commit is moved in the submodule.
        ...(change.submodule
          ? []
          : [
              {
                label: change.index === '?' ? 'Delete Unversioned File' : 'Rollback',
                confirm: change.index === '?' ? `Delete ${nameOf(path)}?` : `Discard all changes to ${nameOf(path)}?`,
                danger: true,
                onSelect: () => act(() => client.rollback(path)),
              },
            ]),
      )
    }
    return items
  }

  // propertiesMenu is IntelliJ's File Properties: line separators,
  // encoding, the byte order mark, and whether the file is read-only.
  const propertiesMenu = (path: string, p: TextProps): MenuItem[] => {
    const convert = (to: { separator?: Separator; encoding?: Encoding; bom?: boolean }) =>
      act(async () => void (await client.convert(path, to)))
    const items: MenuItem[] = []
    if (p.encoding) {
      items.push({ heading: 'Line Separators' })
      for (const sep of ['LF', 'CRLF', 'CR'] as Separator[]) {
        items.push({
          label: { LF: 'LF - Unix and macOS (\\n)', CRLF: 'CRLF - Windows (\\r\\n)', CR: 'CR - Classic Mac OS (\\r)' }[
            sep
          ],
          checked: p.separator === sep,
          onSelect: () => convert({ separator: sep }),
        })
      }
      items.push({ heading: 'Encoding' })
      for (const enc of ['UTF-8', 'UTF-16LE', 'UTF-16BE', 'ISO-8859-1'] as Encoding[]) {
        items.push({ label: enc, checked: p.encoding === enc, onSelect: () => convert({ encoding: enc }) })
      }
      items.push('separator', {
        label: p.bom ? 'Remove BOM' : 'Add BOM',
        disabled: p.encoding === 'ISO-8859-1' || (p.bom && p.encoding !== 'UTF-8'),
        detail: p.encoding !== 'UTF-8' && p.bom ? 'UTF-16 needs it' : undefined,
        onSelect: () => convert({ bom: !p.bom }),
      })
    } else items.push({ label: 'Not text', disabled: true, onSelect: () => {} })
    items.push('separator', {
      label: p.mode & 0o200 ? 'Make File Read-only' : 'Make File Writable',
      onSelect: () => act(async () => void (await client.setMode(path, { writable: !(p.mode & 0o200) }))),
    })
    items.push({
      label: `${modeString(p.mode)} · ${p.separator && p.separator !== 'mixed' ? p.separator : (p.separator ?? 'one line')}`,
      disabled: true,
      onSelect: () => {},
    })
    return items
  }

  // entryMenu is what a right-click on an entry offers, for the rows
  // chosen; path '' is the root, from a click on the tree's empty space.
  const entryMenu = (path: string, dir: boolean, x: number, y: number): MenuItem[] => {
    const paths = path && chosen.includes(path) ? chosen : path ? [path] : []
    const many = paths.length > 1
    const name = nameOf(path) || nameOf(root) || root
    const abs = path ? `${root}/${path}` : root
    const folder = folderFor(path || null)
    const change = !dir && status?.changes.find((c) => c.path === path)
    const entry = path ? entryOf(path) : undefined
    const items: MenuItem[] = []
    if (!many && !dir) {
      items.push(
        { label: 'Open', onSelect: () => onOpen(path) },
        {
          label: 'Open With…',
          onSelect: () =>
            menu.openAt(
              x,
              y,
              openWith(path, (view) => onOpen(path, view)),
            ),
        },
      )
      if (change) items.push({ label: 'Show Changes', onSelect: () => onDiff(path) })
      items.push('separator')
    }
    items.push({ label: 'New ›', onSelect: () => menu.openAt(x, y, newMenu(folder)) }, 'separator')
    if (paths.length) {
      items.push(
        { label: 'Cut', shortcut: shortcut('Mod+X'), onSelect: () => cut(paths) },
        { label: 'Copy', shortcut: shortcut('Mod+C'), onSelect: () => copy(paths) },
      )
    }
    items.push(
      {
        label: 'Copy Path/Reference ›',
        onSelect: () =>
          menu.openAt(x, y, [
            {
              label: 'Absolute Path',
              detail: abs,
              onSelect: () => copyText(paths.length ? paths.map((p) => `${root}/${p}`).join('\n') : abs),
            },
            { label: 'Path From Project Root', disabled: !path, onSelect: () => copyText(paths.join('\n')) },
            { label: 'File Name', onSelect: () => copyText(paths.length ? paths.map(nameOf).join('\n') : name) },
          ]),
      },
      {
        label: clip ? `Paste ${what(clip.paths)}` : 'Paste',
        shortcut: shortcut('Mod+V'),
        disabled: !clip,
        onSelect: () => paste(path || null),
      },
      'separator',
      { label: 'Find in Folder…', onSelect: () => onFind(folder, false) },
      { label: 'Replace in Folder…', onSelect: () => onFind(folder, true) },
    )
    if (paths.length) {
      items.push(
        'separator',
        { label: 'Rename…', shortcut: 'F2', disabled: many, onSelect: () => setEditing({ kind: 'rename', path }) },
        {
          label: 'Move…',
          shortcut: 'F6',
          onSelect: () => setDialog({ kind: 'transfer', op: 'move', paths, dir: dirOf(paths[0]) }),
        },
        {
          label: 'Copy…',
          shortcut: 'F5',
          onSelect: () => setDialog({ kind: 'transfer', op: 'copy', paths, dir: dirOf(paths[0]) }),
        },
        { label: 'Delete', shortcut: deleteKey, danger: true, onSelect: () => remove(paths) },
      )
    }
    const files = paths.filter((p) => !isDir(p))
    if (files.length && files.length === paths.length && files.length <= 2) {
      items.push('separator', {
        label: 'Compare ›',
        onSelect: () =>
          menu.openAt(x, y, [
            {
              label: 'Compare Files',
              disabled: files.length !== 2,
              onSelect: () => onTab({ kind: 'compare', path: files[1], other: files[0] }),
            },
            {
              label: 'Compare with Editor',
              disabled: files.length !== 1 || !selected || selected === files[0],
              detail: selected ? nameOf(selected) : undefined,
              onSelect: () => selected && onTab({ kind: 'compare', path: selected, other: files[0] }),
            },
            {
              label: 'Compare with Clipboard',
              disabled: files.length !== 1 || !navigator.clipboard?.readText,
              onSelect: () =>
                navigator.clipboard.readText().then(
                  (text) => onTab({ kind: 'compare', path: files[0], other: keepClipboard(text) }),
                  () => setError('The browser would not read the clipboard.'),
                ),
            },
          ]),
      })
    }
    if (!many && status?.repo) {
      items.push('separator', { label: 'Git ›', onSelect: () => menu.openAt(x, y, gitMenu(path, dir || !path, x, y)) })
    }
    if (!many && path && !dir) {
      items.push(
        {
          label: 'File Properties ›',
          onSelect: () =>
            client.textProps(path).then(
              (p) => menu.openAt(x, y, propertiesMenu(path, p)),
              (e: Error) => setError(e.message),
            ),
        },
        {
          label: entry?.exec ? 'Remove Executable Permission' : 'Make Executable',
          onSelect: () => act(async () => void (await client.setMode(path, { exec: !entry?.exec }))),
        },
      )
    }
    items.push(
      'separator',
      { label: 'Upload Files…', onSelect: () => pick(folder, false) },
      { label: 'Upload Folder…', onSelect: () => pick(folder, true) },
      {
        label: paths.length ? `Download ${what(paths)}` : `Download ${name}`,
        onSelect: () => save(paths.length ? paths : ['']),
      },
      'separator',
      { label: 'Open in Terminal', onSelect: () => onTerminal(folder ? `${root}/${folder}` : root) },
    )
    if (!many && dir && path) items.push({ label: 'Open Folder in This Tab', onSelect: () => onFolderHere(abs) })
    return items
  }

  // --- Choosing rows ---------------------------------------------------

  const goTo = (path: string, extend = false) => {
    setCursor(path)
    if (extend && anchor.current) {
      const all = rows().map((r) => r.path)
      const a = all.indexOf(anchor.current)
      const b = all.indexOf(path)
      if (a >= 0 && b >= 0) setChosen(all.slice(Math.min(a, b), Math.max(a, b) + 1))
    } else {
      setChosen([path])
      anchor.current = path
    }
    requestAnimationFrame(() =>
      tree.current?.querySelector(`[data-path="${CSS.escape(path)}"]`)?.scrollIntoView({ block: 'nearest' }),
    )
  }

  // click chooses a row as a file manager does: alone, a range from the
  // last chosen with Shift, one more or less with Ctrl or ⌘. Alone, a
  // folder opens or closes and a file opens.
  const click = (e: React.MouseEvent, row: Row) => {
    tree.current?.focus()
    setQuery('')
    const more = isMac ? e.metaKey : e.ctrlKey
    if (e.shiftKey) {
      goTo(row.path, true)
      return
    }
    setCursor(row.path)
    if (more) {
      setChosen((c) => (c.includes(row.path) ? c.filter((p) => p !== row.path) : [...c, row.path]))
      anchor.current = row.path
      return
    }
    setChosen([row.path])
    anchor.current = row.path
    if (row.dir) toggle(row.path)
    else onOpen(row.path, undefined, true)
  }

  // find puts the cursor on the best match for q, the earliest of the best
  // from the cursor on.
  const find = (q: string) => {
    const all = rows()
    const at = Math.max(
      0,
      all.findIndex((r) => r.path === cursorRef.current),
    )
    let best: { path: string; score: number; i: number } | null = null
    all.forEach((r, i) => {
      const m = match(r.name, q)
      if (!m) return
      const order = (i - at + all.length) % all.length
      if (!best || m.score > best.score || (m.score === best.score && order < best.i)) {
        best = { path: r.path, score: m.score, i: order }
      }
    })
    if (best) goTo((best as { path: string }).path)
  }

  // step moves the cursor by one row, or to the next match while searching;
  // with Shift it takes the rows passed into the choice.
  const step = (by: 1 | -1, extend: boolean) => {
    const all = rows()
    const n = all.length
    if (!n) return
    const at = all.findIndex((r) => r.path === cursorRef.current)
    const q = typed.current
    if (!q) {
      const i = at < 0 ? (by > 0 ? 0 : n - 1) : Math.max(0, Math.min(n - 1, at + by))
      goTo(all[i].path, extend)
      return
    }
    const from = at < 0 ? (by > 0 ? -1 : 0) : at
    for (let k = 1; k <= n; k++) {
      const i = (((from + by * k) % n) + n) % n
      if (match(all[i].name, q)) {
        goTo(all[i].path)
        return
      }
    }
  }

  const onKey = (e: React.KeyboardEvent) => {
    if (e.target !== e.currentTarget) return
    const all = rows()
    const cursor = cursorRef.current
    const row = all.find((r) => r.path === cursor)
    const parent = cursor?.includes('/') ? dirOf(cursor) : null
    const key = e.key
    const mod = isMac ? e.metaKey && !e.ctrlKey : e.ctrlKey && !e.metaKey
    const k = key.length === 1 ? key.toLowerCase() : key
    if (mod && !e.altKey && !e.shiftKey && ['c', 'x', 'v', 'z', 'a'].includes(k)) {
      if (k === 'c') copy(targets())
      else if (k === 'x') cut(targets())
      else if (k === 'v') paste(cursorRef.current)
      else if (k === 'z') undo()
      else {
        setChosen(all.map((r) => r.path))
        anchor.current = all[0]?.path ?? null
      }
    } else if ((key === 'F2' || (key === 'F6' && e.shiftKey)) && row) setEditing({ kind: 'rename', path: row.path })
    else if ((key === 'F5' || key === 'F6') && !e.shiftKey && targets().length) {
      const paths = targets()
      setDialog({ kind: 'transfer', op: key === 'F5' ? 'copy' : 'move', paths, dir: dirOf(paths[0]) })
    } else if (key === 'Delete' || (key === 'Backspace' && (mod || !typed.current))) remove(targets())
    else if (e.altKey && (key === 'Insert' || e.code === 'KeyN')) {
      const el = tree.current?.querySelector(`[data-path="${CSS.escape(cursor ?? '')}"]`)?.getBoundingClientRect()
      const box = el ?? tree.current?.getBoundingClientRect()
      if (box) menu.openAt(box.left + 24, box.bottom, newMenu(folderFor(cursor)))
    } else if (key === 'ArrowDown' || key === 'ArrowUp') step(key === 'ArrowDown' ? 1 : -1, e.shiftKey)
    else if (key === 'ArrowRight' && row?.dir) {
      if (!expanded.has(row.path)) toggle(row.path)
      else {
        const first = all[all.indexOf(row) + 1]
        if (first?.path.startsWith(row.path + '/')) goTo(first.path)
      }
    } else if (key === 'ArrowLeft' && row) {
      if (row.dir && expanded.has(row.path)) toggle(row.path)
      else if (parent) goTo(parent)
    } else if ((key === 'Enter' || key === 'F4' || (mod && key === 'ArrowDown')) && row) {
      setQuery('')
      if (row.dir) toggle(row.path)
      else onOpen(row.path)
    } else if (key === 'Escape' && typed.current) setQuery('')
    else if (key === 'Escape' && clip?.op === 'cut') setFileClip(null)
    else if (key === 'Backspace' && typed.current) {
      const q = typed.current.slice(0, -1)
      setQuery(q)
      if (q) find(q)
    } else if (key.length === 1 && !e.metaKey && !e.ctrlKey && !e.altKey && (typed.current || key !== ' ')) {
      const q = typed.current + key
      setQuery(q)
      find(q)
    } else return
    e.preventDefault()
  }

  // --- Dragging --------------------------------------------------------

  // A drag of rows carries their paths; one from the user's computer, files.
  const dragType = 'application/x-hangar-paths'

  const dragStart = (e: React.DragEvent, row: Row) => {
    const paths = chosen.includes(row.path) ? chosen : [row.path]
    if (!chosen.includes(row.path)) {
      setChosen([row.path])
      anchor.current = row.path
    }
    e.dataTransfer.setData(dragType, JSON.stringify(paths))
    e.dataTransfer.setData('text/plain', paths.map((p) => `${root}/${p}`).join('\n'))
    e.dataTransfer.effectAllowed = 'copyMove'
  }

  // dragOver takes a drag over a row (or the top, for null): the folder it
  // would land in is marked, and a closed one opens after a moment.
  const dragOver = (e: React.DragEvent, path: string | null) => {
    const types = e.dataTransfer.types
    const ours = types.includes(dragType)
    if (!ours && !types.includes('Files')) return
    e.preventDefault()
    e.stopPropagation()
    e.dataTransfer.dropEffect = !ours || e.altKey || (isMac ? false : e.ctrlKey) ? 'copy' : 'move'
    const to = folderFor(path)
    if (to !== dropAt) {
      setDropAt(to)
      clearTimeout(dropTimer.current)
      if (to && !expandedRef.current.has(to)) dropTimer.current = setTimeout(() => toggle(to), 700)
    }
  }

  const drop = (e: React.DragEvent) => {
    const to = dropAt
    setDropAt(null)
    clearTimeout(dropTimer.current)
    if (to === null) return
    e.preventDefault()
    e.stopPropagation()
    const raw = e.dataTransfer.getData(dragType)
    if (raw) {
      const paths = (JSON.parse(raw) as string[]).filter((p) => p !== to)
      const copying = e.altKey || (!isMac && e.ctrlKey)
      if (paths.length) transfer(copying ? 'copy' : 'move', paths, to)
      return
    }
    filesOf(e.dataTransfer).then(
      (files) => upload(files, to),
      (err: Error) => setError(err.message),
    )
  }

  const renderDir = (dir: string, depth: number): React.ReactNode => {
    const list = children[dir]
    const pending = editing && editing.kind !== 'rename' && editing.parent === dir
    return (
      <>
        {pending && <NameInput depth={depth} dir={editing.kind === 'dir'} initial="" onDone={submit} />}
        {list?.filter(visible).map((e) => {
          const path = join(dir, e.name)
          const open = e.dir && expanded.has(path)
          if (editing?.kind === 'rename' && editing.path === path) {
            return <NameInput key={path} depth={depth} dir={e.dir} initial={e.name} onDone={submit} />
          }
          const row: Row = { path, name: e.name, dir: e.dir, entry: e }
          return (
            <div key={path} className={dropAt === path && e.dir ? 'code-tree-drop' : undefined}>
              <div
                className={[
                  'code-tree-row',
                  selected === path && 'code-tree-row-on',
                  chosen.includes(path) && 'code-tree-row-chosen',
                  cursor === path && 'code-tree-row-cursor',
                  clip?.op === 'cut' && clip.paths.includes(path) && 'code-tree-row-cut',
                  e.ignored && 'code-tree-row-ignored',
                ]
                  .filter(Boolean)
                  .join(' ')}
                data-path={path}
                style={{ paddingLeft: 6 + depth * 14 }}
                draggable
                onDragStart={(ev) => dragStart(ev, row)}
                onDragOver={(ev) => dragOver(ev, path)}
                onDrop={drop}
                onClick={(ev) => click(ev, row)}
                onContextMenu={(ev) => {
                  if (!chosen.includes(path)) {
                    setChosen([path])
                    anchor.current = path
                  }
                  setCursor(path)
                  menu.open(ev, entryMenu(path, e.dir, ev.clientX, ev.clientY))
                }}
                title={path}
              >
                {e.dir ? (
                  <ChevronRight size={13} className={open ? 'code-chev code-chev-open' : 'code-chev'} />
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
                <span className={`code-tree-name ${tone(path, e.dir)}`}>
                  {query ? marked(e.name, match(e.name, query)?.ranges ?? []) : e.name}
                </span>
              </div>
              {open && renderDir(path, depth + 1)}
            </div>
          )
        })}
      </>
    )
  }

  const queryFound = !query || rows().some((r) => match(r.name, query))

  const optionsMenu = (b: DOMRect): void => menu.openAt(b.right, b.bottom + 2, treeOptionsMenu(), 'right')

  // Every folder closed when the Workspace window asks.
  const collapseAll = embedded?.collapseAll ?? 0
  useEffect(() => {
    if (collapseAll) setExpanded(new Set())
  }, [collapseAll])

  const rootName = root ? nameOf(root) || root : 'Project'
  const toolbar = (
    <>
      <button
        type="button"
        className="code-tool-btn"
        title={`Go to File (${shortcut('Mod+Shift+O')})`}
        onClick={onGotoFile}
      >
        <FileSearch size={14} />
      </button>
      <button type="button" className="code-tool-btn" title="Open another folder in this tab" onClick={onOpenFolder}>
        <FolderInput size={14} />
      </button>
      <button
        type="button"
        className="code-tool-btn"
        title="New file"
        onClick={() => startNew('file', folderFor(chosen.length === 1 ? chosen[0] : null))}
      >
        <FilePlus size={14} />
      </button>
      <button
        type="button"
        className="code-tool-btn"
        title="New folder"
        onClick={() => startNew('dir', folderFor(chosen.length === 1 ? chosen[0] : null))}
      >
        <FolderPlus size={14} />
      </button>
      <button
        type="button"
        className="code-tool-btn"
        title="Select the opened file"
        disabled={!selected || selected.startsWith('/')}
        onClick={() => selected && show(selected)}
      >
        <LocateFixed size={13} />
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
      <button
        type="button"
        className="code-tool-btn"
        title="Options"
        onClick={(e) => optionsMenu(e.currentTarget.getBoundingClientRect())}
      >
        <Settings2 size={13} />
      </button>
    </>
  )
  const body = (
    <>
      {query && (
        <div className={queryFound ? 'code-speed' : 'code-speed code-speed-none'}>
          <Search size={12} />
          <span>{query}</span>
        </div>
      )}
      <div
        className={['code-tree', dropAt === '' && 'code-tree-drop', embedded && 'code-tree-embedded']
          .filter(Boolean)
          .join(' ')}
        ref={tree}
        tabIndex={0}
        onKeyDown={onKey}
        onBlur={() => setQuery('')}
        onFocus={() => {
          if (!cursor) setCursor(selected ?? rows()[0]?.path ?? null)
        }}
        onContextMenu={(ev) => {
          if (ev.target === ev.currentTarget || (ev.target as HTMLElement).closest('.code-tree-root')) {
            setChosen([])
            menu.open(ev, entryMenu('', true, ev.clientX, ev.clientY))
          }
        }}
        onDragOver={(ev) => dragOver(ev, null)}
        onDragLeave={(ev) => {
          if (!ev.currentTarget.contains(ev.relatedTarget as Node | null)) setDropAt(null)
        }}
        onDrop={drop}
        onClick={(ev) => {
          if (ev.target === ev.currentTarget) setChosen([])
        }}
      >
        {embedded ? (
          <div
            className="code-tree-root code-tree-project"
            title={root}
            onClick={embedded.onCollapse}
            onContextMenu={(ev) => {
              ev.preventDefault()
              ev.stopPropagation()
              setChosen([])
              menu.open(ev, [...embedded.menu, 'separator', ...entryMenu('', true, ev.clientX, ev.clientY)])
            }}
          >
            {embedded.collapsed ? (
              <ChevronRight size={12} className="code-chev" />
            ) : (
              <ChevronDown size={12} className="code-chev" />
            )}
            <Folder size={14} className="code-icon-project" />
            <span className="strong">{rootName}</span>
            <span className="muted small code-tree-rootpath">{root}</span>
            {embedded.branch && (
              <span className="code-tree-branch">
                <GitBranch size={10} />
                {embedded.branch}
              </span>
            )}
          </div>
        ) : (
          <div className="code-tree-root" title={root}>
            <FolderOpen size={14} className="code-icon-dir" />
            <span className="strong">{rootName}</span>
            <span className="muted small code-tree-rootpath">{root}</span>
          </div>
        )}
        {!embedded?.collapsed && renderDir('', 1)}
      </div>
      {(error || done || busy) && (
        <div className={error ? 'code-tree-status code-note-error' : 'code-tree-status'}>
          <span>{error || busy || done}</span>
          {!error && !busy && done && undos.current.length > 0 && !done.startsWith('Undid') && (
            <button type="button" className="code-tool-btn" title={`Undo (${shortcut('Mod+Z')})`} onClick={undo}>
              <Undo2 size={13} />
              Undo
            </button>
          )}
          <button
            type="button"
            className="code-tool-btn"
            title="Dismiss"
            onClick={() => {
              setError('')
              setDone('')
            }}
          >
            ×
          </button>
        </div>
      )}
      {menu.menu}
      {dialog?.kind === 'transfer' && (
        <TransferDialog
          client={client}
          op={dialog.op}
          paths={dialog.paths}
          dir={dialog.dir}
          onClose={() => setDialog(null)}
          onDone={(to, name) => {
            setDialog(null)
            transfer(dialog.op, dialog.paths, to, name)
          }}
        />
      )}
      {dialog?.kind === 'clash' && (
        <ClashDialog
          names={dialog.names}
          to={dialog.to}
          op={dialog.op}
          onChoose={(c) => {
            setDialog(null)
            dialog.answer(c)
          }}
        />
      )}
    </>
  )
  if (embedded) return <div className="code-project">{body}</div>
  return (
    <ToolWindow title="Project" onHide={onHide} actions={toolbar}>
      {body}
    </ToolWindow>
  )
}

// modeString is a mode as ls writes it: rwxr-xr-x.
function modeString(mode: number): string {
  return [6, 3, 0]
    .map((s) => {
      const m = (mode >> s) & 7
      return `${m & 4 ? 'r' : '-'}${m & 2 ? 'w' : '-'}${m & 1 ? 'x' : '-'}`
    })
    .join('')
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
          e.stopPropagation()
          if (e.key === 'Enter') onDone(e.currentTarget.value)
          if (e.key === 'Escape') onDone('')
        }}
        onBlur={(e) => onDone(e.currentTarget.value)}
      />
    </div>
  )
}
