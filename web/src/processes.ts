import type { ProcessInfo } from './api'

// The orders a process list can be in, and how each compares two.
export type SortKey = 'cpu' | 'memory' | 'pid' | 'name'

export const sorts: Record<SortKey, (a: ProcessInfo, b: ProcessInfo) => number> = {
  cpu: (a, b) => b.cpu_percent - a.cpu_percent || b.rss_bytes - a.rss_bytes,
  memory: (a, b) => b.rss_bytes - a.rss_bytes,
  pid: (a, b) => a.pid - b.pid,
  name: (a, b) => a.name.localeCompare(b.name) || a.pid - b.pid,
}

// states names the state letters /proc gives.
export const states: Record<string, string> = {
  R: 'running',
  S: 'sleeping',
  D: 'waiting on I/O',
  Z: 'zombie',
  T: 'stopped',
  t: 'traced',
  I: 'idle',
}
