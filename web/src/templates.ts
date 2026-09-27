import { useQuery } from '@tanstack/react-query'

import { api, type Me, type Spec, type Team, type Template } from './api'

export const templatesKey = ['templates'] as const

export function useTemplates() {
  return useQuery({ queryKey: templatesKey, queryFn: api.templates, refetchInterval: 15_000 })
}

// A name becomes the environment's hostname, whatever else a template asks.
export const envNamePattern = /^[a-z0-9]([a-z0-9-]{0,61}[a-z0-9])?$/

// checkName says what is wrong with a name for a template, or nothing. The
// server decides; this only lets the form say so before it is asked. A
// pattern JavaScript cannot compile is left to the server, whose regular
// expressions differ in small ways.
export function checkName(name: string, t: Template): string | null {
  if (name === '') return null
  if (!envNamePattern.test(name)) return 'Lowercase letters, digits and hyphens only.'
  if (t.name_pattern) {
    let re: RegExp
    try {
      re = new RegExp(`^(?:${t.name_pattern})$`)
    } catch {
      return null
    }
    if (!re.test(name)) return t.name_hint || `Must match ${t.name_pattern}`
  }
  return null
}

// branchFor fills an environment's name into a template's branch pattern.
export function branchFor(pattern: string | undefined, name: string): string | undefined {
  return pattern ? pattern.replaceAll('{name}', name || '{name}') : undefined
}

export interface TemplateGroups {
  mine: Template[]
  // teams are the templates of each team the caller is in, in the order
  // the teams are given.
  teams: { team: Team; templates: Template[] }[]
  collaborating: Template[]
  others: Template[]
}

// groupTemplates splits templates by the caller's relationship to them: their
// own, their teams', ones they collaborate on -- themselves or through a
// team -- and everything else they can see.
export function groupTemplates(list: Template[], me: Me, teams: Team[] = []): TemplateGroups {
  const mineTeams = teams.filter((tm) => tm.role)
  const inTeam = new Set(mineTeams.map((tm) => tm.id))
  const byTeam = new Map<string, Template[]>(mineTeams.map((tm) => [tm.id, []]))
  const groups: TemplateGroups = { mine: [], teams: [], collaborating: [], others: [] }
  for (const t of list) {
    if (t.team && byTeam.has(t.team.id)) byTeam.get(t.team.id)!.push(t)
    else if (!t.team && t.owner.id === me.id) groups.mine.push(t)
    else if (t.collaborators.some((c) => c.id === me.id) || t.collaborator_teams.some((c) => inTeam.has(c.id)))
      groups.collaborating.push(t)
    else groups.others.push(t)
  }
  groups.teams = mineTeams.map((team) => ({ team, templates: byTeam.get(team.id)! }))
  return groups
}

// ownerLabel says whose a template is, as the caller sees it.
export function ownerLabel(t: Template, me: Me): string {
  if (t.team) return `team ${t.team.name}`
  if (t.owner.id === me.id) return 'yours'
  return `by ${t.owner.display_name || t.owner.username}`
}

// Hangar's own images, which the template editor suggests.
export const hangarImages = [
  { ref: 'ghcr.io/csnewman/hangar/base:ubuntu-26.04', label: 'Ubuntu 26.04, with desktop' },
  { ref: 'ghcr.io/csnewman/hangar/minimal:ubuntu-26.04', label: 'Ubuntu 26.04, minimal' },
  { ref: 'ghcr.io/csnewman/hangar/base:rocky-10', label: 'Rocky Linux 10, with desktop' },
  { ref: 'ghcr.io/csnewman/hangar/minimal:rocky-10', label: 'Rocky Linux 10, minimal' },
]

export const blankSpec: Spec = {
  image: 'ghcr.io/csnewman/hangar/base:ubuntu-26.04',
  cpus: 2,
  memory_mib: 4096,
  display: 'desktop',
  gpu: 'none',
  repos: [],
  placement: {},
}

// repoName is the short form of a repository URL: its owner and name.
export function repoName(url: string): string {
  const tail = url.replace(/\.git$/, '').split(/[/:]/).filter(Boolean)
  return tail.slice(-2).join('/') || url
}
