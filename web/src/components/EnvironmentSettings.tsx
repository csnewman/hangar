import { useMutation, useQueryClient } from '@tanstack/react-query'
import { useState } from 'react'

import { api, type Environment, type EnvironmentSettings, type TemplateSetting } from '../api'
import { changeable, environmentsKey } from '../environments'
import { MemoryField } from './MemoryField'

const settingNames: Record<TemplateSetting, string> = {
  image: 'image',
  cpus: 'vCPUs',
  memory: 'memory',
  display: 'display',
  gpu: 'GPU',
  dax: 'image file mapping',
  repos: 'repositories',
  editor_path: 'editor folder',
  access: 'access',
  trusted_folders: 'trusted folders',
  packs: 'packs',
  web_names: 'web server names',
  placement: 'placement',
  name: 'accepted names',
}

// TemplateDrift says how an environment differs from its template's
// current settings, and offers to reset it to the template, keeping its
// disks.
export function TemplateDrift({ env }: { env: Environment }) {
  const qc = useQueryClient()
  const reset = useMutation({
    mutationFn: () => api.resetEnvironmentToTemplate(env.id),
    onSettled: () => qc.invalidateQueries({ queryKey: environmentsKey }),
  })
  const changes = env.template_changes ?? []
  if (changes.length === 0 || !env.template_id) return null
  const names = changes.map((c) => settingNames[c]).join(', ')
  const ok = changeable(env)
  return (
    <div className="notice drift">
      <div>
        {env.template_updated ? (
          <>
            Its template, <strong>{env.template}</strong>, has changed since this environment was made. They differ in:{' '}
            {names}.
          </>
        ) : (
          <>
            This environment has been changed from its template, <strong>{env.template}</strong>: {names}.
          </>
        )}
      </div>
      <div className="drift-actions">
        <button
          type="button"
          className="btn btn-ghost"
          disabled={!ok || reset.isPending}
          title={ok ? 'Take the template’s settings, keeping this environment’s disks' : 'Stop the environment first'}
          onClick={() => reset.mutate()}
        >
          Reset to template
        </button>
        {!ok && <span className="muted small">Stop it to reset.</span>}
      </div>
      {reset.error && <div className="alert">{reset.error.message}</div>}
    </div>
  )
}

// SettingsForm changes an environment's settings, which it may only while
// it is stopped. onCancel, if given, is offered as Cancel.
export function SettingsForm({
  env,
  onDone,
  onCancel,
  disabled = false,
}: {
  env: Environment
  onDone: () => void
  onCancel?: () => void
  disabled?: boolean
}) {
  const qc = useQueryClient()
  const [form, setForm] = useState<EnvironmentSettings>({
    cpus: env.spec.cpus,
    memory_mib: env.spec.memory_mib,
    display: env.spec.display,
    gpu: env.spec.gpu,
    dax: env.spec.dax ?? false,
  })
  const save = useMutation({
    mutationFn: (body: EnvironmentSettings) => api.updateEnvironmentSettings(env.id, body),
    onSuccess: () => {
      qc.invalidateQueries({ queryKey: environmentsKey })
      onDone()
    },
  })

  return (
    <form
      onSubmit={(e) => {
        e.preventDefault()
        save.mutate(form)
      }}
    >
      <fieldset disabled={disabled} className="form-stack">
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
            <span>Memory</span>
            <MemoryField mib={form.memory_mib} onChange={(memory_mib) => setForm({ ...form, memory_mib })} />
          </label>
        </div>
        <div className="field-row">
          <label className="field">
            <span>Display</span>
            <select
              value={form.display}
              onChange={(e) => setForm({ ...form, display: e.target.value as EnvironmentSettings['display'] })}
            >
              <option value="desktop">Desktop</option>
              <option value="none">Headless</option>
            </select>
          </label>
          <label className="field">
            <span>GPU</span>
            <select
              value={form.gpu}
              onChange={(e) => setForm({ ...form, gpu: e.target.value as EnvironmentSettings['gpu'] })}
            >
              <option value="none">None</option>
              <option value="virtual">Virtual</option>
              {env.spec.gpu === 'passthrough' && <option value="passthrough">Passthrough</option>}
            </select>
          </label>
        </div>
        <label className="check">
          <input
            type="checkbox"
            checked={form.dax ?? false}
            onChange={(e) => setForm({ ...form, dax: e.target.checked })}
          />
          <span>
            Map image files from the host (DAX)
            <small className="muted">
              Shares the image's files in memory with other environments. Programs that run from large files, such as a
              browser, can be many times slower with it.
            </small>
          </span>
        </label>
        {save.error && <div className="alert">{save.error.message}</div>}
        <div className="form-actions">
          {onCancel && (
            <button type="button" className="btn btn-ghost" onClick={onCancel}>
              Cancel
            </button>
          )}
          <button type="submit" className="btn btn-primary" disabled={save.isPending}>
            Save
          </button>
        </div>
      </fieldset>
    </form>
  )
}
