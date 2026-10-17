import { useMutation, useQueryClient } from '@tanstack/react-query'
import { GitBranch, X } from 'lucide-react'
import { useState, type FormEvent } from 'react'
import { Link, useNavigate, useSearchParams } from 'react-router'

import { api, type EnvironmentSettings, type Template } from '../api'
import { MemoryField } from '../components/MemoryField'
import { Row, Rows, SearchField, Segmented, Tile } from '../components/Pane'
import { desktop } from '../desktop'
import { PhoneFoot, PhoneHead, PhoneNav, PhoneRow, PhoneSection, PhoneValue } from '../components/Phone'
import { environmentsKey, useEnvironments } from '../environments'
import { useMe } from '../session'
import {
  checkName,
  fillTemplate,
  groupTemplates,
  ownerLabel,
  patternGroups,
  repoName,
  specLine,
  templateVars,
  useTemplates,
} from '../templates'
import { useTeams } from '../teams'

// NewEnvironmentPage is a phone's way to make an environment. On a computer
// it is the New Environment dialog over the environments.
export function NewEnvironmentPage() {
  const me = useMe()
  const qc = useQueryClient()
  const navigate = useNavigate()
  const [params, setParams] = useSearchParams()
  const templates = useTemplates()
  const [name, setName] = useState('')

  const chosenID = params.get('template')
  const chosen = templates.data?.find((t) => t.id === chosenID)
  const choose = (t: Template) => setParams({ template: t.id }, { replace: true })

  const create = useMutation({
    mutationFn: () => api.createEnvironment({ template_id: chosen!.id, name }),
    onSuccess: (env) => {
      qc.invalidateQueries({ queryKey: environmentsKey })
      navigate(`/environments/${env.id}`)
    },
  })

  const problem = chosen ? checkName(name, chosen) : null
  const submit = (e: FormEvent) => {
    e.preventDefault()
    if (chosen && !problem) create.mutate()
  }

  const teams = useTeams()
  const groups = groupTemplates(templates.data ?? [], me, teams.data)
  const ordered = [...groups.mine, ...groups.teams.flatMap((g) => g.templates), ...groups.collaborating, ...groups.others]

  // Choosing and naming are two screens: the name's is the template's, with
  // step=name.
  if (chosen && params.get('step') === 'name') {
    return (
      <form className="phone-screen" onSubmit={submit}>
        <PhoneNav
          back={{ to: `/environments/new?template=${chosen.id}`, label: 'templates' }}
          title="Name it"
          sub={chosen.name}
        />
        <div className="phone-screen-body">
          <label className="field">
            <span>Name</span>
            <input
              autoFocus
              required
              autoCapitalize="none"
              autoCorrect="off"
              value={name}
              onChange={(e) => setName(e.target.value)}
              placeholder={chosen.name_hint ? undefined : 'my-agent'}
              aria-invalid={problem !== null}
            />
            <small className={problem ? 'field-error' : undefined}>
              {problem ?? chosen.name_hint ?? 'It becomes the environment’s hostname, in lower case.'}
            </small>
          </label>
          <PhonePreview template={chosen} name={name} />
          {create.error && <div className="alert">{create.error.message}</div>}
        </div>
        <PhoneFoot>
          <button type="submit" className="btn btn-primary" disabled={create.isPending || !name || problem !== null}>
            {create.isPending ? 'Creating…' : 'Create environment'}
          </button>
        </PhoneFoot>
      </form>
    )
  }
  const section = (title: string, list: Template[]) =>
    list.length > 0 && (
      <PhoneSection key={title} title={title}>
        <div role="radiogroup" aria-label={title} className="phone-radios">
          {list.map((t) => (
            <button
              key={t.id}
              type="button"
              role="radio"
              aria-checked={t.id === chosenID}
              className="phone-row phone-radio"
              onClick={() => choose(t)}
            >
              <span className="phone-radio-mark" aria-hidden="true" />
              <span className="phone-row-main">
                <span className="phone-radio-head">
                  <span className="strong">{t.name}</span>
                  <span className="phone-row-sub">{ownerLabel(t, me)}</span>
                </span>
                {t.description && <span className="phone-radio-desc">{t.description}</span>}
                <span className="phone-row-sub">{specLine(t.spec, t.name_pattern)}</span>
              </span>
            </button>
          ))}
        </div>
      </PhoneSection>
    )
  return (
    <div className="phone-screen">
      <div className="phone-screen-body">
        <PhoneHead back={{ to: '/environments', label: 'Environments' }} title="New environment" />
        {templates.isSuccess && ordered.length === 0 && (
          <p className="phone-note">
            There are no templates yet. <Link to="/templates/new">Make one</Link> first.
          </p>
        )}
        {section('Yours', groups.mine)}
        {groups.teams.map((g) => section(`Team ${g.team.name}`, g.templates))}
        {section('Shared with you to edit', groups.collaborating)}
        {section(me.admin ? 'Everyone else’s' : 'Shared by others', groups.others)}
      </div>
      <PhoneFoot>
        <button
          type="button"
          className="btn btn-primary"
          disabled={!chosen}
          onClick={() => setParams({ template: chosen!.id, step: 'name' })}
        >
          Next
        </button>
      </PhoneFoot>
    </div>
  )
}

// PhonePreview is what the environment will be, with what is known of it
// filled into its template: all but its short ID, which it is given when
// made.
function PhonePreview({ template: t, name }: { template: Template; name: string }) {
  const me = useMe()
  const vars = templateVars(t, name, me.username)
  const groups = patternGroups(t.name_pattern)
  return (
    <PhoneSection title={`${t.name} gives it`}>
      <PhoneRow label="Image" sub={t.spec.image} mono />
      {t.spec.repos.map((r) => (
        <PhoneRow
          key={r.path}
          label="Clones"
          mono
          sub={
            `${repoName(r.url)}${r.ref ? ` at ${fillTemplate(r.ref, vars)}` : ''} into ${fillTemplate(r.path, vars)}` +
            (r.branch ? `, on ${fillTemplate(r.branch, vars)}` : '')
          }
        />
      ))}
      {t.spec.editor_path && <PhoneRow label="Editor opens" sub={fillTemplate(t.spec.editor_path, vars)} mono />}
      {groups.length > 0 && (
        <PhoneRow
          label="From the name"
          mono
          sub={groups.map((g) => `{${g}} = ${vars[g] === undefined ? '…' : vars[g] || '(empty)'}`).join(', ')}
        />
      )}
      <PhoneRow label="Runs as" end={<PhoneValue>{specLine(t.spec)}</PhoneValue>} />
    </PhoneSection>
  )
}

// The browser remembers whether a new environment's workspace is opened in
// a tab of its own.
const openTabKey = 'hangar.new.openTab'

// repoPath is where a repository given in place of a template's is cloned:
// a folder named after it under /workspace.
const repoPath = (url: string) => `/workspace/${repoName(url).split('/').pop() || 'repo'}`

// NewEnvironmentDialog makes an environment, over the environments, as an
// IDE's New Project dialog does: the templates on the left; on the right,
// what the chosen one gives, the name to fill in, and the repository and
// machine, which may be changed from the template's. The template and a
// repository may come from the address (?template=, ?repo=). Creating opens
// the workspace in a tab of its own, unless asked not to.
export function NewEnvironmentDialog({ onClose }: { onClose: () => void }) {
  const me = useMe()
  const qc = useQueryClient()
  const [params, setParams] = useSearchParams()
  const templates = useTemplates()
  const teams = useTeams()
  const envs = useEnvironments()
  const [query, setQuery] = useState('')
  const [name, setName] = useState('')
  const [repo, setRepo] = useState<string | null>(() => params.get('repo'))
  const [machine, setMachine] = useState<EnvironmentSettings | null>(null)
  const [openTab, setOpenTab] = useState(() => {
    try {
      return localStorage.getItem(openTabKey) !== 'false'
    } catch {
      return true
    }
  })

  const groups = groupTemplates(templates.data ?? [], me, teams.data)
  const ordered = [...groups.mine, ...groups.teams.flatMap((g) => g.templates), ...groups.collaborating, ...groups.others]
  // With none asked for, the template of the user's newest environment, or
  // the first.
  const newest = [...(envs.data ?? [])]
    .filter((e) => e.owner_id === me.id && e.template_id)
    .sort((a, b) => b.created_at.localeCompare(a.created_at))[0]
  const chosen =
    ordered.find((t) => t.id === params.get('template')) ??
    ordered.find((t) => t.id === newest?.template_id) ??
    ordered[0]
  const choose = (t: Template) => {
    const next = new URLSearchParams(params)
    next.set('template', t.id)
    setParams(next, { replace: true })
    setMachine(null)
  }

  const create = useMutation({
    mutationFn: (t: Template) =>
      api.createEnvironment({
        template_id: t.id,
        name,
        ...(machine ? { settings: machine } : {}),
        ...(repo !== null
          ? {
              repos: repo.trim()
                ? [{ url: repo.trim(), path: repoPath(repo.trim()), branch: t.spec.repos[0]?.branch }]
                : [],
            }
          : {}),
      }),
  })

  const problem = chosen ? checkName(name, chosen) : null
  const submit = (e: FormEvent) => {
    e.preventDefault()
    if (!chosen || problem || !name) return
    // The tab is opened in the click, which a browser allows, and sent to
    // the workspace once the environment is made.
    const tab = openTab && !desktop ? window.open('', '_blank') : null
    create.mutate(chosen, {
      onSuccess: (env) => {
        qc.invalidateQueries({ queryKey: environmentsKey })
        if (tab) tab.location.href = `/environments/${env.id}`
        else if (openTab) window.open(`/environments/${env.id}`, '_blank')
        onClose()
      },
      onError: () => tab?.close(),
    })
  }

  const q = query.toLowerCase()
  const listed = (list: Template[]) => list.filter((t) => !q || t.name.toLowerCase().includes(q))
  const group = (title: string, list: Template[]) =>
    listed(list).length > 0 && (
      <div key={title} role="group" aria-label={title}>
        <div className="md-group">{title}</div>
        {listed(list).map((t) => (
          <button
            key={t.id}
            type="button"
            role="radio"
            aria-checked={t.id === chosen?.id}
            className={t.id === chosen?.id ? 'md-item active' : 'md-item'}
            onClick={() => choose(t)}
          >
            <Tile name={t.name} small />
            <span className="md-item-main">
              <span className="md-item-title">{t.name}</span>
              <span className="md-item-sub">{specLine(t.spec)}</span>
            </span>
          </button>
        ))}
      </div>
    )

  return (
    <div
      className="dialog-backdrop"
      onMouseDown={(e) => {
        if (e.target === e.currentTarget) onClose()
      }}
    >
      <form
        className="newenv"
        role="dialog"
        aria-modal="true"
        aria-label="New environment"
        onSubmit={submit}
        onKeyDown={(e) => {
          if (e.key === 'Escape') {
            e.preventDefault()
            onClose()
          }
        }}
      >
        <header className="newenv-head">
          <h2>New Environment</h2>
          <button type="button" className="pane-icon-btn" aria-label="Close" onClick={onClose}>
            <X size={15} />
          </button>
        </header>
        <div className="newenv-body">
          <div className="newenv-list">
            <div className="md-list-head">
              <SearchField value={query} onChange={setQuery} placeholder="Templates" />
            </div>
            <div className="md-list-body" role="radiogroup" aria-label="Template">
              {templates.isSuccess && ordered.length === 0 && (
                <p className="md-empty-list">
                  There are no templates yet. <Link to="/templates/new">Make one</Link> first.
                </p>
              )}
              {group('Yours', groups.mine)}
              {groups.teams.map((g) => group(g.team.name, g.templates))}
              {group('Shared with you to edit', groups.collaborating)}
              {group(me.admin ? 'Everyone else’s' : 'Shared by others', groups.others)}
            </div>
          </div>
          {chosen ? (
            <NewEnvironmentForm
              template={chosen}
              name={name}
              onName={setName}
              problem={problem}
              repo={repo}
              onRepo={setRepo}
              machine={machine}
              onMachine={setMachine}
            />
          ) : (
            <div className="md-none">{templates.isPending ? 'Loading…' : 'Choose a template.'}</div>
          )}
        </div>
        <footer className="newenv-foot">
          <label className="check">
            <input
              type="checkbox"
              checked={openTab}
              onChange={(e) => {
                setOpenTab(e.target.checked)
                try {
                  localStorage.setItem(openTabKey, String(e.target.checked))
                } catch {
                  // The choice holds for this dialog regardless.
                }
              }}
            />
            <span>Open its workspace in a new tab</span>
          </label>
          {create.error && <span className="action-error">{create.error.message}</span>}
          <span className="newenv-spacer" />
          <button type="button" className="btn btn-ghost btn-sm" onClick={onClose}>
            Cancel
          </button>
          <button
            type="submit"
            className="btn btn-primary btn-sm"
            disabled={!chosen || !name || problem !== null || create.isPending}
          >
            {create.isPending ? 'Creating…' : 'Create'}
          </button>
        </footer>
      </form>
    </div>
  )
}

// NewEnvironmentForm is what the chosen template gives a new environment,
// filled in for its name as it is typed, with its repository and machine
// to change. repo is a repository given in place of the template's ('' for
// none), or null for the template's; machine likewise.
function NewEnvironmentForm({
  template: t,
  name,
  onName,
  problem,
  repo,
  onRepo,
  machine,
  onMachine,
}: {
  template: Template
  name: string
  onName: (v: string) => void
  problem: string | null
  repo: string | null
  onRepo: (v: string | null) => void
  machine: EnvironmentSettings | null
  onMachine: (m: EnvironmentSettings | null) => void
}) {
  const me = useMe()
  const vars = templateVars(t, name, me.username)
  const groups = patternGroups(t.name_pattern)
  const own = repo?.trim() ?? ''
  const branch = fillTemplate(t.spec.repos[0]?.branch, vars)
  const s = t.spec
  const m = machine ?? { cpus: s.cpus, memory_mib: s.memory_mib, display: s.display, gpu: s.gpu, dax: s.dax ?? false }
  const editor = repo !== null ? (own ? repoPath(own) : '') : fillTemplate(s.editor_path, vars)

  return (
    <div className="newenv-form">
      <div className="newenv-about">
        <Tile name={t.name} />
        <div>
          <div className="strong">{t.name}</div>
          {t.description && <div className="muted small">{t.description}</div>}
          <div className="muted small">{ownerLabel(t, me)}</div>
        </div>
      </div>
      <Rows>
        <Row label="Name">
          <input
            className="pane-input newenv-name"
            autoFocus
            required
            aria-label="Name"
            aria-invalid={problem !== null}
            value={name}
            onChange={(e) => onName(e.target.value)}
            placeholder={t.name_hint ? undefined : 'my-agent'}
          />
          <span className={problem ? 'field-error small' : 'muted small'}>
            {problem ?? t.name_hint ?? 'It becomes its hostname, in lower case.'}
          </span>
        </Row>
        <Row label="Repository" modified={repo !== null}>
          {repo === null ? (
            <>
              {s.repos.length === 0 && <span className="muted">None</span>}
              {s.repos.map((r) => (
                <span key={r.path} className="newenv-repo">
                  <span className="mono">{repoName(r.url)}</span>
                  {r.ref && <span className="muted">at {fillTemplate(r.ref, vars)}</span>}
                  <span className="muted">into</span>
                  <span className="mono">{fillTemplate(r.path, vars)}</span>
                  {r.branch && (
                    <span className="branch">
                      <GitBranch size={11} />
                      <span className="mono">{fillTemplate(r.branch, vars)}</span>
                    </span>
                  )}
                </span>
              ))}
              <button type="button" className="pane-link" onClick={() => onRepo('')}>
                {s.repos.length === 0 ? 'Add one…' : 'Use another…'}
              </button>
            </>
          ) : (
            <>
              <input
                className="pane-input mono newenv-url"
                aria-label="Repository URL"
                placeholder="https://github.com/… (empty for none)"
                value={repo}
                onChange={(e) => onRepo(e.target.value)}
              />
              {own && (
                <span className="newenv-repo">
                  <span className="muted">into</span>
                  <span className="mono">{repoPath(own)}</span>
                  {branch && (
                    <span className="branch">
                      <GitBranch size={11} />
                      <span className="mono">{branch}</span>
                    </span>
                  )}
                </span>
              )}
              <button type="button" className="pane-link" onClick={() => onRepo(null)}>
                Use the template’s
              </button>
            </>
          )}
        </Row>
        <Row label="Machine" modified={machine !== null}>
          {machine === null ? (
            <>
              <span>{specLine({ ...s, repos: [] })}</span>
              <button type="button" className="pane-link" onClick={() => onMachine(m)}>
                Change…
              </button>
            </>
          ) : (
            <span className="newenv-machine">
              <label>
                vCPUs
                <input
                  type="number"
                  className="pane-input pane-number"
                  min={1}
                  max={64}
                  value={m.cpus}
                  onChange={(e) => onMachine({ ...m, cpus: Number(e.target.value) })}
                />
              </label>
              <label>
                Memory
                <MemoryField
                  className="pane-input"
                  mib={m.memory_mib}
                  onChange={(memory_mib) => onMachine({ ...m, memory_mib })}
                />
              </label>
              <Segmented
                label="Display"
                value={m.display}
                onChange={(display) => onMachine({ ...m, display })}
                options={[
                  { value: 'desktop', label: 'Desktop' },
                  { value: 'none', label: 'Headless' },
                ]}
              />
              <label>
                GPU
                <select
                  className="pane-input"
                  value={m.gpu}
                  onChange={(e) => onMachine({ ...m, gpu: e.target.value as EnvironmentSettings['gpu'] })}
                >
                  <option value="none">None</option>
                  <option value="virtual">Virtual</option>
                  {s.gpu === 'passthrough' && <option value="passthrough">Passthrough</option>}
                </select>
              </label>
              <button type="button" className="pane-link" onClick={() => onMachine(null)}>
                Use the template’s
              </button>
            </span>
          )}
        </Row>
        {editor && (
          <Row label="Editor opens">
            <span className="mono">{editor}</span>
          </Row>
        )}
        {groups.length > 0 && (
          <Row label="From the name">
            <span className="mono">
              {groups.map((g) => `{${g}} = ${vars[g] === undefined ? '…' : vars[g] || '(empty)'}`).join(', ')}
            </span>
          </Row>
        )}
        <Row label="Image">
          <span className="mono muted">{s.image}</span>
        </Row>
      </Rows>
    </div>
  )
}
