import type { DesiredState, Phase } from '../api'

const tone: Record<Phase, string> = {
  pending: 'wait',
  starting: 'busy',
  running: 'good',
  stopping: 'busy',
  stopped: 'idle',
  suspending: 'busy',
  suspended: 'idle',
  failed: 'bad',
  deleting: 'busy',
}

// settled is the phase an environment rests in once it has done what it was
// asked, so a different phase means it is still on its way.
const settled: Record<DesiredState, Phase[]> = {
  running: ['running', 'failed'],
  stopped: ['stopped'],
  // A worker that was not running it leaves it stopped.
  suspended: ['suspended', 'stopped'],
  deleted: [],
}

// live are the phases that hold only while their worker keeps them: a
// stopped or suspended environment stays so with its worker gone.
const live: Phase[] = ['starting', 'running', 'stopping', 'suspending', 'deleting']

// unreachable is whether the phase is one its worker would have to be
// reporting, and it is not (worker_online false).
function unreachable(phase: Phase, workerOnline?: boolean) {
  return workerOnline === false && live.includes(phase)
}

const unreachableTitle = (phase: Phase) =>
  `Its worker has not reported for a while, so this is not known. It was last ${phase}.`

// PhaseDot is the status as a single dot, for the sidebar's tree.
export function PhaseDot({ phase, workerOnline }: { phase: Phase; workerOnline?: boolean }) {
  if (unreachable(phase, workerOnline)) {
    return <span className="phase-dot tone-bad" aria-label="unreachable" title={unreachableTitle(phase)} />
  }
  return <span className={`phase-dot tone-${tone[phase]}`} aria-label={phase} />
}

export function PhaseBadge({
  phase,
  desired,
  workerOnline,
}: {
  phase: Phase
  desired?: DesiredState
  workerOnline?: boolean
}) {
  if (unreachable(phase, workerOnline)) {
    return (
      <span className="status">
        <span className="badge badge-bad" title={unreachableTitle(phase)}>
          <span className="dot" />
          unreachable
        </span>
      </span>
    )
  }
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

// RendererBadge says whether a virtual GPU renders on a GPU or the CPU.
export function RendererBadge({ software }: { software: boolean }) {
  return software ? (
    <span className="badge badge-idle" title="Rendered on the worker's CPU, outside the environment's vCPUs">
      software
    </span>
  ) : (
    <span className="badge badge-good" title="Rendered on the worker's GPU">
      hardware
    </span>
  )
}
