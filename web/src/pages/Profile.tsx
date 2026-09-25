import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { Copy, KeyRound, Plus } from 'lucide-react'
import { useEffect, useState, type FormEvent } from 'react'

import { api, type LoginKey, type ProfileFile, type SSHKey } from '../api'
import { ConfirmButton } from '../components/ConfirmButton'
import { PageHeader } from '../components/PageHeader'
import { useMe } from '../session'

const profileKey = ['profile']

// What each part of a profile is, for people who did not write the list.
const describe: [string, string][] = [
  ['.claude/.credentials.json', "Claude's sign-in"],
  ['.claude/settings.json', "Claude's settings"],
  ['.claude/CLAUDE.md', "Claude's instructions for every project"],
  ['.claude/agents/', 'Claude subagents'],
  ['.claude/commands/', 'Claude slash commands'],
  ['.claude/skills/', 'Claude skills'],
  ['.claude/output-styles/', 'Claude output styles'],
  ['.gitconfig', "git's settings"],
  ['.git-credentials', "git's stored HTTPS credentials"],
  ['.netrc', 'Logins for curl, git and others'],
  ['.config/gh/hosts.yml', 'GitHub CLI sign-in'],
  ['.config/gh/config.yml', 'GitHub CLI settings'],
  ['.config/glab-cli/config.yml', 'GitLab CLI sign-in and settings'],
  ['.docker/config.json', 'Container registry logins'],
  ['.npmrc', 'npm registry logins'],
  ['.pypirc', 'PyPI logins'],
  ['.aws/credentials', 'AWS keys'],
  ['.aws/config', 'AWS profiles'],
  ['.kube/config', 'Kubernetes clusters and credentials'],
  ['.config/gcloud/application_default_credentials.json', 'Google Cloud application credentials'],
  ['.config/gcloud/configurations/', 'Google Cloud configurations'],
  ['.config/gcloud/active_config', 'Google Cloud active configuration'],
  ['.vscode-server-oss/data/User/settings.json', 'VS Code settings'],
  ['.vscode-server-oss/data/User/keybindings.json', 'VS Code keybindings'],
  ['.vscode-server-oss/data/User/snippets/', 'VS Code snippets'],
]

function describePath(path: string): string | undefined {
  for (const [p, what] of describe) {
    if (path === p || (p.endsWith('/') && path.startsWith(p))) return what
  }
  return undefined
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

  if (profile.isPending) return <div className="page page-narrow" />
  if (profile.isError) return <div className="page page-narrow alert">{profile.error.message}</div>
  const { files, keys, login_keys, paths, own_paths, secrets } = profile.data

  return (
    <div className="page page-narrow">
      <PageHeader
        title="Profile"
        subtitle="Follows you into every environment you own, and stays the same in all of them as you change it here or there."
      />
      {!secrets && (
        <div className="alert">
          This server has no secret key (HANGAR_SECRET_KEY_FILE), so it cannot keep Claude's sign-in or SSH keys.
        </div>
      )}
      <section className="section">
        <h2 className="section-title">Files</h2>
        <div className="panel">
          {files.length === 0 ? (
            <div className="empty">
              Nothing yet. Sign in to Claude or change a setting in any environment and it appears here, or add a file
              below.
            </div>
          ) : (
            <table className="table">
              <tbody>
                {files.map((f) => (
                  <FileRow
                    key={f.path}
                    file={f}
                    open={open === f.path}
                    onOpen={() => setOpen(open === f.path ? null : f.path)}
                  />
                ))}
              </tbody>
            </table>
          )}
        </div>
        <AddFile paths={paths} existing={files.map((f) => f.path)} onAdded={setOpen} />
      </section>
      <SharedPaths paths={paths} own={own_paths} />
      <SSHKeys keys={keys} disabled={!secrets} />
      <LoginKeys keys={login_keys} />
    </div>
  )
}

function FileRow({ file, open, onOpen }: { file: ProfileFile; open: boolean; onOpen: () => void }) {
  const qc = useQueryClient()
  const remove = useMutation({
    mutationFn: () => api.deleteProfileFile(file.path),
    onSettled: () => qc.invalidateQueries({ queryKey: profileKey }),
  })
  return (
    <>
      <tr>
        <td>
          {file.secret ? (
            <span className="mono">{file.path}</span>
          ) : (
            <button type="button" className="file-link mono" onClick={onOpen}>
              {file.path}
            </button>
          )}
          <div className="muted small">{describePath(file.path)}</div>
        </td>
        <td className="muted nowrap">{file.secret ? 'hidden' : `${file.size} bytes`}</td>
        <td className="muted nowrap">{new Date(file.updated_at).toLocaleString()}</td>
        <td className="num">
          <ConfirmButton
            label={file.secret ? 'Sign out' : 'Remove'}
            confirmLabel={file.secret ? 'Sign out everywhere?' : 'Remove everywhere?'}
            onConfirm={() => remove.mutate()}
            disabled={remove.isPending}
          />
        </td>
      </tr>
      {open && (
        <tr>
          <td colSpan={4}>
            <FileEditor path={file.path} updatedAt={file.updated_at} />
          </td>
        </tr>
      )}
    </>
  )
}

// FileEditor edits one file. While it has no unsaved change it follows the
// file as environments change it; once it has one, it says when the file has
// changed underneath rather than overwriting what is being typed.
function FileEditor({ path, updatedAt }: { path: string; updatedAt: string }) {
  const qc = useQueryClient()
  const file = useQuery({
    queryKey: ['profile-file', path, updatedAt],
    queryFn: () => api.profileFile(path),
  })
  const [text, setText] = useState<string | null>(null)
  const [base, setBase] = useState<string | null>(null)
  const saved = file.data?.content ?? null
  const dirty = text !== null && text !== base

  useEffect(() => {
    if (saved !== null && !dirty) {
      setText(saved)
      setBase(saved)
    }
  }, [saved, dirty])

  const save = useMutation({
    mutationFn: () => api.putProfileFile(path, text ?? ''),
    onSuccess: () => {
      setBase(text)
      qc.invalidateQueries({ queryKey: profileKey })
    },
  })

  if (file.isPending || text === null) return <div className="muted small">Loading…</div>
  return (
    <div className="form">
      <textarea
        className="mono profile-editor"
        spellCheck={false}
        value={text}
        onChange={(e) => setText(e.target.value)}
        rows={Math.min(24, Math.max(6, text.split('\n').length + 1))}
      />
      {dirty && saved !== base && (
        <div className="notice">It has changed in an environment since you started editing; saving replaces that.</div>
      )}
      {save.error && <div className="alert">{save.error.message}</div>}
      <div className="form-actions">
        <button type="button" className="btn btn-ghost" disabled={!dirty} onClick={() => setText(saved)}>
          Discard
        </button>
        <button
          type="button"
          className="btn btn-primary"
          disabled={!dirty || save.isPending}
          onClick={() => save.mutate()}
        >
          Save
        </button>
      </div>
    </div>
  )
}

function AddFile({ paths, existing, onAdded }: { paths: string[]; existing: string[]; onAdded: (p: string) => void }) {
  const qc = useQueryClient()
  const choices = paths.filter((p) => !existing.includes(p) && !p.endsWith('.credentials.json'))
  const [path, setPath] = useState('')
  const add = useMutation({
    mutationFn: () => api.putProfileFile(path, ''),
    onSuccess: () => {
      qc.invalidateQueries({ queryKey: profileKey })
      onAdded(path)
      setPath('')
    },
  })
  const submit = (e: FormEvent) => {
    e.preventDefault()
    if (path && !path.endsWith('/')) add.mutate()
  }
  return (
    <form className="inline-form" onSubmit={submit}>
      <input
        className="mono grow"
        list="profile-paths"
        placeholder="Add a file: .claude/commands/review.md"
        value={path}
        onChange={(e) => setPath(e.target.value)}
      />
      <datalist id="profile-paths">
        {choices.map((p) => (
          <option key={p} value={p}>
            {describePath(p)}
          </option>
        ))}
      </datalist>
      <button type="submit" className="btn btn-ghost" disabled={!path || path.endsWith('/') || add.isPending}>
        <Plus size={14} />
        Add
      </button>
      {add.error && <span className="action-error">{add.error.message}</span>}
    </form>
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
  const remove = useMutation({ mutationFn: (p: string) => api.removeProfilePath(p), onSettled: refresh })
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
  const remove = useMutation({ mutationFn: (id: string) => api.deleteLoginKey(id), onSettled: refresh })
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
