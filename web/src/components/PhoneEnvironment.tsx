import { useQuery } from '@tanstack/react-query'
import {
  Activity,
  ChevronLeft,
  ChevronRight,
  FileCode2,
  GitBranch,
  Monitor,
  Plus,
  ScrollText,
  SquareTerminal,
  type LucideIcon,
} from 'lucide-react'
import { Link } from 'react-router'

import { api, type Environment } from '../api'
import { useMe } from '../session'
import { EnvironmentActions } from './EnvironmentActions'
import { formatAgo, formatMemory } from './format'
import { StartProgress } from './StartProgress'
import { PhaseBadge } from './Status'

// PhoneEnvironmentHead is the top of an environment's screen on a phone,
// above its details: what it is, its controls, its terminal sessions -- what
// a phone is mostly for -- and a row for each other way into it that works
// on a phone.
export function PhoneEnvironmentHead({ env }: { env: Environment }) {
  const me = useMe()
  const running = env.phase === 'running'
  return (
    <div className="phone-env-head">
      <Link to="/environments" className="phone-back">
        <ChevronLeft size={22} />
        <span>Environments</span>
      </Link>
      <div className="phone-env-ident">
        <h1 className="phone-title">
          {env.owner_id !== me.id && <span className="muted">{env.owner} / </span>}
          {env.name}
        </h1>
        <PhaseBadge phase={env.phase} desired={env.desired} workerOnline={env.worker_online} />
        <div className="muted">
          {env.template} · {env.cpus} vCPU · {formatMemory(env.memory_mib)}
        </div>
      </div>
      {env.phase === 'starting' ? (
        <StartProgress env={env} />
      ) : (
        env.reason && (
          <div className="notice">
            {env.reason}
            {env.phase === 'failed' && (
              <>
                {' '}
                <Link to="logs">See its logs</Link>
              </>
            )}
          </div>
        )
      )}
      <div className="phone-env-actions">
        <EnvironmentActions env={env} afterDelete="/environments" />
      </div>

      {running && <Sessions env={env} />}

      <h2 className="phone-heading">Workspace</h2>
      <div className="phone-list">
        {running && <Row to="code" icon={GitBranch} label="Changes" about="What is uncommitted, and not pushed" />}
        {running && <Row to="code?view=files" icon={FileCode2} label="Files" />}
        <Row to="performance" icon={Activity} label="Performance" />
        <Row to="logs" icon={ScrollText} label="Logs" />
        {env.spec.display === 'desktop' && <Row to="desktop" icon={Monitor} label="Desktop" />}
      </div>

      <h2 className="phone-heading">Details</h2>
    </div>
  )
}

// Sessions are the environment's terminal sessions, each opened by a tap,
// and a new one.
function Sessions({ env }: { env: Environment }) {
  // The terminal tab's query, so each shows what the other learnt.
  const sessions = useQuery({ queryKey: ['terminals', env.id], queryFn: () => api.terminals(env.id), refetchInterval: 3000 })
  const list = sessions.data ?? []
  return (
    <>
      <h2 className="phone-heading">Terminal sessions</h2>
      <div className="phone-list">
        {list.map((s, i) => (
          <Link key={s.id} to={`terminal?session=${encodeURIComponent(s.id)}`} className="phone-row">
            <SquareTerminal size={18} />
            <span className="phone-row-main">
              <span>{s.title || `Shell ${i + 1}`}</span>
              <span className="phone-row-sub">
                started {formatAgo(s.created_at)}
                {s.clients > 0 && ` · ${s.clients} ${s.clients === 1 ? 'window' : 'windows'} attached`}
              </span>
            </span>
            <ChevronRight size={16} className="phone-row-chev" />
          </Link>
        ))}
        <Link to="terminal?new" className="phone-row phone-row-accent">
          <Plus size={18} />
          <span className="phone-row-main">New session</span>
        </Link>
      </div>
    </>
  )
}

function Row({ to, icon: Icon, label, about }: { to: string; icon: LucideIcon; label: string; about?: string }) {
  return (
    <Link to={to} className="phone-row">
      <Icon size={18} />
      <span className="phone-row-main">
        <span>{label}</span>
        {about && <span className="phone-row-sub">{about}</span>}
      </span>
      <ChevronRight size={16} className="phone-row-chev" />
    </Link>
  )
}
