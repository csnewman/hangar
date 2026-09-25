import {
  Activity,
  Code2,
  Copy,
  Cpu,
  FileCode2,
  GitBranch,
  History,
  LayoutDashboard,
  Monitor,
  SquareTerminal,
  type LucideIcon,
} from 'lucide-react'
import { useState } from 'react'
import { Link, NavLink, Outlet, useLocation, useOutletContext, useParams } from 'react-router'

import type { Environment } from '../api'
import { useMe } from '../session'
import { EnvironmentActions } from '../components/EnvironmentActions'
import { formatAgo } from '../components/format'
import { PageHeader } from '../components/PageHeader'
import { SpecChips } from '../components/SpecChips'
import { StartProgress } from '../components/StartProgress'
import { PhaseBadge } from '../components/Status'
import { useEnvironments } from '../environments'
import { useTitle } from '../title'
import { Activity as AuditActivity } from '../components/Activity'

const tabs: { to: string; label: string; icon: LucideIcon; end?: boolean }[] = [
  { to: '', label: 'Summary', icon: LayoutDashboard, end: true },
  { to: 'metrics', label: 'Metrics', icon: Activity },
  { to: 'processes', label: 'Processes', icon: Cpu },
  { to: 'terminal', label: 'Terminal', icon: SquareTerminal },
  { to: 'code', label: 'Code', icon: FileCode2 },
  { to: 'editor', label: 'VS Code', icon: Code2 },
  { to: 'desktop', label: 'Desktop', icon: Monitor },
  { to: 'activity', label: 'Activity', icon: History },
]

// EnvironmentPage is one environment: its header and actions, and a tab for
// each way into it. The tabs are routes, so a console can be linked to and
// survives a reload.
export function EnvironmentPage() {
  const { id } = useParams()
  const me = useMe()
  const envs = useEnvironments()
  const env = envs.data?.find((e) => e.id === id)
  // The tab is the path's segment after the environment's ID.
  const segment = useLocation().pathname.split('/')[3] ?? ''
  const tab = tabs.find((t) => t.to === segment) ?? tabs[0]
  useTitle(env?.name, env && tab.label)

  if (envs.isPending) return <div className="page" />
  if (!env) {
    return (
      <div className="page">
        <PageHeader crumbs={[{ label: 'Environments', to: '/environments' }]} title="Not found" />
        <div className="panel empty-state">
          This environment does not exist, or it is not yours. <Link to="/environments">Back to environments</Link>
        </div>
      </div>
    )
  }

  return (
    <div className="page page-flush">
      <div className="env-bar">
        <div className="env-ident">
          {env.owner_id !== me.id && (
            <>
              <span className="env-owner">{env.owner}</span>
              <span className="crumb-sep">/</span>
            </>
          )}
          <h1 className="env-name" title={env.name}>
            {env.name}
          </h1>
          <PhaseBadge phase={env.phase} desired={env.desired} />
        </div>
        <nav className="tabs env-tabs">
          {tabs.map((t) => (
            <NavLink key={t.label} to={t.to} end={t.end} className="tab" title={t.label}>
              <t.icon size={15} />
              <span className="tab-label">{t.label}</span>
            </NavLink>
          ))}
        </nav>
        <div className="env-actions">
          <EnvironmentActions env={env} afterDelete="/environments" />
        </div>
      </div>
      {env.phase === 'starting' ? (
        <StartProgress env={env} />
      ) : (
        env.reason && <div className="notice env-reason">{env.reason}</div>
      )}
      <div className="tab-body">
        <Outlet context={env} />
      </div>
    </div>
  )
}

export function useEnv() {
  return useOutletContext<Environment>()
}

export function SummaryTab() {
  const env = useEnv()
  const me = useMe()
  return (
    <div className="page-pad">
      <div className="panel">
        <dl className="props">
          <Prop label="Status">
            <PhaseBadge phase={env.phase} desired={env.desired} />
          </Prop>
          <Prop label="Asked to be">{env.desired}</Prop>
          <Prop label="Owner">{env.owner}</Prop>
          <Prop label="Worker">{env.worker ?? <span className="muted">not yet placed</span>}</Prop>
          <Prop label="Template">
            {env.template_id ? (
              <Link to={`/templates/${env.template_id}`}>{env.template}</Link>
            ) : (
              <span>
                {env.template} <span className="muted">(deleted)</span>
              </span>
            )}
          </Prop>
          <Prop label="Image">
            <span className="mono">{env.image}</span>
          </Prop>
          <Prop label="Resources">
            <SpecChips spec={env.spec} />
          </Prop>
          {env.spec.repos.map((r) => (
            <Prop key={r.path} label="Repository">
              <span className="mono">{r.url}</span> <span className="muted">in</span>{' '}
              <span className="mono">{r.path}</span>
              {r.ref && (
                <>
                  {' '}
                  <span className="muted">at</span> <span className="mono">{r.ref}</span>
                </>
              )}
              {r.branch && (
                <span className="branch">
                  <GitBranch size={12} />
                  <span className="mono">{r.branch}</span>
                </span>
              )}
            </Prop>
          ))}
          {me.ssh && (
            <Prop label="SSH">
              <SSHCommand command={sshCommand(me.ssh, env, env.owner_id === me.id)} />
            </Prop>
          )}
          {env.spec.editor_path && (
            <Prop label="Editor opens">
              <span className="mono">{env.spec.editor_path}</span>
            </Prop>
          )}
          <Prop label="Created">
            {new Date(env.created_at).toLocaleString()} ({formatAgo(env.created_at)})
          </Prop>
          <Prop label="Last change">{formatAgo(env.updated_at)}</Prop>
          <Prop label="ID">
            <span className="mono">{env.id}</span>
          </Prop>
        </dl>
      </div>
    </div>
  )
}

// sshCommand reaches the environment through the SSH gateway: by its name
// alone for its owner, and by owner/name for anyone else who can see it.
function sshCommand(gw: { host: string; port: number }, env: Environment, own: boolean) {
  const user = own ? env.name : `${env.owner}/${env.name}`
  return `ssh ${user}@${gw.host}` + (gw.port === 22 ? '' : ` -p ${gw.port}`)
}

function SSHCommand({ command }: { command: string }) {
  const [copied, setCopied] = useState(false)
  const copy = () => {
    navigator.clipboard.writeText(command).then(() => {
      setCopied(true)
      setTimeout(() => setCopied(false), 1500)
    })
  }
  return (
    <span className="ssh-command">
      <code className="mono">{command}</code>
      <button type="button" className="btn btn-ghost" onClick={copy} title="Copy">
        <Copy size={13} />
        {copied ? 'Copied' : 'Copy'}
      </button>
      <Link to="/profile#sign-in-keys" className="muted small">
        with a sign-in key
      </Link>
    </span>
  )
}

function Prop({ label, children }: { label: string; children: React.ReactNode }) {
  return (
    <div className="prop">
      <dt>{label}</dt>
      <dd>{children}</dd>
    </div>
  )
}

// ActivityTab is everything recorded about the environment: who asked for
// what, where Hangar placed it, what its worker reported, who opened it.
export function ActivityTab() {
  const env = useEnv()
  return (
    <div className="page-pad">
      <AuditActivity subjects={[`environment:${env.id}`]} />
    </div>
  )
}
