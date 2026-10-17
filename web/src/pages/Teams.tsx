import './teams.css'

import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { ChevronDown, Plus, Undo2 } from 'lucide-react'
import { useState, type FormEvent, type ReactNode } from 'react'
import { Link, useNavigate, useParams } from 'react-router'

import { api, type CreateTeam, type Team, type TeamMember, type TeamRole } from '../api'
import { ConfirmButton } from '../components/ConfirmButton'
import { PageHeader } from '../components/PageHeader'
import {
  ApplyBar,
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
  Tile,
} from '../components/Pane'
import { usePhone } from '../phone'
import { useMe } from '../session'
import { templatesKey, useTemplates } from '../templates'
import { canGive, roleHints, roleLabels, teamsKey, useTeams } from '../teams'
import { Activity } from '../components/Activity'

// TeamsPage lists every team, with the caller's role in each, beside the
// chosen one (TeamPage). Administrators make teams here. A phone shows the
// list and each team as screens of their own.
export function TeamsPage() {
  if (usePhone()) return <PhoneTeams />
  return <TeamsPane />
}

export function TeamPage() {
  const { id } = useParams()
  if (usePhone()) return <PhoneTeam />
  return <TeamsPane id={id} />
}

function PhoneTeams() {
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
                  <td className="num" data-label="Members">
                    {t.member_count}
                  </td>
                  <td className="num" data-label="Templates">
                    {t.templates}
                  </td>
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

// PhoneTeam shows a team: its members and roles, run by its admins, and the
// templates it owns.
function PhoneTeam() {
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

function Members({ team: t, pane = false }: { team: Team; pane?: boolean }) {
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

  const hints = (
    <p className={pane ? 'pane-hint teams-roles' : 'muted small'}>
      {(['viewer', 'member', 'admin'] as const).map((r) => (
        <span key={r}>
          <strong>{roleLabels[r]}</strong>: {roleHints[r].toLowerCase()}.{' '}
        </span>
      ))}
    </p>
  )
  const adder = t.can_manage && (
    <div className={pane ? 'teams-add' : 'inline-form panel-foot'}>
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
        className={pane ? 'btn btn-ghost btn-sm' : 'btn btn-ghost'}
        disabled={!adding || set.isPending}
        onClick={() => set.mutate({ user: adding, role: addRole })}
      >
        Add
      </button>
    </div>
  )
  const rows = members.map((m) => (
    <MemberRow
      key={m.person.id}
      member={m}
      manage={t.can_manage}
      busy={set.isPending || remove.isPending}
      onRole={(role) => set.mutate({ user: m.person.id, role })}
      onRemove={() => remove.mutate(m.person.id)}
    />
  ))
  if (pane) {
    return (
      <Section title={`Members · ${members.length}`}>
        {members.length === 0 ? (
          <p className="muted">No members yet.</p>
        ) : (
          <table className="pane-table">
            <tbody>{rows}</tbody>
          </table>
        )}
        {adder}
        {error && <div className="alert">{error.message}</div>}
        {hints}
      </Section>
    )
  }
  return (
    <section className="section">
      <h2 className="section-title">
        Members <span className="section-count">{members.length}</span>
      </h2>
      <div className="panel">
        <table className="table">
          <tbody>
            {rows}
            {members.length === 0 && (
              <tr>
                <td className="empty">No members yet.</td>
              </tr>
            )}
          </tbody>
        </table>
        {adder}
      </div>
      {error && <div className="alert">{error.message}</div>}
      {hints}
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

// TeamsPane is the teams on a computer: the list on the left, mine first,
// and the chosen team, or a new one being made, beside it.
function TeamsPane({ id }: { id?: string }) {
  const me = useMe()
  const teams = useTeams()
  const [query, setQuery] = useState('')
  const [creating, setCreating] = useState(false)
  // Choosing a team leaves a new one unmade.
  const [shownID, setShownID] = useState(id)
  if (id !== shownID) {
    setShownID(id)
    setCreating(false)
  }
  const q = query.trim().toLowerCase()
  const list = (teams.data ?? []).filter(
    (t) => !q || t.name.toLowerCase().includes(q) || t.slug.includes(q) || t.description.toLowerCase().includes(q),
  )
  const mine = list.filter((t) => t.role)
  const others = list.filter((t) => !t.role)
  const item = (t: Team) => (
    <ListItem
      key={t.id}
      to={`/teams/${t.id}`}
      mark={<Tile name={t.name} small />}
      title={t.name}
      sub={`${t.member_count} member${t.member_count === 1 ? '' : 's'}${t.role ? ` · ${roleLabels[t.role].toLowerCase()}` : ''}`}
      end={t.templates > 0 ? `${t.templates} template${t.templates === 1 ? '' : 's'}` : undefined}
    />
  )

  return (
    <PaneRoot>
      <MasterDetail
        list={
          <ListPane
            head={
              <>
                <SearchField value={query} onChange={setQuery} placeholder="Search teams" />
                {me.admin && (
                  <button
                    type="button"
                    className="btn btn-ghost btn-sm teams-new"
                    aria-label="New team"
                    title="New team"
                    onClick={() => setCreating(true)}
                  >
                    <Plus size={14} />
                  </button>
                )}
              </>
            }
          >
            {teams.isError && <div className="md-empty-list">Could not load teams: {teams.error.message}</div>}
            <ListGroup title="Yours">
              {mine.map(item)}
              {teams.isSuccess && mine.length === 0 && (
                <div className="md-empty-list">
                  {q
                    ? 'No team of yours matches.'
                    : me.admin
                      ? 'You are in no team.'
                      : 'You are in no team. An administrator adds people to teams.'}
                </div>
              )}
            </ListGroup>
            {others.length > 0 && <ListGroup title="Other teams">{others.map(item)}</ListGroup>}
          </ListPane>
        }
      >
        {creating ? (
          <NewTeam onDone={() => setCreating(false)} />
        ) : id ? (
          <TeamDetail key={id} id={id} />
        ) : (
          <NoneChosen>
            Teams own templates, and give each member a role over them. Choose a team to see its members and templates
            {me.admin ? ', or make one with the + above the list.' : '.'}
          </NoneChosen>
        )}
      </MasterDetail>
    </PaneRoot>
  )
}

// NewTeam makes a team, and shows it once made.
function NewTeam({ onDone }: { onDone: () => void }) {
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
    <>
      <DetailHead title="New team" sub={<span>A team owns templates, and gives each member a role over them.</span>} />
      <DetailPage>
        <form onSubmit={submit}>
          <fieldset className="pane-fields">
            <Section title="Team">
              <Rows>
                <Row label="Name">
                  <input
                    className="pane-wide"
                    autoFocus
                    required
                    maxLength={100}
                    aria-label="Name"
                    value={form.name}
                    onChange={(e) =>
                      setForm({ ...form, name: e.target.value, slug: slugEdited ? form.slug : slugFor(e.target.value) })
                    }
                    placeholder="Platform"
                  />
                </Row>
                <Row label="Slug">
                  <input
                    className="pane-wide mono"
                    required
                    maxLength={63}
                    pattern="[a-z0-9]+([._\-][a-z0-9]+)*"
                    aria-label="Slug"
                    value={form.slug}
                    onChange={(e) => {
                      setSlugEdited(true)
                      setForm({ ...form, slug: e.target.value })
                    }}
                    placeholder="platform"
                  />
                </Row>
                <Row label="Description">
                  <input
                    className="pane-wide"
                    maxLength={1000}
                    aria-label="Description"
                    value={form.description ?? ''}
                    onChange={(e) => setForm({ ...form, description: e.target.value })}
                  />
                </Row>
                <Row label="">
                  <button type="submit" className="btn btn-primary btn-sm" disabled={create.isPending}>
                    Create Team
                  </button>
                  <button type="button" className="btn btn-ghost btn-sm" onClick={onDone}>
                    Cancel
                  </button>
                  {create.error && <span className="action-error">{create.error.message}</span>}
                </Row>
              </Rows>
              <p className="pane-hint">The slug names the team in paths, such as its images. It cannot change later.</p>
            </Section>
          </fieldset>
        </form>
      </DetailPage>
    </>
  )
}

// TeamDetail is a team beside the list: its members and their roles, run
// by its admins, the templates it owns, its name and description, what has
// been done to it, and deleting it, for administrators.
function TeamDetail({ id }: { id: string }) {
  const me = useMe()
  const qc = useQueryClient()
  const navigate = useNavigate()
  const team = useQuery({ queryKey: [...teamsKey, id], queryFn: () => api.team(id), refetchInterval: 30_000 })
  const templates = useTemplates()
  const remove = useMutation({
    mutationFn: () => api.deleteTeam(id),
    onSuccess: () => {
      qc.invalidateQueries({ queryKey: teamsKey })
      navigate('/teams')
    },
  })

  if (team.isPending) return null
  if (team.isError) return <NoneChosen>This team does not exist, or you cannot see it.</NoneChosen>
  const t = team.data
  const owned = (templates.data ?? []).filter((x) => x.team?.id === t.id)
  const give = canGive(t, me.admin)

  return (
    <TeamFrame team={t}>
      {(settings) => (
        <>
          <DetailHead
            mark={<Tile name={t.name} />}
            title={t.name}
            sub={
              <>
                <span className="mono">{t.slug}</span>
                <span>·</span>
                <span>
                  {t.member_count} member{t.member_count === 1 ? '' : 's'}
                </span>
                {t.role && (
                  <>
                    <span>·</span>
                    <span>you are {roleLabels[t.role].toLowerCase()}</span>
                  </>
                )}
              </>
            }
            actions={
              give && (
                <Link to={`/templates/new?team=${t.id}`} className="btn btn-ghost btn-sm">
                  <Plus size={13} />
                  New Template
                </Link>
              )
            }
          />
          <DetailPage>
            {t.description && <p className="teams-lead">{t.description}</p>}
            <Members team={t} pane />
            <Section title={`Templates · ${owned.length}`}>
              {owned.length === 0 ? (
                <p className="muted">
                  The team owns no templates{t.templates > owned.length ? ' you can see' : ''}.
                  {give && (
                    <>
                      {' '}
                      <Link to={`/templates/new?team=${t.id}`}>Make one</Link>.
                    </>
                  )}
                </p>
              ) : (
                <table className="pane-table">
                  <tbody>
                    {owned.map((x) => (
                      <tr key={x.id}>
                        <td>
                          <Link to={`/templates/${x.id}`} className="strong">
                            {x.name}
                          </Link>
                          {x.description && <div className="muted small">{x.description}</div>}
                        </td>
                        <td className="muted num">{x.visibility}</td>
                      </tr>
                    ))}
                  </tbody>
                </table>
              )}
            </Section>
            {settings}
            <Section title="Activity">
              <Activity subjects={[`team:${t.id}`]} pageSize={10} />
            </Section>
            {me.admin && (
              <Section title="Delete">
                <div className="teams-delete">
                  <p className="muted">
                    {t.templates > 0
                      ? 'It owns templates: move or delete them before deleting the team.'
                      : 'Deleting the team removes its members’ roles. It cannot be undone.'}
                  </p>
                  <ConfirmButton
                    label={`Delete ${t.name}`}
                    confirmLabel="Delete?"
                    disabled={t.templates > 0 || remove.isPending}
                    onConfirm={() => remove.mutate()}
                  />
                </div>
                {remove.error && <div className="alert">{remove.error.message}</div>}
              </Section>
            )}
          </DetailPage>
        </>
      )}
    </TeamFrame>
  )
}

// TeamFrame holds a team's name and description as they are being changed,
// by those who run it, and puts the bar that applies them at the foot. Its
// child is given the section that changes them.
function TeamFrame({ team: t, children }: { team: Team; children: (settings: ReactNode) => ReactNode }) {
  const qc = useQueryClient()
  const [name, setName] = useState(t.name)
  const [description, setDescription] = useState(t.description)
  // A change saved elsewhere replaces what is shown here.
  const [base, setBase] = useState({ name: t.name, description: t.description })
  if (base.name !== t.name || base.description !== t.description) {
    setBase({ name: t.name, description: t.description })
    setName(t.name)
    setDescription(t.description)
  }
  const save = useMutation({
    mutationFn: () => api.updateTeam(t.id, { name, description: description || undefined }),
    onSuccess: (updated) => {
      qc.setQueryData([...teamsKey, t.id], updated)
      qc.invalidateQueries({ queryKey: teamsKey })
      qc.invalidateQueries({ queryKey: templatesKey })
    },
  })
  const changes = [
    name !== t.name && `name “${t.name}” → “${name}”`,
    description !== t.description && 'description',
  ].filter(Boolean) as string[]

  const settings = t.can_manage && (
    <Section title="Settings">
      <fieldset className="pane-fields">
        <Rows>
          <Row label="Name" modified={name !== t.name}>
            <input
              className="pane-wide"
              required
              maxLength={100}
              aria-label="Name"
              value={name}
              onChange={(e) => setName(e.target.value)}
            />
          </Row>
          <Row label="Description" modified={description !== t.description}>
            <input
              className="pane-wide"
              maxLength={1000}
              aria-label="Description"
              value={description}
              onChange={(e) => setDescription(e.target.value)}
            />
          </Row>
        </Rows>
      </fieldset>
    </Section>
  )

  return (
    <>
      {children(settings)}
      {changes.length > 0 && (
        <ApplyBar
          count={changes.length}
          actions={
            <>
              <button
                type="button"
                className="btn btn-ghost btn-sm"
                disabled={save.isPending}
                onClick={() => {
                  setName(t.name)
                  setDescription(t.description)
                }}
              >
                <Undo2 size={13} />
                Reset
              </button>
              <button
                type="button"
                className="btn btn-primary btn-sm"
                disabled={save.isPending || !name.trim()}
                onClick={() => save.mutate()}
              >
                Apply
              </button>
            </>
          }
        >
          {save.error ? (
            <span className="action-error">{save.error.message}</span>
          ) : (
            <>Its {changes.join(' and ')}.</>
          )}
        </ApplyBar>
      )}
    </>
  )
}
