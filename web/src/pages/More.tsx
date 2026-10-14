import { useMutation, useQueryClient } from '@tanstack/react-query'
import {
  Boxes,
  ChevronRight,
  FolderSync,
  History,
  LayoutTemplate,
  LogOut,
  Server,
  Users,
  UsersRound,
  type LucideIcon,
} from 'lucide-react'
import { Link, useNavigate } from 'react-router'

import { api } from '../api'
import { announceSessionChange, useMe } from '../session'
import { useTitle } from '../title'

// MorePage is the phone's way to what the sidebar holds beyond the
// environments and the account: the shared things, the user's files,
// administration, and signing out.
export function MorePage() {
  const me = useMe()
  const qc = useQueryClient()
  const navigate = useNavigate()
  useTitle('More')
  const logout = useMutation({
    mutationFn: api.logout,
    onSettled: () => {
      qc.clear()
      announceSessionChange()
      navigate('/login', { replace: true })
    },
  })

  return (
    <div className="page phone-page">
      <h1 className="phone-title">More</h1>
      <div className="phone-list">
        <Row to="/templates" icon={LayoutTemplate} label="Templates" />
        <Row to="/images" icon={Boxes} label="Images" />
        <Row to="/teams" icon={UsersRound} label="Teams" />
        <Row to="/files" icon={FolderSync} label="Files" />
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
      <div className="phone-list">
        <button type="button" className="phone-row phone-row-danger" onClick={() => logout.mutate()}>
          <LogOut size={18} />
          <span className="phone-row-main">Sign out of {me.display_name || me.username}</span>
        </button>
      </div>
    </div>
  )
}

function Row({ to, icon: Icon, label }: { to: string; icon: LucideIcon; label: string }) {
  return (
    <Link to={to} className="phone-row">
      <Icon size={18} />
      <span className="phone-row-main">{label}</span>
      <ChevronRight size={16} className="phone-row-chev" />
    </Link>
  )
}
