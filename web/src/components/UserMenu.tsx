import { useMutation, useQueryClient } from '@tanstack/react-query'
import { ChevronDown, LogOut, UserRound, FolderSync } from 'lucide-react'
import { useEffect, useRef, useState } from 'react'
import { Link, useNavigate } from 'react-router'

import { api } from '../api'
import { announceSessionChange, useMe } from '../session'

export function UserMenu() {
  const me = useMe()
  const qc = useQueryClient()
  const navigate = useNavigate()
  const [open, setOpen] = useState(false)
  const ref = useRef<HTMLDivElement>(null)

  useEffect(() => {
    if (!open) return
    const close = (e: MouseEvent) => {
      if (ref.current && !ref.current.contains(e.target as Node)) setOpen(false)
    }
    const escape = (e: KeyboardEvent) => e.key === 'Escape' && setOpen(false)
    document.addEventListener('mousedown', close)
    document.addEventListener('keydown', escape)
    return () => {
      document.removeEventListener('mousedown', close)
      document.removeEventListener('keydown', escape)
    }
  }, [open])

  const logout = useMutation({
    mutationFn: api.logout,
    onSettled: () => {
      qc.clear()
      announceSessionChange()
      navigate('/login', { replace: true })
    },
  })

  const name = me.display_name || me.username
  return (
    <div className="user-menu" ref={ref}>
      <button type="button" className="user-button" onClick={() => setOpen(!open)} aria-expanded={open}>
        <span className="avatar">{initials(name)}</span>
        <span className="user-name">{name}</span>
        {me.admin && <span className="role">admin</span>}
        <ChevronDown size={14} />
      </button>
      {open && (
        <div className="menu" role="menu">
          <div className="menu-header">
            <div className="strong">{name}</div>
            <div className="muted small">{me.username}</div>
          </div>
          <Link to="/account" className="menu-item" role="menuitem" onClick={() => setOpen(false)}>
            <UserRound size={15} />
            Account
          </Link>
          <Link to="/profile" className="menu-item" role="menuitem" onClick={() => setOpen(false)}>
            <FolderSync size={15} />
            Profile
          </Link>
          <button type="button" className="menu-item" role="menuitem" onClick={() => logout.mutate()}>
            <LogOut size={15} />
            Sign out
          </button>
        </div>
      )}
    </div>
  )
}

function initials(name: string): string {
  const parts = name.split(/[\s._@-]+/).filter(Boolean)
  const letters = parts.length > 1 ? parts[0][0] + parts[1][0] : name.slice(0, 2)
  return letters.toUpperCase()
}
