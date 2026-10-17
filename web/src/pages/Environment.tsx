import {
  Activity,
  Code2,
  FileCode2,
  LayoutDashboard,
  Monitor,
  ScrollText,
  SquareTerminal,
  type LucideIcon,
} from 'lucide-react'
import { lazy, Suspense, useContext } from 'react'
import { Link, Outlet, useLocation, useNavigate, useOutletContext, useParams, useSearchParams } from 'react-router'

import type { Environment } from '../api'
import { PageHeader } from '../components/PageHeader'
import { PhoneNav } from '../components/Phone'
import { PhoneEnvironmentDetails, PhoneEnvironmentHead, PhoneEnvironmentSettings } from '../components/PhoneEnvironment'
import { StartProgress } from '../components/StartProgress'
import { EnvContext } from '../envContext'
import { useEnvironments } from '../environments'
import { usePhone } from '../phone'
import { useTitle } from '../title'

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

// SummaryTab is a phone's screen for an environment: what it is, the way
// into each of its tabs, and its settings as a screen of their own. On a
// computer the workspace's Environment tab shows it.
export function SummaryTab() {
  const env = useEnv()
  const [params, setParams] = useSearchParams()
  const location = useLocation()
  const navigate = useNavigate()
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
