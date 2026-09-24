import { useMutation } from '@tanstack/react-query'
import { useState, type FormEvent } from 'react'

import { api } from '../api'
import { useMe } from '../session'
import { PageHeader } from '../components/PageHeader'
import { Activity } from '../components/Activity'

export function AccountPage() {
  const me = useMe()
  return (
    <div className="page page-narrow">
      <PageHeader title="Account" />
      <div className="panel">
        <dl className="props">
          <div className="prop">
            <dt>Username</dt>
            <dd>{me.username}</dd>
          </div>
          <div className="prop">
            <dt>Name</dt>
            <dd>{me.display_name || <span className="muted">not set</span>}</dd>
          </div>
          <div className="prop">
            <dt>Role</dt>
            <dd>{me.admin ? 'Administrator' : 'User'}</dd>
          </div>
        </dl>
      </div>
      {me.has_password && <ChangePassword />}
      <section className="section">
        <h2 className="section-title">Your activity</h2>
        <Activity subjects={[`actor:${me.id}`]} />
      </section>
    </div>
  )
}

function ChangePassword() {
  const [current, setCurrent] = useState('')
  const [next, setNext] = useState('')
  const [confirm, setConfirm] = useState('')
  const change = useMutation({
    mutationFn: () => api.changePassword(current, next),
    onSuccess: () => {
      setCurrent('')
      setNext('')
      setConfirm('')
    },
  })
  const mismatch = confirm !== '' && confirm !== next

  const submit = (e: FormEvent) => {
    e.preventDefault()
    if (!mismatch) change.mutate()
  }

  return (
    <section className="section">
      <h2 className="section-title">Change password</h2>
      <form className="panel form" onSubmit={submit}>
        <label className="field">
          <span>Current password</span>
          <input
            type="password"
            required
            autoComplete="current-password"
            value={current}
            onChange={(e) => setCurrent(e.target.value)}
          />
        </label>
        <div className="field-row">
          <label className="field">
            <span>New password</span>
            <input
              type="password"
              required
              minLength={8}
              autoComplete="new-password"
              value={next}
              onChange={(e) => setNext(e.target.value)}
            />
          </label>
          <label className="field">
            <span>Confirm</span>
            <input
              type="password"
              required
              autoComplete="new-password"
              value={confirm}
              onChange={(e) => setConfirm(e.target.value)}
            />
          </label>
        </div>
        <small className="muted">Changing it signs you out everywhere else.</small>
        {mismatch && <div className="alert">The passwords do not match.</div>}
        {change.error && <div className="alert">{change.error.message}</div>}
        {change.isSuccess && <div className="notice notice-good">Password changed.</div>}
        <div className="form-actions">
          <button type="submit" className="btn btn-primary" disabled={change.isPending || mismatch}>
            Change password
          </button>
        </div>
      </form>
    </section>
  )
}
