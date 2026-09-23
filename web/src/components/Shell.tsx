import { Outlet } from 'react-router'

import { Logo } from './Logo'
import { Sidebar } from './Sidebar'
import { UserMenu } from './UserMenu'

// Shell is the frame every signed-in page sits in: a bar across the top, the
// inventory down the left, and the page beside it.
export function Shell() {
  return (
    <div className="app">
      <header className="topbar">
        <div className="brand">
          <Logo />
          <span>Hangar</span>
        </div>
        <div className="topbar-spacer" />
        <UserMenu />
      </header>
      <div className="app-body">
        <Sidebar />
        <main className="main">
          <Outlet />
        </main>
      </div>
    </div>
  )
}
