import { ChevronRight, LayoutGrid, Plus, Server, Users } from 'lucide-react'
import { useState } from 'react'
import { Link, NavLink } from 'react-router'

import type { Environment } from '../api'
import { useMe } from '../session'
import { splitByOwner, useEnvironments } from '../environments'
import { PhaseDot } from './Status'

// Sidebar is the inventory: every environment the user can reach, as a tree,
// with administration beneath it for administrators.
export function Sidebar() {
  const me = useMe()
  const envs = useEnvironments()
  const { mine, others } = splitByOwner(envs.data ?? [], me)

  return (
    <aside className="sidebar">
      <nav className="side-nav">
        <NavLink to="/environments" end className="side-link">
          <LayoutGrid size={16} />
          <span>Overview</span>
        </NavLink>
      </nav>

      <div className="side-section">
        <div className="side-heading">
          <span>Environments</span>
          <Link to="/environments/new" className="icon-btn" title="New environment" aria-label="New environment">
            <Plus size={15} />
          </Link>
        </div>
        {me.admin ? (
          <>
            <TreeGroup id="mine" label="Mine" environments={mine} defaultOpen />
            {others.map((g) => (
              <TreeGroup key={g.owner} id={`owner:${g.owner}`} label={g.owner} environments={g.environments} />
            ))}
          </>
        ) : (
          <TreeItems environments={mine} />
        )}
        {envs.isSuccess && envs.data.length === 0 && <div className="side-empty">No environments yet.</div>}
      </div>

      {me.admin && (
        <div className="side-section">
          <div className="side-heading">
            <span>Administration</span>
          </div>
          <NavLink to="/admin/workers" className="side-link">
            <Server size={16} />
            <span>Workers</span>
          </NavLink>
          <NavLink to="/admin/users" className="side-link">
            <Users size={16} />
            <span>Users</span>
          </NavLink>
        </div>
      )}
    </aside>
  )
}

function readOpen(id: string, fallback: boolean): boolean {
  try {
    const v = localStorage.getItem(`hangar.tree.${id}`)
    return v === null ? fallback : v === '1'
  } catch {
    return fallback
  }
}

function TreeGroup({
  id,
  label,
  environments,
  defaultOpen = false,
}: {
  id: string
  label: string
  environments: Environment[]
  defaultOpen?: boolean
}) {
  const [open, setOpen] = useState(() => readOpen(id, defaultOpen))
  const toggle = () => {
    setOpen(!open)
    try {
      localStorage.setItem(`hangar.tree.${id}`, open ? '0' : '1')
    } catch {
      // Remembering which groups are open is a convenience.
    }
  }
  return (
    <div className="tree-group">
      <button type="button" className="tree-toggle" onClick={toggle} aria-expanded={open}>
        <ChevronRight size={14} className={open ? 'chev chev-open' : 'chev'} />
        <span className="tree-label">{label}</span>
        <span className="tree-count">{environments.length}</span>
      </button>
      {open && <TreeItems environments={environments} nested />}
    </div>
  )
}

function TreeItems({ environments, nested = false }: { environments: Environment[]; nested?: boolean }) {
  return (
    <ul className={nested ? 'tree tree-nested' : 'tree'}>
      {environments.map((e) => (
        <li key={e.id}>
          <NavLink to={`/environments/${e.id}`} className="tree-item" title={`${e.name} — ${e.phase}`}>
            <PhaseDot phase={e.phase} />
            <span className="tree-name">{e.name}</span>
          </NavLink>
        </li>
      ))}
    </ul>
  )
}
