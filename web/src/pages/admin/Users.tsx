import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { ChevronDown, Plus } from 'lucide-react'
import { useState, type FormEvent } from 'react'

import { api, type CreateUser, type UpdateUser, type User } from '../../api'
import { useMe } from '../../session'
import { ConfirmButton } from '../../components/ConfirmButton'
import { formatAgo } from '../../components/format'
import { PageHeader } from '../../components/PageHeader'
import { Link } from 'react-router'

const usersKey = ['users'] as const

export function UsersPage() {
  const [creating, setCreating] = useState(false)
  const users = useQuery({ queryKey: usersKey, queryFn: api.users, refetchInterval: 10_000 })

  return (
    <div className="page">
      <PageHeader
        crumbs={[{ label: 'Administration' }]}
        title="Users"
        subtitle="Who can sign in, and who can administer Hangar."
        actions={
          !creating && (
            <button type="button" className="btn btn-primary" onClick={() => setCreating(true)}>
              <Plus size={15} />
              New user
            </button>
          )
        }
      />

      {creating && <CreateForm onDone={() => setCreating(false)} />}
      {users.isError && <div className="alert">Could not load users: {users.error.message}</div>}

      <div className="panel">
        <table className="table">
          <thead>
            <tr>
              <th>User</th>
              <th>Role</th>
              <th>Status</th>
              <th className="num">Environments</th>
              <th>Created</th>
              <th />
            </tr>
          </thead>
          <tbody>
            {users.data?.map((u) => <UserRow key={u.id} user={u} />)}
            {users.isPending && (
              <tr>
                <td colSpan={6} className="empty">
                  Loading…
                </td>
              </tr>
            )}
          </tbody>
        </table>
      </div>
    </div>
  )
}

function UserRow({ user: u }: { user: User }) {
  const me = useMe()
  const qc = useQueryClient()
  const [resetting, setResetting] = useState(false)
  const update = useMutation({
    mutationFn: (body: UpdateUser) => api.updateUser(u.id, body),
    onSettled: () => qc.invalidateQueries({ queryKey: usersKey }),
  })
  const remove = useMutation({
    mutationFn: () => api.deleteUser(u.id),
    onSettled: () => qc.invalidateQueries({ queryKey: usersKey }),
  })
  const busy = update.isPending || remove.isPending
  const error = update.error ?? remove.error
  const self = u.id === me.id

  return (
    <tr className={u.disabled ? 'row-disabled' : undefined}>
      <td>
        <div className="strong">
          {u.display_name || u.username}
          {self && <span className="you">you</span>}
        </div>
        <div className="muted small">{u.username}</div>
      </td>
      <td>
        <RoleSelect
          admin={u.admin}
          disabled={busy || self}
          title={self ? 'Another administrator must change your role' : undefined}
          onChange={(admin) => update.mutate({ admin })}
        />
      </td>
      <td>
        {u.disabled ? (
          <span className="badge badge-bad">
            <span className="dot" />
            disabled
          </span>
        ) : (
          <span className="badge badge-good">
            <span className="dot" />
            active
          </span>
        )}
        {error && <div className="reason reason-bad">{error.message}</div>}
        {resetting && (
          <ResetPassword
            onCancel={() => setResetting(false)}
            onSubmit={(password) => update.mutate({ password }, { onSuccess: () => setResetting(false) })}
          />
        )}
      </td>
      <td className="num">{u.environments}</td>
      <td className="muted nowrap">{formatAgo(u.created_at)}</td>
      <td>
        <div className="actions">
          {u.has_password && !self && (
            <button type="button" className="btn btn-ghost" disabled={busy} onClick={() => setResetting(true)}>
              Reset password
            </button>
          )}
          {!self && (
            <button
              type="button"
              className="btn btn-ghost"
              disabled={busy}
              onClick={() => update.mutate({ disabled: !u.disabled })}
            >
              {u.disabled ? 'Enable' : 'Disable'}
            </button>
          )}
          <Link to={`/admin/audit?subject=${encodeURIComponent(`user:${u.id}`)}`} className="btn btn-ghost">
            Activity
          </Link>
          {!self && (
            <ConfirmButton
              label="Delete"
              confirmLabel="Delete?"
              disabled={busy || u.environments > 0}
              title={u.environments > 0 ? 'Owns environments: delete them first, or disable the user' : undefined}
              onConfirm={() => remove.mutate()}
            />
          )}
        </div>
      </td>
    </tr>
  )
}

// RoleSelect shows a user's role as a badge that is also the control for
// changing it.
function RoleSelect({
  admin,
  disabled,
  title,
  onChange,
}: {
  admin: boolean
  disabled: boolean
  title?: string
  onChange: (admin: boolean) => void
}) {
  return (
    <span className={admin ? 'role-select role-admin' : 'role-select'} title={title}>
      <select
        aria-label="Role"
        value={admin ? 'admin' : 'user'}
        disabled={disabled}
        onChange={(e) => onChange(e.target.value === 'admin')}
      >
        <option value="user">User</option>
        <option value="admin">Administrator</option>
      </select>
      <ChevronDown size={12} />
    </span>
  )
}

function ResetPassword({ onSubmit, onCancel }: { onSubmit: (p: string) => void; onCancel: () => void }) {
  const [password, setPassword] = useState('')
  return (
    <form
      className="inline-form"
      onSubmit={(e) => {
        e.preventDefault()
        onSubmit(password)
      }}
    >
      <input
        autoFocus
        type="password"
        minLength={8}
        required
        placeholder="New password"
        autoComplete="new-password"
        value={password}
        onChange={(e) => setPassword(e.target.value)}
      />
      <button type="submit" className="btn btn-primary">
        Set
      </button>
      <button type="button" className="btn btn-ghost" onClick={onCancel}>
        Cancel
      </button>
    </form>
  )
}

const blank: CreateUser = { username: '', display_name: '', password: '', admin: false }

function CreateForm({ onDone }: { onDone: () => void }) {
  const qc = useQueryClient()
  const [form, setForm] = useState<CreateUser>(blank)
  const create = useMutation({
    mutationFn: api.createUser,
    onSuccess: () => {
      qc.invalidateQueries({ queryKey: usersKey })
      onDone()
    },
  })
  const submit = (e: FormEvent) => {
    e.preventDefault()
    create.mutate({ ...form, display_name: form.display_name || undefined })
  }

  return (
    <form className="panel form form-spaced" onSubmit={submit}>
      <div className="field-row">
        <label className="field">
          <span>Username</span>
          <input
            autoFocus
            required
            autoComplete="off"
            value={form.username}
            onChange={(e) => setForm({ ...form, username: e.target.value })}
          />
        </label>
        <label className="field">
          <span>Display name</span>
          <input value={form.display_name} onChange={(e) => setForm({ ...form, display_name: e.target.value })} />
        </label>
        <label className="field">
          <span>Password</span>
          <input
            type="password"
            required
            minLength={8}
            autoComplete="new-password"
            value={form.password}
            onChange={(e) => setForm({ ...form, password: e.target.value })}
          />
        </label>
      </div>
      <label className="check">
        <input type="checkbox" checked={form.admin} onChange={(e) => setForm({ ...form, admin: e.target.checked })} />
        Administrator — can see every environment, and manage users and workers
      </label>
      {create.error && <div className="alert">{create.error.message}</div>}
      <div className="form-actions">
        <button type="button" className="btn btn-ghost" onClick={onDone}>
          Cancel
        </button>
        <button type="submit" className="btn btn-primary" disabled={create.isPending}>
          Create user
        </button>
      </div>
    </form>
  )
}
