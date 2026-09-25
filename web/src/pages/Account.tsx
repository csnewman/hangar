import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { Copy, Plus } from 'lucide-react'
import { useState, type FormEvent } from 'react'

import { api } from '../api'
import { useMe } from '../session'
import { PageHeader } from '../components/PageHeader'
import { Activity } from '../components/Activity'
import { ConfirmButton } from '../components/ConfirmButton'
import { formatAgo } from '../components/format'

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
      <AccessTokens />
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

const tokensKey = ['tokens']

// AccessTokens sign tools in as the user -- the VS Code extension, scripts
// calling the API -- each revocable on its own.
function AccessTokens() {
  const qc = useQueryClient()
  const tokens = useQuery({ queryKey: tokensKey, queryFn: api.tokens })
  const [name, setName] = useState('')
  const [days, setDays] = useState('90')
  const [created, setCreated] = useState<{
    name: string
    token: string
  } | null>(null)
  const [copied, setCopied] = useState(false)
  const refresh = () => qc.invalidateQueries({ queryKey: tokensKey })
  const create = useMutation({
    mutationFn: () => api.createToken(name.trim(), days ? Number(days) : undefined),
    onSuccess: (t) => {
      setCreated({ name: t.info.name, token: t.token })
      setCopied(false)
      setName('')
    },
    onSettled: refresh,
  })
  const revoke = useMutation({
    mutationFn: (id: string) => api.revokeToken(id),
    onSettled: refresh,
  })
  const submit = (e: FormEvent) => {
    e.preventDefault()
    if (name.trim()) create.mutate()
  }
  const copy = (token: string) => {
    navigator.clipboard.writeText(token).then(() => setCopied(true))
  }

  return (
    <section className="section">
      <h2 className="section-title">Access tokens</h2>
      <p className="muted small">
        For tools that act as you, such as the VS Code extension. A token can do anything you can, so give each tool its
        own and revoke it when the tool is done with.
      </p>
      {created && (
        <div className="panel form">
          <div>
            <span className="strong">{created.name}</span>
            <span className="muted"> — copy it: it is shown this once.</span>
          </div>
          <code className="mono token-reveal">{created.token}</code>
          <div className="form-actions">
            <button type="button" className="btn btn-ghost" onClick={() => setCreated(null)}>
              Done
            </button>
            <button type="button" className="btn btn-primary" onClick={() => copy(created.token)}>
              <Copy size={14} />
              {copied ? 'Copied' : 'Copy'}
            </button>
          </div>
        </div>
      )}
      <div className="panel">
        {tokens.isError ? (
          <div className="alert">{tokens.error.message}</div>
        ) : !tokens.data || tokens.data.length === 0 ? (
          <div className="empty">{tokens.isPending ? '' : 'No tokens.'}</div>
        ) : (
          <table className="table">
            <tbody>
              {tokens.data.map((t) => (
                <tr key={t.id}>
                  <td className="strong">{t.name}</td>
                  <td className="muted nowrap">
                    {t.last_used_at ? `used ${formatAgo(t.last_used_at)}` : 'never used'}
                  </td>
                  <td className="muted nowrap">
                    {t.expires_at ? `expires ${new Date(t.expires_at).toLocaleDateString()}` : 'does not expire'}
                  </td>
                  <td className="num">
                    <ConfirmButton
                      label="Revoke"
                      confirmLabel="Revoke?"
                      onConfirm={() => revoke.mutate(t.id)}
                      disabled={revoke.isPending}
                    />
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        )}
      </div>
      <form className="inline-form" onSubmit={submit}>
        <input
          className="grow"
          placeholder="Name: VS Code on my laptop"
          value={name}
          onChange={(e) => setName(e.target.value)}
        />
        <select value={days} onChange={(e) => setDays(e.target.value)}>
          <option value="30">30 days</option>
          <option value="90">90 days</option>
          <option value="365">1 year</option>
          <option value="">No expiry</option>
        </select>
        <button type="submit" className="btn btn-ghost" disabled={!name.trim() || create.isPending}>
          <Plus size={14} />
          Create token
        </button>
      </form>
      {create.error && <div className="alert">{create.error.message}</div>}
      {revoke.error && <div className="alert">{revoke.error.message}</div>}
    </section>
  )
}
