import { useMutation, useQueryClient } from '@tanstack/react-query'
import { useState, type FormEvent } from 'react'
import { Link, useNavigate } from 'react-router'

import { api, type CreateEnvironment } from '../api'
import { PageHeader } from '../components/PageHeader'
import { environmentsKey } from '../environments'

const defaults: CreateEnvironment = {
  name: '',
  image: 'ghcr.io/csnewman/hangar/base-ubuntu2604:latest',
  cpus: 2,
  memory_mib: 4096,
}

export function NewEnvironmentPage() {
  const qc = useQueryClient()
  const navigate = useNavigate()
  const [form, setForm] = useState<CreateEnvironment>(defaults)
  const create = useMutation({
    mutationFn: api.createEnvironment,
    onSuccess: (env) => {
      qc.invalidateQueries({ queryKey: environmentsKey })
      navigate(`/environments/${env.id}`)
    },
  })

  const submit = (e: FormEvent) => {
    e.preventDefault()
    create.mutate(form)
  }

  return (
    <div className="page page-narrow">
      <PageHeader crumbs={[{ label: 'Environments', to: '/environments' }]} title="New environment" />
      <form className="panel form" onSubmit={submit}>
        <label className="field">
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
          <small>Lowercase letters, digits and hyphens. It becomes the environment's hostname.</small>
        </label>
        <label className="field">
          <span>Image</span>
          <input
            required
            className="mono"
            value={form.image}
            onChange={(e) => setForm({ ...form, image: e.target.value })}
          />
          <small>Any OCI image built on a Hangar base.</small>
        </label>
        <div className="field-row">
          <label className="field">
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
          <label className="field">
            <span>Memory (MiB)</span>
            <input
              type="number"
              min={512}
              max={262144}
              step={512}
              required
              value={form.memory_mib}
              onChange={(e) => setForm({ ...form, memory_mib: Number(e.target.value) })}
            />
          </label>
        </div>
        {create.error && <div className="alert">{create.error.message}</div>}
        <div className="form-actions">
          <Link to="/environments" className="btn btn-ghost">
            Cancel
          </Link>
          <button type="submit" className="btn btn-primary" disabled={create.isPending}>
            {create.isPending ? 'Creating…' : 'Create environment'}
          </button>
        </div>
      </form>
    </div>
  )
}
