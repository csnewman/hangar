export function Logo({ size = 22 }: { size?: number }) {
  return (
    <svg width={size} height={size} viewBox="0 0 32 32" aria-hidden="true">
      <rect width="32" height="32" rx="7" fill="var(--accent)" />
      <path d="M6 24V14l10-6 10 6v10h-4v-8H10v8z" fill="#fff" />
    </svg>
  )
}
