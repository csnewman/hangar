import { useQuery, useQueryClient, useMutation } from '@tanstack/react-query'
import { Plus } from 'lucide-react'
import { useState, type FormEvent } from 'react'

import { api } from '../api'
import { ConfirmButton } from '../components/ConfirmButton'
import { describePath, FileBrowser, type FileSource } from '../components/FileBrowser'
import { PageHeader } from '../components/PageHeader'
import { profileKey } from '../profile'
import { PackList } from './Packs'

const profileSource: FileSource = {
  key: profileKey,
  get: api.profileFile,
  put: api.putProfileFile,
  settings: api.setProfileFileSettings,
  remove: api.deleteProfileFile,
  writable: true,
  empty: 'Nothing yet. Sign in to Claude or change a setting in any environment and it appears here.',
  example: '.claude/commands/review.md',
}

// FilesPage is the files environments share: the user's profile, which
// follows them into every environment they own, and the file packs
// templates give theirs. Changes here reach running environments at once,
// and changes made in an environment show up here.
export function FilesPage() {
  // Refetched often: an environment changes these as much as this page does.
  const profile = useQuery({
    queryKey: profileKey,
    queryFn: api.profile,
    refetchInterval: 3000,
  })
  const [open, setOpen] = useState<string | null>(null)

  if (profile.isPending) return <div className="page" />
  if (profile.isError) return <div className="page alert">{profile.error.message}</div>
  const { files, paths, own_paths } = profile.data

  return (
    <div className="page">
      <PageHeader
        title="Files"
        subtitle="Shared with your environments, the same in all of them as they change there or here."
      />
      <section className="section">
        <h2 className="section-title">Your profile</h2>
        <p className="muted small">In your home directory, in every environment you own.</p>
        <FileBrowser source={profileSource} files={files} paths={paths} open={open} onOpen={setOpen} />
      </section>
      <div className="page-narrow">
        <SharedPaths paths={paths} own={own_paths} />
      </div>
      <PackList />
    </div>
  )
}

// SharedPaths is what the profile shares, and what it leaves out of the
// directories it shares ("!path"): everyone's, and the user's own, which
// they add and remove.
function SharedPaths({ paths, own }: { paths: string[]; own: string[] }) {
  const shared = paths.filter((p) => !p.startsWith('!'))
  const ownLeftOut = paths.filter((p) => p.startsWith('!') && own.includes(p))
  const leftOut = paths.filter((p) => p.startsWith('!') && !own.includes(p)).map((p) => p.slice(1))
  const qc = useQueryClient()
  const [path, setPath] = useState('')
  const refresh = () => qc.invalidateQueries({ queryKey: profileKey })
  const add = useMutation({
    mutationFn: () => api.addProfilePath(path.trim()),
    onSuccess: () => setPath(''),
    onSettled: refresh,
  })
  const remove = useMutation({
    mutationFn: (p: string) => api.removeProfilePath(p),
    onSettled: refresh,
  })
  const submit = (e: FormEvent) => {
    e.preventDefault()
    if (path.trim()) add.mutate()
  }
  return (
    <section className="section">
      <h2 className="section-title">Shared paths</h2>
      <p className="muted small">
        Relative to your home directory. A path ending in <code>/</code> shares a directory and everything in it; one
        starting with <code>!</code> leaves a path in a shared directory to each environment. Removing a shared path
        leaves each environment its copy.
      </p>
      <div className="panel">
        <table className="table">
          <tbody>
            {shared.map((p) => (
              <tr key={p}>
                <td className="mono">{p}</td>
                <td className="muted small">{own.includes(p) ? 'yours' : (describePath(p) ?? 'everyone')}</td>
                <td className="num">
                  {own.includes(p) && (
                    <ConfirmButton
                      label="Stop sharing"
                      confirmLabel="Stop?"
                      onConfirm={() => remove.mutate(p)}
                      disabled={remove.isPending}
                    />
                  )}
                </td>
              </tr>
            ))}
            {ownLeftOut.map((p) => (
              <tr key={p}>
                <td className="mono">{p}</td>
                <td className="muted small">yours, left to each environment</td>
                <td className="num">
                  <ConfirmButton
                    label="Share it"
                    confirmLabel="Share?"
                    onConfirm={() => remove.mutate(p)}
                    disabled={remove.isPending}
                  />
                </td>
              </tr>
            ))}
          </tbody>
        </table>
      </div>
      {leftOut.length > 0 && (
        <details className="muted small">
          <summary>
            Always left to each environment: {leftOut.length} {leftOut.length === 1 ? 'path' : 'paths'} programs rewrite
            constantly or keep per machine
          </summary>
          <p className="mono">{leftOut.join('  ')}</p>
        </details>
      )}
      <form className="inline-form" onSubmit={submit}>
        <input
          className="mono grow"
          placeholder="Share another: .config/nvim/ or .bash_aliases; leave one out: !.claude/agents/"
          value={path}
          onChange={(e) => setPath(e.target.value)}
        />
        <button type="submit" className="btn btn-ghost" disabled={!path.trim() || add.isPending}>
          <Plus size={14} />
          Share
        </button>
      </form>
      {add.error && <div className="alert">{add.error.message}</div>}
    </section>
  )
}
