import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { ChevronDown, Plus } from 'lucide-react'
import { useState, type FormEvent } from 'react'
import { Link, useNavigate, useParams } from 'react-router'

import { api, type CreateTeam, type Team, type TeamMember, type TeamRole } from '../api'
import { ConfirmButton } from '../components/ConfirmButton'
import { PageHeader } from '../components/PageHeader'
import { useMe } from '../session'
import { templatesKey, useTemplates } from '../templates'
import { roleHints, roleLabels, teamsKey, useTeams } from '../teams'
import { Activity } from '../components/Activity'

// TeamsPage lists every team, with the caller's role in each. Administrators
// make teams here.
export function TeamsPage() {
  const me = useMe()
  const [creating, setCreating] = useState(false)
  const teams = useTeams()
  const list = teams.data ?? []
  const mine = list.filter((t) => t.role)
  const others = list.filter((t) => !t.role)

  return (
    <div className="page">
      <PageHeader
        title="Teams"
        subtitle="Teams own templates, and give each member a role over them."
        actions={
          me.admin &&
          !creating && (
            <button type="button" className="btn btn-primary" onClick={() => setCreating(true)}>
              <Plus size={15} />
              New team
            </button>
          )
        }
      />

      {creating && <CreateForm onDone={() => setCreating(false)} />}
      {teams.isError && <div className="alert">Could not load teams: {teams.error.message}</div>}

      <TeamTable
        title="Yours"
        teams={mine}
        loading={teams.isPending}
        empty={me.admin ? 'You are in no team.' : 'You are in no team. An administrator adds people to teams.'}
      />
      {others.length > 0 && <TeamTable title="Other teams" teams={others} />}
    </div>
  )
}

function TeamTable({
  title,
  teams,
  loading = false,
  empty,
}: {
  title: string
  teams: Team[]
  loading?: boolean
  empty?: string
}) {
  return (
    <section className="section">
      <h2 className="section-title">
        {title} <span className="section-count">{teams.length}</span>
      </h2>
      {!loading && teams.length === 0 && empty && <div className="panel empty-state">{empty}</div>}
      {teams.length > 0 && (
        <div className="panel">
          <table className="table">
            <thead>
              <tr>
                <th>Team</th>
                <th>Your role</th>
                <th className="num">Members</th>
                <th className="num">Templates</th>
              </tr>
            </thead>
            <tbody>
              {teams.map((t) => (
                <tr key={t.id}>
                  <td>
                    <Link to={`/teams/${t.id}`} className="strong">
                      {t.name}
                    </Link>
                    <div className="muted small mono">{t.slug}</div>
                  </td>
                  <td>{t.role ? roleLabels[t.role] : <span className="muted">—</span>}</td>
                  <td className="num">{t.member_count}</td>
                  <td className="num">{t.templates}</td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )}
    </section>
  )
}

const blank: CreateTeam = { slug: '', name: '', description: '' }

function CreateForm({ onDone }: { onDone: () => void }) {
  const qc = useQueryClient()
  const navigate = useNavigate()
  const [form, setForm] = useState<CreateTeam>(blank)
  const [slugEdited, setSlugEdited] = useState(false)
  const create = useMutation({
    mutationFn: api.createTeam,
    onSuccess: (t) => {
      qc.invalidateQueries({ queryKey: teamsKey })
      onDone()
      navigate(`/teams/${t.id}`)
    },
  })
  const submit = (e: FormEvent) => {
    e.preventDefault()
    create.mutate({ ...form, description: form.description || undefined })
  }

  return (
    <form className="panel form form-spaced" onSubmit={submit}>
      <div className="field-row">
        <label className="field">
          <span>Name</span>
          <input
            autoFocus
            required
            maxLength={100}
            value={form.name}
            onChange={(e) =>
              setForm({ ...form, name: e.target.value, slug: slugEdited ? form.slug : slugFor(e.target.value) })
            }
            placeholder="Platform"
          />
        </label>
        <label className="field">
          <span>Slug</span>
          <input
            required
            className="mono"
            maxLength={63}
            pattern="[a-z0-9]+([._\-][a-z0-9]+)*"
            value={form.slug}
            onChange={(e) => {
              setSlugEdited(true)
              setForm({ ...form, slug: e.target.value })
            }}
            placeholder="platform"
          />
          <small>Names the team in paths, such as its images. It cannot change later.</small>
        </label>
      </div>
      <label className="field">
        <span>Description</span>
        <input
          maxLength={1000}
          value={form.description ?? ''}
          onChange={(e) => setForm({ ...form, description: e.target.value })}
        />
      </label>
      {create.error && <div className="alert">{create.error.message}</div>}
      <div className="form-actions">
        <button type="button" className="btn btn-ghost" onClick={onDone}>
          Cancel
        </button>
        <button type="submit" className="btn btn-primary" disabled={create.isPending}>
          Create team
        </button>
      </div>
    </form>
  )
}

// slugFor suggests a slug for a team's name.
function slugFor(name: string): string {
  return name
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, '-')
    .replace(/^-+|-+$/g, '')
    .slice(0, 63)
}

// TeamPage shows a team: its members and roles, run by its admins, and the
// templates it owns.
export function TeamPage() {
  const { id } = useParams()
  const me = useMe()
  const qc = useQueryClient()
  const navigate = useNavigate()
  const team = useQuery({ queryKey: [...teamsKey, id], queryFn: () => api.team(id!), refetchInterval: 30_000 })
  const templates = useTemplates()
  const [editing, setEditing] = useState(false)
  const remove = useMutation({
    mutationFn: () => api.deleteTeam(id!),
    onSuccess: () => {
      qc.invalidateQueries({ queryKey: teamsKey })
      navigate('/teams')
    },
  })

  if (team.isPending) return <div className="page" />
  if (team.isError) {
    return (
      <div className="page">
        <PageHeader crumbs={[{ label: 'Teams', to: '/teams' }]} title="Not found" />
        <div className="panel empty-state">
          This team does not exist. <Link to="/teams">Back to teams</Link>
        </div>
      </div>
    )
  }
  const t = team.data
  const owned = (templates.data ?? []).filter((x) => x.team?.id === t.id)

  return (
    <div className="page page-narrow">
      <PageHeader
        crumbs={[{ label: 'Teams', to: '/teams' }]}
        title={t.name}
        subtitle={
          <span className="subtitle-row">
            <span className="mono">{t.slug}</span>
            {t.role && <span>· you are {roleLabels[t.role].toLowerCase()}</span>}
          </span>
        }
        actions={
          <>
            {t.can_manage && !editing && (
              <button type="button" className="btn btn-ghost" onClick={() => setEditing(true)}>
                Edit
              </button>
            )}
            {me.admin && (
              <ConfirmButton
                label="Delete"
                confirmLabel="Delete?"
                disabled={t.templates > 0}
                title={t.templates > 0 ? 'Owns templates: move or delete them first' : undefined}
                onConfirm={() => remove.mutate()}
              />
            )}
          </>
        }
      />
      {remove.error && <div className="alert">{remove.error.message}</div>}
      {editing ? (
        <EditTeam team={t} onDone={() => setEditing(false)} />
      ) : (
        t.description && <p className="page-lead">{t.description}</p>
      )}

      <Members team={t} />

      <section className="section">
        <h2 className="section-title">
          Templates <span className="section-count">{owned.length}</span>
        </h2>
        {owned.length === 0 ? (
          <div className="panel empty-state">
            The team owns no templates{t.templates > owned.length ? ' you can see' : ''}.
            {(t.role === 'member' || t.role === 'admin' || me.admin) && (
              <>
                {' '}
                <Link to={`/templates/new?team=${t.id}`}>Make one</Link>.
              </>
            )}
          </div>
        ) : (
          <div className="panel">
            <table className="table">
              <tbody>
                {owned.map((x) => (
                  <tr key={x.id}>
                    <td>
                      <Link to={`/templates/${x.id}`} className="strong">
                        {x.name}
                      </Link>
                      {x.description && <div className="muted small">{x.description}</div>}
                    </td>
                    <td className="muted">{x.visibility}</td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        )}
      </section>

      <section className="section">
        <h2 className="section-title">Activity</h2>
        <Activity subjects={[`team:${t.id}`]} />
      </section>
    </div>
  )
}

function EditTeam({ team: t, onDone }: { team: Team; onDone: () => void }) {
  const qc = useQueryClient()
  const [name, setName] = useState(t.name)
  const [description, setDescription] = useState(t.description)
  const save = useMutation({
    mutationFn: () => api.updateTeam(t.id, { name, description: description || undefined }),
    onSuccess: (updated) => {
      qc.setQueryData([...teamsKey, t.id], updated)
      qc.invalidateQueries({ queryKey: teamsKey })
      qc.invalidateQueries({ queryKey: templatesKey })
      onDone()
    },
  })
  return (
    <form
      className="panel form form-spaced"
      onSubmit={(e) => {
        e.preventDefault()
        save.mutate()
      }}
    >
      <label className="field">
        <span>Name</span>
        <input autoFocus required maxLength={100} value={name} onChange={(e) => setName(e.target.value)} />
      </label>
      <label className="field">
        <span>Description</span>
        <input maxLength={1000} value={description} onChange={(e) => setDescription(e.target.value)} />
      </label>
      {save.error && <div className="alert">{save.error.message}</div>}
      <div className="form-actions">
        <button type="button" className="btn btn-ghost" onClick={onDone}>
          Cancel
        </button>
        <button type="submit" className="btn btn-primary" disabled={save.isPending}>
          Save
        </button>
      </div>
    </form>
  )
}

function Members({ team: t }: { team: Team }) {
  const qc = useQueryClient()
  const [adding, setAdding] = useState('')
  const [addRole, setAddRole] = useState<TeamRole>('member')
  const people = useQuery({ queryKey: ['people'], queryFn: api.people, enabled: t.can_manage, refetchInterval: false })
  const done = (updated: Team) => {
    qc.setQueryData([...teamsKey, t.id], updated)
    qc.invalidateQueries({ queryKey: teamsKey })
    qc.invalidateQueries({ queryKey: templatesKey })
  }
  const set = useMutation({
    mutationFn: ({ user, role }: { user: string; role: TeamRole }) => api.setTeamMember(t.id, user, role),
    onSuccess: (updated) => {
      done(updated)
      setAdding('')
    },
  })
  const remove = useMutation({
    mutationFn: (user: string) => api.removeTeamMember(t.id, user),
    onSuccess: done,
  })
  const members = t.members ?? []
  const ids = members.map((m) => m.person.id)
  const candidates = (people.data ?? []).filter((p) => !ids.includes(p.id))
  const error = set.error ?? remove.error

  return (
    <section className="section">
      <h2 className="section-title">
        Members <span className="section-count">{members.length}</span>
      </h2>
      <div className="panel">
        <table className="table">
          <tbody>
            {members.map((m) => (
              <MemberRow
                key={m.person.id}
                member={m}
                manage={t.can_manage}
                busy={set.isPending || remove.isPending}
                onRole={(role) => set.mutate({ user: m.person.id, role })}
                onRemove={() => remove.mutate(m.person.id)}
              />
            ))}
            {members.length === 0 && (
              <tr>
                <td className="empty">No members yet.</td>
              </tr>
            )}
          </tbody>
        </table>
        {t.can_manage && (
          <div className="inline-form panel-foot">
            <select value={adding} onChange={(e) => setAdding(e.target.value)} aria-label="Add a member">
              <option value="">Add a person…</option>
              {candidates.map((p) => (
                <option key={p.id} value={p.id}>
                  {p.display_name ? `${p.display_name} (${p.username})` : p.username}
                </option>
              ))}
            </select>
            <select value={addRole} onChange={(e) => setAddRole(e.target.value as TeamRole)} aria-label="Role">
              {(['viewer', 'member', 'admin'] as const).map((r) => (
                <option key={r} value={r} title={roleHints[r]}>
                  {roleLabels[r]}
                </option>
              ))}
            </select>
            <button
              type="button"
              className="btn btn-ghost"
              disabled={!adding || set.isPending}
              onClick={() => set.mutate({ user: adding, role: addRole })}
            >
              Add
            </button>
          </div>
        )}
      </div>
      {error && <div className="alert">{error.message}</div>}
      <p className="muted small">
        {(['viewer', 'member', 'admin'] as const).map((r) => (
          <span key={r}>
            <strong>{roleLabels[r]}</strong>: {roleHints[r].toLowerCase()}.{' '}
          </span>
        ))}
      </p>
    </section>
  )
}

function MemberRow({
  member: m,
  manage,
  busy,
  onRole,
  onRemove,
}: {
  member: TeamMember
  manage: boolean
  busy: boolean
  onRole: (r: TeamRole) => void
  onRemove: () => void
}) {
  const me = useMe()
  return (
    <tr>
      <td>
        <div className="strong">
          {m.person.display_name || m.person.username}
          {m.person.id === me.id && <span className="you">you</span>}
        </div>
        <div className="muted small">{m.person.username}</div>
      </td>
      <td>
        {manage ? (
          <span className={m.role === 'admin' ? 'role-select role-admin' : 'role-select'} title={roleHints[m.role]}>
            <select aria-label="Role" value={m.role} disabled={busy} onChange={(e) => onRole(e.target.value as TeamRole)}>
              {(['viewer', 'member', 'admin'] as const).map((r) => (
                <option key={r} value={r}>
                  {roleLabels[r]}
                </option>
              ))}
            </select>
            <ChevronDown size={12} />
          </span>
        ) : (
          <span title={roleHints[m.role]}>{roleLabels[m.role]}</span>
        )}
      </td>
      <td>
        {manage && (
          <div className="actions">
            <ConfirmButton
              label={m.person.id === me.id ? 'Leave' : 'Remove'}
              confirmLabel={m.person.id === me.id ? 'Leave?' : 'Remove?'}
              disabled={busy}
              onConfirm={onRemove}
            />
          </div>
        )}
      </td>
    </tr>
  )
}
