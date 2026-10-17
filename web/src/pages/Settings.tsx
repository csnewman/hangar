import './settings.css'

import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import {
  Code2,
  Copy,
  History,
  KeyRound,
  LockKeyhole,
  MonitorUp,
  Palette,
  Plus,
  TicketCheck,
  UserRound,
  type LucideIcon,
} from 'lucide-react'
import { useState, type FormEvent } from 'react'
import { Link, Navigate, useLocation, useParams, useSearchParams } from 'react-router'

import { api, type LoginKey, type SSHKey } from '../api'
import { Activity } from '../components/Activity'
import { ConfirmButton } from '../components/ConfirmButton'
import { formatAgo } from '../components/format'
import {
  CopyField,
  DetailHead,
  DetailPage,
  ListGroup,
  ListItem,
  ListPane,
  MasterDetail,
  PaneRoot,
  Row,
  Rows,
  SearchField,
  Section,
  Segmented,
} from '../components/Pane'
import { desktop } from '../desktop'
import { usePhone } from '../phone'
import { profileKey } from '../profile'
import { useMe } from '../session'
import type { Theme } from '../theme'
import { tokensKey, useThemeChoice } from './themeChoice'
import { PhoneAccount } from './Account'

type Page = {
  id: string
  group: 'You' | 'Keys' | 'Apps'
  label: string
  icon: LucideIcon
  about: string
}

// The pages of settings, in the groups the list shows them in.
const pages: Page[] = [
  { id: 'account', group: 'You', label: 'Account', icon: UserRound, about: 'Who you are, and your password.' },
  {
    id: 'appearance',
    group: 'You',
    label: 'Appearance',
    icon: Palette,
    about: 'Kept with your account, so it follows you to every browser and the desktop app.',
  },
  { id: 'activity', group: 'You', label: 'Your activity', icon: History, about: 'What you have done, newest first.' },
  {
    id: 'ssh-keys',
    group: 'Keys',
    label: 'SSH keys',
    icon: KeyRound,
    about: 'Your environments sign with these through an SSH agent. The private key stays on the server.',
  },
  {
    id: 'sign-in-keys',
    group: 'Keys',
    label: 'Sign-in keys',
    icon: LockKeyhole,
    about: 'Your own public keys, from the machines you work on, which reach your environments over SSH.',
  },
  {
    id: 'access-tokens',
    group: 'Keys',
    label: 'Access tokens',
    icon: TicketCheck,
    about: 'For tools that act as you, such as the VS Code extension.',
  },
  {
    id: 'vscode',
    group: 'Apps',
    label: 'VS Code',
    icon: Code2,
    about: 'Your environments in VS Code on your own machine, over Remote-SSH.',
  },
  {
    id: 'desktop-app',
    group: 'Apps',
    label: 'Desktop app',
    icon: MonitorUp,
    about: 'Hangar in a window of its own, with a tab for each environment.',
  },
]

// accountPage is the settings page an old Account link names: by its
// anchor, as an environment's page links to the sign-in keys, or by the
// phone's ?show=.
function accountPage(hash: string, show: string | null): string {
  const named: Record<string, string> = {
    'sign-in-keys': 'sign-in-keys',
    'ssh-keys': 'ssh-keys',
    tokens: 'access-tokens',
    'access-tokens': 'access-tokens',
    password: 'account',
    activity: 'activity',
  }
  return named[show ?? ''] ?? named[hash.replace(/^#/, '')] ?? 'account'
}

// AccountPage is the account on a phone, which shows it as a screen of its
// own; on a computer it is the matching page of Settings.
export function AccountPage() {
  const { hash } = useLocation()
  const [params] = useSearchParams()
  if (usePhone()) return <PhoneAccount />
  return <Navigate to={`/settings/${accountPage(hash, params.get('show'))}`} replace />
}

// SettingsPage is the user's own settings, laid out as an IDE lays out its
// own: a list of pages in groups on the left, searched by name, and the
// chosen one beside it. A phone shows its account screen.
export function SettingsPage() {
  const { page: id } = useParams()
  const me = useMe()
  const [query, setQuery] = useState('')
  if (usePhone()) return <PhoneAccount />
  const page = pages.find((p) => p.id === id)
  if (!page) return <Navigate to="/settings/account" replace />
  const q = query.trim().toLowerCase()
  const shown = pages.filter((p) => !q || p.label.toLowerCase().includes(q) || p.about.toLowerCase().includes(q))
  const groups = (['You', 'Keys', 'Apps'] as const).map((g) => ({ g, list: shown.filter((p) => p.group === g) }))

  return (
    <PaneRoot>
      <MasterDetail
        list={
          <ListPane head={<SearchField value={query} onChange={setQuery} placeholder="Search settings" />}>
            {groups.map(
              ({ g, list }) =>
                list.length > 0 && (
                  <ListGroup key={g} title={g}>
                    {list.map((p) => (
                      <ListItem key={p.id} to={`/settings/${p.id}`} mark={<p.icon size={15} />} title={p.label} />
                    ))}
                  </ListGroup>
                ),
            )}
            {shown.length === 0 && <div className="md-empty-list">No setting matches.</div>}
          </ListPane>
        }
      >
        <DetailHead title={page.label} sub={<span>{page.about}</span>} />
        <DetailPage wide={page.id === 'activity'}>
          {page.id === 'account' && <AccountSettings />}
          {page.id === 'appearance' && <AppearanceSettings />}
          {page.id === 'activity' && <Activity subjects={[`actor:${me.id}`]} />}
          {page.id === 'ssh-keys' && <SSHKeySettings />}
          {page.id === 'sign-in-keys' && <SignInKeySettings />}
          {page.id === 'access-tokens' && <TokenSettings />}
          {page.id === 'vscode' && <VSCodeSettings />}
          {page.id === 'desktop-app' && <DesktopAppSettings />}
        </DetailPage>
      </MasterDetail>
    </PaneRoot>
  )
}

function AccountSettings() {
  const me = useMe()
  return (
    <>
      <Section title="You">
        <Rows>
          <Row label="Username">
            <span className="mono">{me.username}</span>
          </Row>
          <Row label="Name">{me.display_name || <span className="muted">not set</span>}</Row>
          <Row label="Role">{me.admin ? 'Administrator' : 'User'}</Row>
        </Rows>
      </Section>
      {me.has_password && <ChangePassword />}
    </>
  )
}

function ChangePassword() {
  const [current, setCurrent] = useState('')
  const [next, setNext] = useState('')
  const [confirm, setConfirm] = useState('')
  const change = useMutation({
    mutationFn: () => api.changePassword(current, next),
    onSuccess: () => {
      setCurrent('')
      setNext('')
      setConfirm('')
    },
  })
  const mismatch = confirm !== '' && confirm !== next
  const submit = (e: FormEvent) => {
    e.preventDefault()
    if (!mismatch) change.mutate()
  }
  return (
    <Section title="Password">
      <form onSubmit={submit}>
        <fieldset className="pane-fields">
          <Rows>
            <Row label="Current password">
              <input
                type="password"
                className="pane-wide"
                required
                autoComplete="current-password"
                aria-label="Current password"
                value={current}
                onChange={(e) => setCurrent(e.target.value)}
              />
            </Row>
            <Row label="New password">
              <input
                type="password"
                className="pane-wide"
                required
                minLength={8}
                autoComplete="new-password"
                aria-label="New password"
                value={next}
                onChange={(e) => setNext(e.target.value)}
              />
            </Row>
            <Row label="Confirm">
              <input
                type="password"
                className="pane-wide"
                required
                autoComplete="new-password"
                aria-label="Confirm the new password"
                value={confirm}
                onChange={(e) => setConfirm(e.target.value)}
              />
            </Row>
            <Row label="">
              <button type="submit" className="btn btn-primary btn-sm" disabled={change.isPending || mismatch}>
                Change Password
              </button>
              {mismatch && <span className="action-error">The passwords do not match.</span>}
              {change.error && <span className="action-error">{change.error.message}</span>}
              {change.isSuccess && <span className="settings-done">Password changed.</span>}
            </Row>
          </Rows>
        </fieldset>
      </form>
      <p className="pane-hint">Changing it signs you out everywhere else.</p>
    </Section>
  )
}

function AppearanceSettings() {
  const [theme, choose, save] = useThemeChoice()
  return (
    <Section title="Theme">
      <Rows>
        <Row label="Theme">
          <Segmented<Theme>
            label="Theme"
            value={theme}
            onChange={choose}
            options={[
              { value: 'system', label: 'System' },
              { value: 'light', label: 'Light' },
              { value: 'dark', label: 'Dark' },
            ]}
          />
          <span className="muted small">
            {theme === 'system'
              ? 'Light or dark as your device is set'
              : theme === 'light'
                ? 'Always light'
                : 'Always dark'}
          </span>
          {save.error && <span className="action-error">{save.error.message}</span>}
        </Row>
      </Rows>
    </Section>
  )
}

function useProfile() {
  return useQuery({ queryKey: profileKey, queryFn: api.profile })
}

function SSHKeySettings() {
  const profile = useProfile()
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
  if (profile.isPending) return <p className="muted">Loading…</p>
  if (profile.isError) return <div className="alert">{profile.error.message}</div>
  const { keys, secrets } = profile.data
  const submit = (e: FormEvent) => {
    e.preventDefault()
    add.mutate()
  }
  return (
    <>
      {!secrets && (
        <div className="alert">This server has no secret key (HANGAR_SECRET_KEY_FILE), so it cannot keep SSH keys.</div>
      )}
      <Section title="Keys">
        {keys.length === 0 ? (
          <p className="muted">No keys.</p>
        ) : (
          <table className="pane-table">
            <thead>
              <tr>
                <th>Name</th>
                <th>Public key</th>
                <th />
              </tr>
            </thead>
            <tbody>
              {keys.map((k) => (
                <SSHKeyRow key={k.id} sshKey={k} />
              ))}
            </tbody>
          </table>
        )}
        <p className="pane-hint">
          Add the public key to GitHub, or wherever git pushes, to push from your environments.
        </p>
      </Section>
      <Section title="Add a key">
        <form onSubmit={submit}>
          <fieldset className="pane-fields" disabled={!secrets}>
            <Rows>
              <Row label="Name">
                <input
                  className="pane-wide"
                  required
                  aria-label="Name"
                  value={name}
                  onChange={(e) => setName(e.target.value)}
                  placeholder="hangar"
                />
              </Row>
              <Row label="">
                <label className="check">
                  <input type="checkbox" checked={importing} onChange={(e) => setImporting(e.target.checked)} />
                  <span>Import an existing private key rather than generate one</span>
                </label>
              </Row>
              {importing && (
                <Row label="Private key">
                  <textarea
                    className="mono pane-wide"
                    rows={6}
                    required
                    spellCheck={false}
                    aria-label="Private key, without a passphrase"
                    value={privateKey}
                    onChange={(e) => setPrivateKey(e.target.value)}
                    placeholder="-----BEGIN OPENSSH PRIVATE KEY-----"
                  />
                </Row>
              )}
              <Row label="">
                <button type="submit" className="btn btn-primary btn-sm" disabled={add.isPending}>
                  <KeyRound size={13} />
                  {importing ? 'Import Key' : 'Generate Key'}
                </button>
                {add.error && <span className="action-error">{add.error.message}</span>}
              </Row>
            </Rows>
          </fieldset>
        </form>
        {importing && <p className="pane-hint">A key with a passphrase cannot be used: remove it first.</p>}
      </Section>
    </>
  )
}

function SSHKeyRow({ sshKey }: { sshKey: SSHKey }) {
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
      <td className="settings-key-name">
        <div className="strong">{sshKey.name}</div>
        <div className="muted small mono">{sshKey.fingerprint}</div>
      </td>
      <td>
        <code className="mono settings-public-key" title={sshKey.public_key}>
          {sshKey.public_key}
        </code>
      </td>
      <td className="num nowrap">
        <span className="settings-row-actions">
          <button type="button" className="btn btn-ghost btn-sm" onClick={copy}>
            <Copy size={13} />
            {copied ? 'Copied' : 'Copy Public Key'}
          </button>
          <ConfirmButton
            label="Delete"
            confirmLabel="Delete?"
            onConfirm={() => remove.mutate()}
            disabled={remove.isPending}
          />
        </span>
      </td>
    </tr>
  )
}

function SignInKeySettings() {
  const me = useMe()
  const profile = useProfile()
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
  if (profile.isPending) return <p className="muted">Loading…</p>
  if (profile.isError) return <div className="alert">{profile.error.message}</div>
  const keys: LoginKey[] = profile.data.login_keys
  const gw = me.ssh
  const submit = (e: FormEvent) => {
    e.preventDefault()
    if (publicKey.trim()) add.mutate()
  }
  return (
    <>
      <Section title="Keys">
        {keys.length === 0 ? (
          <p className="muted">No keys.</p>
        ) : (
          <table className="pane-table">
            <thead>
              <tr>
                <th>Name</th>
                <th>Added</th>
                <th />
              </tr>
            </thead>
            <tbody>
              {keys.map((k) => (
                <tr key={k.id}>
                  <td>
                    <div className="strong">{k.name}</div>
                    <div className="muted small mono">{k.fingerprint}</div>
                  </td>
                  <td className="muted nowrap" title={new Date(k.created_at).toLocaleString()}>
                    {formatAgo(k.created_at)}
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
        {remove.error && <div className="alert">{remove.error.message}</div>}
        <p className="pane-hint settings-text">
          {gw ? (
            <>
              With one of them,{' '}
              <code className="mono">
                ssh &lt;environment&gt;@{gw.host}
                {gw.port === 22 ? '' : ` -p ${gw.port}`}
              </code>{' '}
              reaches any environment of yours, and VS Code&rsquo;s Remote-SSH does the same.
            </>
          ) : (
            'This server runs no SSH gateway (HANGAR_SSH_LISTEN), so the keys reach nothing yet.'
          )}
        </p>
      </Section>
      <Section title="Add a key">
        <form onSubmit={submit}>
          <fieldset className="pane-fields">
            <Rows>
              <Row label="Public key">
                <textarea
                  className="mono pane-wide"
                  rows={3}
                  required
                  spellCheck={false}
                  aria-label="Public key, as in ~/.ssh/id_ed25519.pub"
                  value={publicKey}
                  onChange={(e) => setPublicKey(e.target.value)}
                  placeholder="ssh-ed25519 AAAA… you@laptop"
                />
              </Row>
              <Row label="">
                <button type="submit" className="btn btn-primary btn-sm" disabled={!publicKey.trim() || add.isPending}>
                  <Plus size={13} />
                  Add Key
                </button>
                {add.error && <span className="action-error">{add.error.message}</span>}
              </Row>
            </Rows>
          </fieldset>
        </form>
        <p className="pane-hint">As in ~/.ssh/id_ed25519.pub on the machine you work on.</p>
      </Section>
    </>
  )
}

function TokenSettings() {
  const qc = useQueryClient()
  const tokens = useQuery({ queryKey: tokensKey, queryFn: api.tokens })
  const [name, setName] = useState('')
  const [days, setDays] = useState('90')
  const [created, setCreated] = useState<{ name: string; token: string } | null>(null)
  const refresh = () => qc.invalidateQueries({ queryKey: tokensKey })
  const create = useMutation({
    mutationFn: () => api.createToken(name.trim(), days ? Number(days) : undefined),
    onSuccess: (t) => {
      setCreated({ name: t.info.name, token: t.token })
      setName('')
    },
    onSettled: refresh,
  })
  const revoke = useMutation({
    mutationFn: (id: string) => api.revokeToken(id),
    onSettled: refresh,
  })
  const submit = (e: FormEvent) => {
    e.preventDefault()
    if (name.trim()) create.mutate()
  }
  return (
    <>
      {created && (
        <Section title={`New token: ${created.name}`}>
          <p className="settings-reveal-note">Copy it: it is shown this once.</p>
          <div className="settings-reveal">
            <CopyField text={created.token} />
            <button type="button" className="btn btn-ghost btn-sm" onClick={() => setCreated(null)}>
              Done
            </button>
          </div>
        </Section>
      )}
      <Section title="Tokens">
        {tokens.isError ? (
          <div className="alert">{tokens.error.message}</div>
        ) : !tokens.data || tokens.data.length === 0 ? (
          <p className="muted">{tokens.isPending ? 'Loading…' : 'No tokens.'}</p>
        ) : (
          <table className="pane-table">
            <thead>
              <tr>
                <th>Name</th>
                <th>Last used</th>
                <th>Expires</th>
                <th />
              </tr>
            </thead>
            <tbody>
              {tokens.data.map((t) => (
                <tr key={t.id}>
                  <td className="strong">{t.name}</td>
                  <td className="muted nowrap">{t.last_used_at ? formatAgo(t.last_used_at) : 'never'}</td>
                  <td className="muted nowrap">
                    {t.expires_at ? new Date(t.expires_at).toLocaleDateString() : 'does not expire'}
                  </td>
                  <td className="num">
                    <ConfirmButton
                      label="Revoke"
                      confirmLabel="Revoke?"
                      onConfirm={() => revoke.mutate(t.id)}
                      disabled={revoke.isPending}
                    />
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        )}
        {revoke.error && <div className="alert">{revoke.error.message}</div>}
        <p className="pane-hint">
          A token can do anything you can, so give each tool its own and revoke it when the tool is done with.
        </p>
      </Section>
      <Section title="Create a token">
        <form onSubmit={submit}>
          <fieldset className="pane-fields">
            <Rows>
              <Row label="Name">
                <input
                  className="pane-wide"
                  aria-label="Name"
                  placeholder="VS Code on my laptop"
                  value={name}
                  onChange={(e) => setName(e.target.value)}
                />
              </Row>
              <Row label="Expires">
                <select aria-label="Expires" value={days} onChange={(e) => setDays(e.target.value)}>
                  <option value="30">In 30 days</option>
                  <option value="90">In 90 days</option>
                  <option value="365">In a year</option>
                  <option value="">Never</option>
                </select>
              </Row>
              <Row label="">
                <button type="submit" className="btn btn-primary btn-sm" disabled={!name.trim() || create.isPending}>
                  <Plus size={13} />
                  Create Token
                </button>
                {create.error && <span className="action-error">{create.error.message}</span>}
              </Row>
            </Rows>
          </fieldset>
        </form>
      </Section>
    </>
  )
}

function VSCodeSettings() {
  return (
    <>
      <Section title="Signing in">
        <Rows>
          <Row label="Extension">
            <span>
              Hangar&rsquo;s extension, <span className="mono">hangar.hangar-remote</span>, lists your environments in
              VS Code and opens each over Remote-SSH, starting it first if it is stopped.
            </span>
          </Row>
          <Row label="Sign in">
            <span>
              Run <strong>Hangar: Sign In</strong> and give this server&rsquo;s address,{' '}
              <span className="mono">{window.location.origin}</span>. Your browser opens Hangar, which asks you to allow
              it, and gives it an access token for a year.
            </span>
          </Row>
          <Row label="With a token">
            <span>
              <strong>Hangar: Sign In with an Access Token</strong> takes one made under{' '}
              <Link to="/settings/access-tokens">Access tokens</Link> instead.
            </span>
          </Row>
          <Row label="Reaching them">
            <span>
              It connects through the SSH gateway with a key from your{' '}
              <Link to="/settings/sign-in-keys">sign-in keys</Link>, and offers to add one of yours if none is there.
            </span>
          </Row>
        </Rows>
      </Section>
      <p className="pane-hint">Signing it out, or revoking its token under Access tokens, ends its access.</p>
    </>
  )
}

function DesktopAppSettings() {
  const q = new URLSearchParams({ server: window.location.origin, path: '/environments' })
  return (
    <Section title="Opening Hangar in it">
      <Rows>
        <Row label="This server">
          {desktop ? (
            <span>You are using it: this window is one server, with a tab for each environment.</span>
          ) : (
            <>
              <a className="btn btn-ghost btn-sm" href={`hangar://open?${q}`}>
                <MonitorUp size={13} />
                Open in the Desktop App
              </a>
              <span className="muted small">If it is installed, it opens this server in a window of its own.</span>
            </>
          )}
        </Row>
        <Row label="Links">
          <span>
            A link of the form <span className="mono">hangar://open?server=…&amp;path=…</span> opens that page in it, an
            environment&rsquo;s in that environment&rsquo;s tab.
          </span>
        </Row>
      </Rows>
    </Section>
  )
}
