import type { ReactNode } from 'react'

// marked shows text with the ranges a match found marked.
export function marked(text: string, ranges: [number, number][]) {
  if (!ranges.length) return text
  const out: ReactNode[] = []
  let at = 0
  for (const [from, to] of ranges) {
    if (from > at) out.push(text.slice(at, from))
    out.push(<mark key={from}>{text.slice(from, to)}</mark>)
    at = to
  }
  out.push(text.slice(at))
  return out
}

// match finds what was typed in a name, ignoring case: whole, as
// a run of characters (best at the start of a part of the name, after a
// slash, dash, underscore or dot), else its characters in order. Nothing
// typed matches every name.
export function match(name: string, query: string): { score: number; ranges: [number, number][] } | null {
  if (!query) return { score: 0, ranges: [] }
  const n = name.toLowerCase()
  const q = query.toLowerCase()
  let best = -1
  for (let i = n.indexOf(q); i >= 0; i = n.indexOf(q, i + 1)) {
    if (i === 0 || '/-_.'.includes(n[i - 1])) {
      best = i
      break
    }
    if (best < 0) best = i
  }
  if (best >= 0) {
    const atPart = best === 0 || '/-_.'.includes(n[best - 1])
    return { score: (atPart ? 3000 : 2000) - name.length, ranges: [[best, best + q.length]] }
  }
  const ranges: [number, number][] = []
  let j = 0
  for (let i = 0; i < n.length && j < q.length; i++) {
    if (n[i] !== q[j]) continue
    const last = ranges[ranges.length - 1]
    if (last && last[1] === i) last[1] = i + 1
    else ranges.push([i, i + 1])
    j++
  }
  if (j < q.length) return null
  return { score: 1000 - ranges.length * 10 - name.length, ranges }
}
