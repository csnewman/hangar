package registry

import (
	"context"
	"errors"
	"fmt"
	"io"
	"net/http"
	"os"
	"path/filepath"
	"strconv"
	"strings"
	"time"

	"github.com/jackc/pgx/v5"
	"github.com/opencontainers/go-digest"

	"github.com/csnewman/hangar/internal/db"
)

// blobPath is where a blob's bytes are: blobs/<algorithm>/<first two>/<hex>.
func (reg *Registry) blobPath(d digest.Digest) string {
	hex := d.Encoded()
	return filepath.Join(reg.dir, "blobs", string(d.Algorithm()), hex[:2], hex)
}

func (reg *Registry) uploadPath(id string) string {
	return filepath.Join(reg.dir, "uploads", id)
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
		f, err := os.Open(reg.blobPath(d))
		if err != nil {
			reg.fail(w, r, fmt.Errorf("blob %s is recorded but not on disk: %w", d, err))
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
	path := reg.uploadPath(id)
	switch r.Method {
	case http.MethodPatch:
		if !inOrder(w, r, name, id, path) {
			return
		}
		size, err := appendTo(path, r.Body)
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
		if !inOrder(w, r, name, id, path) {
			return
		}
		if _, err := appendTo(path, r.Body); err != nil {
			reg.fail(w, r, err)
			return
		}
		if err := reg.finish(ctx, a.id, path, d); err != nil {
			reg.fail(w, r, err)
			return
		}
		reg.endUpload(ctx, id)
		blobCreated(w, name, d)
	case http.MethodGet:
		size, err := fileSize(path)
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
		os.Remove(path)
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
		f, err := os.CreateTemp(filepath.Join(reg.dir, "uploads"), "whole-")
		if err != nil {
			reg.fail(w, r, err)
			return
		}
		_, err = io.Copy(f, r.Body)
		if cerr := f.Close(); err == nil {
			err = cerr
		}
		if err == nil {
			err = reg.finish(ctx, a.id, f.Name(), d)
		}
		os.Remove(f.Name())
		if err != nil {
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
	if err == nil {
		err = os.WriteFile(reg.uploadPath(id), nil, 0o644)
	}
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
	if err != nil {
		reg.log.Warn("registry: ending an upload", "upload", id, "err", err)
	}
}

// finish checks that the file at path has digest d, moves it into place as
// that blob -- or drops it, if the blob is already there -- and gives the
// repository the blob.
func (reg *Registry) finish(ctx context.Context, repo, path string, d digest.Digest) error {
	f, err := os.Open(path)
	if err != nil {
		return err
	}
	v := d.Verifier()
	size, err := io.Copy(v, f)
	f.Close()
	if err != nil {
		return err
	}
	if !v.Verified() {
		os.Remove(path)
		return failure(http.StatusBadRequest, "DIGEST_INVALID", "the upload's content does not match its digest")
	}
	dst := reg.blobPath(d)
	if _, err := os.Stat(dst); errors.Is(err, os.ErrNotExist) {
		if err := os.MkdirAll(filepath.Dir(dst), 0o755); err != nil {
			return err
		}
		if err := os.Rename(path, dst); err != nil {
			return err
		}
	} else {
		os.Remove(path)
	}
	return reg.db.Transact(ctx, func(tx db.Tx) error {
		if _, err := tx.Exec(ctx, `INSERT INTO registry_blobs (digest, size) VALUES ($1, $2) ON CONFLICT DO NOTHING`,
			d.String(), size); err != nil {
			return err
		}
		_, err := tx.Exec(ctx, `INSERT INTO image_repository_blobs (repository_id, digest) VALUES ($1, $2)
			ON CONFLICT DO NOTHING`, repo, d.String())
		return err
	})
}

// inOrder checks that a chunk's Content-Range, if it has one, starts where
// the upload has got to, and answers 416 if not.
func inOrder(w http.ResponseWriter, r *http.Request, name, id, path string) bool {
	cr := r.Header.Get("Content-Range")
	if cr == "" {
		return true
	}
	size, err := fileSize(path)
	if err != nil {
		size = 0
	}
	start, _, ok := strings.Cut(cr, "-")
	if n, err := strconv.ParseInt(start, 10, 64); ok && err == nil && n == size {
		return true
	}
	w.Header().Set("Range", rangeOf(size))
	w.Header().Set("Location", uploadLocation(name, id))
	writeErr(w, http.StatusRequestedRangeNotSatisfiable, "BLOB_UPLOAD_INVALID",
		fmt.Sprintf("the chunk starts at %s, and the upload has %d bytes", start, size))
	return false
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

func fileSize(path string) (int64, error) {
	st, err := os.Stat(path)
	if errors.Is(err, os.ErrNotExist) {
		return 0, failure(http.StatusNotFound, "BLOB_UPLOAD_UNKNOWN", "upload not found")
	}
	if err != nil {
		return 0, err
	}
	return st.Size(), nil
}

// appendTo adds what r holds to the end of the file at path, returning its
// new size.
func appendTo(path string, r io.Reader) (int64, error) {
	f, err := os.OpenFile(path, os.O_WRONLY|os.O_APPEND, 0)
	if errors.Is(err, os.ErrNotExist) {
		return 0, failure(http.StatusNotFound, "BLOB_UPLOAD_UNKNOWN", "upload not found")
	}
	if err != nil {
		return 0, err
	}
	if _, err := io.Copy(f, r); err != nil {
		f.Close()
		return 0, err
	}
	st, err := f.Stat()
	if cerr := f.Close(); err == nil {
		err = cerr
	}
	if err != nil {
		return 0, err
	}
	return st.Size(), nil
}
