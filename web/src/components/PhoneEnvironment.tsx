import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
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
import { changeable, environmentsKey, settingsSummary, sshCommand, webURL } from '../environments'
import { useMe } from '../session'
import { Activity as AuditActivity } from './Activity'
import { EnvironmentActions } from './EnvironmentActions'
import { EnvironmentPacks } from './EnvironmentPacks'
import { SettingsForm, TemplateDrift } from './EnvironmentSettings'
import { formatAgo, formatMemory, shortDigest } from './format'
import { ImageUpdate } from './ImageUpdate'
import { PhoneChoice, PhoneCopy, PhoneNav, PhoneRow, PhoneSection, PhoneValue } from './Phone'
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
    </div>
  )
}

// Sessions are the environment's terminal sessions, each opened by a tap,
// and a new one.
function Sessions({ env }: { env: Environment }) {
  // The terminal tab's query, so each shows what the other learnt.
  const sessions = useQuery({
    queryKey: ['terminals', env.id],
    queryFn: () => api.terminals(env.id),
    refetchInterval: 3000,
  })
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

// PhoneEnvironmentDetails is the rest of an environment's screen on a
// phone, under its head: what needs doing about its template or image,
// then what it is, how to reach it, its settings, its files and what has
// happened to it.
export function PhoneEnvironmentDetails({ env }: { env: Environment }) {
  const me = useMe()
  const own = env.owner_id === me.id
  const suffix = me.environment_host_suffix
  const s = env.spec
  return (
    <>
      <TemplateDrift env={env} />
      <ImageUpdate env={env} />
      <PhoneSection>
        <PhoneRow
          label="Template"
          to={env.template_id ? `/templates/${env.template_id}` : undefined}
          end={<PhoneValue>{env.template_id ? env.template : `${env.template} (deleted)`}</PhoneValue>}
        />
        <PhoneRow
          label="Image"
          sub={env.image_digest ? `${env.image} · ${shortDigest(env.image_digest)}` : env.image}
          mono
        />
        <PhoneRow
          label="Resources"
          end={
            <PhoneValue>
              {s.cpus} vCPU · {formatMemory(s.memory_mib)} · {s.display === 'desktop' ? 'Desktop' : 'Headless'}
              {s.gpu !== 'none' && ` · ${s.gpu === 'virtual' ? 'Virtual GPU' : 'GPU passthrough'}`}
            </PhoneValue>
          }
        />
        {env.gpu_renderer && (
          <PhoneRow
            label="Virtual GPU"
            sub={env.gpu_software ? `${env.gpu_renderer}, on the worker's CPU` : env.gpu_renderer}
          />
        )}
        <PhoneRow label="Worker" end={<PhoneValue>{env.worker ?? 'not yet placed'}</PhoneValue>} />
        {!own && <PhoneRow label="Owner" end={<PhoneValue>{env.owner}</PhoneValue>} />}
        {s.repos.map((r) => (
          <PhoneRow
            key={r.path}
            label="Repository"
            sub={`${r.url} in ${r.path}${r.ref ? ` at ${r.ref}` : ''}${r.branch ? `, on ${r.branch}` : ''}`}
            mono
          />
        ))}
        <PhoneRow label="Created" end={<PhoneValue>{formatAgo(env.created_at)}</PhoneValue>} />
      </PhoneSection>

      {me.ssh && (
        <PhoneSection title="SSH">
          <PhoneCopy text={sshCommand(me.ssh, env, own)} />
          <PhoneRow to="/account#sign-in-keys" label="Sign-in keys" sub="Add one to reach it over SSH" />
        </PhoneSection>
      )}

      {suffix !== undefined && <PhoneWebServers env={env} suffix={suffix} />}

      <PhoneSection note={changeable(env) ? 'Changes take effect when it next starts.' : 'Stop it to change these.'}>
        <PhoneRow to="?show=settings" label="Settings" sub={settingsSummary(env)} />
      </PhoneSection>

      <EnvironmentPacks env={env} />

      <section className="phone-section">
        <h2 className="phone-heading">Activity</h2>
        <AuditActivity subjects={[`environment:${env.id}`]} pageSize={5} />
      </section>
    </>
  )
}

function PhoneWebServers({ env, suffix }: { env: Environment; suffix: string }) {
  const qc = useQueryClient()
  const set = useMutation({
    mutationFn: (pub: boolean) => api.setEnvironmentPorts(env.id, pub),
    onSettled: () => qc.invalidateQueries({ queryKey: environmentsKey }),
  })
  const pub = env.ports_public ?? false
  return (
    <section className="phone-section">
      <h2 className="phone-heading">Web servers</h2>
      <div className="phone-list">
        {['', ...(env.spec.web_names ?? [])].map((n) => (
          <PhoneRow
            key={n}
            href={webURL(n, env.short_id, suffix)}
            label={n || 'Its own'}
            sub={webURL(n, env.short_id, suffix)
              .replace(/^https?:\/\//, '')
              .replace(/\/$/, '')}
            mono
          />
        ))}
      </div>
      <PhoneChoice
        label="Who reaches them"
        value={pub ? 'public' : 'private'}
        options={[
          { value: 'private', label: 'Private' },
          { value: 'public', label: 'Public' },
        ]}
        disabled={set.isPending}
        onChange={(v) => set.mutate(v === 'public')}
      />
      <p className="phone-note">
        {pub
          ? 'Anyone who can reach Hangar, without signing in.'
          : 'Only people who may use this environment, signed in to Hangar.'}{' '}
        HTTPS reaches its port 443 and HTTP its port 80.
      </p>
      {set.error && <div className="alert">{set.error.message}</div>}
    </section>
  )
}

// PhoneEnvironmentSettings is an environment's settings as a screen of
// their own, changed while it is stopped.
export function PhoneEnvironmentSettings({ env, onBack }: { env: Environment; onBack: () => void }) {
  const ok = changeable(env)
  return (
    <div className="phone-screen">
      <PhoneNav back={{ onBack, label: env.name }} title="Settings" sub={env.name} />
      <div className="phone-screen-body">
        {!ok && <div className="notice">Stop {env.name} to change these. Changes take effect when it next starts.</div>}
        <SettingsForm
          key={`${env.spec.cpus}:${env.spec.memory_mib}:${env.spec.display}:${env.spec.gpu}:${env.spec.dax}`}
          env={env}
          disabled={!ok}
          onDone={onBack}
        />
      </div>
    </div>
  )
}
