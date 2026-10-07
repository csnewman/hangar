import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { ChevronRight, FileText, Folder, FolderOpen, Plus, Search } from 'lucide-react'
import { useEffect, useMemo, useState, type FormEvent } from 'react'

import type { ProfileFile, ProfileFileContent } from '../api'
import { ConfirmButton } from './ConfirmButton'

// What each part of a profile is, for people who did not write the list.
const describe: [string, string][] = [
  ['.claude/.credentials.json', "Claude's sign-in"],
  ['.claude/settings.json', "Claude's settings"],
  ['.claude/CLAUDE.md', "Claude's instructions for every project"],
  ['.claude/agents/', 'Claude subagents'],
  ['.claude/commands/', 'Claude slash commands'],
  ['.claude/skills/', 'Claude skills'],
  ['.claude/output-styles/', 'Claude output styles'],
  ['.claude/statusline-command.sh', "Claude's status line script"],
  ['.claude/', "Claude's settings, sign-in, instructions and extensions"],
  ['.gitconfig', "git's settings"],
  ['.git-credentials', "git's stored HTTPS credentials"],
  ['.netrc', 'Logins for curl, git and others'],
  ['.config/gh/hosts.yml', 'GitHub CLI sign-in'],
  ['.config/gh/config.yml', 'GitHub CLI settings'],
  ['.config/glab-cli/config.yml', 'GitLab CLI sign-in and settings'],
  ['.docker/config.json', 'Container registry logins'],
  ['.npmrc', 'npm registry logins'],
  ['.pypirc', 'PyPI logins'],
  ['.aws/credentials', 'AWS keys'],
  ['.aws/config', 'AWS profiles'],
  ['.kube/config', 'Kubernetes clusters and credentials'],
  ['.config/gcloud/application_default_credentials.json', 'Google Cloud application credentials'],
  ['.config/gcloud/configurations/', 'Google Cloud configurations'],
  ['.config/gcloud/active_config', 'Google Cloud active configuration'],
  ['.vscode-server-oss/data/User/settings.json', 'VS Code settings'],
  ['.vscode-server-oss/data/User/keybindings.json', 'VS Code keybindings'],
  ['.vscode-server-oss/data/User/snippets/', 'VS Code snippets'],
]

// describeExactly is what a listed path itself is, for the browser's rows,
// where what a folder holds is said once, at the folder.
function describeExactly(path: string): string | undefined {
  return describe.find(([p]) => p === path)?.[1]
}

export function describePath(path: string): string | undefined {
  for (const [p, what] of describe) {
    if (path === p || (p.endsWith('/') && path.startsWith(p))) return what
  }
  return undefined
}

// A folder of the browser's tree. Its path ends in a slash; the top's is "".
type FolderNode = {
  path: string
  name: string
  folders: FolderNode[]
  files: ProfileFile[]
  count: number
  size: number
}

// treeOf arranges files into folders, each level's folders first and by
// name, counting what is under each folder. Absolute paths, a pack's, are
// under "/".
function treeOf(files: ProfileFile[]): FolderNode {
  const root: FolderNode = {
    path: '',
    name: '',
    folders: [],
    files: [],
    count: 0,
    size: 0,
  }
  for (const f of files) {
    const abs = f.path.startsWith('/')
    let at = root
    at.count++
    at.size += f.size
    if (abs) at.path = '/'
    for (const part of (abs ? f.path.slice(1) : f.path).split('/').slice(0, -1)) {
      const path = at.path + part + '/'
      let next = at.folders.find((d) => d.path === path)
      if (!next) {
        next = { path, name: part, folders: [], files: [], count: 0, size: 0 }
        at.folders.push(next)
      }
      at = next
      at.count++
      at.size += f.size
    }
    at.files.push(f)
  }
  const sort = (d: FolderNode) => {
    d.folders.sort((a, b) => a.name.localeCompare(b.name))
    d.files.sort((a, b) => a.path.localeCompare(b.path))
    d.folders.forEach(sort)
  }
  sort(root)
  return root
}

// foldersOf is every folder a file is in: "a/b/c" is in "a/" and "a/b/".
function foldersOf(path: string): string[] {
  const parts = path.split('/').slice(0, -1)
  return parts.map((_, i) => parts.slice(0, i + 1).join('/') + '/')
}

function bytes(n: number): string {
  if (n < 1024) return `${n} B`
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KiB`
  return `${(n / 1024 / 1024).toFixed(1)} MiB`
}

// The folders left open, remembered in this browser only.
const expandedKey = 'hangar.profile.expanded'

function loadExpanded(): Set<string> {
  try {
    return new Set(JSON.parse(localStorage.getItem(expandedKey) ?? '[]') as string[])
  } catch {
    return new Set()
  }
}

function saveExpanded(expanded: Set<string>) {
  try {
    localStorage.setItem(expandedKey, JSON.stringify([...expanded]))
  } catch {
    // Remembered for this visit only.
  }
}

// FileSource is where a browser's files are kept: a profile, or a pack.
export type FileSource = {
  // key lists the files; a change here refreshes it.
  key: readonly unknown[]
  get: (path: string) => Promise<ProfileFileContent>
  put: (path: string, content: string) => Promise<unknown>
  settings: (path: string, mode: number, sensitive: boolean) => Promise<unknown>
  remove: (path: string) => Promise<unknown>
  // writable is whether the files may be changed here.
  writable: boolean
  // empty is said when there are no files, and example is a new file's
  // path, as a hint.
  empty: string
  example: string
}

// FileBrowser is a source's files as folders: a tree on one side, the file
// chosen from it on the other.
export function FileBrowser({
  source,
  files,
  paths,
  open,
  onOpen,
}: {
  source: FileSource
  files: ProfileFile[]
  paths: string[]
  open: string | null
  onOpen: (path: string | null) => void
}) {
  const [expanded, setExpanded] = useState(loadExpanded)
  const [filter, setFilter] = useState('')
  const query = filter.trim().toLowerCase()
  const shown = useMemo(
    () => (query ? files.filter((f) => f.path.toLowerCase().includes(query)) : files),
    [files, query],
  )
  const tree = useMemo(() => treeOf(shown), [shown])
  const total = useMemo(() => files.reduce((n, f) => n + f.size, 0), [files])
  const selected = files.find((f) => f.path === open) ?? null

  const expand = (next: Set<string>) => {
    setExpanded(next)
    saveExpanded(next)
  }
  const toggle = (path: string) => {
    const next = new Set(expanded)
    if (next.has(path)) next.delete(path)
    else next.add(path)
    expand(next)
  }
  // Choosing a file opens the folders it is in, so it stays in view.
  const choose = (path: string) => {
    onOpen(path)
    const missing = foldersOf(path).filter((d) => !expanded.has(d))
    if (missing.length > 0) expand(new Set([...expanded, ...missing]))
  }
  // While filtering, every folder with a match is open.
  const isOpen = (path: string) => query !== '' || expanded.has(path)

  return (
    <div className="panel fb">
      <div className="fb-tree">
        <label className="fb-filter">
          <Search size={14} />
          <input placeholder="Filter files" value={filter} onChange={(e) => setFilter(e.target.value)} />
        </label>
        <div className="fb-rows">
          {files.length === 0 ? (
            <div className="empty small">{source.empty}</div>
          ) : shown.length === 0 ? (
            <div className="empty small">No file matches.</div>
          ) : (
            <FolderRows folder={tree} depth={0} isOpen={isOpen} toggle={toggle} open={open} choose={choose} />
          )}
        </div>
      </div>
      <div className="fb-detail">
        {selected ? (
          <FileDetail key={selected.path} source={source} file={selected} onRemoved={() => onOpen(null)} />
        ) : (
          <div className="fb-none muted">
            {files.length} {files.length === 1 ? 'file' : 'files'}, {bytes(total)}. Choose one to see or edit it.
          </div>
        )}
        {source.writable && (
          <AddFile
            source={source}
            paths={paths}
            existing={files.map((f) => f.path)}
            folder={selected ? (foldersOf(selected.path).at(-1) ?? '') : ''}
            onAdded={choose}
          />
        )}
      </div>
    </div>
  )
}

function FolderRows({
  folder,
  depth,
  isOpen,
  toggle,
  open,
  choose,
}: {
  folder: FolderNode
  depth: number
  isOpen: (path: string) => boolean
  toggle: (path: string) => void
  open: string | null
  choose: (path: string) => void
}) {
  return (
    <>
      {folder.folders.map((d) => {
        const expanded = isOpen(d.path)
        const what = describeExactly(d.path)
        return (
          <div key={d.path}>
            <button
              type="button"
              className="fb-row"
              style={{ paddingLeft: 8 + depth * 16 }}
              onClick={() => toggle(d.path)}
              aria-expanded={expanded}
            >
              <ChevronRight size={13} className={`chev${expanded ? ' chev-open' : ''}`} />
              {expanded ? <FolderOpen size={14} /> : <Folder size={14} />}
              <span className="fb-name mono">{d.name}</span>
              {what && <span className="fb-what">{what}</span>}
              <span className="fb-meta">{d.count}</span>
            </button>
            {expanded && (
              <FolderRows folder={d} depth={depth + 1} isOpen={isOpen} toggle={toggle} open={open} choose={choose} />
            )}
          </div>
        )
      })}
      {folder.files.map((f) => {
        const what = describeExactly(f.path)
        return (
          <button
            key={f.path}
            type="button"
            className={`fb-row${open === f.path ? ' active' : ''}`}
            style={{ paddingLeft: 8 + depth * 16 + 19 }}
            onClick={() => choose(f.path)}
          >
            <FileText size={14} />
            <span className="fb-name mono">{f.path.split('/').at(-1)}</span>
            {what && <span className="fb-what">{what}</span>}
            <span className="fb-meta">{bytes(f.size)}</span>
          </button>
        )
      })}
    </>
  )
}

// FileDetail is one file: what it is, and its contents to edit.
function FileDetail({ source, file, onRemoved }: { source: FileSource; file: ProfileFile; onRemoved: () => void }) {
  const qc = useQueryClient()
  const remove = useMutation({
    mutationFn: () => source.remove(file.path),
    onSuccess: onRemoved,
    onSettled: () => qc.invalidateQueries({ queryKey: source.key }),
  })
  const what = describePath(file.path)
  return (
    <div className="fb-file">
      <div className="fb-head">
        <div className="fb-title">
          <div className="mono fb-path">{file.path}</div>
          <div className="muted small">
            {what && `${what} · `}
            {bytes(file.size)} · changed {new Date(file.updated_at).toLocaleString()}
          </div>
        </div>
        {source.writable && (
          <ConfirmButton
            label="Remove"
            confirmLabel="Remove everywhere?"
            onConfirm={() => remove.mutate()}
            disabled={remove.isPending}
          />
        )}
      </div>
      <FileSettings source={source} file={file} />
      <FileEditor source={source} path={file.path} updatedAt={file.updated_at} />
    </div>
  )
}

// The modes offered, with what each lets others do; another set elsewhere
// is offered as it is.
const modes: [number, string][] = [
  [0o600, 'only you can read and write it'],
  [0o644, 'anyone can read it'],
  [0o640, 'your group can read it'],
  [0o700, 'only you, and it runs'],
  [0o755, 'anyone can read and run it'],
]

// FileSettings are a file's settings, each saved as it is changed.
function FileSettings({ source, file }: { source: FileSource; file: ProfileFile }) {
  const qc = useQueryClient()
  const save = useMutation({
    mutationFn: (s: { mode: number; sensitive: boolean }) => source.settings(file.path, s.mode, s.sensitive),
    onSettled: () => qc.invalidateQueries({ queryKey: source.key }),
  })
  const disabled = save.isPending || !source.writable
  const choices = modes.some(([m]) => m === file.mode) ? modes : [[file.mode, 'as it is'] as [number, string], ...modes]
  return (
    <div className="fb-settings">
      <label className="field-inline">
        <span>Mode</span>
        <select
          className="mono"
          value={file.mode}
          disabled={disabled}
          onChange={(e) =>
            save.mutate({
              mode: Number(e.target.value),
              sensitive: file.sensitive,
            })
          }
        >
          {choices.map(([m, what]) => (
            <option key={m} value={m}>
              {m.toString(8).padStart(3, '0')} — {what}
            </option>
          ))}
        </select>
      </label>
      <label className="check">
        <input
          type="checkbox"
          checked={file.sensitive}
          disabled={disabled}
          onChange={(e) => save.mutate({ mode: file.mode, sensitive: e.target.checked })}
        />
        <span>
          Sensitive
          <small className="muted">
            A credential: environments whose template withholds sensitive files are not given it.
          </small>
        </span>
      </label>
      {save.error && <span className="action-error">{save.error.message}</span>}
    </div>
  )
}

// FileEditor edits one file. While it has no unsaved change it follows the
// file as environments change it; once it has one, it says when the file has
// changed underneath rather than overwriting what is being typed.
function FileEditor({ source, path, updatedAt }: { source: FileSource; path: string; updatedAt: string }) {
  const qc = useQueryClient()
  const file = useQuery({
    queryKey: [...source.key, 'file', path, updatedAt],
    queryFn: () => source.get(path),
  })
  const [text, setText] = useState<string | null>(null)
  const [base, setBase] = useState<string | null>(null)
  const saved = file.data?.content ?? null
  const dirty = text !== null && text !== base

  useEffect(() => {
    if (saved !== null && !dirty) {
      setText(saved)
      setBase(saved)
    }
  }, [saved, dirty])

  const save = useMutation({
    mutationFn: () => source.put(path, text ?? ''),
    onSuccess: () => {
      setBase(text)
      qc.invalidateQueries({ queryKey: source.key })
    },
  })

  if (file.isPending || text === null) return <div className="muted small">Loading…</div>
  return (
    <div className="form">
      <textarea
        className="mono profile-editor"
        spellCheck={false}
        value={text}
        readOnly={!source.writable}
        onChange={(e) => setText(e.target.value)}
        rows={Math.min(24, Math.max(6, text.split('\n').length + 1))}
      />
      {dirty && saved !== base && (
        <div className="notice">It has changed in an environment since you started editing; saving replaces that.</div>
      )}
      {save.error && <div className="alert">{save.error.message}</div>}
      {source.writable && (
        <div className="form-actions">
          <button type="button" className="btn btn-ghost" disabled={!dirty} onClick={() => setText(saved)}>
            Discard
          </button>
          <button
            type="button"
            className="btn btn-primary"
            disabled={!dirty || save.isPending}
            onClick={() => save.mutate()}
          >
            Save
          </button>
        </div>
      )}
    </div>
  )
}

function AddFile({
  source,
  paths,
  existing,
  folder,
  onAdded,
}: {
  source: FileSource
  paths: string[]
  existing: string[]
  folder: string
  onAdded: (p: string) => void
}) {
  const qc = useQueryClient()
  const choices = paths.filter((p) => !p.startsWith('!') && !existing.includes(p))
  const [path, setPath] = useState('')
  const add = useMutation({
    mutationFn: (p: string) => source.put(p, ''),
    onSuccess: (_, p) => {
      qc.invalidateQueries({ queryKey: source.key })
      onAdded(p)
      setPath('')
    },
  })
  const submit = (e: FormEvent) => {
    e.preventDefault()
    if (path && !path.endsWith('/')) add.mutate(path)
  }
  return (
    <form className="inline-form fb-add" onSubmit={submit}>
      <input
        className="mono grow"
        list="file-paths"
        placeholder={`New file: ${folder || source.example}`}
        value={path}
        // Starts in the folder of the file open beside it.
        onFocus={() => {
          if (!path && folder) setPath(folder)
        }}
        onChange={(e) => setPath(e.target.value)}
      />
      <datalist id="file-paths">
        {choices.map((p) => (
          <option key={p} value={p}>
            {describePath(p)}
          </option>
        ))}
      </datalist>
      <button type="submit" className="btn btn-ghost" disabled={!path || path.endsWith('/') || add.isPending}>
        <Plus size={14} />
        Add
      </button>
      {add.error && <span className="action-error">{add.error.message}</span>}
    </form>
  )
}
