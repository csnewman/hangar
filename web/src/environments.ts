import { useQuery } from '@tanstack/react-query'

import { api, type Environment, type Me } from './api'

export const environmentsKey = ['environments'] as const

// useEnvironments is every environment the signed-in user may reach. The
// sidebar and the pages share one query, so they cannot disagree.
//
// watching keeps it polling while the tab is hidden, for a view that charts
// what it sees: without it the chart would have a hole for every minute the
// tab spent in the background.
export function useEnvironments({ watching = false }: { watching?: boolean } = {}) {
  return useQuery({ queryKey: environmentsKey, queryFn: api.environments, refetchIntervalInBackground: watching })
}

export interface OwnerGroup {
  owner: string
  environments: Environment[]
}

// splitByOwner puts the user's own environments first and groups everyone
// else's by owner, which is how an administrator's views are laid out.
export function splitByOwner(envs: Environment[], me: Me): { mine: Environment[]; others: OwnerGroup[] } {
  const mine: Environment[] = []
  const byOwner = new Map<string, Environment[]>()
  for (const e of envs) {
    if (e.owner_id === me.id) {
      mine.push(e)
      continue
    }
    const list = byOwner.get(e.owner) ?? []
    list.push(e)
    byOwner.set(e.owner, list)
  }
  const byName = (a: Environment, b: Environment) => a.name.localeCompare(b.name)
  mine.sort(byName)
  const others = [...byOwner.entries()]
    .sort(([a], [b]) => a.localeCompare(b))
    .map(([owner, environments]) => ({ owner, environments: environments.sort(byName) }))
  return { mine, others }
}
