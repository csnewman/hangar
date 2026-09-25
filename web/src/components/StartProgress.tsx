import { Check, LoaderCircle } from 'lucide-react'

import type { Environment } from '../api'
import { formatBytes } from './format'

type Progress = NonNullable<Environment['progress']>
type Step = Progress['step']

// The steps of a start, in the order they happen.
const steps: { step: Step; label: string }[] = [
  { step: 'download', label: 'Download image' },
  { step: 'unpack', label: 'Unpack image' },
  { step: 'disks', label: 'Prepare disks' },
  { step: 'boot', label: 'Boot' },
  { step: 'network', label: 'Connect network' },
  { step: 'workspace', label: 'Set up workspace' },
]

function fraction(p: Progress): number | null {
  return p.total ? Math.min(1, (p.done ?? 0) / p.total) : null
}

function amount(p: Progress): string {
  const done = p.done ?? 0
  const total = p.total ?? 0
  return p.unit === 'bytes'
    ? `${formatBytes(done)} of ${formatBytes(total)}`
    : `${done.toLocaleString()} of ${total.toLocaleString()} objects`
}

function reasonText(env: Environment): string {
  const r = env.reason ?? ''
  return r ? r[0].toUpperCase() + r.slice(1) : 'Starting'
}

function formatLeft(seconds: number): string {
  if (seconds < 60) return `${Math.max(1, Math.round(seconds))} s left`
  if (seconds < 3600) return `${Math.round(seconds / 60)} min left`
  return `${Math.round(seconds / 3600)} h left`
}

// StartProgress is where a starting environment has got: each step of the
// start, the one in hand, and how much of it is done where that can be
// measured.
export function StartProgress({ env }: { env: Environment }) {
  const p = env.progress
  const current = p ? steps.findIndex((s) => s.step === p.step) : -1
  const f = p ? fraction(p) : null
  const resuming = env.reason?.startsWith('resuming')
  return (
    <div className="start-progress" role="status">
      <ol className="start-steps">
        {steps.map((s, i) => (
          <li
            key={s.step}
            className={
              i < current ? 'start-step start-step-done' : i === current ? 'start-step start-step-on' : 'start-step'
            }
          >
            {i < current ? (
              <Check size={13} />
            ) : i === current ? (
              <LoaderCircle size={13} className="spin" />
            ) : (
              <span className="start-step-dot" />
            )}
            {s.step === 'boot' && resuming ? 'Resume' : s.label}
          </li>
        ))}
      </ol>
      <div className="start-detail">
        <span>{reasonText(env)}</span>
        {p && f !== null && (
          <span className="muted">
            {Math.floor(f * 100)}% · {amount(p)}
            {p.rate && p.unit === 'bytes' && ` · ${formatBytes(p.rate)}/s`}
            {p.rate && ` · ${formatLeft(((p.total ?? 0) - (p.done ?? 0)) / p.rate)}`}
          </span>
        )}
      </div>
      {f !== null && (
        <div className="start-bar" aria-hidden="true">
          <div style={{ width: `${f * 100}%` }} />
        </div>
      )}
    </div>
  )
}

// StartLine is a start's progress in a line, for lists: what it is doing
// and, where measured, how far through.
export function StartLine({ env }: { env: Environment }) {
  const p = env.progress
  const f = p ? fraction(p) : null
  return (
    <div className="reason start-line">
      <span>{reasonText(env)}</span>
      {f !== null && (
        <>
          <span className="start-line-bar" aria-hidden="true">
            <span style={{ width: `${f * 100}%` }} />
          </span>
          <span>{Math.floor(f * 100)}%</span>
        </>
      )}
    </div>
  )
}
