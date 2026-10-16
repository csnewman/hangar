import { createContext } from 'react'

// Maximise is the bottom panel's way to take the whole workspace, and to
// give it back, offered to the tool windows in it.
export const Maximise = createContext<{ maximised: boolean; toggle: () => void } | null>(null)
