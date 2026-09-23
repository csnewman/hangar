import type { DesiredState, Phase } from '../api'

const tone: Record<Phase, string> = {
  pending: 'wait',
  starting: 'busy',
  running: 'good',
  stopping: 'busy',
  stopped: 'idle',
  failed: 'bad',
  deleting: 'busy',
}

// settled is the phase an environment rests in once it has done what it was
// asked, so a different phase means it is still on its way.
const settled: Record<DesiredState, Phase[]> = {
  running: ['running', 'failed'],
  stopped: ['stopped'],
  deleted: [],
}

// PhaseDot is the status as a single dot, for the sidebar's tree.
export function PhaseDot({ phase }: { phase: Phase }) {
  return <span className={`phase-dot tone-${tone[phase]}`} aria-label={phase} />
}

export function PhaseBadge({ phase, desired }: { phase: Phase; desired?: DesiredState }) {
  const moving = desired !== undefined && !settled[desired].includes(phase)
  return (
    <span className="status">
      <span className={`badge badge-${tone[phase]}`}>
        <span className="dot" />
        {phase}
      </span>
      {moving && phase !== desired && <span className="toward">→ {desired}</span>}
    </span>
  )
}

export function OnlineBadge({ online, revoked }: { online: boolean; revoked: boolean }) {
  if (revoked) {
    return (
      <span className="badge badge-bad">
        <span className="dot" />
        revoked
      </span>
    )
  }
  return (
    <span className={`badge ${online ? 'badge-good' : 'badge-idle'}`}>
      <span className="dot" />
      {online ? 'online' : 'offline'}
    </span>
  )
}
