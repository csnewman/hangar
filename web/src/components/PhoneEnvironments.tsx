import { useMutation, useQueryClient } from '@tanstack/react-query'
import { ChevronRight, Play, Plus, Share, SquarePlus, X } from 'lucide-react'
import { useState } from 'react'
import { Link } from 'react-router'

import { api, type Environment } from '../api'
import { environmentsKey, splitByOwner, useEnvironments } from '../environments'
import { installable } from '../phone'
import { useMe } from '../session'
import { useTitle } from '../title'
import { formatMemory } from './format'
import { StartLine } from './StartProgress'
import { PhaseBadge } from './Status'

// PhoneEnvironments is the environments on a phone: the running ones
// first, each a row leading to its screen, and the rest with a button to
// start each.
export function PhoneEnvironments() {
  const me = useMe()
  const envs = useEnvironments()
  useTitle('Environments')
  const { mine, others } = splitByOwner(envs.data ?? [], me)
  const running = mine.filter((e) => e.desired === 'running')
  const resting = mine.filter((e) => e.desired !== 'running')
  const otherEnvs = others.flatMap((g) => g.environments)

  return (
    <div className="page phone-page">
      <div className="phone-head">
        <h1 className="phone-title">Environments</h1>
        <Link to="/environments/new" className="phone-icon-btn" aria-label="New environment" title="New environment">
          <Plus size={22} />
        </Link>
      </div>
      <InstallHint />
      {envs.isError && <div className="alert">Could not load environments: {envs.error.message}</div>}
      {envs.isPending && <div className="muted">Loading…</div>}
      {envs.isSuccess && mine.length === 0 && (
        <div className="phone-empty">
          You have no environments. <Link to="/environments/new">Create one</Link>.
        </div>
      )}
      <Group title="Running" environments={running} />
      <Group title="Stopped" environments={resting} />
      {me.admin && <Group title="Other users" environments={otherEnvs} showOwner />}
    </div>
  )
}

function Group({
  title,
  environments,
  showOwner = false,
}: {
  title: string
  environments: Environment[]
  showOwner?: boolean
}) {
  if (environments.length === 0) return null
  return (
    <section>
      <h2 className="phone-heading">
        {title} · {environments.length}
      </h2>
      <div className="phone-list">
        {environments.map((e) => (
          <Row key={e.id} env={e} showOwner={showOwner} />
        ))}
      </div>
    </section>
  )
}

function Row({ env, showOwner }: { env: Environment; showOwner: boolean }) {
  const startable = env.desired !== 'running' && env.desired !== 'deleted'
  return (
    <div className="phone-row phone-env">
      <Link to={`/environments/${env.id}`} className="phone-env-link">
        <span className="phone-row-main">
          <span className="phone-env-name">
            {showOwner && <span className="muted">{env.owner} / </span>}
            {env.name}
          </span>
          <span className="phone-env-sub">
            {env.template} · {env.cpus} vCPU · {formatMemory(env.memory_mib)}
          </span>
          {env.phase === 'starting' ? <StartLine env={env} /> : env.reason && <span className="reason">{env.reason}</span>}
        </span>
        <PhaseBadge phase={env.phase} desired={env.desired} workerOnline={env.worker_online} />
        {!startable && <ChevronRight size={16} className="phone-row-chev" />}
      </Link>
      {startable && <StartButton env={env} />}
    </div>
  )
}

// StartButton starts a stopped environment, or resumes a suspended one, from
// the list.
function StartButton({ env }: { env: Environment }) {
  const qc = useQueryClient()
  const start = useMutation({
    mutationFn: () => api.startEnvironment(env.id),
    onSettled: () => qc.invalidateQueries({ queryKey: environmentsKey }),
  })
  return (
    <button
      type="button"
      className="btn btn-ghost phone-env-start"
      disabled={start.isPending || env.phase === 'stopping' || env.phase === 'suspending'}
      title={start.error?.message}
      onClick={() => start.mutate()}
    >
      <Play size={14} />
      {env.phase === 'suspended' ? 'Resume' : 'Start'}
    </button>
  )
}

const hintKey = 'hangar.install.dismissed'

// InstallHint says how to put Hangar on the home screen, in Safari on an
// iPhone, until it is dismissed.
function InstallHint() {
  const [shown, setShown] = useState(() => {
    try {
      return installable() && localStorage.getItem(hintKey) !== 'true'
    } catch {
      return installable()
    }
  })
  if (!shown) return null
  const dismiss = () => {
    setShown(false)
    try {
      localStorage.setItem(hintKey, 'true')
    } catch {
      // Shown again next time.
    }
  }
  return (
    <div className="phone-card install-hint">
      <SquarePlus size={22} className="install-hint-icon" />
      <div className="install-hint-text">
        <span className="strong">Add Hangar to your Home Screen</span>
        <span className="muted">
          In Safari's Share menu <Share size={14} className="install-hint-share" aria-hidden="true" />, choose Add to
          Home Screen. Hangar then opens full screen, like an app, and asks you to sign in once more.
        </span>
      </div>
      <button type="button" className="phone-icon-btn" aria-label="Dismiss" onClick={dismiss}>
        <X size={18} />
      </button>
    </div>
  )
}
