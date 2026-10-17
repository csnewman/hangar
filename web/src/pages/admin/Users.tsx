import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { Plus } from 'lucide-react'
import { useState, type FormEvent } from 'react'
import { Link, useNavigate, useParams } from 'react-router'

import { api, type CreateUser, type UpdateUser, type User } from '../../api'
import { useMe } from '../../session'
import { Activity } from '../../components/Activity'
import { ConfirmButton } from '../../components/ConfirmButton'
import { formatAgo, formatMemory } from '../../components/format'
import {
  DetailHead,
  DetailPage,
  ListGroup,
  ListItem,
  ListPane,
  MasterDetail,
  NoneChosen,
  PaneRoot,
  Row,
  Rows,
  SearchField,
  Section,
  Segmented,
  Tile,
} from '../../components/Pane'
import { PhaseBadge } from '../../components/Status'
import { useEnvironments } from '../../environments'
import { useEnvironmentTarget } from '../../workspaceLinks'

const usersKey = ['users'] as const

// UsersPage is who can sign in, and who can administer Hangar: a list of
// them beside the one chosen, or the form for a new one.
export function UsersPage() {
  const { id } = useParams()
  return <UsersPane id={id} />
}

function UsersPane({ id }: { id?: string }) {
  const users = useQuery({ queryKey: usersKey, queryFn: api.users, refetchInterval: 10_000 })
  const [query, setQuery] = useState('')
  const q = query.trim().toLowerCase()
  const list = (users.data ?? []).filter(
    (u) => !q || u.username.toLowerCase().includes(q) || u.display_name.toLowerCase().includes(q),
  )
  const admins = list.filter((u) => u.admin)
  const others = list.filter((u) => !u.admin)
  const chosen = users.data?.find((u) => u.id === id)
  const item = (u: User) => (
    <ListItem
      key={u.id}
      to={`/admin/users/${u.id}`}
      mark={<Tile name={u.display_name || u.username} small />}
      title={u.display_name || u.username}
      sub={`${u.username}${u.disabled ? ' · disabled' : ''}`}
      end={u.environments > 0 ? `${u.environments} env${u.environments === 1 ? '' : 's'}` : undefined}
    />
  )

  return (
    <PaneRoot>
      <MasterDetail
        chosen={!!id}
        back={{ to: '/admin/users', label: 'Users' }}
        list={
          <ListPane
            title="Users"
            back={{ to: '/more', label: 'More' }}
            head={
              <>
                <SearchField value={query} onChange={setQuery} placeholder="Users" />
                <Link to="/admin/users/new" className="btn btn-ghost btn-sm" title="New user" aria-label="New user">
                  <Plus size={14} />
                </Link>
              </>
            }
          >
            {users.isError && <div className="md-empty-list">Could not load users: {users.error.message}</div>}
            {admins.length > 0 && <ListGroup title="Administrators">{admins.map(item)}</ListGroup>}
            {others.length > 0 && <ListGroup title="Users">{others.map(item)}</ListGroup>}
          </ListPane>
        }
      >
        {id === 'new' ? (
          <NewUser />
        ) : chosen ? (
          <UserDetail key={chosen.id} user={chosen} />
        ) : (
          <NoneChosen>
            {users.isPending
              ? 'Loading…'
              : id
                ? 'No such user.'
                : 'Choose someone to change their role, reset their password, or see what they have done.'}
          </NoneChosen>
        )}
      </MasterDetail>
    </PaneRoot>
  )
}

// UserDetail is one user: who they are and their role, signing in, their
// environments, and what they have done.
function UserDetail({ user: u }: { user: User }) {
  const me = useMe()
  const qc = useQueryClient()
  const navigate = useNavigate()
  const envs = useEnvironments()
  const target = useEnvironmentTarget()
  const [resetting, setResetting] = useState(false)
  const update = useMutation({
    mutationFn: (body: UpdateUser) => api.updateUser(u.id, body),
    onSettled: () => qc.invalidateQueries({ queryKey: usersKey }),
  })
  const remove = useMutation({
    mutationFn: () => api.deleteUser(u.id),
    onSuccess: () => navigate('/admin/users', { replace: true }),
    onSettled: () => qc.invalidateQueries({ queryKey: usersKey }),
  })
  const busy = update.isPending || remove.isPending
  const error = update.error ?? remove.error
  const self = u.id === me.id
  const theirs = (envs.data ?? []).filter((e) => e.owner_id === u.id)

  return (
    <>
      <DetailHead
        mark={<Tile name={u.display_name || u.username} />}
        title={
          <>
            {u.display_name || u.username}
            {self && <span className="you">you</span>}
          </>
        }
        sub={
          <>
            <span className="mono">{u.username}</span>
            <span>·</span>
            <span>{u.admin ? 'Administrator' : 'User'}</span>
            <span>·</span>
            <span>joined {formatAgo(u.created_at)}</span>
          </>
        }
        actions={
          <>
            {error && <span className="action-error">{error.message}</span>}
            {!self && (
              <button
                type="button"
                className="btn btn-ghost btn-sm"
                disabled={busy}
                onClick={() => update.mutate({ disabled: !u.disabled })}
              >
                {u.disabled ? 'Enable' : 'Disable'}
              </button>
            )}
            {!self && (
              <ConfirmButton
                label="Delete"
                confirmLabel="Delete?"
                disabled={busy || u.environments > 0}
                title={u.environments > 0 ? 'Owns environments: delete them first, or disable the user' : undefined}
                onConfirm={() => remove.mutate()}
              />
            )}
          </>
        }
      />
      <DetailPage
        sections={[
          { id: 'account', label: 'Account' },
          { id: 'environments', label: 'Environments' },
          { id: 'activity', label: 'Activity' },
        ]}
      >
        <Section id="account" title="Account">
          <Rows>
            <Row label="Username">
              <span className="mono">{u.username}</span>
            </Row>
            <Row label="Display name">{u.display_name || <span className="muted">not set</span>}</Row>
            <Row label="Role">
              <Segmented
                label="Role"
                value={u.admin}
                disabled={busy || self}
                options={[
                  { value: false, label: 'User' },
                  { value: true, label: 'Administrator' },
                ]}
                onChange={(admin) => update.mutate({ admin })}
              />
              {self && <span className="muted small">Another administrator must change your role.</span>}
            </Row>
            <Row label="State">
              {u.disabled ? (
                <span className="pane-pill pane-pill-warn">disabled</span>
              ) : (
                <span className="pane-pill pane-pill-good">active</span>
              )}
              <span className="muted small">
                {u.disabled ? 'Cannot sign in. Their environments are kept.' : 'Can sign in.'}
              </span>
            </Row>
            <Row label="Password">
              {!u.has_password ? (
                <span className="muted">Signs in without one</span>
              ) : self ? (
                <span className="muted">Changed from your own settings</span>
              ) : resetting ? (
                <ResetPassword
                  onCancel={() => setResetting(false)}
                  onSubmit={(password) => update.mutate({ password }, { onSuccess: () => setResetting(false) })}
                />
              ) : (
                <button type="button" className="btn btn-ghost btn-sm" disabled={busy} onClick={() => setResetting(true)}>
                  Reset Password
                </button>
              )}
            </Row>
          </Rows>
        </Section>
        <Section id="environments" title="Environments">
          {theirs.length === 0 ? (
            <p className="muted">
              {u.environments > 0 ? `Owns ${u.environments}.` : 'Owns none.'}
            </p>
          ) : (
            <table className="pane-table">
              <thead>
                <tr>
                  <th>Name</th>
                  <th>Status</th>
                  <th className="num">Size</th>
                  <th>Worker</th>
                </tr>
              </thead>
              <tbody>
                {theirs.map((e) => (
                  <tr key={e.id}>
                    <td>
                      <Link to={`/environments/${e.id}`} target={target(e.id)} className="strong">
                        {e.name}
                      </Link>
                      <div className="muted small">{e.template}</div>
                    </td>
                    <td>
                      <PhaseBadge phase={e.phase} desired={e.desired} workerOnline={e.worker_online} />
                    </td>
                    <td className="num nowrap">
                      {e.cpus} vCPU · {formatMemory(e.memory_mib)}
                    </td>
                    <td>{e.worker ?? <span className="muted">not placed</span>}</td>
                  </tr>
                ))}
              </tbody>
            </table>
          )}
        </Section>
        <Section
          id="activity"
          title="Activity"
          extra={<Link to={`/admin/audit?subject=${encodeURIComponent(`user:${u.id}`)}`}>In the audit log</Link>}
        >
          <Activity subjects={[`user:${u.id}`]} pageSize={10} />
        </Section>
      </DetailPage>
    </>
  )
}

// NewUser makes a user, then shows them.
function NewUser() {
  const navigate = useNavigate()
  return (
    <>
      <DetailHead title="New user" sub="Someone who can sign in, and whether they can administer Hangar." />
      <DetailPage>
        <CreateForm
          onCreated={(u) => navigate(`/admin/users/${u.id}`, { replace: true })}
          onDone={() => navigate('/admin/users')}
        />
      </DetailPage>
    </>
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

function CreateForm({ onDone, onCreated }: { onDone: () => void; onCreated?: (u: User) => void }) {
  const qc = useQueryClient()
  const [form, setForm] = useState<CreateUser>(blank)
  const create = useMutation({
    mutationFn: api.createUser,
    onSuccess: (u) => {
      qc.invalidateQueries({ queryKey: usersKey })
      if (onCreated) onCreated(u)
      else onDone()
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
