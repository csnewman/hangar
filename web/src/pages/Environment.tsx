import {
  Activity,
  Code2,
  Copy,
  ExternalLink,
  FileCode2,
  GitBranch,
  LayoutDashboard,
  Monitor,
  ScrollText,
  SquareTerminal,
  type LucideIcon,
} from 'lucide-react'
import { lazy, Suspense, useContext, useState } from 'react'
import { Link, Outlet, useLocation, useNavigate, useOutletContext, useParams, useSearchParams } from 'react-router'

import type { Environment } from '../api'
import { useMe } from '../session'
import { formatAgo, shortDigest } from '../components/format'
import { PageHeader } from '../components/PageHeader'
import { PhoneNav } from '../components/Phone'
import { PhoneEnvironmentDetails, PhoneEnvironmentHead, PhoneEnvironmentSettings } from '../components/PhoneEnvironment'
import { SpecChips } from '../components/SpecChips'
import { EnvironmentPacks } from '../components/EnvironmentPacks'
import { SettingsPanel, TemplateDrift } from '../components/EnvironmentSettings'
import { EnvironmentPorts } from '../components/EnvironmentPorts'
import { ImageUpdate } from '../components/ImageUpdate'
import { StartProgress } from '../components/StartProgress'
import { EnvContext } from '../envContext'
import { PhaseBadge, RendererBadge } from '../components/Status'
import { sshCommand, useEnvironments, webURL } from '../environments'
import { usePhone } from '../phone'
import { useTitle } from '../title'
import { Activity as AuditActivity } from '../components/Activity'

const tabs: { to: string; label: string; icon: LucideIcon; end?: boolean }[] = [
  { to: '', label: 'Summary', icon: LayoutDashboard, end: true },
  { to: 'performance', label: 'Performance', icon: Activity },
  { to: 'terminal', label: 'Terminal', icon: SquareTerminal },
  { to: 'code', label: 'Code', icon: FileCode2 },
  { to: 'editor', label: 'VS Code', icon: Code2 },
  { to: 'desktop', label: 'Desktop', icon: Monitor },
  { to: 'logs', label: 'Logs', icon: ScrollText },
]

// The workspace carries the editor, terminal and desktop, so it is fetched
// when an environment is first opened, not with the app.
const Workspace = lazy(() => import('../code/CodeTab').then((m) => ({ default: m.Workspace })))

// EnvironmentPage is one environment. On a computer it is the environment's
// workspace, the whole page, with every way into it as a tab or tool window
// there. On a phone the summary is the environment's screen, leading to each
// tab, and a tab fills the screen under a bar back to it; the tabs are
// routes, so one can be linked to and survives a reload.
export function EnvironmentPage() {
  const phone = usePhone()
  const { id } = useParams()
  const envs = useEnvironments()
  const env = envs.data?.find((e) => e.id === id)
  // The tab is the path's segment after the environment's ID.
  const segment = useLocation().pathname.split('/')[3] ?? ''
  const tab = tabs.find((t) => t.to === segment) ?? tabs[0]
  useTitle(env?.name, env && phone && tab.label)

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

  if (phone) {
    const notice =
      env.phase === 'starting' ? (
        <StartProgress env={env} />
      ) : (
        env.reason && (
          <div className="notice env-reason">
            {env.reason}
            {env.phase === 'failed' && <Link to={`/environments/${env.id}/logs`}>See its logs</Link>}
          </div>
        )
      )

    const summary = tab === tabs[0]
    // The Code and Terminal tabs have bars of their own, with the way back.
    const bar = !summary && tab.to !== 'code' && tab.to !== 'terminal'
    return (
      <div className="page page-flush">
        {bar && <PhoneNav back={{ to: `/environments/${env.id}`, label: env.name }} title={tab.label} sub={env.name} />}
        {!summary && notice}
        <div className="tab-body">
          <Outlet context={env} />
        </div>
      </div>
    )
  }

  return (
    <EnvContext.Provider value={env}>
      <Suspense fallback={<div className="workspace" />}>
        <Workspace env={env} />
      </Suspense>
    </EnvContext.Provider>
  )
}

// useEnv is the environment the page is about: the workspace's, or the
// one its route's outlet was given.
export function useEnv() {
  const shown = useContext(EnvContext)
  const routed = useOutletContext<Environment>()
  return shown ?? routed
}

export function SummaryTab() {
  const env = useEnv()
  const me = useMe()
  const phone = usePhone()
  const [params, setParams] = useSearchParams()
  const location = useLocation()
  const navigate = useNavigate()
  if (phone) {
    if (params.get('show') === 'settings') {
      // Back is a step back, unless the screen was opened directly.
      const back = () => (location.key !== 'default' ? navigate(-1) : setParams({}, { replace: true }))
      return <PhoneEnvironmentSettings env={env} onBack={back} />
    }
    return (
      <div className="page-pad phone-env-page">
        <PhoneEnvironmentHead env={env} />
        <PhoneEnvironmentDetails env={env} />
      </div>
    )
  }
  return (
    <div className="page-pad">
      <TemplateDrift env={env} />
      <ImageUpdate env={env} />
      <div className="panel">
        <dl className="props">
          <Prop label="Status">
            <PhaseBadge phase={env.phase} desired={env.desired} workerOnline={env.worker_online} />
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
            {env.image_digest && (
              <div
                className="muted small mono"
                title={`Boots from this copy of the image, whatever the reference names later: ${env.image_digest}`}
              >
                {shortDigest(env.image_digest)}
              </div>
            )}
          </Prop>
          <Prop label="Resources">
            <SpecChips spec={env.spec} />
          </Prop>
          {env.gpu_renderer && (
            <Prop label="Virtual GPU">
              <RendererBadge software={!!env.gpu_software} /> <span className="mono">{env.gpu_renderer}</span>
              {env.gpu_software && (
                <div className="muted small">
                  Rendered on the worker's CPU, beside this environment's vCPUs. Performance shows how much.
                </div>
              )}
            </Prop>
          )}
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
          {me.environment_host_suffix !== undefined && (
            <Prop label="Web servers">
              <span className="web-links">
                {['', ...(env.spec.web_names ?? [])].map((n) => (
                  <a
                    key={n}
                    className="mono"
                    href={webURL(n, env.short_id, me.environment_host_suffix ?? '')}
                    target="_blank"
                    rel="noreferrer"
                    title={env.phase === 'running' ? undefined : 'The environment is not running'}
                  >
                    {n || 'default'} <ExternalLink size={12} />
                  </a>
                ))}
              </span>
            </Prop>
          )}
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
      <EnvironmentPorts env={env} />
      <EnvironmentPacks env={env} />
      <SettingsPanel
        key={`${env.id}:${env.spec.cpus}:${env.spec.memory_mib}:${env.spec.display}:${env.spec.gpu}:${env.spec.dax}`}
        env={env}
      />
      <section className="section">
        <h2 className="section-title">Activity</h2>
        <AuditActivity subjects={[`environment:${env.id}`]} pageSize={5} />
      </section>
    </div>
  )
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
      <Link to="/account#sign-in-keys" className="muted small">
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
