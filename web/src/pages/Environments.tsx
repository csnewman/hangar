import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { useState, type FormEvent } from 'react'

import { api, type CreateEnvironment, type Environment } from '../api'
import { ConfirmButton } from '../components/ConfirmButton'
import { formatAgo, formatMemory } from '../components/format'
import { PhaseBadge } from '../components/Status'

export function EnvironmentsPage() {
  const [creating, setCreating] = useState(false)
  const envs = useQuery({ queryKey: ['environments'], queryFn: api.environments })

  return (
    <section>
      <div className="page-head">
        <div>
          <h1>Environments</h1>
          <p className="muted">Isolated microVMs, each with a workspace, Docker, an IDE and a desktop.</p>
        </div>
        {!creating && (
          <button type="button" className="btn btn-primary" onClick={() => setCreating(true)}>
            New environment
          </button>
        )}
      </div>

      {creating && <CreateForm onDone={() => setCreating(false)} />}

      {envs.isError && <div className="alert">Could not load environments: {envs.error.message}</div>}

      <div className="panel">
        <table className="table">
          <thead>
            <tr>
              <th>Name</th>
              <th>Status</th>
              <th>Image</th>
              <th className="num">Size</th>
              <th>Worker</th>
              <th>Updated</th>
              <th />
            </tr>
          </thead>
          <tbody>
            {envs.data?.map((e) => <EnvironmentRow key={e.id} env={e} />)}
            {envs.data?.length === 0 && (
              <tr>
                <td colSpan={7} className="empty">
                  No environments yet.
                </td>
              </tr>
            )}
            {envs.isPending && (
              <tr>
                <td colSpan={7} className="empty">
                  Loading…
                </td>
              </tr>
            )}
          </tbody>
        </table>
      </div>
    </section>
  )
}

function EnvironmentRow({ env }: { env: Environment }) {
  const qc = useQueryClient()
  const refresh = () => qc.invalidateQueries({ queryKey: ['environments'] })
  const start = useMutation({ mutationFn: () => api.startEnvironment(env.id), onSettled: refresh })
  const stop = useMutation({ mutationFn: () => api.stopEnvironment(env.id), onSettled: refresh })
  const remove = useMutation({ mutationFn: () => api.deleteEnvironment(env.id), onSettled: refresh })
  const busy = start.isPending || stop.isPending || remove.isPending
  const deleting = env.desired === 'deleted'
  const error = start.error ?? stop.error ?? remove.error

  return (
    <tr>
      <td>
        <div className="strong">{env.name}</div>
        <div className="mono muted small">{env.id.slice(0, 8)}</div>
      </td>
      <td>
        <PhaseBadge phase={env.phase} desired={env.desired} />
        {env.reason && <div className="reason">{env.reason}</div>}
        {error && <div className="reason reason-bad">{error.message}</div>}
      </td>
      <td className="mono small">{env.image}</td>
      <td className="num nowrap">
        {env.cpus} vCPU · {formatMemory(env.memory_mib)}
      </td>
      <td>{env.worker ?? <span className="muted">unplaced</span>}</td>
      <td className="muted nowrap">{formatAgo(env.updated_at)}</td>
      <td>
        <div className="actions">
          {env.desired === 'running' ? (
            <button type="button" className="btn btn-ghost" disabled={busy} onClick={() => stop.mutate()}>
              Stop
            </button>
          ) : (
            <button
              type="button"
              className="btn btn-ghost"
              disabled={busy || deleting}
              onClick={() => start.mutate()}
            >
              Start
            </button>
          )}
          <ConfirmButton label="Delete" confirmLabel="Delete?" disabled={busy || deleting} onConfirm={() => remove.mutate()} />
        </div>
      </td>
    </tr>
  )
}

const defaults: CreateEnvironment = {
  name: '',
  image: 'ghcr.io/csnewman/hangar/base-ubuntu2604:latest',
  cpus: 2,
  memory_mib: 4096,
}

function CreateForm({ onDone }: { onDone: () => void }) {
  const qc = useQueryClient()
  const [form, setForm] = useState<CreateEnvironment>(defaults)
  const create = useMutation({
    mutationFn: api.createEnvironment,
    onSuccess: () => {
      qc.invalidateQueries({ queryKey: ['environments'] })
      onDone()
    },
  })

  const submit = (e: FormEvent) => {
    e.preventDefault()
    create.mutate(form)
  }

  return (
    <form className="panel form" onSubmit={submit}>
      <div className="form-grid">
        <label>
          <span>Name</span>
          <input
            autoFocus
            required
            pattern="[a-z0-9]([a-z0-9\-]{0,61}[a-z0-9])?"
            title="Lowercase letters, digits and hyphens"
            placeholder="my-agent"
            value={form.name}
            onChange={(e) => setForm({ ...form, name: e.target.value })}
          />
        </label>
        <label className="wide">
          <span>Image</span>
          <input
            required
            className="mono"
            value={form.image}
            onChange={(e) => setForm({ ...form, image: e.target.value })}
          />
        </label>
        <label>
          <span>vCPUs</span>
          <input
            type="number"
            min={1}
            max={64}
            required
            value={form.cpus}
            onChange={(e) => setForm({ ...form, cpus: Number(e.target.value) })}
          />
        </label>
        <label>
          <span>Memory (MiB)</span>
          <input
            type="number"
            min={512}
            step={512}
            required
            value={form.memory_mib}
            onChange={(e) => setForm({ ...form, memory_mib: Number(e.target.value) })}
          />
        </label>
      </div>
      {create.error && <div className="alert">{create.error.message}</div>}
      <div className="form-actions">
        <button type="button" className="btn btn-ghost" onClick={onDone}>
          Cancel
        </button>
        <button type="submit" className="btn btn-primary" disabled={create.isPending}>
          {create.isPending ? 'Creating…' : 'Create'}
        </button>
      </div>
    </form>
  )
}
