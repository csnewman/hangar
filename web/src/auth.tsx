import { useQuery } from '@tanstack/react-query'
import type { ReactNode } from 'react'
import { Navigate, Outlet, useLocation } from 'react-router'

import { api, ApiError } from './api'
import { MeContext, meKey, useMe } from './session'

// RequireAuth renders its routes for a signed-in user and sends anyone else
// to the login page, remembering where they were going.
export function RequireAuth() {
  const location = useLocation()
  const me = useQuery({ queryKey: meKey, queryFn: api.me, retry: false, refetchInterval: false, staleTime: 60_000 })

  if (me.isPending) return <div className="splash" />
  if (me.isError) {
    if (me.error instanceof ApiError && me.error.status === 401) {
      const next = location.pathname + location.search
      return <Navigate to={`/login?next=${encodeURIComponent(next)}`} replace />
    }
    return <div className="splash splash-error">Could not reach Hangar: {me.error.message}</div>
  }
  return (
    <MeContext.Provider value={me.data}>
      <Outlet />
    </MeContext.Provider>
  )
}

// RequireAdmin hides administration from everyone else. The server refuses
// them regardless; this only keeps them from a page that could not load.
export function RequireAdmin({ children }: { children: ReactNode }) {
  const me = useMe()
  if (!me.admin) return <Navigate to="/environments" replace />
  return children
}
