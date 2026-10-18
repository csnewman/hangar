import { useMutation, useQueryClient } from '@tanstack/react-query'
import { GitBranch, X } from 'lucide-react'
import { useState, type FormEvent } from 'react'
import { Link, useNavigate, useSearchParams } from 'react-router'

import { api, type EnvironmentSettings, type Template } from '../api'
import { CPUField, MemoryField } from '../components/MachineFields'
import { Row, Rows, SearchField, Segmented, Tile } from '../components/Pane'
import { desktop } from '../desktop'
import { PhoneFoot, PhoneNav } from '../components/Phone'
import { environmentsKey, useEnvironments } from '../environments'
import { usePhone } from '../phone'
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
  const phone = usePhone()
  const navigate = useNavigate()
  // On a phone the templates are a screen of their own, chosen from the
  // form.
  const [picking, setPicking] = useState(false)
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
    setPicking(false)
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
    if (phone) {
      create.mutate(chosen, {
        onSuccess: (env) => {
          qc.invalidateQueries({ queryKey: environmentsKey })
          navigate(`/environments/${env.id}`)
        },
      })
      return
    }
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

  const form = chosen && (
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
  )
  const list = (
    <>
      {templates.isSuccess && ordered.length === 0 && (
        <p className="md-empty-list">
          There are no templates yet. <Link to="/templates/new">Make one</Link> first.
        </p>
      )}
      {group('Yours', groups.mine)}
      {groups.teams.map((g) => group(g.team.name, g.templates))}
      {group('Shared with you to edit', groups.collaborating)}
      {group(me.admin ? 'Everyone else’s' : 'Shared by others', groups.others)}
    </>
  )

  if (phone) {
    return (
      <form className="newenv-phone" onSubmit={submit}>
        <PhoneNav
          back={
            picking
              ? { onBack: () => setPicking(false), label: 'New environment' }
              : { onBack: onClose, label: 'Environments' }
          }
          title={picking ? 'Choose a template' : 'New environment'}
        />
        {picking ? (
          <div className="newenv-phone-list">
            <div className="md-list-head">
              <SearchField value={query} onChange={setQuery} placeholder="Templates" />
            </div>
            <div role="radiogroup" aria-label="Template">
              {list}
            </div>
          </div>
        ) : (
          <>
            <div className="newenv-phone-body">
              {chosen ? (
                <>
                  <button type="button" className="newenv-phone-template" onClick={() => setPicking(true)}>
                    <Tile name={chosen.name} />
                    <span className="md-item-main">
                      <span className="md-item-title">{chosen.name}</span>
                      <span className="md-item-sub">{specLine(chosen.spec)}</span>
                    </span>
                    <span className="pane-link">Change</span>
                  </button>
                  {form}
                </>
              ) : (
                <div className="md-none">{templates.isPending ? 'Loading…' : list}</div>
              )}
            </div>
            <PhoneFoot>
              {create.error && <div className="alert">{create.error.message}</div>}
              <button
                type="submit"
                className="btn btn-primary"
                disabled={!chosen || !name || problem !== null || create.isPending}
              >
                {create.isPending ? 'Creating…' : 'Create'}
              </button>
            </PhoneFoot>
          </>
        )}
      </form>
    )
  }

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
              {list}
            </div>
          </div>
          {form || (
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
  const projects =
    repo !== null
      ? own
        ? [repoPath(own)]
        : []
      : (s.projects?.length ? s.projects : s.repos.map((r) => r.path)).map((p) => fillTemplate(p, vars) ?? p)

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
                <span>vCPUs</span>
                <CPUField
                  className="pane-input"
                  cpus={m.cpus}
                  memory={m.memory_mib}
                  onChange={(cpus) => onMachine({ ...m, cpus })}
                />
              </label>
              <label>
                <span>Memory</span>
                <MemoryField
                  className="pane-input"
                  mib={m.memory_mib}
                  cpus={m.cpus}
                  onChange={(memory_mib) => onMachine({ ...m, memory_mib })}
                />
              </label>
              <span className="newenv-machine-row">
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
                  <span>GPU</span>
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
            </span>
          )}
        </Row>
        {projects.length > 0 && (
          <Row label={projects.length === 1 ? 'Project' : 'Projects'}>
            <span className="mono">
              {projects.map((p, i) => (
                <span key={i} className="newenv-project">
                  {p}
                </span>
              ))}
            </span>
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
