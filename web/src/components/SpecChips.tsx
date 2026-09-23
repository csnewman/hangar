import { Cpu, GitBranch, MapPin, Monitor, MonitorOff, Tag } from 'lucide-react'

import type { Spec } from '../api'
import { repoName } from '../templates'
import { formatMemory } from './format'

// SpecChips summarises what an environment or template is, at a glance.
export function SpecChips({ spec, namePattern }: { spec: Spec; namePattern?: string }) {
  const placement = Object.entries(spec.placement ?? {})
  return (
    <div className="chips">
      <span className="chip">
        {spec.cpus} vCPU · {formatMemory(spec.memory_mib)}
      </span>
      {spec.display === 'desktop' ? (
        <span className="chip">
          <Monitor size={12} />
          Desktop
        </span>
      ) : (
        <span className="chip chip-muted">
          <MonitorOff size={12} />
          Headless
        </span>
      )}
      {spec.gpu !== 'none' && (
        <span className={spec.gpu === 'passthrough' ? 'chip chip-strong' : 'chip'}>
          <Cpu size={12} />
          {spec.gpu === 'passthrough' ? 'GPU passthrough' : 'Virtual GPU'}
        </span>
      )}
      {spec.repos.length > 0 && (
        <span className="chip">
          <GitBranch size={12} />
          {spec.repos.length === 1 ? repoName(spec.repos[0].url) : `${spec.repos.length} repos`}
        </span>
      )}
      {namePattern && (
        <span className="chip" title={`Names must match ${namePattern}`}>
          <Tag size={12} />
          Named
        </span>
      )}
      {placement.map(([k, v]) => (
        <span key={k} className="chip chip-muted mono" title="Runs only on workers with this label">
          <MapPin size={12} />
          {k}={v}
        </span>
      ))}
    </div>
  )
}
