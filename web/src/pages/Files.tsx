import { PageHeader } from '../components/PageHeader'
import { PackList } from './Packs'

// FilesPage is the packs the user may use, the built-in profile first. A
// pack is a list of paths shared between environments; each is used
// through a copy of its files, the user's own or a shared one.
export function FilesPage() {
  return (
    <div className="page">
      <PageHeader
        title="Files"
        subtitle="Packs share files between environments: each is a list of paths, and an environment uses a copy of its files -- your own, or one shared with a team."
      />
      <PackList />
    </div>
  )
}
