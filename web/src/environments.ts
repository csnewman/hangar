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

// changeable is whether an environment's settings may change: stopped, or
// not yet placed, so there is no machine holding the ones it has.
export function changeable(env: Environment): boolean {
  if (env.phase === 'suspended' || env.desired === 'suspended' || env.phase === 'stopping') return false
  return env.desired === 'stopped' || (!env.worker_id && env.phase === 'pending')
}

// webURL is where an environment's web server called name is reached, or
// its plain one with no name, over the same scheme as Hangar itself.
export function webURL(name: string, short: string, suffix: string): string {
  return `${window.location.protocol}//${name ? `${name}-` : ''}env${short}${suffix}/`
}

// A DNS label is at most this long, and the name before -env<short> shares
// one with it.
const maxLabel = 63

// longestWebName is how long a name before an environment's web host may
// be. In the prefix style the machine's own name shares the label too.
export function longestWebName(short: string, suffix: string): number {
  const sharing = suffix.startsWith('-') ? suffix.split('.')[0].length : 0
  return maxLabel - `-env${short}`.length - sharing
}

// sshCommand reaches the environment through the SSH gateway: by its name
// alone for its owner, and by owner/name for anyone else who can see it.
export function sshCommand(gw: { host: string; port: number }, env: Environment, own: boolean) {
  const user = own ? env.name : `${env.owner}/${env.name}`
  return `ssh ${user}@${gw.host}` + (gw.port === 22 ? '' : ` -p ${gw.port}`)
}

// settingsSummary is an environment's settings in a line.
export function settingsSummary(env: Environment): string {
  const s = env.spec
  return (
    `${s.cpus} vCPU${s.cpus === 1 ? '' : 's'}, ${s.memory_mib} MiB, ` +
    `${s.display === 'desktop' ? 'desktop' : 'headless'}, ${s.gpu === 'none' ? 'no GPU' : `${s.gpu} GPU`}` +
    (s.dax ? ', image files mapped from the host' : '')
  )
}

// memorySizes are the sizes of memory offered for a machine, in MiB; an
// environment's or template's own is offered besides, if it is none of them.
export const memorySizes = [1, 2, 3, 4, 6, 8, 12, 16, 24, 32, 48, 64].map((g) => g * 1024)

// withSize is memorySizes with mib among them.
export const withSize = (mib: number) =>
  memorySizes.includes(mib) ? memorySizes : [...memorySizes, mib].sort((a, b) => a - b)
