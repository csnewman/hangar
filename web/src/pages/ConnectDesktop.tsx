import { useMutation, useQuery } from '@tanstack/react-query'
import { useState, type FormEvent } from 'react'
import { useSearchParams } from 'react-router'

import { api, ApiError } from '../api'
import { Logo } from '../components/Logo'
import { useMe } from '../session'

// ConnectDesktopPage approves an app signing in through the browser: the
// app shows a code and opens this page with it, and the person, signed in
// here as themselves, lets it sign in with an access token of its own, or
// refuses it. Without a code in the address it asks for one.
export function ConnectDesktopPage() {
  const me = useMe()
  const [params, setParams] = useSearchParams()
  const code = params.get('code') ?? ''
  const [typed, setTyped] = useState('')

  const grant = useQuery({
    queryKey: ['sign-in', code],
    queryFn: () => api.signIn(code),
    enabled: code !== '',
    retry: false,
    refetchInterval: false,
  })
  const approve = useMutation({ mutationFn: () => api.approveSignIn(code) })
  const refuse = useMutation({ mutationFn: () => api.refuseSignIn(code) })

  const enter = (e: FormEvent) => {
    e.preventDefault()
    setParams({ code: typed.trim() })
  }

  let body
  if (code === '') {
    body = (
      <form className="login-form" onSubmit={enter}>
        <h1>Sign in an app</h1>
        <p className="muted">Type the code Hangar Desktop shows.</p>
        <label className="field">
          <span>Code</span>
          <input autoFocus required placeholder="XXXX-XXXX" value={typed} onChange={(e) => setTyped(e.target.value)} />
        </label>
        <button type="submit" className="btn btn-primary btn-block">
          Continue
        </button>
      </form>
    )
  } else if (approve.isSuccess) {
    body = (
      <>
        <h1>Signed in</h1>
        <p>{grant.data?.name ?? 'The app'} is signed in as you. You can close this tab and go back to it.</p>
        <p className="muted">Its access token is on your Account page, where you can revoke it.</p>
      </>
    )
  } else if (refuse.isSuccess) {
    body = (
      <>
        <h1>Refused</h1>
        <p>The app was told, and is not signed in.</p>
      </>
    )
  } else if (grant.isPending) {
    body = <p className="muted">Looking for the app…</p>
  } else if (grant.isError) {
    const gone = grant.error instanceof ApiError && grant.error.status === 404
    body = (
      <>
        <h1>Nothing is waiting</h1>
        <p>
          {gone
            ? `No app is waiting to sign in under ${code}. A sign-in lasts ten minutes and is used once: start again from the app.`
            : grant.error.message}
        </p>
      </>
    )
  } else {
    const g = grant.data
    const failed = approve.error ?? refuse.error
    body = (
      <>
        <h1>Sign in {g.name}?</h1>
        <p>
          It will act as <strong>{me.display_name || me.username}</strong> on this server, with an access token of its
          own.
        </p>
        <div className="connect-code-label muted">Check the app shows this code:</div>
        <div className="connect-code">{g.code}</div>
        {failed && <div className="alert">{failed.message}</div>}
        <div className="connect-actions">
          <button
            type="button"
            className="btn"
            onClick={() => refuse.mutate()}
            disabled={approve.isPending || refuse.isPending}
          >
            Refuse
          </button>
          <button
            type="button"
            className="btn btn-primary"
            onClick={() => approve.mutate()}
            disabled={approve.isPending || refuse.isPending}
          >
            {approve.isPending ? 'Signing in…' : 'Sign In'}
          </button>
        </div>
        <p className="muted">If you did not start this from an app of your own, refuse it.</p>
      </>
    )
  }

  return (
    <div className="login">
      <div className="login-card">
        <div className="login-brand">
          <Logo size={28} />
          <span>Hangar</span>
        </div>
        {body}
      </div>
    </div>
  )
}
