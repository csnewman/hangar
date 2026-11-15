import { useUpdated } from '../update'

// UpdateNotice offers to reload a page whose server has a newer UI.
export function UpdateNotice() {
  const updated = useUpdated()
  if (!updated) return null
  return (
    <div className="update-notice" role="status">
      <span>Hangar has been updated.</span>
      <button className="btn btn-primary btn-sm" onClick={() => location.reload()}>
        Reload
      </button>
    </div>
  )
}
