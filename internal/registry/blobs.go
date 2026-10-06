package registry

import (
	"bytes"
	"context"
	"crypto/rand"
	"errors"
	"fmt"
	"io"
	"net/http"
	"strconv"
	"strings"
	"time"

	"github.com/jackc/pgx/v5"
	"github.com/opencontainers/go-digest"

	"github.com/csnewman/hangar/internal/blob"
	"github.com/csnewman/hangar/internal/db"
)

// blobKey is where a blob's bytes are kept: registry/blobs/<algorithm>/<hex>.
func blobKey(d digest.Digest) string {
	return "registry/blobs/" + string(d.Algorithm()) + "/" + d.Encoded()
}

// An upload in progress is its chunks, one object each, named by the
// offset each starts at, padded so they list in order. Any replica can take
// the next chunk.
func chunksPrefix(id string) string { return "registry/uploads/" + id + "/" }

func chunkKey(id string, offset int64) string {
	return fmt.Sprintf("%s%020d", chunksPrefix(id), offset)
}

// stagingKey is where an upload's whole content is written while its digest
// is checked, before it becomes the blob.
func stagingKey() string { return "registry/staging/" + rand.Text() }

// Expiries are the registry's objects its bucket deletes by age: staging
// copies and chunks of uploads left behind, which no upload uses once it is
// more than a day old (PruneUploads).
var Expiries = []blob.Expiry{
	{Prefix: "registry/staging/", Days: 2},
	{Prefix: "registry/uploads/", Days: 2},
}

// parseDigest reads a digest in a supported algorithm.
func parseDigest(s string) (digest.Digest, error) {
	d, err := digest.Parse(s)
	if err != nil || !d.Algorithm().Available() {
		return "", failure(http.StatusBadRequest, "DIGEST_INVALID", "invalid digest "+s)
	}
	return d, nil
}

// blobs serves one of a repository's blobs, or deletes it from the
// repository.
func (reg *Registry) blobs(ctx context.Context, w http.ResponseWriter, r *http.Request, c caller, name, ref string) {
	a, err := reg.open(ctx, c, name, false)
	if err != nil {
		reg.fail(w, r, err)
		return
	}
	d, err := parseDigest(ref)
	if err != nil {
		reg.fail(w, r, err)
		return
	}
	switch r.Method {
	case http.MethodGet, http.MethodHead:
		size, err := reg.hasBlob(ctx, a.id, d)
		if err != nil {
			reg.fail(w, r, err)
			return
		}
		f, err := reg.store.Get(ctx, blobKey(d))
		if err != nil {
			reg.fail(w, r, fmt.Errorf("blob %s is recorded but not in the blob store: %w", d, err))
			return
		}
		defer f.Close()
		w.Header().Set("Docker-Content-Digest", d.String())
		w.Header().Set("Content-Type", "application/octet-stream")
		w.Header().Set("Content-Length", strconv.FormatInt(size, 10))
		// ServeContent answers HEAD and ranges, and sets the length for
		// what it sends.
		http.ServeContent(w, r, "", time.Time{}, f)
	case http.MethodDelete:
		if !a.push {
			reg.fail(w, r, fmt.Errorf("%w: you may not delete from %s", errDenied, name))
			return
		}
		err := reg.db.Transact(ctx, func(tx db.Tx) error {
			var used bool
			if err := tx.QueryRow(ctx, `SELECT EXISTS (SELECT 1 FROM image_manifest_refs
				WHERE repository_id = $1 AND ref = $2)`, a.id, d.String()).Scan(&used); err != nil {
				return err
			}
			if used {
				return failure(http.StatusConflict, "DENIED", "a manifest in the repository refers to the blob")
			}
			tag, err := tx.Exec(ctx, `DELETE FROM image_repository_blobs WHERE repository_id = $1 AND digest = $2`,
				a.id, d.String())
			if err != nil {
				return err
			}
			if tag.RowsAffected() == 0 {
				return failure(http.StatusNotFound, "BLOB_UNKNOWN", "blob not found")
			}
			return nil
		})
		if err != nil {
			reg.fail(w, r, err)
			return
		}
		w.WriteHeader(http.StatusAccepted)
	default:
		writeErr(w, http.StatusMethodNotAllowed, "UNSUPPORTED", "method not allowed")
	}
}

// hasBlob returns the size of a blob the repository has.
func (reg *Registry) hasBlob(ctx context.Context, repo string, d digest.Digest) (int64, error) {
	var size int64
	err := reg.db.Transact(ctx, func(tx db.Tx) error {
		return tx.QueryRow(ctx, `SELECT b.size FROM image_repository_blobs rb JOIN registry_blobs b ON b.digest = rb.digest
			WHERE rb.repository_id = $1 AND rb.digest = $2`, repo, d.String()).Scan(&size)
	})
	if errors.Is(err, pgx.ErrNoRows) {
		return 0, failure(http.StatusNotFound, "BLOB_UNKNOWN", "blob not found")
	}
	return size, err
}

// uploads handles pushing a blob: starting an upload, in one request or
// several, sending its chunks, asking how far it has got, finishing it, and
// abandoning it. Starting one may instead mount a blob from another
// repository the caller may pull.
func (reg *Registry) uploads(ctx context.Context, w http.ResponseWriter, r *http.Request, c caller, name, id string) {
	a, err := reg.open(ctx, c, name, r.Method == http.MethodPost && id == "")
	if err != nil {
		reg.fail(w, r, err)
		return
	}
	if !a.push {
		reg.fail(w, r, fmt.Errorf("%w: you may not push to %s", errDenied, name))
		return
	}
	if id == "" {
		if r.Method != http.MethodPost {
			writeErr(w, http.StatusMethodNotAllowed, "UNSUPPORTED", "method not allowed")
			return
		}
		reg.startUpload(ctx, w, r, c, a)
		return
	}
	if err := reg.checkUpload(ctx, a.id, id); err != nil {
		reg.fail(w, r, err)
		return
	}
	switch r.Method {
	case http.MethodPatch:
		at, ok := reg.inOrder(ctx, w, r, name, id)
		if !ok {
			return
		}
		size, err := reg.appendChunk(ctx, id, at, r.Body)
		if err != nil {
			reg.fail(w, r, err)
			return
		}
		w.Header().Set("Location", uploadLocation(name, id))
		w.Header().Set("Range", rangeOf(size))
		w.Header().Set("Docker-Upload-UUID", id)
		w.WriteHeader(http.StatusAccepted)
	case http.MethodPut:
		d, err := parseDigest(r.URL.Query().Get("digest"))
		if err != nil {
			reg.fail(w, r, err)
			return
		}
		at, ok := reg.inOrder(ctx, w, r, name, id)
		if !ok {
			return
		}
		if _, err := reg.appendChunk(ctx, id, at, r.Body); err != nil {
			reg.fail(w, r, err)
			return
		}
		if err := reg.finish(ctx, a.id, reg.chunks(ctx, id), d); err != nil {
			reg.fail(w, r, err)
			return
		}
		reg.endUpload(ctx, id)
		blobCreated(w, name, d)
	case http.MethodGet:
		size, err := reg.uploadSize(ctx, id)
		if err != nil {
			reg.fail(w, r, err)
			return
		}
		w.Header().Set("Location", uploadLocation(name, id))
		w.Header().Set("Range", rangeOf(size))
		w.Header().Set("Docker-Upload-UUID", id)
		w.WriteHeader(http.StatusNoContent)
	case http.MethodDelete:
		reg.endUpload(ctx, id)
		w.WriteHeader(http.StatusNoContent)
	default:
		writeErr(w, http.StatusMethodNotAllowed, "UNSUPPORTED", "method not allowed")
	}
}

func (reg *Registry) startUpload(ctx context.Context, w http.ResponseWriter, r *http.Request, c caller, a access) {
	q := r.URL.Query()
	if mount := q.Get("mount"); mount != "" && q.Get("from") != "" {
		if d, err := parseDigest(mount); err == nil {
			if reg.mount(ctx, c, a, q.Get("from"), d) {
				blobCreated(w, a.path, d)
				return
			}
		}
		// A blob that cannot be mounted is uploaded instead.
	}
	if ds := q.Get("digest"); ds != "" {
		// The whole blob, in this request.
		d, err := parseDigest(ds)
		if err != nil {
			reg.fail(w, r, err)
			return
		}
		if err := reg.finish(ctx, a.id, r.Body, d); err != nil {
			reg.fail(w, r, err)
			return
		}
		blobCreated(w, a.path, d)
		return
	}
	var id string
	err := reg.db.Transact(ctx, func(tx db.Tx) error {
		return tx.QueryRow(ctx, `INSERT INTO registry_uploads (repository_id) VALUES ($1) RETURNING id::text`,
			a.id).Scan(&id)
	})
	if err != nil {
		reg.fail(w, r, err)
		return
	}
	w.Header().Set("Location", uploadLocation(a.path, id))
	w.Header().Set("Range", rangeOf(0))
	w.Header().Set("Docker-Upload-UUID", id)
	w.Header().Set("Content-Length", "0")
	w.WriteHeader(http.StatusAccepted)
}

// mount gives a repository a blob another has, if the caller may pull that
// one.
func (reg *Registry) mount(ctx context.Context, c caller, a access, from string, d digest.Digest) bool {
	src, err := reg.open(ctx, c, from, false)
	if err != nil {
		return false
	}
	if _, err := reg.hasBlob(ctx, src.id, d); err != nil {
		return false
	}
	err = reg.db.Transact(ctx, func(tx db.Tx) error {
		_, err := tx.Exec(ctx, `INSERT INTO image_repository_blobs (repository_id, digest) VALUES ($1, $2)
			ON CONFLICT DO NOTHING`, a.id, d.String())
		return err
	})
	return err == nil
}

func (reg *Registry) checkUpload(ctx context.Context, repo, id string) error {
	if !db.ValidUUID(id) {
		return failure(http.StatusNotFound, "BLOB_UPLOAD_UNKNOWN", "upload not found")
	}
	var ok bool
	err := reg.db.Transact(ctx, func(tx db.Tx) error {
		return tx.QueryRow(ctx, `SELECT EXISTS (SELECT 1 FROM registry_uploads WHERE id = $1 AND repository_id = $2)`,
			id, repo).Scan(&ok)
	})
	if err != nil {
		return err
	}
	if !ok {
		return failure(http.StatusNotFound, "BLOB_UPLOAD_UNKNOWN", "upload not found")
	}
	return nil
}

func (reg *Registry) endUpload(ctx context.Context, id string) {
	err := reg.db.Transact(ctx, func(tx db.Tx) error {
		_, err := tx.Exec(ctx, `DELETE FROM registry_uploads WHERE id = $1`, id)
		return err
	})
	if err == nil {
		err = reg.dropChunks(ctx, id)
	}
	if err != nil {
		reg.log.Warn("registry: ending an upload", "upload", id, "err", err)
	}
}

func (reg *Registry) dropChunks(ctx context.Context, id string) error {
	chunks, err := reg.store.List(ctx, chunksPrefix(id))
	if err != nil {
		return err
	}
	var errs []error
	for _, c := range chunks {
		errs = append(errs, reg.store.Delete(ctx, c.Key))
	}
	return errors.Join(errs...)
}

// finish checks that content has digest d, makes it that blob -- unless
// the blob is there already -- and gives the repository the blob. The
// content is written to a staging object while its digest is checked, so
// what is kept under a digest is only ever content that has it.
func (reg *Registry) finish(ctx context.Context, repo string, content io.Reader, d digest.Digest) error {
	staging := stagingKey()
	defer reg.store.Delete(context.WithoutCancel(ctx), staging)
	v := d.Verifier()
	counted := &counter{r: io.TeeReader(content, v)}
	if err := reg.store.Put(ctx, staging, counted, -1); err != nil {
		return err
	}
	if !v.Verified() {
		return failure(http.StatusBadRequest, "DIGEST_INVALID", "the upload's content does not match its digest")
	}
	return reg.db.Transact(ctx, func(tx db.Tx) error {
		// Held while the blob is put in place and recorded, so collecting
		// it cannot delete it meanwhile (see Collect).
		if err := lockBlob(ctx, tx, d); err != nil {
			return err
		}
		if _, err := reg.store.Stat(ctx, blobKey(d)); errors.Is(err, blob.ErrNotFound) {
			if err := reg.store.Copy(ctx, staging, blobKey(d)); err != nil {
				return err
			}
		} else if err != nil {
			return err
		}
		if _, err := tx.Exec(ctx, `INSERT INTO registry_blobs (digest, size) VALUES ($1, $2) ON CONFLICT DO NOTHING`,
			d.String(), counted.n); err != nil {
			return err
		}
		_, err := tx.Exec(ctx, `INSERT INTO image_repository_blobs (repository_id, digest) VALUES ($1, $2)
			ON CONFLICT DO NOTHING`, repo, d.String())
		return err
	})
}

type counter struct {
	r io.Reader
	n int64
}

func (c *counter) Read(p []byte) (int, error) {
	n, err := c.r.Read(p)
	c.n += int64(n)
	return n, err
}

// lockBlob holds, until the transaction ends, the lock that putting a blob
// in place and deleting it both take.
func lockBlob(ctx context.Context, tx db.Tx, d digest.Digest) error {
	_, err := tx.Exec(ctx, `SELECT pg_advisory_xact_lock(hashtext('registry.blob:' || $1))`, d.String())
	return err
}

// inOrder returns where an upload has got to, which a chunk's
// Content-Range, if it has one, must start at; if it does not, it answers
// 416.
func (reg *Registry) inOrder(ctx context.Context, w http.ResponseWriter, r *http.Request, name, id string) (int64, bool) {
	size, err := reg.uploadSize(ctx, id)
	if err != nil {
		reg.fail(w, r, err)
		return 0, false
	}
	cr := r.Header.Get("Content-Range")
	if cr == "" {
		return size, true
	}
	start, _, ok := strings.Cut(cr, "-")
	if n, err := strconv.ParseInt(start, 10, 64); ok && err == nil && n == size {
		return size, true
	}
	w.Header().Set("Range", rangeOf(size))
	w.Header().Set("Location", uploadLocation(name, id))
	writeErr(w, http.StatusRequestedRangeNotSatisfiable, "BLOB_UPLOAD_INVALID",
		fmt.Sprintf("the chunk starts at %s, and the upload has %d bytes", start, size))
	return 0, false
}

func blobCreated(w http.ResponseWriter, name string, d digest.Digest) {
	w.Header().Set("Location", "/v2/"+name+"/blobs/"+d.String())
	w.Header().Set("Docker-Content-Digest", d.String())
	w.Header().Set("Content-Length", "0")
	w.WriteHeader(http.StatusCreated)
}

func uploadLocation(name, id string) string { return "/v2/" + name + "/blobs/uploads/" + id }

// rangeOf is the Range header for an upload of size bytes: the span it
// holds, inclusive, which for an empty one is written 0-0.
func rangeOf(size int64) string {
	if size == 0 {
		return "0-0"
	}
	return "0-" + strconv.FormatInt(size-1, 10)
}

// uploadSize is how many bytes an upload holds: its chunks' sizes.
func (reg *Registry) uploadSize(ctx context.Context, id string) (int64, error) {
	chunks, err := reg.store.List(ctx, chunksPrefix(id))
	if err != nil {
		return 0, err
	}
	var size int64
	for _, c := range chunks {
		size += c.Size
	}
	return size, nil
}

// appendChunk keeps what r holds as the upload's chunk starting at offset,
// returning the upload's new size. An empty chunk is not kept.
func (reg *Registry) appendChunk(ctx context.Context, id string, offset int64, r io.Reader) (int64, error) {
	c := &counter{r: r}
	first := make([]byte, 1)
	if _, err := io.ReadFull(c, first); err == io.EOF {
		return offset, nil
	} else if err != nil {
		return 0, err
	}
	if err := reg.store.Put(ctx, chunkKey(id, offset), io.MultiReader(bytes.NewReader(first), c), -1); err != nil {
		return 0, err
	}
	return offset + c.n, nil
}

// chunks reads an upload's chunks in order, as one stream.
func (reg *Registry) chunks(ctx context.Context, id string) io.Reader {
	pr, pw := io.Pipe()
	go func() {
		list, err := reg.store.List(ctx, chunksPrefix(id))
		for _, c := range list {
			if err != nil {
				break
			}
			var f blob.Reader
			if f, err = reg.store.Get(ctx, c.Key); err != nil {
				break
			}
			_, err = io.Copy(pw, f)
			f.Close()
		}
		pw.CloseWithError(err)
	}()
	return pr
}
