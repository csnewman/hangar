import type { ReactNode } from 'react'
import { Link } from 'react-router'

export interface Crumb {
  label: string
  to?: string
}

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
