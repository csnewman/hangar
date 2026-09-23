import { useEffect, useState } from 'react'

// ConfirmButton asks for a second click before doing something destructive,
// and forgets the first click after a few seconds.
export function ConfirmButton({
  label,
  confirmLabel = 'Confirm',
  onConfirm,
  disabled,
}: {
  label: string
  confirmLabel?: string
  onConfirm: () => void
  disabled?: boolean
}) {
  const [armed, setArmed] = useState(false)

  useEffect(() => {
    if (!armed) return
    const t = setTimeout(() => setArmed(false), 3000)
    return () => clearTimeout(t)
  }, [armed])

  return (
    <button
      type="button"
      className={armed ? 'btn btn-danger' : 'btn btn-ghost'}
      disabled={disabled}
      onClick={() => {
        if (armed) {
          setArmed(false)
          onConfirm()
        } else {
          setArmed(true)
        }
      }}
    >
      {armed ? confirmLabel : label}
    </button>
  )
}
