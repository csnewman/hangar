import {
  Boxes,
  ChevronRight,
  FolderSync,
  History,
  LayoutTemplate,
  Server,
  Users,
  UsersRound,
  type LucideIcon,
} from 'lucide-react'
import { Link } from 'react-router'

import { useMe } from '../session'
import { useTitle } from '../title'

// MorePage is the phone's way to what the sidebar holds beyond the
// environments and the settings: what environments are made from and
// share, and administration.
export function MorePage() {
  const me = useMe()
  useTitle('More')
  return (
    <div className="page phone-page">
      <h1 className="phone-title">More</h1>
      <div className="phone-list">
        <Row to="/templates" icon={LayoutTemplate} label="Templates" sub="What environments are made from" />
        <Row to="/images" icon={Boxes} label="Images" sub="Hangar's own registry" />
        <Row to="/files" icon={FolderSync} label="Files" sub="Packs of files environments share" />
        <Row to="/teams" icon={UsersRound} label="Teams" sub="Who shares templates and images" />
      </div>
      {me.admin && (
        <>
          <h2 className="phone-heading">Administration</h2>
          <div className="phone-list">
            <Row to="/admin/workers" icon={Server} label="Workers" />
            <Row to="/admin/users" icon={Users} label="Users" />
            <Row to="/admin/audit" icon={History} label="Audit log" />
          </div>
        </>
      )}
    </div>
  )
}

function Row({ to, icon: Icon, label, sub }: { to: string; icon: LucideIcon; label: string; sub?: string }) {
  return (
    <Link to={to} className="phone-row">
      <span className="phone-row-mark">
        <Icon size={17} />
      </span>
      <span className="phone-row-main">
        <span>{label}</span>
        {sub && <span className="phone-row-sub">{sub}</span>}
      </span>
      <ChevronRight size={16} className="phone-row-chev" />
    </Link>
  )
}
