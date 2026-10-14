import type { ReactNode } from 'react'
import { Link, useLocation } from 'react-router'

import { usePhone } from '../phone'
import { useTitle } from '../title'
import { PhoneHead } from './Phone'

export interface Crumb {
  label: string
  to?: string
}

// PageHeader heads a page: where it is, its title and what it is, and what
// can be done to it. On a phone the way back is the nearest crumb that leads
// somewhere, or the More tab that pages outside the environments are reached
// from, and the actions sit under the title.
export function PageHeader({
  crumbs,
  title,
  subtitle,
  actions,
}: {
  crumbs?: Crumb[]
  title: ReactNode
  subtitle?: ReactNode
  actions?: ReactNode
}) {
  useTitle(typeof title === 'string' && title)
  const phone = usePhone()
  const { pathname } = useLocation()
  if (phone) {
    const up = crumbs?.filter((c) => c.to).at(-1)
    const back = up?.to
      ? { to: up.to, label: up.label }
      : pathname.startsWith('/environments')
        ? undefined
        : { to: '/more', label: 'More' }
    return (
      <PhoneHead back={back} title={title}>
        {subtitle && <div className="phone-note">{subtitle}</div>}
        {actions && <div className="page-actions phone-page-actions">{actions}</div>}
      </PhoneHead>
    )
  }
  return (
    <div className="page-head">
      <div className="page-title">
        {crumbs && crumbs.length > 0 && (
          <div className="crumbs">
            {crumbs.map((c, i) => (
              <span key={i}>
                {c.to ? <Link to={c.to}>{c.label}</Link> : c.label}
                <span className="crumb-sep">/</span>
              </span>
            ))}
          </div>
        )}
        <h1>{title}</h1>
        {subtitle && <div className="page-subtitle">{subtitle}</div>}
      </div>
      {actions && <div className="page-actions">{actions}</div>}
    </div>
  )
}
