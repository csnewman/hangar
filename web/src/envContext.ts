import { createContext } from 'react'

import type { Environment } from './api'

// EnvContext is the environment a page is about, for views shown outside
// its routes' outlet: the workspace shows its sections as tabs and tool
// windows of its own rather than as routes.
export const EnvContext = createContext<Environment | null>(null)
