import { useEffect, useRef, useState } from 'react'

import type { Clash, CodeClient } from './client'
import { Popup } from './Search'
import { what } from './treeOps'

// TransferDialog is IntelliJ's Move and Copy: the folder to put things in,
// typed with the folders there offered as it is, and for one thing, its
// name there.
export function TransferDialog({
  client,
  op,
  paths,
  dir,
  onDone,
  onClose,
}: {
  client: CodeClient
  op: 'copy' | 'move'
  paths: string[]
  // dir is the folder offered, from the root ('' for the root).
  dir: string
  onDone: (to: string, name?: string) => void
  onClose: () => void
}) {
  const one = paths.length === 1 ? paths[0] : null
  const [to, setTo] = useState(dir)
  const [name, setName] = useState(one ? one.slice(one.lastIndexOf('/') + 1) : '')
  const [folders, setFolders] = useState<string[]>([])
  const nameRef = useRef<HTMLInputElement>(null)

  // The folders in the one being typed, for the input to offer.
  useEffect(() => {
    let live = true
    const parent = to.includes('/') ? to.slice(0, to.lastIndexOf('/')) : ''
    const under = to.endsWith('/') ? to.replace(/\/+$/, '') : parent
    client.list(under).then(
      (es) => live && setFolders(es.filter((e) => e.dir).map((e) => (under ? `${under}/${e.name}` : e.name))),
      () => live && setFolders([]),
    )
    return () => {
      live = false
    }
  }, [client, to])

  useEffect(() => {
    const el = nameRef.current
    if (!el) return
    const dot = name.lastIndexOf('.')
    el.focus()
    el.setSelectionRange(0, dot > 0 ? dot : name.length)
    // Selected once, as the dialog opens.
  }, [])

  const title = op === 'copy' ? 'Copy' : 'Move'
  const submit = () => onDone(to.replace(/^\/+|\/+$/g, ''), one ? name.trim() : undefined)
  return (
    <Popup className="code-dialog" onClose={onClose}>
      <form
        onSubmit={(e) => {
          e.preventDefault()
          submit()
        }}
      >
        <div className="code-popup-head">
          <span className="strong">
            {title} {what(paths)}
          </span>
        </div>
        {one && (
          <label className="code-dialog-field">
            <span>New name</span>
            <input
              ref={nameRef}
              className="code-popup-input"
              spellCheck={false}
              value={name}
              onChange={(e) => setName(e.target.value)}
            />
          </label>
        )}
        <label className="code-dialog-field">
          <span>To folder</span>
          <input
            className="code-popup-input"
            spellCheck={false}
            list="code-dialog-folders"
            placeholder="The project's top folder"
            autoFocus={!one}
            value={to}
            onChange={(e) => setTo(e.target.value)}
          />
          <datalist id="code-dialog-folders">
            {folders.map((f) => (
              <option key={f} value={f} />
            ))}
          </datalist>
        </label>
        <div className="code-popup-actions">
          <span className="muted small">A folder from the project&rsquo;s top; it is made if it is not there.</span>
          <span className="code-tool-spacer" />
          <button type="button" className="btn btn-ghost btn-sm" onClick={onClose}>
            Cancel
          </button>
          <button type="submit" className="btn btn-primary btn-sm" disabled={!!one && !name.trim()}>
            {title}
          </button>
        </div>
      </form>
    </Popup>
  )
}

// ClashDialog asks what to do with names a folder already has, as
// IntelliJ's and a file manager's do.
export function ClashDialog({
  names,
  to,
  op,
  onChoose,
}: {
  names: string[]
  to: string
  op: 'copy' | 'move' | 'upload'
  onChoose: (c: Clash | null) => void
}) {
  const verb = { copy: 'Copying', move: 'Moving', upload: 'Uploading' }[op]
  return (
    <Popup className="code-dialog" onClose={() => onChoose(null)}>
      <div className="code-popup-head">
        <span className="strong">
          {names.length === 1 ? `${names[0]} already exists` : `${names.length} names already exist`} in{' '}
          {to || 'the top folder'}
        </span>
      </div>
      <div className="code-dialog-body">
        {verb} would put {names.length === 1 ? 'another' : 'others'} there.
        {names.length > 1 && (
          <ul className="code-dialog-names">
            {names.slice(0, 8).map((n) => (
              <li key={n}>{n}</li>
            ))}
            {names.length > 8 && <li className="muted">and {names.length - 8} more</li>}
          </ul>
        )}
        <p className="muted small">What is replaced goes in the trash, and comes back with Undo.</p>
      </div>
      <div className="code-popup-actions">
        <span className="code-tool-spacer" />
        <button type="button" className="btn btn-ghost btn-sm" onClick={() => onChoose(null)}>
          Cancel
        </button>
        <button type="button" className="btn btn-ghost btn-sm" onClick={() => onChoose('skip')}>
          Skip
        </button>
        <button type="button" className="btn btn-ghost btn-sm" onClick={() => onChoose('rename')}>
          Keep Both
        </button>
        <button type="button" className="btn btn-primary btn-sm" autoFocus onClick={() => onChoose('replace')}>
          Replace
        </button>
      </div>
    </Popup>
  )
}
