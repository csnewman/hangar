import { useMutation, useQueryClient } from '@tanstack/react-query'
import { useEffect, useState } from 'react'

import { api } from '../api'
import { meKey, useMe } from '../session'
import { onThemeChoice, setTheme, type Theme } from '../theme'

// The user's access tokens, as the account's pages list them.
export const tokensKey = ['tokens']

// useThemeChoice is the person's theme, and a way to choose another.
export function useThemeChoice() {
  const me = useMe()
  const qc = useQueryClient()
  const [theme, setChosen] = useState<Theme>(me.theme)
  // Chosen in another tab, it shows here as chosen too.
  useEffect(() => onThemeChoice(setChosen), [])
  const save = useMutation({
    mutationFn: (t: Theme) => api.setPreferences({ theme: t }),
    onSuccess: () => qc.invalidateQueries({ queryKey: meKey }),
  })
  const choose = (t: Theme) => {
    setChosen(t)
    setTheme(t)
    save.mutate(t)
  }
  return [theme, choose, save] as const
}
