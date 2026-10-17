import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { Copy, History, KeyRound, LockKeyhole, Plus, TicketCheck } from 'lucide-react'
import { useState, type FormEvent } from 'react'
import { useLocation, useNavigate, useSearchParams } from 'react-router'

import { api } from '../api'
import { profileKey } from '../profile'
import { useMe } from '../session'
import { Activity } from '../components/Activity'
import { ConfirmButton } from '../components/ConfirmButton'
import { Keys } from '../components/Keys'
import { formatAgo } from '../components/format'
import { PhoneChoice, PhoneHead, PhoneNav, PhoneRow, PhoneSection, PhoneValue } from '../components/Phone'
import { tokensKey, useThemeChoice } from './themeChoice'

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
    <section className="section">
      <h2 className="section-title">Change password</h2>
      <form className="panel form" onSubmit={submit}>
        <label className="field">
          <span>Current password</span>
          <input
            type="password"
            required
            autoComplete="current-password"
            value={current}
            onChange={(e) => setCurrent(e.target.value)}
          />
        </label>
        <div className="field-row">
          <label className="field">
            <span>New password</span>
            <input
              type="password"
              required
              minLength={8}
              autoComplete="new-password"
              value={next}
              onChange={(e) => setNext(e.target.value)}
            />
          </label>
          <label className="field">
            <span>Confirm</span>
            <input
              type="password"
              required
              autoComplete="new-password"
              value={confirm}
              onChange={(e) => setConfirm(e.target.value)}
            />
          </label>
        </div>
        <small className="muted">Changing it signs you out everywhere else.</small>
        {mismatch && <div className="alert">The passwords do not match.</div>}
        {change.error && <div className="alert">{change.error.message}</div>}
        {change.isSuccess && <div className="notice notice-good">Password changed.</div>}
        <div className="form-actions">
          <button type="submit" className="btn btn-primary" disabled={change.isPending || mismatch}>
            Change password
          </button>
        </div>
      </form>
    </section>
  )
}

// AccessTokens sign tools in as the user -- the VS Code extension, scripts
// calling the API -- each revocable on its own, as a phone lists them.
function AccessTokens() {
  const qc = useQueryClient()
  const tokens = useQuery({ queryKey: tokensKey, queryFn: api.tokens })
  const [name, setName] = useState('')
  const [days, setDays] = useState('90')
  const [created, setCreated] = useState<{
    name: string
    token: string
  } | null>(null)
  const [copied, setCopied] = useState(false)
  const refresh = () => qc.invalidateQueries({ queryKey: tokensKey })
  const create = useMutation({
    mutationFn: () => api.createToken(name.trim(), days ? Number(days) : undefined),
    onSuccess: (t) => {
      setCreated({ name: t.info.name, token: t.token })
      setCopied(false)
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
  const copy = (token: string) => {
    navigator.clipboard.writeText(token).then(() => setCopied(true))
  }

  return (
    <section className="section">
      <h2 className="section-title">Access tokens</h2>
      <p className="muted small">
        For tools that act as you, such as the VS Code extension. A token can do anything you can, so give each tool its
        own and revoke it when the tool is done with.
      </p>
      {created && (
        <div className="panel form">
          <div>
            <span className="strong">{created.name}</span>
            <span className="muted"> — copy it: it is shown this once.</span>
          </div>
          <code className="mono token-reveal">{created.token}</code>
          <div className="form-actions">
            <button type="button" className="btn btn-ghost" onClick={() => setCreated(null)}>
              Done
            </button>
            <button type="button" className="btn btn-primary" onClick={() => copy(created.token)}>
              <Copy size={14} />
              {copied ? 'Copied' : 'Copy'}
            </button>
          </div>
        </div>
      )}
      <div className="phone-list">
        {tokens.isError && <div className="alert">{tokens.error.message}</div>}
        {tokens.data?.length === 0 && <div className="phone-row phone-row-empty">No tokens.</div>}
        {tokens.data?.map((t) => (
          <div key={t.id} className="phone-row phone-key">
            <span className="phone-row-main">
              <span className="strong">{t.name}</span>
              <span className="phone-row-sub">
                {t.last_used_at ? `used ${formatAgo(t.last_used_at)}` : 'never used'} ·{' '}
                {t.expires_at ? `expires ${new Date(t.expires_at).toLocaleDateString()}` : 'does not expire'}
              </span>
            </span>
            <ConfirmButton
              label="Revoke"
              confirmLabel="Revoke?"
              onConfirm={() => revoke.mutate(t.id)}
              disabled={revoke.isPending}
            />
          </div>
        ))}
      </div>
      <form className="phone-form" onSubmit={submit}>
        <input
          className="grow"
          placeholder="Name: VS Code on my laptop"
          value={name}
          onChange={(e) => setName(e.target.value)}
        />
        <select value={days} onChange={(e) => setDays(e.target.value)}>
          <option value="30">30 days</option>
          <option value="90">90 days</option>
          <option value="365">1 year</option>
          <option value="">No expiry</option>
        </select>
        <button type="submit" className="btn btn-ghost" disabled={!name.trim() || create.isPending}>
          <Plus size={14} />
          Create token
        </button>
      </form>
      {create.error && <div className="alert">{create.error.message}</div>}
      {revoke.error && <div className="alert">{revoke.error.message}</div>}
    </section>
  )
}

const screens = {
  password: 'Password',
  'sign-in-keys': 'Sign-in keys',
  'ssh-keys': 'SSH keys',
  tokens: 'Access tokens',
  activity: 'Your activity',
} as const

type Screen = keyof typeof screens

// PhoneAccount is the account as a phone shows it: who the person is and
// their theme, then a row for each kind of key or token, each a screen of
// its own.
export function PhoneAccount() {
  const me = useMe()
  const location = useLocation()
  const navigate = useNavigate()
  const [params, setParams] = useSearchParams()
  const [theme, choose, save] = useThemeChoice()
  const profile = useQuery({ queryKey: profileKey, queryFn: api.profile })
  const tokens = useQuery({ queryKey: tokensKey, queryFn: api.tokens })
  // An environment's page links to the sign-in keys by their anchor.
  const shown = (params.get('show') ?? (location.hash === '#sign-in-keys' ? 'sign-in-keys' : '')) as Screen | ''

  if (shown && shown in screens) {
    const back = () => (location.key !== 'default' ? navigate(-1) : setParams({}, { replace: true }))
    return (
      <div className="phone-screen">
        <PhoneNav back={{ onBack: back, label: 'Account' }} title={screens[shown]} />
        <div className="phone-screen-body phone-screen-sections">
          {shown === 'password' && <ChangePassword />}
          {shown === 'ssh-keys' && <Keys only="ssh" />}
          {shown === 'sign-in-keys' && <Keys only="sign-in" />}
          {shown === 'tokens' && <AccessTokens />}
          {shown === 'activity' && <Activity subjects={[`actor:${me.id}`]} />}
        </div>
      </div>
    )
  }

  const count = (n: number | undefined) => n !== undefined && <PhoneValue>{n}</PhoneValue>
  return (
    <div className="page phone-page">
      <PhoneHead title="Account" />
      <PhoneSection>
        <PhoneRow
          label={me.display_name || me.username}
          sub={`${me.username} · ${me.admin ? 'Administrator' : 'User'}`}
        />
      </PhoneSection>
      <PhoneSection
        title="Appearance"
        note="Kept with your account, so it follows you to every browser and the desktop app."
      >
        <div className="phone-row phone-row-plain">
          <span className="phone-row-main">
            <PhoneChoice
              label="Theme"
              value={theme}
              options={[
                { value: 'system', label: 'System' },
                { value: 'light', label: 'Light' },
                { value: 'dark', label: 'Dark' },
              ]}
              onChange={choose}
            />
          </span>
        </div>
      </PhoneSection>
      {save.error && <div className="alert">{save.error.message}</div>}
      <PhoneSection title="Signing in">
        {me.has_password && (
          <PhoneRow
            to="?show=password"
            icon={LockKeyhole}
            label="Password"
            sub="Changing it signs you out everywhere else"
          />
        )}
        <PhoneRow
          to="?show=sign-in-keys"
          icon={KeyRound}
          label="Sign-in keys"
          sub="Your keys, to reach environments over SSH"
          end={count(profile.data?.login_keys.length)}
        />
        <PhoneRow
          to="?show=tokens"
          icon={TicketCheck}
          label="Access tokens"
          sub="For tools that act as you"
          end={count(tokens.data?.length)}
        />
      </PhoneSection>
      <PhoneSection title="Git">
        <PhoneRow
          to="?show=ssh-keys"
          icon={KeyRound}
          label="SSH keys"
          sub="Environments sign with these; the key stays on the server"
          end={count(profile.data?.keys.length)}
        />
      </PhoneSection>
      <PhoneSection>
        <PhoneRow to="?show=activity" icon={History} label="Your activity" />
      </PhoneSection>
    </div>
  )
}
