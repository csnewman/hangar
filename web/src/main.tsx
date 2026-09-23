import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { StrictMode } from 'react'
import { createRoot } from 'react-dom/client'
import { createBrowserRouter, Navigate, RouterProvider } from 'react-router'

import { Layout } from './components/Layout'
import './index.css'
import { EnvironmentsPage } from './pages/Environments'
import { WorkersPage } from './pages/Workers'

const queryClient = new QueryClient({
  defaultOptions: {
    queries: {
      // Environments move through their phases on their own, so every view
      // polls rather than waiting to be told.
      refetchInterval: 2000,
      retry: 1,
    },
  },
})

const router = createBrowserRouter([
  {
    element: <Layout />,
    children: [
      { index: true, element: <Navigate to="/environments" replace /> },
      { path: 'environments', element: <EnvironmentsPage /> },
      { path: 'workers', element: <WorkersPage /> },
      { path: '*', element: <Navigate to="/environments" replace /> },
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
