package registry

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"mime"
	"net/http"
	"net/url"
	"sort"
	"strconv"

	"github.com/jackc/pgx/v5"
	"github.com/opencontainers/go-digest"

	"github.com/csnewman/hangar/internal/audit"
	"github.com/csnewman/hangar/internal/db"
)

// The manifest kinds the registry keeps: images, which name blobs, and
// indexes, which name other manifests.
const (
	ociManifest    = "application/vnd.oci.image.manifest.v1+json"
	ociIndex       = "application/vnd.oci.image.index.v1+json"
	dockerManifest = "application/vnd.docker.distribution.manifest.v2+json"
	dockerList     = "application/vnd.docker.distribution.manifest.list.v2+json"
)

// foreignLayer are the media types of layers not distributed through
// registries.
var foreignLayer = map[string]bool{
	"application/vnd.oci.image.layer.nondistributable.v1.tar":      true,
	"application/vnd.oci.image.layer.nondistributable.v1.tar+gzip": true,
	"application/vnd.oci.image.layer.nondistributable.v1.tar+zstd": true,
	"application/vnd.docker.image.rootfs.foreign.diff.tar.gzip":    true,
}

// maxManifest is the largest manifest accepted, as registries generally
// limit them.
const maxManifest = 4 << 20

type descriptor struct {
	MediaType    string            `json:"mediaType"`
	Digest       string            `json:"digest"`
	Size         int64             `json:"size"`
	ArtifactType string            `json:"artifactType,omitempty"`
	Annotations  map[string]string `json:"annotations,omitempty"`
}

type manifest struct {
	SchemaVersion int               `json:"schemaVersion"`
	MediaType     string            `json:"mediaType"`
	ArtifactType  string            `json:"artifactType"`
	Config        *descriptor       `json:"config"`
	Layers        []descriptor      `json:"layers"`
	Manifests     []descriptor      `json:"manifests"`
	Subject       *descriptor       `json:"subject"`
	Annotations   map[string]string `json:"annotations"`
}

func (reg *Registry) manifests(ctx context.Context, w http.ResponseWriter, r *http.Request, c caller, name, ref string) {
	a, err := reg.open(ctx, c, name, r.Method == http.MethodPut)
	if err != nil {
		reg.fail(w, r, err)
		return
	}
	switch r.Method {
	case http.MethodGet, http.MethodHead:
		mt, d, content, err := reg.readManifest(ctx, a.id, ref)
		if err != nil {
			reg.fail(w, r, err)
			return
		}
		w.Header().Set("Content-Type", mt)
		w.Header().Set("Docker-Content-Digest", d)
		w.Header().Set("Content-Length", strconv.Itoa(len(content)))
		w.WriteHeader(http.StatusOK)
		if r.Method == http.MethodGet {
			w.Write(content)
		}
	case http.MethodPut:
		if !a.push {
			reg.fail(w, r, fmt.Errorf("%w: you may not push to %s", errDenied, name))
			return
		}
		d, subject, err := reg.putManifest(ctx, a, r, ref)
		if err != nil {
			reg.fail(w, r, err)
			return
		}
		w.Header().Set("Location", "/v2/"+name+"/manifests/"+d.String())
		w.Header().Set("Docker-Content-Digest", d.String())
		if subject != "" {
			w.Header().Set("OCI-Subject", subject)
		}
		w.Header().Set("Content-Length", "0")
		w.WriteHeader(http.StatusCreated)
	case http.MethodDelete:
		if !a.push {
			reg.fail(w, r, fmt.Errorf("%w: you may not delete from %s", errDenied, name))
			return
		}
		if err := reg.deleteManifest(ctx, a, ref); err != nil {
			reg.fail(w, r, err)
			return
		}
		w.WriteHeader(http.StatusAccepted)
	default:
		writeErr(w, http.StatusMethodNotAllowed, "UNSUPPORTED", "method not allowed")
	}
}

// readManifest finds a manifest by tag or digest.
func (reg *Registry) readManifest(ctx context.Context, repo, ref string) (mediaType, dgst string, content []byte, err error) {
	var q string
	if _, derr := digest.Parse(ref); derr == nil {
		q = `SELECT media_type, digest, content FROM image_manifests WHERE repository_id = $1 AND digest = $2`
	} else if validTag.MatchString(ref) {
		q = `SELECT m.media_type, m.digest, m.content FROM image_tags t
			JOIN image_manifests m ON m.repository_id = t.repository_id AND m.digest = t.digest
			WHERE t.repository_id = $1 AND t.tag = $2`
	} else {
		// Nothing can have a name that is neither.
		return "", "", nil, failure(http.StatusNotFound, "MANIFEST_UNKNOWN", "manifest not found")
	}
	err = reg.db.Transact(ctx, func(tx db.Tx) error {
		return tx.QueryRow(ctx, q, repo, ref).Scan(&mediaType, &dgst, &content)
	})
	if errors.Is(err, pgx.ErrNoRows) {
		err = failure(http.StatusNotFound, "MANIFEST_UNKNOWN", "manifest not found")
	}
	return mediaType, dgst, content, err
}

// putManifest checks and keeps a manifest, tagging it when ref is a tag.
// Everything it names must already be in the repository: an image's blobs,
// or an index's manifests. Its subject need not be.
func (reg *Registry) putManifest(ctx context.Context, a access, r *http.Request, ref string) (digest.Digest, string, error) {
	content, err := io.ReadAll(io.LimitReader(r.Body, maxManifest+1))
	if err != nil {
		return "", "", err
	}
	if len(content) > maxManifest {
		return "", "", failure(http.StatusRequestEntityTooLarge, "SIZE_INVALID", "the manifest is larger than 4 MiB")
	}
	var m manifest
	if err := json.Unmarshal(content, &m); err != nil {
		return "", "", failure(http.StatusBadRequest, "MANIFEST_INVALID", "the manifest is not JSON: "+err.Error())
	}
	mt, _, _ := mime.ParseMediaType(r.Header.Get("Content-Type"))
	if mt == "" {
		mt = m.MediaType
	}
	if m.MediaType != "" && m.MediaType != mt {
		return "", "", failure(http.StatusBadRequest, "MANIFEST_INVALID", "the manifest's mediaType is not its Content-Type")
	}

	// A digest reference names the digest's algorithm; a tag is sha256.
	var d digest.Digest
	tag := ""
	if want, derr := digest.Parse(ref); derr == nil {
		if !want.Algorithm().Available() {
			return "", "", failure(http.StatusBadRequest, "DIGEST_INVALID", "unsupported digest algorithm")
		}
		d = want.Algorithm().FromBytes(content)
		if d != want {
			return "", "", failure(http.StatusBadRequest, "DIGEST_INVALID", "the manifest's content does not match its digest")
		}
	} else if validTag.MatchString(ref) {
		tag = ref
		d = digest.FromBytes(content)
	} else {
		return "", "", failure(http.StatusBadRequest, "MANIFEST_INVALID", "invalid tag "+ref)
	}

	var blobs, manifests []string
	artifactType := m.ArtifactType
	switch mt {
	case ociManifest, dockerManifest:
		if m.Config == nil {
			return "", "", failure(http.StatusBadRequest, "MANIFEST_INVALID", "an image manifest needs a config")
		}
		blobs = append(blobs, m.Config.Digest)
		for _, l := range m.Layers {
			// A non-distributable layer is fetched from where its URLs
			// say, never pushed here.
			if !foreignLayer[l.MediaType] {
				blobs = append(blobs, l.Digest)
			}
		}
		if artifactType == "" {
			artifactType = m.Config.MediaType
		}
	case ociIndex, dockerList:
		for _, x := range m.Manifests {
			manifests = append(manifests, x.Digest)
		}
	default:
		return "", "", failure(http.StatusBadRequest, "MANIFEST_INVALID", "unsupported manifest type "+mt)
	}
	for _, ref := range append(append([]string{}, blobs...), manifests...) {
		if _, err := parseDigest(ref); err != nil {
			return "", "", failure(http.StatusBadRequest, "MANIFEST_INVALID", "the manifest names an invalid digest "+ref)
		}
	}
	var subject *string
	if m.Subject != nil {
		if _, err := parseDigest(m.Subject.Digest); err != nil {
			return "", "", failure(http.StatusBadRequest, "MANIFEST_INVALID", "the manifest's subject is not a digest")
		}
		subject = &m.Subject.Digest
	}
	var annotations []byte
	if len(m.Annotations) > 0 {
		if annotations, err = json.Marshal(m.Annotations); err != nil {
			return "", "", err
		}
	}

	err = reg.db.Transact(ctx, func(tx db.Tx) error {
		var missing string
		if len(blobs) > 0 {
			err := tx.QueryRow(ctx, `SELECT coalesce((SELECT d FROM unnest($2::text[]) d
				WHERE NOT EXISTS (SELECT 1 FROM image_repository_blobs WHERE repository_id = $1 AND digest = d) LIMIT 1), '')`,
				a.id, blobs).Scan(&missing)
			if err != nil {
				return err
			}
			if missing != "" {
				return failure(http.StatusBadRequest, "MANIFEST_BLOB_UNKNOWN", "the repository has no blob "+missing)
			}
		}
		if len(manifests) > 0 {
			err := tx.QueryRow(ctx, `SELECT coalesce((SELECT d FROM unnest($2::text[]) d
				WHERE NOT EXISTS (SELECT 1 FROM image_manifests WHERE repository_id = $1 AND digest = d) LIMIT 1), '')`,
				a.id, manifests).Scan(&missing)
			if err != nil {
				return err
			}
			if missing != "" {
				return failure(http.StatusBadRequest, "MANIFEST_BLOB_UNKNOWN", "the repository has no manifest "+missing)
			}
		}
		tagged, err := tx.Exec(ctx, `INSERT INTO image_manifests (repository_id, digest, media_type, artifact_type, subject,
			annotations, content) VALUES ($1, $2, $3, $4, $5, $6, $7) ON CONFLICT DO NOTHING`,
			a.id, d.String(), mt, artifactType, subject, annotations, content)
		if err != nil {
			return err
		}
		if tagged.RowsAffected() > 0 {
			if _, err := tx.Exec(ctx, `INSERT INTO image_manifest_refs (repository_id, digest, ref)
				SELECT $1, $2, unnest($3::text[]) ON CONFLICT DO NOTHING`,
				a.id, d.String(), append(append([]string{}, blobs...), manifests...)); err != nil {
				return err
			}
		}
		if tag != "" {
			if _, err := tx.Exec(ctx, `INSERT INTO image_tags (repository_id, tag, digest) VALUES ($1, $2, $3)
				ON CONFLICT (repository_id, tag) DO UPDATE SET digest = excluded.digest, updated_at = now()`,
				a.id, tag, d.String()); err != nil {
				return err
			}
		}
		if _, err := tx.Exec(ctx, `UPDATE image_repositories SET updated_at = now() WHERE id = $1`, a.id); err != nil {
			return err
		}
		details := map[string]any{"digest": d.String()}
		if tag != "" {
			details["tag"] = tag
		}
		return audit.Record(ctx, tx, audit.Event{Action: "image.push",
			Target: audit.Ref{Type: audit.KindImage, ID: a.path, Name: a.path}, Details: details})
	})
	if err != nil {
		return "", "", err
	}
	s := ""
	if subject != nil {
		s = *subject
	}
	return d, s, nil
}

// deleteManifest removes a manifest, with its tags, or a tag alone.
func (reg *Registry) deleteManifest(ctx context.Context, a access, ref string) error {
	return reg.db.Transact(ctx, func(tx db.Tx) error {
		var q string
		details := map[string]any{}
		if _, err := digest.Parse(ref); err == nil {
			q = `DELETE FROM image_manifests WHERE repository_id = $1 AND digest = $2`
			details["digest"] = ref
		} else if validTag.MatchString(ref) {
			q = `DELETE FROM image_tags WHERE repository_id = $1 AND tag = $2`
			details["tag"] = ref
		} else {
			return failure(http.StatusBadRequest, "MANIFEST_INVALID", "invalid reference "+ref)
		}
		tag, err := tx.Exec(ctx, q, a.id, ref)
		if err != nil {
			return err
		}
		if tag.RowsAffected() == 0 {
			return failure(http.StatusNotFound, "MANIFEST_UNKNOWN", "manifest not found")
		}
		return audit.Record(ctx, tx, audit.Event{Action: "image.delete",
			Target: audit.Ref{Type: audit.KindImage, ID: a.path, Name: a.path}, Details: details})
	})
}

// tags lists a repository's tags, in order, a page at a time.
func (reg *Registry) tags(ctx context.Context, w http.ResponseWriter, r *http.Request, c caller, name string) {
	if r.Method != http.MethodGet {
		writeErr(w, http.StatusMethodNotAllowed, "UNSUPPORTED", "method not allowed")
		return
	}
	a, err := reg.open(ctx, c, name, false)
	if err != nil {
		reg.fail(w, r, err)
		return
	}
	q := r.URL.Query()
	limit := -1
	if n := q.Get("n"); n != "" {
		if limit, err = strconv.Atoi(n); err != nil || limit < 0 {
			reg.fail(w, r, failure(http.StatusBadRequest, "PAGINATION_NUMBER_INVALID", "invalid n"))
			return
		}
	}
	var tags []string
	err = reg.db.Transact(ctx, func(tx db.Tx) error {
		rows, err := tx.Query(ctx, `SELECT tag FROM image_tags WHERE repository_id = $1 AND tag > $2 ORDER BY tag COLLATE "C"`,
			a.id, q.Get("last"))
		if err != nil {
			return err
		}
		tags, err = pgx.CollectRows(rows, pgx.RowTo[string])
		return err
	})
	if err != nil {
		reg.fail(w, r, err)
		return
	}
	sort.Strings(tags)
	if tags == nil {
		tags = []string{}
	}
	if limit >= 0 && len(tags) > limit {
		tags = tags[:limit]
		if limit > 0 {
			next := url.Values{"n": {strconv.Itoa(limit)}, "last": {tags[len(tags)-1]}}
			w.Header().Set("Link", `</v2/`+name+`/tags/list?`+next.Encode()+`>; rel="next"`)
		}
	}
	writeJSON(w, http.StatusOK, struct {
		Name string   `json:"name"`
		Tags []string `json:"tags"`
	}{name, tags})
}

// referrers lists the manifests whose subject is a digest, as an index.
func (reg *Registry) referrers(ctx context.Context, w http.ResponseWriter, r *http.Request, c caller, name, ref string) {
	if r.Method != http.MethodGet {
		writeErr(w, http.StatusMethodNotAllowed, "UNSUPPORTED", "method not allowed")
		return
	}
	d, err := parseDigest(ref)
	if err != nil {
		reg.fail(w, r, err)
		return
	}
	// Nothing refers to anything in a repository that does not exist, or
	// that the caller cannot see: the answer is the same empty list.
	a, err := reg.open(ctx, c, name, false)
	if err != nil && !errors.Is(err, errNameUnknown) {
		reg.fail(w, r, err)
		return
	}
	filter := r.URL.Query().Get("artifactType")
	list := []descriptor{}
	err = reg.db.Transact(ctx, func(tx db.Tx) error {
		if a.id == "" {
			return nil
		}
		rows, err := tx.Query(ctx, `SELECT media_type, digest, length(content), artifact_type, annotations
			FROM image_manifests WHERE repository_id = $1 AND subject = $2 AND ($3 = '' OR artifact_type = $3)
			ORDER BY digest`, a.id, d.String(), filter)
		if err != nil {
			return err
		}
		defer rows.Close()
		for rows.Next() {
			var x descriptor
			var annotations []byte
			if err := rows.Scan(&x.MediaType, &x.Digest, &x.Size, &x.ArtifactType, &annotations); err != nil {
				return err
			}
			if annotations != nil {
				if err := json.Unmarshal(annotations, &x.Annotations); err != nil {
					return err
				}
			}
			list = append(list, x)
		}
		return rows.Err()
	})
	if err != nil {
		reg.fail(w, r, err)
		return
	}
	if filter != "" {
		w.Header().Set("OCI-Filters-Applied", "artifactType")
	}
	w.Header().Set("Content-Type", ociIndex)
	w.WriteHeader(http.StatusOK)
	json.NewEncoder(w).Encode(struct {
		SchemaVersion int          `json:"schemaVersion"`
		MediaType     string       `json:"mediaType"`
		Manifests     []descriptor `json:"manifests"`
	}{2, ociIndex, list})
}
