import {
  ChevronRight,
  FolderSync,
  History,
  LayoutGrid,
  LayoutTemplate,
  MonitorUp,
  PanelLeftClose,
  PanelLeftOpen,
  Plus,
  Server,
  UserRound,
  Users,
} from 'lucide-react'
import { useState, type ReactNode } from 'react'
import { Link, NavLink, useLocation } from 'react-router'

import type { Environment } from '../api'
import { desktop } from '../desktop'
import { useMe } from '../session'
import { splitByOwner, useEnvironments } from '../environments'
import { Logo } from './Logo'
import { PhaseDot } from './Status'
import { UserMenu } from './UserMenu'

// Sidebar is the inventory: every environment the user can reach, as a tree,
// then the user's own settings, and administration for administrators. Who
// is signed in sits at its foot, below whatever scrolls.
export function Sidebar({ collapsed = false, onToggle }: { collapsed?: boolean; onToggle: () => void }) {
  const me = useMe()
  const envs = useEnvironments()
  const { mine, others } = splitByOwner(envs.data ?? [], me)

  if (collapsed) {
    // Folded to its icons: the environments are on the Overview.
    return (
      <aside className="sidebar sidebar-collapsed">
        <SidebarHead collapsed onToggle={onToggle} />
        <div className="sidebar-scroll">
          <nav className="side-nav">
            <SideIcon to="/environments" end icon={<LayoutGrid size={17} />} label="Overview" />
            <SideIcon to="/templates" icon={<LayoutTemplate size={17} />} label="Templates" />
            <SideIcon to="/environments/new" icon={<Plus size={17} />} label="New environment" />
          </nav>
          <nav className="side-nav side-group">
            <SideIcon to="/profile" icon={<FolderSync size={17} />} label="Profile" />
            <SideIcon to="/account" icon={<UserRound size={17} />} label="Account" />
          </nav>
          {me.admin && (
            <nav className="side-nav side-group">
              <SideIcon to="/admin/workers" icon={<Server size={17} />} label="Workers" />
              <SideIcon to="/admin/users" icon={<Users size={17} />} label="Users" />
              <SideIcon to="/admin/audit" icon={<History size={17} />} label="Audit log" />
            </nav>
          )}
        </div>
        <div className="sidebar-foot">
          <OpenInDesktop collapsed />
          <UserMenu collapsed />
        </div>
      </aside>
    )
  }

  return (
    <aside className="sidebar">
      <SidebarHead onToggle={onToggle} />
      <div className="sidebar-scroll">
        <nav className="side-nav">
          <NavLink to="/environments" end className="side-link">
            <LayoutGrid size={16} />
            <span>Overview</span>
          </NavLink>
          <NavLink to="/templates" className="side-link">
            <LayoutTemplate size={16} />
            <span>Templates</span>
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

        <div className="side-section">
          <div className="side-heading">
            <span>You</span>
          </div>
          <NavLink to="/profile" className="side-link">
            <FolderSync size={16} />
            <span>Profile</span>
          </NavLink>
          <NavLink to="/account" className="side-link">
            <UserRound size={16} />
            <span>Account</span>
          </NavLink>
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
            <NavLink to="/admin/audit" className="side-link">
              <History size={16} />
              <span>Audit log</span>
            </NavLink>
          </div>
        )}
      </div>
      <div className="sidebar-foot">
        <OpenInDesktop />
        <UserMenu />
      </div>
    </aside>
  )
}

// SidebarHead is the brand, and the button that folds the sidebar to its
// icons and back (Ctrl+B).
function SidebarHead({ collapsed = false, onToggle }: { collapsed?: boolean; onToggle: () => void }) {
  return (
    <div className={collapsed ? 'sidebar-head sidebar-head-collapsed' : 'sidebar-head'}>
      {!collapsed && (
        <Link to="/environments" className="brand">
          <Logo />
          <span>Hangar</span>
        </Link>
      )}
      <button
        type="button"
        className="icon-btn sidebar-toggle"
        onClick={onToggle}
        title={collapsed ? 'Show the sidebar (Ctrl+B)' : 'Fold the sidebar (Ctrl+B)'}
        aria-label={collapsed ? 'Show the sidebar' : 'Fold the sidebar'}
      >
        {collapsed ? <PanelLeftOpen size={17} /> : <PanelLeftClose size={17} />}
      </button>
    </div>
  )
}

// OpenInDesktop opens this page in the Hangar desktop app: a hangar:// link
// naming this server and page, which the app opens in this server's window
// -- an environment's page in that environment's tab. It is not shown
// inside the app.
function OpenInDesktop({ collapsed = false }: { collapsed?: boolean }) {
  const { pathname, search } = useLocation()
  if (desktop) return null
  const q = new URLSearchParams({ server: window.location.origin, path: pathname + search })
  const label = 'Open in desktop app'
  return (
    <a
      href={`hangar://open?${q}`}
      className={collapsed ? 'side-link side-icon' : 'side-link'}
      title={collapsed ? label : 'Opens this page in the Hangar desktop app, if it is installed'}
      aria-label={label}
    >
      <MonitorUp size={collapsed ? 17 : 16} />
      {!collapsed && <span>{label}</span>}
    </a>
  )
}

function SideIcon({ to, icon, label, end }: { to: string; icon: ReactNode; label: string; end?: boolean }) {
  return (
    <NavLink to={to} end={end} className="side-link side-icon" title={label} aria-label={label}>
      {icon}
    </NavLink>
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
