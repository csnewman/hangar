import { Plus } from 'lucide-react'
import { Link } from 'react-router'

import type { Environment } from '../api'
import { useMe } from '../session'
import { EnvironmentActions } from '../components/EnvironmentActions'
import { formatAgo, formatMemory } from '../components/format'
import { PageHeader } from '../components/PageHeader'
import { PhaseBadge } from '../components/Status'
import { splitByOwner, useEnvironments } from '../environments'

export function OverviewPage() {
  const me = useMe()
  const envs = useEnvironments()
  const { mine, others } = splitByOwner(envs.data ?? [], me)
  const otherEnvs = others.flatMap((g) => g.environments)

  return (
    <div className="page">
      <PageHeader
        title="Environments"
        subtitle="Isolated microVMs, each with a workspace, Docker, an editor and a desktop."
        actions={
          <Link to="/environments/new" className="btn btn-primary">
            <Plus size={15} />
            New environment
          </Link>
        }
      />

      {envs.isError && <div className="alert">Could not load environments: {envs.error.message}</div>}

      <section className="section">
        {me.admin && <h2 className="section-title">Yours</h2>}
        <EnvironmentTable
          environments={mine}
          loading={envs.isPending}
          empty={
            <>
              You have no environments. <Link to="/environments/new">Create one</Link>.
            </>
          }
        />
      </section>

      {me.admin && (
        <section className="section">
          <h2 className="section-title">
            Other users <span className="section-count">{otherEnvs.length}</span>
          </h2>
          <EnvironmentTable
            environments={otherEnvs}
            loading={envs.isPending}
            showOwner
            empty="No other user has an environment."
          />
        </section>
      )}
    </div>
  )
}

function EnvironmentTable({
  environments,
  loading,
  showOwner = false,
  empty,
}: {
  environments: Environment[]
  loading: boolean
  showOwner?: boolean
  empty: React.ReactNode
}) {
  const cols = showOwner ? 7 : 6
  return (
    <div className="panel">
      <table className="table">
        <thead>
          <tr>
            <th>Name</th>
            {showOwner && <th>Owner</th>}
            <th>Status</th>
            <th className="num">Size</th>
            <th>Worker</th>
            <th>Updated</th>
            <th />
          </tr>
        </thead>
        <tbody>
          {environments.map((e) => (
            <tr key={e.id}>
              <td>
                <Link to={`/environments/${e.id}`} className="strong row-link">
                  {e.name}
                </Link>
                <div className="muted small">{e.template}</div>
              </td>
              {showOwner && <td>{e.owner}</td>}
              <td>
                <PhaseBadge phase={e.phase} desired={e.desired} />
                {e.reason && <div className="reason">{e.reason}</div>}
              </td>
              <td className="num nowrap">
                {e.cpus} vCPU · {formatMemory(e.memory_mib)}
              </td>
              <td>{e.worker ?? <span className="muted">unplaced</span>}</td>
              <td className="muted nowrap">{formatAgo(e.updated_at)}</td>
              <td>
                <EnvironmentActions env={e} />
              </td>
            </tr>
          ))}
          {!loading && environments.length === 0 && (
            <tr>
              <td colSpan={cols} className="empty">
                {empty}
              </td>
            </tr>
          )}
          {loading && (
            <tr>
              <td colSpan={cols} className="empty">
                Loading…
              </td>
            </tr>
          )}
        </tbody>
      </table>
    </div>
  )
}
