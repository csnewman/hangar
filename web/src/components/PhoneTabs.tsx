import { Ellipsis, LayoutGrid, UserRound } from 'lucide-react'
import { NavLink, useLocation } from 'react-router'

// PhoneTabs is the phone's bar at the foot of the screen, where a desktop
// has the sidebar: the environments, the user's account, and everything
// else, which stays chosen on the pages it leads to.
export function PhoneTabs() {
  const { pathname } = useLocation()
  const more = ['/templates', '/images', '/teams', '/files', '/admin'].some((p) => pathname.startsWith(p))
  return (
    <nav className="phone-tabs" aria-label="Main">
      <NavLink to="/environments" className="phone-tab">
        <LayoutGrid size={22} />
        <span>Environments</span>
      </NavLink>
      <NavLink to="/account" className="phone-tab">
        <UserRound size={22} />
        <span>Account</span>
      </NavLink>
      <NavLink to="/more" className={({ isActive }) => (isActive || more ? 'phone-tab active' : 'phone-tab')}>
        <Ellipsis size={22} />
        <span>More</span>
      </NavLink>
    </nav>
  )
}
