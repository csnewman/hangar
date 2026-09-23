import { MutationCache, QueryCache, QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { StrictMode } from 'react'
import { createRoot } from 'react-dom/client'
import { createBrowserRouter, Navigate, RouterProvider } from 'react-router'

import { ApiError } from './api'
import { RequireAdmin, RequireAuth } from './auth'
import { meKey, onSessionChange } from './session'
import { Shell } from './components/Shell'
import './index.css'
import { AccountPage } from './pages/Account'
import { UsersPage } from './pages/admin/Users'
import { WorkersPage } from './pages/admin/Workers'
import { ConsoleTab, EnvironmentPage, SummaryTab } from './pages/Environment'
import { LoginPage } from './pages/Login'
import { MetricsTab } from './pages/Metrics'
import { WorkerDetailPage } from './pages/admin/WorkerDetail'
import { NewEnvironmentPage } from './pages/NewEnvironment'
import { OverviewPage } from './pages/Overview'
import { TemplateEditorPage } from './pages/TemplateEditor'
import { TemplatesPage } from './pages/Templates'

// A 401 from anything means the session has gone -- expired, signed out
// elsewhere, or the user disabled. Refetching the current user then fails
// too, and RequireAuth sends the browser to the login page.
const signedOut = (error: Error) => {
  if (error instanceof ApiError && error.status === 401) {
    queryClient.invalidateQueries({ queryKey: meKey })
  }
}

const queryClient: QueryClient = new QueryClient({
  queryCache: new QueryCache({
    onError: (error, query) => {
      if (query.queryKey[0] !== meKey[0]) signedOut(error)
    },
  }),
  mutationCache: new MutationCache({ onError: signedOut }),
  defaultOptions: {
    queries: {
      // Environments move through their phases on their own, so every view
      // polls rather than waiting to be told.
      refetchInterval: 2000,
      retry: (count, error) => !(error instanceof ApiError && error.status < 500) && count < 2,
    },
  },
})

// Another tab signed in or out. Whatever this tab has cached belonged to the
// session it had, so all of it is reset, and what is on screen -- including
// who is signed in -- is fetched again.
onSessionChange(() => {
  queryClient.resetQueries()
})

const router = createBrowserRouter([
  { path: '/login', element: <LoginPage /> },
  {
    element: <RequireAuth />,
    children: [
      {
        element: <Shell />,
        children: [
          { index: true, element: <Navigate to="/environments" replace /> },
          { path: 'environments', element: <OverviewPage /> },
          { path: 'environments/new', element: <NewEnvironmentPage /> },
          {
            path: 'environments/:id',
            element: <EnvironmentPage />,
            children: [
              { index: true, element: <SummaryTab /> },
              { path: 'metrics', element: <MetricsTab /> },
              {
                path: 'terminal',
                // The terminal emulator is most of the UI's weight, so it is
                // fetched when a terminal is first opened, not with the page.
                lazy: async () => ({ Component: (await import('./terminal/TerminalTab')).TerminalTab }),
              },
              { path: 'editor', element: <ConsoleTab kind="editor" /> },
              { path: 'desktop', element: <ConsoleTab kind="desktop" /> },
            ],
          },
          { path: 'templates', element: <TemplatesPage /> },
          { path: 'templates/new', element: <TemplateEditorPage /> },
          { path: 'templates/:id', element: <TemplateEditorPage /> },
          { path: 'account', element: <AccountPage /> },
          {
            path: 'admin/workers',
            element: (
              <RequireAdmin>
                <WorkersPage />
              </RequireAdmin>
            ),
          },
          {
            path: 'admin/workers/:id',
            element: (
              <RequireAdmin>
                <WorkerDetailPage />
              </RequireAdmin>
            ),
          },
          {
            path: 'admin/users',
            element: (
              <RequireAdmin>
                <UsersPage />
              </RequireAdmin>
            ),
          },
          { path: '*', element: <Navigate to="/environments" replace /> },
        ],
      },
    ],
  },
])

createRoot(document.getElementById('root')!).render(
  <StrictMode>
    <QueryClientProvider client={queryClient}>
      <RouterProvider router={router} />
    </QueryClientProvider>
  </StrictMode>,
)
