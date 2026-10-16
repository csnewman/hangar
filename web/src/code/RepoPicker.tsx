import { ChevronDown, FolderGit2 } from 'lucide-react'

import type { Repo } from './client'
import { useContextMenu } from './ContextMenu'
import { usable } from './repos'

// RepoPicker chooses one of the folder's repositories, shown where there
// is more than one.
export function RepoPicker({ repos, value, onPick }: { repos: Repo[]; value: string; onPick: (path: string) => void }) {
  const menu = useContextMenu()
  const choices = usable(repos)
  if (choices.length < 2) return null
  const current = choices.find((r) => r.path === value) ?? choices[0]
  return (
    <>
      <button
        type="button"
        className="code-branch-btn"
        title="Show another repository"
        onClick={(e) => {
          const r = e.currentTarget.getBoundingClientRect()
          menu.openAt(
            r.left,
            r.bottom + 2,
            choices.map((c) => ({
              label: c.name,
              detail: c.kind === 'folder' ? undefined : c.kind,
              checked: c.path === current.path,
              onSelect: () => onPick(c.path),
            })),
          )
        }}
      >
        <FolderGit2 size={12} />
        {current.name}
        <ChevronDown size={11} />
      </button>
      {menu.menu}
    </>
  )
}
