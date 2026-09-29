import { useQuery } from '@tanstack/react-query'

import { api, type Me, type Spec, type Team, type Template } from './api'

export const templatesKey = ['templates'] as const

export function useTemplates() {
  return useQuery({ queryKey: templatesKey, queryFn: api.templates, refetchInterval: 15_000 })
}

// A name becomes the environment's hostname, in lower case, whatever else a
// template asks; its own case is kept, for branch names made from it.
export const envNamePattern = /^[A-Za-z0-9]([A-Za-z0-9-]{0,61}[A-Za-z0-9])?$/

// checkName says what is wrong with a name for a template, or nothing. The
// server decides; this only lets the form say so before it is asked. A
// pattern JavaScript cannot compile is left to the server, whose regular
// expressions differ in small ways.
export function checkName(name: string, t: Template): string | null {
  if (name === '') return null
  if (!envNamePattern.test(name)) return 'Letters, digits and hyphens only, not at either end.'
  if (t.name_pattern) {
    const re = jsPattern(t.name_pattern)
    if (!re) return null
    if (!re.test(name)) return t.name_hint || `Must match ${t.name_pattern}`
  }
  return null
}

// The variables every template may use in its repositories and editor
// folder, beside its name pattern's named groups.
export const builtinVariables = ['name', 'owner', 'short_id', 'template']

// jsPattern is a template's name pattern as JavaScript writes it: Go names a
// group (?P<name>...), JavaScript (?<name>...).
function jsPattern(pattern: string): RegExp | null {
  try {
    return new RegExp(`^(?:${pattern.replaceAll('(?P<', '(?<')})$`)
  } catch {
    return null
  }
}

// patternGroups are a name pattern's named groups, which a template may use
// as variables.
export function patternGroups(pattern: string | undefined): string[] {
  if (!pattern) return []
  return [...pattern.matchAll(/\(\?P?<([A-Za-z_][A-Za-z0-9_]*)>/g)].map((m) => m[1])
}

// templateVars are the values an environment of a template would fill in,
// as far as they are known before it is made: its short ID is not.
export function templateVars(t: Template, name: string, owner: string): Record<string, string | undefined> {
  const vars: Record<string, string | undefined> = { name: name || undefined, owner, short_id: undefined, template: t.name }
  const groups = patternGroups(t.name_pattern)
  const m = name && t.name_pattern ? jsPattern(t.name_pattern)?.exec(name) : null
  for (const g of groups) vars[g] = m ? (m.groups?.[g] ?? '') : undefined
  return vars
}

// fillTemplate fills known values into a template field, leaving any not yet
// known as its {placeholder}, as the server fills them when the environment
// is made.
export function fillTemplate(s: string | undefined, vars: Record<string, string | undefined>): string | undefined {
  if (!s) return s
  return s.replace(/\{([A-Za-z_][A-Za-z0-9_]*)(?::(lower|upper))?\}/g, (p, v: string, mod?: string) => {
    const val = vars[v]
    if (val === undefined) return p
    return mod === 'lower' ? val.toLowerCase() : mod === 'upper' ? val.toUpperCase() : val
  })
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
