// How many samples a history keeps: at one every five seconds, the last five
// minutes.
const keep = 60

export interface Sample<T> {
  at: number
  value: T
}

const histories = new Map<string, Sample<unknown>[]>()

// useHistory keeps the recent values of a measurement, as the page polls
// for it. The server holds only the latest figures, so the history is what
// this browser has seen: it starts empty and survives moving between pages.
//
// Recording is idempotent -- a value equal to the last one recorded adds
// nothing, since the worker measures less often than the page polls -- so it
// is safe to do while rendering, which happens on every poll anyway.
export function useHistory<T>(key: string, value: T | undefined | null): Sample<T>[] {
  const list = (histories.get(key) as Sample<T>[] | undefined) ?? []
  if (value === undefined || value === null) return list
  const last = list[list.length - 1]
  if (last && JSON.stringify(last.value) === JSON.stringify(value)) return list
  const next = [...list, { at: Date.now(), value }].slice(-keep)
  histories.set(key, next)
  return next
}
