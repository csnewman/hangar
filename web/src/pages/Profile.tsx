import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { Copy, KeyRound, Plus } from 'lucide-react'
import { useState, type FormEvent } from 'react'
import { Link } from 'react-router'

import { api, type LoginKey, type SSHKey } from '../api'
import { ConfirmButton } from '../components/ConfirmButton'
import { describePath, FileBrowser, type FileSource } from '../components/FileBrowser'
import { PageHeader } from '../components/PageHeader'
import { useMe } from '../session'

const profileKey = ['profile']

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

// ProfilePage is the files and keys that follow the user into every
// environment they own. Changes here reach running environments at once, and
// changes made in an environment show up here.
export function ProfilePage() {
  // Refetched often: an environment changes these as much as this page does.
  const profile = useQuery({
    queryKey: profileKey,
    queryFn: api.profile,
    refetchInterval: 3000,
  })
  const [open, setOpen] = useState<string | null>(null)

  if (profile.isPending) return <div className="page" />
  if (profile.isError) return <div className="page alert">{profile.error.message}</div>
  const { files, keys, login_keys, paths, own_paths, secrets } = profile.data

  return (
    <div className="page">
      <div className="page-narrow">
        <PageHeader
          title="Profile"
          subtitle="Follows you into every environment you own, and stays the same in all of them as you change it here or there."
        />
        {!secrets && (
          <div className="alert">
            This server has no secret key (HANGAR_SECRET_KEY_FILE), so it cannot keep SSH keys.
          </div>
        )}
      </div>
      <section className="section">
        <h2 className="section-title">Files</h2>
        <FileBrowser source={profileSource} files={files} paths={paths} open={open} onOpen={setOpen} />
        <p className="muted small">
          Files a project's environments need, such as its <code>.env</code>, go in a <Link to="/packs">file pack</Link>{' '}
          its templates list.
        </p>
      </section>
      <div className="page-narrow">
        <SharedPaths paths={paths} own={own_paths} />
        <SSHKeys keys={keys} disabled={!secrets} />
        <LoginKeys keys={login_keys} />
      </div>
    </div>
  )
}

function SSHKeys({ keys, disabled }: { keys: SSHKey[]; disabled: boolean }) {
  const qc = useQueryClient()
  const [name, setName] = useState('')
  const [importing, setImporting] = useState(false)
  const [privateKey, setPrivateKey] = useState('')
  const add = useMutation({
    mutationFn: () => api.addSSHKey(name, importing ? privateKey : undefined),
    onSuccess: () => {
      setName('')
      setPrivateKey('')
      setImporting(false)
    },
    onSettled: () => qc.invalidateQueries({ queryKey: profileKey }),
  })
  const submit = (e: FormEvent) => {
    e.preventDefault()
    add.mutate()
  }

  return (
    <section className="section">
      <h2 className="section-title">SSH keys</h2>
      <p className="muted small">
        Environments sign with these through an SSH agent; the private key stays on the server. Add the public key to
        GitHub or wherever git pushes.
      </p>
      <div className="panel">
        {keys.length === 0 ? (
          <div className="empty">No keys.</div>
        ) : (
          <table className="table">
            <tbody>
              {keys.map((k) => (
                <KeyRow key={k.id} sshKey={k} />
              ))}
            </tbody>
          </table>
        )}
      </div>
      <form className="panel form key-form" onSubmit={submit}>
        <div className="field-row">
          <label className="field grow">
            <span>Name</span>
            <input required value={name} onChange={(e) => setName(e.target.value)} placeholder="hangar" />
          </label>
        </div>
        <label className="check">
          <input type="checkbox" checked={importing} onChange={(e) => setImporting(e.target.checked)} />
          <span>Import an existing private key rather than generate one</span>
        </label>
        {importing && (
          <label className="field">
            <span>Private key, without a passphrase</span>
            <textarea
              className="mono"
              rows={6}
              required
              spellCheck={false}
              value={privateKey}
              onChange={(e) => setPrivateKey(e.target.value)}
              placeholder="-----BEGIN OPENSSH PRIVATE KEY-----"
            />
          </label>
        )}
        {add.error && <div className="alert">{add.error.message}</div>}
        <div className="form-actions">
          <button type="submit" className="btn btn-primary" disabled={disabled || add.isPending}>
            <KeyRound size={14} />
            {importing ? 'Import key' : 'Generate key'}
          </button>
        </div>
      </form>
    </section>
  )
}

function KeyRow({ sshKey }: { sshKey: SSHKey }) {
  const qc = useQueryClient()
  const remove = useMutation({
    mutationFn: () => api.deleteSSHKey(sshKey.id),
    onSettled: () => qc.invalidateQueries({ queryKey: profileKey }),
  })
  const [copied, setCopied] = useState(false)
  const copy = () => {
    navigator.clipboard.writeText(sshKey.public_key).then(() => {
      setCopied(true)
      setTimeout(() => setCopied(false), 1500)
    })
  }
  return (
    <tr>
      <td>
        <div className="strong">{sshKey.name}</div>
        <div className="muted small mono">{sshKey.fingerprint}</div>
        <code className="public-key">{sshKey.public_key}</code>
      </td>
      <td className="num nowrap">
        <button type="button" className="btn btn-ghost" onClick={copy}>
          <Copy size={13} />
          {copied ? 'Copied' : 'Copy public key'}
        </button>
        <ConfirmButton
          label="Delete"
          confirmLabel="Delete?"
          onConfirm={() => remove.mutate()}
          disabled={remove.isPending}
        />
      </td>
    </tr>
  )
}

// SharedPaths is what the profile shares: everyone's, and the user's own,
// which they add and remove.
function SharedPaths({ paths, own }: { paths: string[]; own: string[] }) {
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
        Relative to your home directory. A path ending in <code>/</code> shares a directory and everything in it.
        Removing one leaves each environment its copy.
      </p>
      <div className="panel">
        <table className="table">
          <tbody>
            {paths.map((p) => (
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
          </tbody>
        </table>
      </div>
      <form className="inline-form" onSubmit={submit}>
        <input
          className="mono grow"
          placeholder="Share another: .config/nvim/ or .bash_aliases"
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

// LoginKeys are the user's own public keys, which sign them in to their
// environments through the SSH gateway.
function LoginKeys({ keys }: { keys: LoginKey[] }) {
  const me = useMe()
  const qc = useQueryClient()
  const [publicKey, setPublicKey] = useState('')
  const refresh = () => qc.invalidateQueries({ queryKey: profileKey })
  const add = useMutation({
    mutationFn: () => api.addLoginKey(publicKey.trim()),
    onSuccess: () => setPublicKey(''),
    onSettled: refresh,
  })
  const remove = useMutation({
    mutationFn: (id: string) => api.deleteLoginKey(id),
    onSettled: refresh,
  })
  const submit = (e: FormEvent) => {
    e.preventDefault()
    if (publicKey.trim()) add.mutate()
  }
  const gw = me.ssh
  return (
    <section className="section" id="sign-in-keys">
      <h2 className="section-title">Sign-in keys</h2>
      <p className="muted small">
        Your own public keys, from the machines you work on. With one of them,{' '}
        {gw ? (
          <code>
            ssh &lt;environment&gt;@{gw.host}
            {gw.port === 22 ? '' : ` -p ${gw.port}`}
          </code>
        ) : (
          'SSH'
        )}{' '}
        reaches any environment of yours, and VS Code's Remote-SSH does the same.
        {!gw && ' This server runs no SSH gateway (HANGAR_SSH_LISTEN).'}
      </p>
      <div className="panel">
        {keys.length === 0 ? (
          <div className="empty">No keys.</div>
        ) : (
          <table className="table">
            <tbody>
              {keys.map((k) => (
                <tr key={k.id}>
                  <td>
                    <div className="strong">{k.name}</div>
                    <div className="muted small mono public-key">{k.fingerprint}</div>
                    <div className="muted small">added {new Date(k.created_at).toLocaleString()}</div>
                  </td>
                  <td className="num">
                    <ConfirmButton
                      label="Remove"
                      confirmLabel="Remove?"
                      onConfirm={() => remove.mutate(k.id)}
                      disabled={remove.isPending}
                    />
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        )}
      </div>
      <form className="panel form key-form" onSubmit={submit}>
        <label className="field">
          <span>Public key, as in ~/.ssh/id_ed25519.pub</span>
          <textarea
            className="mono"
            rows={3}
            required
            spellCheck={false}
            value={publicKey}
            onChange={(e) => setPublicKey(e.target.value)}
            placeholder="ssh-ed25519 AAAA… you@laptop"
          />
        </label>
        {add.error && <div className="alert">{add.error.message}</div>}
        {remove.error && <div className="alert">{remove.error.message}</div>}
        <div className="form-actions">
          <button type="submit" className="btn btn-primary" disabled={!publicKey.trim() || add.isPending}>
            <Plus size={14} />
            Add key
          </button>
        </div>
      </form>
    </section>
  )
}
