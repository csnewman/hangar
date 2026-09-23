import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { useState, type FormEvent } from 'react'
import { Navigate, useNavigate, useSearchParams } from 'react-router'

import { api } from '../api'
import { Logo } from '../components/Logo'
import { announceSessionChange, meKey } from '../session'

// destination is where to go once signed in. Only a path on this site: a
// full URL here would make the login page an open redirect.
function destination(next: string | null): string {
  return next && next.startsWith('/') && !next.startsWith('//') ? next : '/environments'
}

export function LoginPage() {
  const qc = useQueryClient()
  const navigate = useNavigate()
  const [params] = useSearchParams()
  const [username, setUsername] = useState('')
  const [password, setPassword] = useState('')

  // Already signed in -- in this tab earlier, or in another since -- means
  // there is nothing to do here. It is rechecked whenever the tab regains
  // focus, and when another tab says the session changed.
  const me = useQuery({ queryKey: meKey, queryFn: api.me, retry: false, refetchInterval: false })

  const login = useMutation({
    mutationFn: () => api.login(username, password),
    onSuccess: (me) => {
      qc.clear()
      qc.setQueryData(meKey, me)
      announceSessionChange()
      navigate(destination(params.get('next')), { replace: true })
    },
  })

  if (me.isSuccess) return <Navigate to={destination(params.get('next'))} replace />

  const submit = (e: FormEvent) => {
    e.preventDefault()
    login.mutate()
  }

  return (
    <div className="login">
      <form className="login-card" onSubmit={submit}>
        <div className="login-brand">
          <Logo size={28} />
          <span>Hangar</span>
        </div>
        <h1>Sign in</h1>
        <label className="field">
          <span>Username</span>
          <input
            autoFocus
            required
            autoComplete="username"
            value={username}
            onChange={(e) => setUsername(e.target.value)}
          />
        </label>
        <label className="field">
          <span>Password</span>
          <input
            required
            type="password"
            autoComplete="current-password"
            value={password}
            onChange={(e) => setPassword(e.target.value)}
          />
        </label>
        {login.error && <div className="alert">{login.error.message}</div>}
        <button type="submit" className="btn btn-primary btn-block" disabled={login.isPending}>
          {login.isPending ? 'Signing in…' : 'Sign in'}
        </button>
      </form>
    </div>
  )
}
