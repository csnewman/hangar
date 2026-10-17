import './environments.css'

import { AppWindow, ExternalLink, GitBranch, MoreHorizontal, Play, Plus } from 'lucide-react'
import { useEffect, useRef, useState, type FormEvent } from 'react'
import { Link, useNavigate } from 'react-router'

import type { Environment } from '../api'
import { copyText, useContextMenu, type MenuItem } from '../code/ContextMenu'
import { formatAgo, formatMemory, formatPercent } from '../components/format'
import { Meter, PaneBar, PaneRoot, SearchField, Segmented, Tile } from '../components/Pane'
import { StartLine } from '../components/StartProgress'
import { InstallHint } from '../components/InstallHint'
import { PhaseDot } from '../components/Status'
import { useEnvironmentActions, vscodeItems } from '../environmentActions'
import { splitByOwner, sshCommand, useEnvironments } from '../environments'
import { usePhone } from '../phone'
import { useMe } from '../session'
import { useTeams } from '../teams'
import { groupTemplates, repoName, specLine, useTemplates } from '../templates'
import { useTitle } from '../title'
import { openEnvironment, useOpenWorkspaces } from '../workspaceLinks'
import { NewEnvironmentDialog } from './NewEnvironment'

export function OverviewPage() {
  return <Launcher />
}

// live is whether an environment is running or on its way to or from it.
const live = (e: Environment) =>
  e.desired === 'running' || ['pending', 'starting', 'running', 'stopping', 'suspending'].includes(e.phase)

// matches is whether an environment answers a search: its name, owner,
// template, or a repository or branch it clones.
function matches(e: Environment, q: string) {
  if (!q) return true
  const s = q.toLowerCase()
  return [e.name, e.owner, e.template, ...e.spec.repos.flatMap((r) => [r.url, r.branch ?? ''])].some((v) =>
    v.toLowerCase().includes(s),
  )
}

// Launcher is Hangar's home, as an IDE's welcome screen is:
// work happens in each environment's own tab, so this is where they are
// found, started and opened, and where new ones are made. Running ones come
// first, the most recently changed first. Beside them: templates to start
// from, a repository to start from, and what is running. creating shows
// the New Environment dialog over it. On a phone an environment opens in
// place, as its own screen.
export function Launcher({ creating = false }: { creating?: boolean }) {
  const me = useMe()
  const phone = usePhone()
  const navigate = useNavigate()
  const envs = useEnvironments()
  useTitle('Environments')
  const open = useOpenWorkspaces()
  const [query, setQuery] = useState('')
  const [everyone, setEveryone] = useState(false)
  const search = useRef<HTMLInputElement>(null)

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === 'k' && !e.shiftKey && !e.altKey) {
        e.preventDefault()
        search.current?.focus()
        search.current?.select()
      }
    }
    window.addEventListener('keydown', onKey)
    return () => window.removeEventListener('keydown', onKey)
  }, [])

  const { mine, others } = splitByOwner(envs.data ?? [], me)
  const hasOthers = others.length > 0
  const shown = (everyone && hasOthers ? [...mine, ...others.flatMap((g) => g.environments)] : mine)
    .filter((e) => matches(e, query))
    .sort((a, b) => b.updated_at.localeCompare(a.updated_at))
  const running = shown.filter(live)
  const resting = shown.filter((e) => !live(e))

  return (
    <PaneRoot>
      <PaneBar
        title="Environments"
        aside={
          phone && (
            <Link
              to="/environments/new"
              className="pane-round-btn"
              aria-label="New environment"
              title="New environment"
            >
              <Plus size={20} />
            </Link>
          )
        }
      >
        <SearchField
          inputRef={search}
          value={query}
          onChange={setQuery}
          placeholder="Search names, repositories, branches"
          keys={phone ? undefined : navigator.platform.startsWith('Mac') ? '⌘K' : 'Ctrl K'}
          width={320}
        />
        {hasOthers && (
          <Segmented
            label="Whose environments"
            value={everyone}
            onChange={setEveryone}
            options={[
              { value: false, label: 'Mine' },
              { value: true, label: 'Everyone' },
            ]}
          />
        )}
        {!phone && (
          <Link to="/environments/new" className="btn btn-primary btn-sm">
            <Plus size={14} />
            New Environment
          </Link>
        )}
      </PaneBar>
      <div className="launch">
        <div className="launch-main">
          {phone && <InstallHint />}
          {envs.isError && <div className="alert">Could not load environments: {envs.error.message}</div>}
          {envs.isSuccess && shown.length === 0 && (
            <div className="launch-empty">
              {query ? (
                <>Nothing matches “{query}”.</>
              ) : (
                <>
                  <p>You have no environments yet.</p>
                  <Link to="/environments/new" className="btn btn-primary btn-sm">
                    <Plus size={14} />
                    New Environment
                  </Link>
                </>
              )}
            </div>
          )}
          {shown.length > 0 && (
            <section className="launch-list" aria-label="Environments">
              {running.length > 0 && <h2 className="launch-group">Running</h2>}
              {running.map((e) => (
                <LaunchRow key={e.id} env={e} open={open.has(e.id)} owner={e.owner_id !== me.id} phone={phone} />
              ))}
              {resting.length > 0 && <h2 className="launch-group">Not running</h2>}
              {resting.map((e) => (
                <LaunchRow key={e.id} env={e} open={open.has(e.id)} owner={e.owner_id !== me.id} phone={phone} />
              ))}
            </section>
          )}
        </div>
        <StartPanel environments={mine} phone={phone} />
      </div>
      {creating && <NewEnvironmentDialog onClose={() => navigate('/environments')} />}
    </PaneRoot>
  )
}

// LaunchRow is one environment: where its code is, what it is doing, what
// it is using, and the way into it. open says its workspace is in a tab
// already; owner shows whose it is, for one of someone else's. On a phone
// the row leads to the environment's screen, and offers only to start one
// that is not running.
function LaunchRow({ env, open, owner, phone }: { env: Environment; open: boolean; owner: boolean; phone: boolean }) {
  const me = useMe()
  const navigate = useNavigate()
  const actions = useEnvironmentActions(env)
  const menu = useContextMenu()
  const [error, setError] = useState('')
  const running = env.phase === 'running'
  const busy = actions.busy || env.desired === 'deleted'
  const repo = env.spec.repos[0]
  const s = env.stats

  const enter = () => {
    if (phone) {
      navigate(`/environments/${env.id}`)
      return
    }
    openEnvironment(env.id)
    if (env.desired !== 'running') actions.start.mutate()
  }
  const label =
    env.desired === 'running'
      ? open
        ? 'Switch to Tab'
        : 'Open'
      : env.phase === 'suspended'
        ? 'Resume and Open'
        : 'Start and Open'

  const items = (): MenuItem[] => {
    const out: MenuItem[] = vscodeItems(env.id, running, setError)
    if (me.ssh) {
      const ssh = me.ssh
      out.push({ label: 'Copy SSH Command', onSelect: () => copyText(sshCommand(ssh, env, env.owner_id === me.id)) })
    }
    out.push({
      label: 'Settings and Details',
      detail: 'in its tab',
      onSelect: () => openEnvironment(env.id, 'environment'),
    })
    out.push('separator')
    if (env.desired === 'running') {
      out.push({
        label: 'Suspend',
        detail: 'keeps its memory',
        disabled: busy || !running,
        onSelect: () => actions.suspend.mutate(),
      })
      out.push({ label: 'Stop', disabled: busy, onSelect: () => actions.stop.mutate() })
    } else {
      out.push({
        label: env.phase === 'suspended' ? 'Resume' : 'Start',
        detail: 'without opening it',
        disabled: busy,
        onSelect: () => actions.start.mutate(),
      })
      if (env.desired === 'suspended') {
        out.push({
          label: 'Stop',
          detail: 'discards its memory',
          disabled: busy,
          onSelect: () => actions.stop.mutate(),
        })
      }
    }
    out.push('separator')
    out.push({
      label: 'Delete…',
      danger: true,
      disabled: busy,
      confirm: `Delete ${env.name}? This cannot be undone.`,
      onSelect: () => actions.remove.mutate(),
    })
    return out
  }

  const problem = error || actions.error?.message
  if (phone) {
    return (
      <div className="launch-row launch-row-phone">
        <Link to={`/environments/${env.id}`} className="launch-row-link">
          <Tile name={env.name} />
          <div className="launch-who">
            <div className="launch-name">
              <span className="launch-name-text">{env.name}</span>
              {owner && <span className="pane-pill pane-pill-grey">{env.owner}</span>}
            </div>
            <div className="launch-state">
              <PhaseDot phase={env.phase} workerOnline={env.worker_online} />
              <strong>{env.phase.charAt(0).toUpperCase() + env.phase.slice(1)}</strong>
              {env.phase === 'starting' ? (
                <StartLine env={env} />
              ) : (
                <span>
                  {running ? (env.worker ? `on ${env.worker}` : '') : (env.reason ?? formatAgo(env.updated_at))}
                  {running && s ? ` · CPU ${formatPercent(s.cpu_percent)}` : ''}
                </span>
              )}
            </div>
            {repo && (
              <span className="launch-repo">
                <span className="mono">{repoName(repo.url)}</span>
                {repo.branch && (
                  <span className="branch">
                    <GitBranch size={11} />
                    <span className="mono">{repo.branch}</span>
                  </span>
                )}
              </span>
            )}
            {problem && <div className="action-error">{problem}</div>}
          </div>
        </Link>
        {env.desired !== 'running' && env.desired !== 'deleted' && (
          <button
            type="button"
            className="pane-link launch-start"
            disabled={busy || env.phase === 'stopping' || env.phase === 'suspending'}
            onClick={() => actions.start.mutate()}
          >
            {env.phase === 'suspended' ? 'Resume' : 'Start'}
          </button>
        )}
      </div>
    )
  }
  return (
    <div className="launch-row">
      <Tile name={env.name} />
      <div className="launch-who">
        <div className="launch-name">
          <a
            href={`/environments/${env.id}`}
            onClick={(e) => {
              if (e.metaKey || e.ctrlKey || e.shiftKey) return
              e.preventDefault()
              enter()
            }}
          >
            {env.name}
          </a>
          {owner && <span className="pane-pill pane-pill-grey">{env.owner}</span>}
          {open && (
            <span className="launch-tab" title="Its workspace is open in another tab">
              <AppWindow size={12} />
              tab open
            </span>
          )}
        </div>
        <div className="launch-state">
          <PhaseDot phase={env.phase} workerOnline={env.worker_online} />
          <strong>{env.phase.charAt(0).toUpperCase() + env.phase.slice(1)}</strong>
          {env.phase === 'starting' ? (
            <StartLine env={env} />
          ) : running ? (
            env.worker && <span>on {env.worker}</span>
          ) : (
            <span>{env.reason ?? formatAgo(env.updated_at)}</span>
          )}
        </div>
        {problem && <div className="action-error">{problem}</div>}
      </div>
      <div className="launch-where">
        {repo ? (
          <span className="launch-repo">
            <span className="mono">{repoName(repo.url)}</span>
            {repo.branch && (
              <span className="branch">
                <GitBranch size={11} />
                <span className="mono">{repo.branch}</span>
              </span>
            )}
            {env.spec.repos.length > 1 && <span className="muted">+{env.spec.repos.length - 1}</span>}
          </span>
        ) : (
          <span className="muted small">No repository</span>
        )}
        <span className="muted small">{env.template}</span>
      </div>
      <div className="launch-usage">
        {running && s && (
          <>
            <span>CPU</span>
            <Meter value={s.cpu_percent / 100} width={64} />
            <span className="launch-usage-num">{formatPercent(s.cpu_percent)}</span>
            <span>Mem</span>
            <Meter
              value={s.memory_total_mib ? s.memory_used_mib / s.memory_total_mib : 0}
              width={64}
              colour="var(--series-2)"
            />
            <span className="launch-usage-num">{formatMemory(s.memory_used_mib)}</span>
          </>
        )}
      </div>
      <div className="launch-actions">
        <button
          type="button"
          className={env.desired === 'running' ? 'btn btn-primary btn-sm' : 'btn btn-ghost btn-sm'}
          disabled={busy || env.phase === 'stopping' || env.phase === 'suspending'}
          onClick={enter}
        >
          {env.desired === 'running' ? open ? <AppWindow size={13} /> : <ExternalLink size={13} /> : <Play size={12} />}
          {label}
        </button>
        <button
          type="button"
          className="pane-icon-btn"
          aria-label={`More for ${env.name}`}
          title="VS Code, SSH, Settings, Suspend, Stop, Delete"
          aria-haspopup="menu"
          onClick={(e) => {
            const r = e.currentTarget.getBoundingClientRect()
            menu.openAt(r.right, r.bottom + 4, items(), 'right')
          }}
        >
          <MoreHorizontal size={15} />
        </button>
      </div>
      {menu.menu}
    </div>
  )
}

// StartPanel is beside the list: the templates to make one from, a
// repository to make one from, and what the user's running environments
// take.
function StartPanel({ environments, phone }: { environments: Environment[]; phone: boolean }) {
  const me = useMe()
  const navigate = useNavigate()
  const templates = useTemplates()
  const teams = useTeams()
  const [url, setUrl] = useState('')
  const groups = groupTemplates(templates.data ?? [], me, teams.data)
  const ordered = [
    ...groups.mine,
    ...groups.teams.flatMap((g) => g.templates),
    ...groups.collaborating,
    ...groups.others,
  ]
  const running = environments.filter((e) => e.phase === 'running')
  const suspended = environments.filter((e) => e.phase === 'suspended')
  const cpus = running.reduce((n, e) => n + e.cpus, 0)
  const memory = running.reduce((n, e) => n + e.memory_mib, 0)

  const fromRepo = (e: FormEvent) => {
    e.preventDefault()
    if (url.trim()) navigate(`/environments/new?repo=${encodeURIComponent(url.trim())}`)
  }

  return (
    <aside className="launch-side">
      <section className="launch-card">
        <h3>Start from a template</h3>
        {templates.isSuccess && ordered.length === 0 && (
          <p className="muted small">
            There are no templates yet. <Link to="/templates/new">Make one</Link>.
          </p>
        )}
        {ordered.slice(0, 4).map((t) => (
          <Link key={t.id} to={`/environments/new?template=${t.id}`} className="launch-template">
            <Tile name={t.name} small />
            <span className="launch-template-main">
              <strong>{t.name}</strong>
              <span className="muted small">{specLine(t.spec, t.name_pattern)}</span>
            </span>
            <Plus size={14} />
          </Link>
        ))}
        {ordered.length > 4 && (
          <Link to="/templates" className="small">
            All {ordered.length} templates
          </Link>
        )}
      </section>
      {!phone && (
        <form className="launch-card" onSubmit={fromRepo}>
          <h3>Or from a repository</h3>
          <div className="launch-repo-form">
            <input
              className="pane-input mono"
              aria-label="Repository URL"
              placeholder="https://github.com/…"
              value={url}
              onChange={(e) => setUrl(e.target.value)}
            />
            <button type="submit" className="btn btn-ghost btn-sm" disabled={!url.trim()}>
              Next
            </button>
          </div>
          <p className="muted small">Cloned into a new environment, on the template you choose next.</p>
        </form>
      )}
      {!phone && (
        <section className="launch-card">
          <h3>Running</h3>
          <p className="launch-total">
            {running.length === 0 ? (
              'Nothing is running.'
            ) : (
              <>
                <strong>{running.length}</strong> running, taking <strong>{cpus}</strong> vCPU{cpus === 1 ? '' : 's'}{' '}
                and <strong>{formatMemory(memory)}</strong>.
              </>
            )}
          </p>
          {suspended.length > 0 && (
            <p className="muted small">
              {suspended.length} suspended, keeping {suspended.length === 1 ? 'its' : 'their'} memory on disk.
            </p>
          )}
        </section>
      )}
    </aside>
  )
}

// NewEnvironmentRoute is /environments/new: the New Environment dialog
// over the environments, or on a phone as a screen of its own.
export function NewEnvironmentRoute() {
  const navigate = useNavigate()
  if (usePhone()) return <NewEnvironmentDialog onClose={() => navigate('/environments')} />
  return <Launcher creating />
}
