import {
  Boxes,
  FolderSync,
  History,
  Layers,
  LayoutTemplate,
  MonitorUp,
  PanelLeftClose,
  PanelLeftOpen,
  Server,
  Settings,
  Users,
  UsersRound,
} from 'lucide-react'
import { type ReactNode } from 'react'
import { Link, NavLink, useLocation } from 'react-router'

import { desktop } from '../desktop'
import { useMe } from '../session'
import { useEnvironments } from '../environments'
import { Logo } from './Logo'
import { UserMenu } from './UserMenu'

// Sidebar is Hangar's own sections: environments, then what they are made
// from, then administration for administrators, and at its foot your
// settings and who is signed in. Each environment's workspace is a tab of
// its own, so the environments are a page here, not a tree: the Environments
// item says how many are running.
export function Sidebar({ collapsed = false, onToggle }: { collapsed?: boolean; onToggle: () => void }) {
  const me = useMe()
  const envs = useEnvironments()
  const running = (envs.data ?? []).filter((e) => e.owner_id === me.id && e.phase === 'running').length
  const items: { to: string; label: string; icon: ReactNode; end?: boolean; count?: string }[] = [
    {
      to: '/environments',
      label: 'Environments',
      icon: <Layers size={16} />,
      count: running > 0 ? `${running} running` : undefined,
    },
    { to: '/templates', label: 'Templates', icon: <LayoutTemplate size={16} /> },
    { to: '/images', label: 'Images', icon: <Boxes size={16} /> },
    { to: '/files', label: 'Files', icon: <FolderSync size={16} /> },
    { to: '/teams', label: 'Teams', icon: <UsersRound size={16} /> },
  ]
  const admin: typeof items = [
    { to: '/admin/workers', label: 'Workers', icon: <Server size={16} /> },
    { to: '/admin/users', label: 'Users', icon: <Users size={16} /> },
    { to: '/admin/audit', label: 'Audit log', icon: <History size={16} /> },
  ]
  const link = (i: (typeof items)[number]) =>
    collapsed ? (
      <SideIcon key={i.to} to={i.to} end={i.end} icon={i.icon} label={i.count ? `${i.label}, ${i.count}` : i.label} />
    ) : (
      <NavLink key={i.to} to={i.to} end={i.end} className="side-link">
        {i.icon}
        <span>{i.label}</span>
        {i.count && <span className="side-count">{i.count}</span>}
      </NavLink>
    )
  return (
    <aside className={collapsed ? 'sidebar sidebar-collapsed' : 'sidebar'}>
      <SidebarHead collapsed={collapsed} onToggle={onToggle} />
      <div className="sidebar-scroll">
        <nav className="side-nav" aria-label="Hangar">
          {items.map(link)}
        </nav>
        {me.admin && (
          <nav className="side-section" aria-label="Administration">
            {!collapsed && (
              <div className="side-heading">
                <span>Administration</span>
              </div>
            )}
            {admin.map(link)}
          </nav>
        )}
      </div>
      <div className="sidebar-foot">
        {link({ to: '/settings', label: 'Settings', icon: <Settings size={16} /> })}
        <div className={collapsed ? 'side-user side-user-collapsed' : 'side-user'}>
          <UserMenu collapsed={collapsed} />
          {!collapsed && <OpenInDesktop />}
        </div>
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
function OpenInDesktop() {
  const { pathname, search } = useLocation()
  if (desktop) return null
  const q = new URLSearchParams({ server: window.location.origin, path: pathname + search })
  return (
    <a
      href={`hangar://open?${q}`}
      className="icon-btn side-desktop"
      title="Open this page in the Hangar desktop app, if it is installed"
      aria-label="Open in the desktop app"
    >
      <MonitorUp size={15} />
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
