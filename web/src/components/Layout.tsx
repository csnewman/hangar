import { NavLink, Outlet } from 'react-router'

export function Layout() {
  return (
    <div className="shell">
      <header className="topbar">
        <div className="brand">
          <img src="/favicon.svg" alt="" width={22} height={22} />
          <span>Hangar</span>
        </div>
        <nav className="nav">
          <NavLink to="/environments">Environments</NavLink>
          <NavLink to="/workers">Workers</NavLink>
        </nav>
      </header>
      <main className="content">
        <Outlet />
      </main>
    </div>
  )
}
