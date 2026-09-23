export function formatMemory(mib: number): string {
  if (mib >= 1024) {
    const gib = mib / 1024
    return `${Number.isInteger(gib) ? gib : gib.toFixed(1)} GiB`
  }
  return `${mib} MiB`
}

export function formatAgo(iso: string | undefined, now = Date.now()): string {
  if (!iso) return 'never'
  const s = Math.max(0, Math.round((now - new Date(iso).getTime()) / 1000))
  if (s < 5) return 'just now'
  if (s < 60) return `${s}s ago`
  const m = Math.round(s / 60)
  if (m < 60) return `${m}m ago`
  const h = Math.round(m / 60)
  if (h < 48) return `${h}h ago`
  return `${Math.round(h / 24)}d ago`
}

const units = ['B', 'KB', 'MB', 'GB', 'TB']

// formatBytes shows a byte count in the largest unit that keeps it above 1.
export function formatBytes(n: number): string {
  let i = 0
  let v = n
  while (v >= 1000 && i < units.length - 1) {
    v /= 1000
    i++
  }
  return `${v >= 100 || i === 0 ? Math.round(v) : v.toFixed(1)} ${units[i]}`
}

export function formatRate(bytesPerSecond: number): string {
  return `${formatBytes(bytesPerSecond)}/s`
}

export function formatPercent(p: number): string {
  return `${p >= 10 ? Math.round(p) : p.toFixed(1)}%`
}
