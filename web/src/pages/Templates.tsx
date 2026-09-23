import { Globe, Lock, Plus, Users } from 'lucide-react'
import { Link } from 'react-router'

import type { Template } from '../api'
import { PageHeader } from '../components/PageHeader'
import { SpecChips } from '../components/SpecChips'
import { useMe } from '../session'
import { groupTemplates, useTemplates } from '../templates'

export function TemplatesPage() {
  const me = useMe()
  const templates = useTemplates()
  const { mine, collaborating, others } = groupTemplates(templates.data ?? [], me)

  return (
    <div className="page">
      <PageHeader
        title="Templates"
        subtitle="Recipes for environments: the image, repositories to clone, and how it runs."
        actions={
          <Link to="/templates/new" className="btn btn-primary">
            <Plus size={15} />
            New template
          </Link>
        }
      />

      {templates.isError && <div className="alert">Could not load templates: {templates.error.message}</div>}

      <TemplateSection
        title="Yours"
        templates={mine}
        loading={templates.isPending}
        empty={
          <>
            You have no templates. <Link to="/templates/new">Make one</Link> to stop starting from scratch.
          </>
        }
      />
      {collaborating.length > 0 && <TemplateSection title="Shared with you to edit" templates={collaborating} />}
      <TemplateSection
        title={me.admin ? 'Everyone else’s' : 'Shared by others'}
        templates={others}
        loading={templates.isPending}
        empty="Nobody else has shared a template."
      />
    </div>
  )
}

function TemplateSection({
  title,
  templates,
  loading = false,
  empty,
}: {
  title: string
  templates: Template[]
  loading?: boolean
  empty?: React.ReactNode
}) {
  return (
    <section className="section">
      <h2 className="section-title">
        {title} <span className="section-count">{templates.length}</span>
      </h2>
      {!loading && templates.length === 0 && empty && <div className="panel empty-state">{empty}</div>}
      <div className="cards">
        {templates.map((t) => (
          <TemplateCard key={t.id} template={t} />
        ))}
      </div>
    </section>
  )
}

export function VisibilityBadge({ template: t }: { template: Template }) {
  if (t.visibility === 'shared') {
    return (
      <span className="badge badge-good" title="Everyone can see and use it">
        <Globe size={11} />
        shared
      </span>
    )
  }
  return (
    <span className="badge badge-idle" title="Only its owner and collaborators can see it">
      <Lock size={11} />
      private
    </span>
  )
}

function TemplateCard({ template: t }: { template: Template }) {
  return (
    <div className="card">
      <div className="card-head">
        <Link to={`/templates/${t.id}`} className="card-title">
          {t.name}
        </Link>
        <VisibilityBadge template={t} />
      </div>
      {t.description && <p className="card-desc">{t.description}</p>}
      <SpecChips spec={t.spec} namePattern={t.name_pattern} />
      <div className="card-foot">
        <span className="muted small">
          by {t.owner.display_name || t.owner.username}
          {t.collaborators.length > 0 && (
            <span title={t.collaborators.map((c) => c.display_name || c.username).join(', ')}>
              {' '}
              <Users size={11} className="inline-icon" /> +{t.collaborators.length}
            </span>
          )}
        </span>
        <div className="actions">
          <Link to={`/templates/${t.id}`} className="btn btn-ghost">
            {t.can_edit ? 'Edit' : 'View'}
          </Link>
          <Link to={`/environments/new?template=${t.id}`} className="btn btn-primary">
            Use
          </Link>
        </div>
      </div>
    </div>
  )
}
