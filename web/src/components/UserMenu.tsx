import { useMutation, useQueryClient } from '@tanstack/react-query'
import { ChevronsUpDown, FolderSync, LogOut, UserRound } from 'lucide-react'
import { useEffect, useRef, useState, type CSSProperties } from 'react'
import { createPortal } from 'react-dom'
import { Link, useNavigate } from 'react-router'

import { api } from '../api'
import { announceSessionChange, useMe } from '../session'

// UserMenu is who is signed in, at the foot of the sidebar, with their own
// pages and signing out. Folded, it is their initials alone. Its menu opens
// above the button, drawn on the page itself, not in the sidebar, which
// clips what overflows it when folded.
export function UserMenu({ collapsed = false }: { collapsed?: boolean }) {
  const me = useMe()
  const qc = useQueryClient()
  const navigate = useNavigate()
  const [open, setOpen] = useState(false)
  const ref = useRef<HTMLDivElement>(null)
  const menuRef = useRef<HTMLDivElement>(null)
  const [at, setAt] = useState<CSSProperties>({})

  const toggle = () => {
    const r = ref.current?.getBoundingClientRect()
    if (r) setAt({ left: r.left, bottom: window.innerHeight - r.top + 6 })
    setOpen(!open)
  }

  useEffect(() => {
    if (!open) return
    const close = (e: MouseEvent) => {
      const t = e.target as Node
      if (!ref.current?.contains(t) && !menuRef.current?.contains(t)) setOpen(false)
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
      <button
        type="button"
        className={collapsed ? 'user-button user-button-collapsed' : 'user-button'}
        onClick={toggle}
        aria-expanded={open}
        title={collapsed ? name : undefined}
        aria-label={collapsed ? name : undefined}
      >
        <span className="avatar">{initials(name)}</span>
        {!collapsed && (
          <>
            <span className="user-name">{name}</span>
            {me.admin && <span className="role">admin</span>}
            <ChevronsUpDown size={14} />
          </>
        )}
      </button>
      {open &&
        createPortal(
          <div className="menu menu-fixed" role="menu" ref={menuRef} style={at}>
            <div className="menu-header">
              <div className="strong">{name}</div>
              <div className="muted small">{me.username}</div>
            </div>
            <Link to="/files" className="menu-item" role="menuitem" onClick={() => setOpen(false)}>
              <FolderSync size={15} />
              Files
            </Link>
            <Link to="/account" className="menu-item" role="menuitem" onClick={() => setOpen(false)}>
              <UserRound size={15} />
              Account
            </Link>
            <button type="button" className="menu-item" role="menuitem" onClick={() => logout.mutate()}>
              <LogOut size={15} />
              Sign out
            </button>
          </div>,
          document.body,
        )}
    </div>
  )
}

function initials(name: string): string {
  const parts = name.split(/[\s._@-]+/).filter(Boolean)
  const letters = parts.length > 1 ? parts[0][0] + parts[1][0] : name.slice(0, 2)
  return letters.toUpperCase()
}
