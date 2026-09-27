import { useQuery } from '@tanstack/react-query'

import { api, type Team, type TeamRole } from './api'

export const teamsKey = ['teams'] as const

export function useTeams() {
  return useQuery({ queryKey: teamsKey, queryFn: api.teams, refetchInterval: 30_000 })
}

export const roleLabels: Record<TeamRole, string> = {
  viewer: 'Viewer',
  member: 'Member',
  admin: 'Admin',
}

export const roleHints: Record<TeamRole, string> = {
  viewer: 'Sees and uses what the team owns',
  member: 'Also changes it',
  admin: 'Also runs the team and deletes what it owns',
}

// canGive is whether the caller may give a team a template: as one of its
// members or admins, or an administrator.
export function canGive(t: Team, admin: boolean): boolean {
  return admin || t.role === 'member' || t.role === 'admin'
}
