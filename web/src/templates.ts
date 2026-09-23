import { useQuery } from '@tanstack/react-query'

import { api, type Me, type Spec, type Template } from './api'

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
  collaborating: Template[]
  others: Template[]
}

// groupTemplates splits templates by the caller's relationship to them: their
// own, ones they collaborate on, and everything else they can see.
export function groupTemplates(list: Template[], me: Me): TemplateGroups {
  const groups: TemplateGroups = { mine: [], collaborating: [], others: [] }
  for (const t of list) {
    if (t.owner.id === me.id) groups.mine.push(t)
    else if (t.collaborators.some((c) => c.id === me.id)) groups.collaborating.push(t)
    else groups.others.push(t)
  }
  return groups
}

export const blankSpec: Spec = {
  image: 'ghcr.io/csnewman/hangar/base-ubuntu2604:latest',
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
