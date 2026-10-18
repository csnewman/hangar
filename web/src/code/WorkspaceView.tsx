import { FileSearch, FolderPlus, FolderX, ListCollapse, LocateFixed, Settings2 } from 'lucide-react'
import { useState, type ReactNode } from 'react'

import type { GitStatus } from './client'
import { shortcut, useContextMenu, type MenuItem } from './ContextMenu'
import { treeOptionsMenu, type Embedded } from './ProjectView'
import type { Project } from './projects'
import { ToolWindow } from './ToolWindow'

// WorkspaceView is the Workspace tool window: each project's tree, one
// under the other, each folded from its row, which also has the project's
// menu. Its toolbar acts on them all. A tab working in one folder alone
// shows that folder, and the way back to the workspace.
export function WorkspaceView({
  env,
  projects,
  missing,
  onRemove,
  statuses,
  alone,
  tree,
  projectMenu,
  onAdd,
  onGotoFile,
  onLocate,
  onBack,
  onHide,
}: {
  env: string
  projects: Project[]
  // missing are folders in the workspace that do not exist, and onRemove
  // takes one out of it.
  missing: string[]
  onRemove: (root: string) => void
  statuses: Record<string, GitStatus | null>
  // alone is whether the tab works in one folder alone.
  alone: boolean
  tree: (p: Project, embedded: Embedded) => ReactNode
  projectMenu: (p: Project, i: number) => MenuItem[]
  onAdd: () => void
  onGotoFile: () => void
  onLocate: () => void
  onBack: () => void
  onHide: () => void
}) {
  const key = `hangar.code.${env}.folded`
  const [folded, setFoldedState] = useState<Set<string>>(() => {
    try {
      return new Set(JSON.parse(localStorage.getItem(key) ?? '[]'))
    } catch {
      return new Set()
    }
  })
  const setFolded = (root: string, on: boolean) =>
    setFoldedState((f) => {
      const next = new Set(f)
      if (on) next.add(root)
      else next.delete(root)
      try {
        localStorage.setItem(key, JSON.stringify([...next]))
      } catch {
        // Which projects are folded is a convenience.
      }
      return next
    })
  const [collapseAll, setCollapseAll] = useState(0)
  const menu = useContextMenu()

  const branchOf = (root: string) => {
    const s = statuses[root]
    const r = s?.repos.find((x) => x.kind === 'folder') ?? (s?.repos.length === 1 ? s.repos[0] : undefined)
    if (!r?.branch) return undefined
    return `${r.branch}${r.ahead ? ` ↑${r.ahead}` : ''}${r.behind ? ` ↓${r.behind}` : ''}`
  }

  return (
    <ToolWindow
      title="Workspace"
      onHide={onHide}
      extra={
        <span className="muted small">
          {alone ? 'one folder, this tab only' : `${projects.length} project${projects.length === 1 ? '' : 's'}`}
        </span>
      }
      actions={
        <>
          <button type="button" className="code-tool-btn" title="Add Folder to Workspace…" onClick={onAdd}>
            <FolderPlus size={14} />
          </button>
          <button
            type="button"
            className="code-tool-btn"
            title={`Go to File (${shortcut('Mod+Shift+O')})`}
            onClick={onGotoFile}
          >
            <FileSearch size={14} />
          </button>
          <button type="button" className="code-tool-btn" title="Select the opened file" onClick={onLocate}>
            <LocateFixed size={13} />
          </button>
          <button
            type="button"
            className="code-tool-btn"
            title="Collapse all"
            onClick={() => setCollapseAll((n) => n + 1)}
          >
            <ListCollapse size={14} />
          </button>
          <button
            type="button"
            className="code-tool-btn"
            title="Options"
            onClick={(e) => {
              const b = e.currentTarget.getBoundingClientRect()
              menu.openAt(b.right, b.bottom + 2, treeOptionsMenu(), 'right')
            }}
          >
            <Settings2 size={13} />
          </button>
        </>
      }
    >
      <div className="code-workspace">
        {alone && (
          <div className="code-workspace-alone">
            <span>This tab shows one folder.</span>
            <button type="button" className="code-tool-btn" onClick={onBack}>
              Back to the workspace
            </button>
          </div>
        )}
        {projects.map((p, i) => (
          <div key={p.root} className="code-workspace-project">
            {tree(p, {
              branch: branchOf(p.root),
              collapsed: folded.has(p.root),
              onCollapse: () => setFolded(p.root, !folded.has(p.root)),
              menu: projectMenu(p, i),
              collapseAll,
            })}
          </div>
        ))}
        {missing.map((root) => (
          <div key={root} className="code-workspace-project">
            <div className="code-tree-root code-workspace-missing" title={`${root} does not exist`}>
              <FolderX size={14} />
              <span className="strong">{root.slice(root.lastIndexOf('/') + 1) || root}</span>
              <span className="muted small code-tree-rootpath">{root} does not exist</span>
              <button type="button" className="code-tool-btn" onClick={() => onRemove(root)}>
                Remove
              </button>
            </div>
          </div>
        ))}
        {!alone && (
          <button type="button" className="code-workspace-add" onClick={onAdd}>
            <FolderPlus size={13} />
            Add Folder to Workspace…
          </button>
        )}
      </div>
      {menu.menu}
    </ToolWindow>
  )
}
