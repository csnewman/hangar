package registry_test

import (
	"bytes"
	"context"
	"crypto/sha256"
	"encoding/hex"
	"errors"
	"fmt"
	"io"
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"testing"
	"time"

	"github.com/csnewman/hangar/internal/api"
	"github.com/csnewman/hangar/internal/blob"
	"github.com/csnewman/hangar/internal/db"
	"github.com/csnewman/hangar/internal/dbtest"
	"github.com/csnewman/hangar/internal/profile"
	"github.com/csnewman/hangar/internal/registry"
	"github.com/csnewman/hangar/internal/teams"
	"github.com/csnewman/hangar/internal/templates"
	"github.com/csnewman/hangar/internal/users"
	"github.com/csnewman/hangar/internal/workers"
)

var ctx = context.Background()

// host is the registry's own host, as image references name it.
const host = "registry.hangar.test:8443"

type world struct {
	t     *testing.T
	db    *db.DB
	srv   *httptest.Server
	reg   *registry.Registry
	blobs *blob.Memory
	users *users.Manager
	teams *teams.Manager
	admin users.Principal
	// profiles issues environments' registry credentials.
	profiles *profile.Store
}

// login is someone who talks to the registry: a user with an access token,
// or a worker with its credential.
type login struct {
	name, password string
	p              users.Principal
}

func open(t *testing.T) *world {
	t.Helper()
	d := dbtest.Open(t)
	um := users.NewManager(d)
	sealer, err := profile.NewSealer(bytes.Repeat([]byte{7}, 32))
	if err != nil {
		t.Fatal(err)
	}
	store := profile.NewStore(d, blob.NewMemory(), sealer)
	blobs := blob.NewMemory()
	reg, err := registry.New(registry.Config{DB: d, Blobs: blobs, Host: host, Tokens: um,
		Workers: workers.NewManager(d), Credentials: store})
	if err != nil {
		t.Fatal(err)
	}
	srv := httptest.NewServer(reg)
	t.Cleanup(srv.Close)
	w := &world{t: t, db: d, srv: srv, reg: reg, blobs: blobs, users: um, teams: teams.NewManager(d), profiles: store}
	w.admin = w.person("root", true).p
	return w
}

func (w *world) person(name string, admin bool) login {
	w.t.Helper()
	u, err := w.users.Create(ctx, users.NewUser{Username: name, Password: "password1", Admin: admin})
	if err != nil {
		w.t.Fatal(err)
	}
	tok, _, err := w.users.CreateToken(ctx, u.ID, "registry", 0)
	if err != nil {
		w.t.Fatal(err)
	}
	return login{name: name, password: tok, p: users.Principal{UserID: u.ID, Username: name, Admin: admin}}
}

func (w *world) do(l login, method, path string, body []byte, header ...string) *http.Response {
	w.t.Helper()
	req, err := http.NewRequest(method, w.srv.URL+path, bytes.NewReader(body))
	if err != nil {
		w.t.Fatal(err)
	}
	if l.name != "" {
		req.SetBasicAuth(l.name, l.password)
	}
	for i := 0; i+1 < len(header); i += 2 {
		req.Header.Set(header[i], header[i+1])
	}
	resp, err := http.DefaultClient.Do(req)
	if err != nil {
		w.t.Fatal(err)
	}
	io.Copy(io.Discard, resp.Body)
	resp.Body.Close()
	return resp
}

func sha(b []byte) string {
	sum := sha256.Sum256(b)
	return "sha256:" + hex.EncodeToString(sum[:])
}

// push pushes a one-layer image to repo:tag and returns the status of the
// last request that failed, or of the manifest's.
func (w *world) push(l login, repo, tag string) int {
	w.t.Helper()
	return w.pushLayer(l, repo, tag, "layer of "+repo)
}

// pushLayer pushes an image whose one layer holds content.
func (w *world) pushLayer(l login, repo, tag, content string) int {
	w.t.Helper()
	config := []byte(`{"architecture":"arm64","os":"linux","rootfs":{"type":"layers","diff_ids":[]}}`)
	layer := []byte(content)
	for _, blob := range [][]byte{config, layer} {
		resp := w.do(l, http.MethodPost, "/v2/"+repo+"/blobs/uploads/?digest="+sha(blob), blob)
		if resp.StatusCode != http.StatusCreated {
			return resp.StatusCode
		}
	}
	m := fmt.Sprintf(`{"schemaVersion":2,"mediaType":"application/vnd.oci.image.manifest.v1+json",`+
		`"config":{"mediaType":"application/vnd.oci.image.config.v1+json","digest":%q,"size":%d},`+
		`"layers":[{"mediaType":"application/vnd.oci.image.layer.v1.tar","digest":%q,"size":%d}]}`,
		sha(config), len(config), sha(layer), len(layer))
	return w.do(l, http.MethodPut, "/v2/"+repo+"/manifests/"+tag, []byte(m),
		"Content-Type", "application/vnd.oci.image.manifest.v1+json").StatusCode
}

func (w *world) pull(l login, repo, tag string) int {
	w.t.Helper()
	return w.do(l, http.MethodGet, "/v2/"+repo+"/manifests/"+tag, nil).StatusCode
}

func (w *world) repo(p users.Principal, path string) registry.Repository {
	w.t.Helper()
	list, err := w.reg.List(ctx, p)
	if err != nil {
		w.t.Fatal(err)
	}
	for _, x := range list {
		if x.Path == path {
			return x
		}
	}
	w.t.Fatalf("%s cannot see %s", p.Username, path)
	return registry.Repository{}
}

// Signing in takes a user's access token under their own username, or a
// worker's credential.
func TestSignIn(t *testing.T) {
	w := open(t)
	alice, bob := w.person("alice", false), w.person("bob", false)

	if got := w.do(login{}, http.MethodGet, "/v2/", nil); got.StatusCode != 401 || got.Header.Get("WWW-Authenticate") == "" {
		t.Errorf("no credentials: %d, challenge %q", got.StatusCode, got.Header.Get("WWW-Authenticate"))
	}
	if got := w.do(alice, http.MethodGet, "/v2/", nil).StatusCode; got != 200 {
		t.Errorf("alice's token: %d", got)
	}
	if got := w.do(login{name: "bob", password: alice.password}, http.MethodGet, "/v2/", nil).StatusCode; got != 401 {
		t.Errorf("alice's token under bob's name: %d, want 401", got)
	}
	if got := w.do(login{name: "bob", password: "hgr_nonsense"}, http.MethodGet, "/v2/", nil).StatusCode; got != 401 {
		t.Errorf("a made-up token: %d, want 401", got)
	}
	if got := w.do(bob, http.MethodGet, "/v2/", nil).StatusCode; got != 200 {
		t.Errorf("bob's own token: %d", got)
	}
}

// A user pushes to their own namespace and a team's they are a member or
// admin of, and nowhere else; an administrator anywhere.
func TestNamespaces(t *testing.T) {
	w := open(t)
	alice, bob := w.person("alice", false), w.person("bob", false)
	root := login{name: "root"}
	tok, _, _ := w.users.CreateToken(ctx, w.admin.UserID, "r", 0)
	root.password = tok
	tm, err := w.teams.Create(ctx, w.admin, teams.Input{Slug: "platform", Name: "Platform"})
	if err != nil {
		t.Fatal(err)
	}
	if _, err := w.teams.SetMember(ctx, w.admin, tm.ID, alice.p.UserID, teams.Member); err != nil {
		t.Fatal(err)
	}
	if _, err := w.teams.SetMember(ctx, w.admin, tm.ID, bob.p.UserID, teams.Viewer); err != nil {
		t.Fatal(err)
	}

	for _, c := range []struct {
		who  login
		repo string
		want int
	}{
		{alice, "alice/app", 201},
		{alice, "bob/app", 403},
		{alice, "nobody/app", 403},
		{alice, "platform/base", 201},
		{bob, "platform/other", 403},
		{root, "bob/made-by-root", 201},
		{alice, "Alice/Upper", 400},
	} {
		if got := w.push(c.who, c.repo, "v1"); got != c.want {
			t.Errorf("%s pushing %s: %d, want %d", c.who.name, c.repo, got, c.want)
		}
	}
	if x := w.repo(w.admin, "platform/base"); x.Team == nil || x.Team.Slug != "platform" || x.Owner != nil {
		t.Errorf("platform/base is owned by %+v, team %+v", x.Owner, x.Team)
	}
	if x := w.repo(w.admin, "bob/made-by-root"); x.Owner == nil || x.Owner.Username != "bob" {
		t.Errorf("bob/made-by-root is owned by %+v", x.Owner)
	}
}

// A private repository is pulled only by those it is shared with; shared,
// by everyone; and a worker pulls anything.
func TestPull(t *testing.T) {
	w := open(t)
	alice, bob, carol := w.person("alice", false), w.person("bob", false), w.person("carol", false)
	if got := w.push(alice, "alice/app", "v1"); got != 201 {
		t.Fatalf("push: %d", got)
	}
	if got := w.pull(alice, "alice/app", "v1"); got != 200 {
		t.Errorf("the owner pulling: %d", got)
	}
	if got := w.pull(bob, "alice/app", "v1"); got != 404 {
		t.Errorf("another user pulling a private repository: %d, want 404", got)
	}
	cred, err := workers.NewManager(w.db).Register(ctx, api.RegisterWorker{Name: "w"})
	if err != nil {
		t.Fatal(err)
	}
	if got := w.pull(login{name: "hangar-worker", password: cred.Credential}, "alice/app", "v1"); got != 200 {
		t.Errorf("a worker pulling: %d", got)
	}

	x := w.repo(alice.p, "alice/app")
	if _, err := w.reg.Update(ctx, bob.p, x.ID, "shared", ""); !errors.Is(err, registry.ErrNotFound) {
		t.Errorf("bob changing a repository they cannot see: %v", err)
	}

	// A collaborator pushes; a collaborating team's viewer pulls.
	tm, _ := w.teams.Create(ctx, w.admin, teams.Input{Slug: "qa", Name: "QA"})
	w.teams.SetMember(ctx, w.admin, tm.ID, carol.p.UserID, teams.Viewer)
	if _, err := w.reg.SetCollaborators(ctx, alice.p, x.ID, []string{bob.p.UserID}, []string{tm.ID}); err != nil {
		t.Fatal(err)
	}
	if got := w.push(bob, "alice/app", "v2"); got != 201 {
		t.Errorf("a collaborator pushing: %d", got)
	}
	if got := w.pull(carol, "alice/app", "v2"); got != 200 {
		t.Errorf("a collaborating team's viewer pulling: %d", got)
	}
	if got := w.push(carol, "alice/app", "v3"); got != 403 {
		t.Errorf("a collaborating team's viewer pushing: %d, want 403", got)
	}
	if _, err := w.reg.Update(ctx, bob.p, x.ID, "shared", ""); !errors.Is(err, registry.ErrForbidden) {
		t.Errorf("a collaborator changing visibility: %v, want ErrForbidden", err)
	}

	stranger := w.person("dave", false)
	if _, err := w.reg.Update(ctx, alice.p, x.ID, "shared", "an app"); err != nil {
		t.Fatal(err)
	}
	if got := w.pull(stranger, "alice/app", "v1"); got != 200 {
		t.Errorf("anyone pulling a shared repository: %d", got)
	}
	if got := w.push(stranger, "alice/app", "v9"); got != 403 {
		t.Errorf("anyone pushing to a shared repository: %d, want 403", got)
	}

	got, err := w.reg.Get(ctx, alice.p, x.ID)
	if err != nil || len(got.Tags) != 2 || got.Tags[0].Size == 0 {
		t.Fatalf("tags %+v, %v", got.Tags, err)
	}
	if _, err := w.reg.DeleteTag(ctx, alice.p, x.ID, "v1"); err != nil {
		t.Fatal(err)
	}
	if got := w.pull(alice, "alice/app", "v1"); got != 404 {
		t.Errorf("a deleted tag: %d, want 404", got)
	}
	if err := w.reg.Delete(ctx, bob.p, x.ID); !errors.Is(err, registry.ErrForbidden) {
		t.Errorf("a collaborator deleting the repository: %v", err)
	}
	if err := w.reg.Delete(ctx, alice.p, x.ID); err != nil {
		t.Fatal(err)
	}
	if got := w.pull(alice, "alice/app", "v2"); got != 404 {
		t.Errorf("a deleted repository: %d, want 404", got)
	}
}

// A template may name an image in the registry only if whoever saves it may
// pull it: workers pull whatever a template names, so otherwise a template
// would reach an image its author cannot.
func TestTemplateImages(t *testing.T) {
	w := open(t)
	alice, bob := w.person("alice", false), w.person("bob", false)
	if got := w.push(alice, "alice/app", "v1"); got != 201 {
		t.Fatalf("push: %d", got)
	}
	tm := templates.NewManager(w.db)
	tm.CheckImagesWith(w.reg)
	in := func(image string) templates.Input {
		return templates.Input{Name: "t " + image, Spec: api.TemplateSpec{Spec: api.Spec{Image: image, CPUs: 1, MemoryMiB: 1024}}}
	}
	private := host + "/alice/app:v1"
	if _, err := tm.Create(ctx, bob.p, in(private)); !errors.Is(err, templates.ErrInvalid) {
		t.Errorf("bob naming alice's private image: %v, want ErrInvalid", err)
	}
	if _, err := tm.Create(ctx, alice.p, in(private)); err != nil {
		t.Errorf("alice naming an image of their own: %v", err)
	}
	if _, err := tm.Create(ctx, bob.p, in("ghcr.io/somewhere/else:v1")); err != nil {
		t.Errorf("an image in another registry: %v", err)
	}
	x := w.repo(alice.p, "alice/app")
	if _, err := w.reg.Update(ctx, alice.p, x.ID, "shared", ""); err != nil {
		t.Fatal(err)
	}
	if _, err := tm.Create(ctx, bob.p, in(private)); err != nil {
		t.Errorf("bob naming a shared image: %v", err)
	}
}

// Collecting deletes what no tag keeps, and leaves what is newer than its
// grace.
func TestCollect(t *testing.T) {
	w := open(t)
	alice := w.person("alice", false)
	if got := w.pushLayer(alice, "alice/app", "v1", "first"); got != 201 {
		t.Fatalf("push v1: %d", got)
	}
	if got := w.pushLayer(alice, "alice/app", "v2", "second"); got != 201 {
		t.Fatalf("push v2: %d", got)
	}
	stray := []byte("pushed, never named by a manifest")
	if got := w.do(alice, http.MethodPost, "/v2/alice/app/blobs/uploads/?digest="+sha(stray), stray).StatusCode; got != 201 {
		t.Fatalf("stray blob: %d", got)
	}
	blob := func(content []byte) int {
		return w.do(alice, http.MethodHead, "/v2/alice/app/blobs/"+sha(content), nil).StatusCode
	}

	// Within the grace, nothing goes.
	if c, err := w.reg.Collect(ctx, time.Hour); err != nil || c.Manifests != 0 || c.Blobs != 0 {
		t.Fatalf("collecting within the grace: %+v, %v", c, err)
	}
	if blob(stray) != 200 {
		t.Fatal("a new blob was collected")
	}

	x := w.repo(alice.p, "alice/app")
	if _, err := w.reg.DeleteTag(ctx, alice.p, x.ID, "v1"); err != nil {
		t.Fatal(err)
	}
	c, err := w.reg.Collect(ctx, 0)
	if err != nil {
		t.Fatal(err)
	}
	if c.Manifests != 1 || c.Blobs != 2 || c.Bytes != int64(len("first")+len(stray)) {
		t.Errorf("collected %+v; want v1's manifest, and its layer and the stray blob", c)
	}
	if blob([]byte("first")) != 404 || blob(stray) != 404 {
		t.Error("an unused blob is still served")
	}
	if blob([]byte("second")) != 200 || w.pull(alice, "alice/app", "v2") != 200 {
		t.Error("the tagged image lost something")
	}
	// Pushed again, a collected blob is kept afresh.
	if got := w.pushLayer(alice, "alice/app", "v1", "first"); got != 201 {
		t.Errorf("pushing a collected layer again: %d", got)
	}
	if blob([]byte("first")) != 200 {
		t.Error("a blob pushed again is not served")
	}
}

// An environment's credential acts as its owner, under the owner's
// username, and never as an administrator.
func TestEnvironmentCredential(t *testing.T) {
	w := open(t)
	alice := w.person("alice", false)
	cred, err := w.profiles.IssueRegistryCredential(alice.p.UserID, "some-environment")
	if err != nil {
		t.Fatal(err)
	}
	env := login{name: "alice", password: cred}
	if got := w.push(env, "alice/app", "v1"); got != 201 {
		t.Errorf("pushing with the environment's credential: %d", got)
	}
	if got := w.do(login{name: "root", password: cred}, http.MethodGet, "/v2/", nil).StatusCode; got != 401 {
		t.Errorf("the credential under another username: %d, want 401", got)
	}
	tampered := cred[:len(cred)-2] + "AA"
	if got := w.do(login{name: "alice", password: tampered}, http.MethodGet, "/v2/", nil).StatusCode; got != 401 {
		t.Errorf("a tampered credential: %d, want 401", got)
	}

	rootCred, err := w.profiles.IssueRegistryCredential(w.admin.UserID, "an-admin-environment")
	if err != nil {
		t.Fatal(err)
	}
	if got := w.push(login{name: "root", password: rootCred}, "alice/other", "v1"); got != 403 {
		t.Errorf("an administrator's environment pushing to alice's namespace: %d, want 403", got)
	}
}

// keys are the blob store's keys under prefix.
func (w *world) keys(prefix string) []string {
	w.t.Helper()
	list, err := w.blobs.List(ctx, prefix)
	if err != nil {
		w.t.Fatal(err)
	}
	var out []string
	for _, o := range list {
		out = append(out, o.Key)
	}
	return out
}

func TestChunkedUpload(t *testing.T) {
	w := open(t)
	alice := w.person("alice", false)
	start := w.do(alice, http.MethodPost, "/v2/alice/app/blobs/uploads/", nil)
	if start.StatusCode != http.StatusAccepted {
		t.Fatalf("starting an upload: %d", start.StatusCode)
	}
	at := start.Header.Get("Location")
	content := []byte("first part, second part")
	if r := w.do(alice, http.MethodPatch, at, content[:11], "Content-Range", "0-10"); r.StatusCode != http.StatusAccepted || r.Header.Get("Range") != "0-10" {
		t.Fatalf("first chunk: %d, range %q", r.StatusCode, r.Header.Get("Range"))
	}
	if r := w.do(alice, http.MethodPatch, at, content[11:], "Content-Range", "5-20"); r.StatusCode != http.StatusRequestedRangeNotSatisfiable || r.Header.Get("Range") != "0-10" {
		t.Errorf("a chunk out of order: %d, range %q", r.StatusCode, r.Header.Get("Range"))
	}
	if r := w.do(alice, http.MethodGet, at, nil); r.StatusCode != http.StatusNoContent || r.Header.Get("Range") != "0-10" {
		t.Errorf("asking how far: %d, range %q", r.StatusCode, r.Header.Get("Range"))
	}
	if r := w.do(alice, http.MethodPut, at+"?digest="+sha(content), content[11:]); r.StatusCode != http.StatusCreated {
		t.Fatalf("finishing: %d", r.StatusCode)
	}
	data, err := blob.ReadAll(ctx, w.blobs, "registry/blobs/sha256/"+sha(content)[len("sha256:"):])
	if err != nil || !bytes.Equal(data, content) {
		t.Errorf("kept %q, %v", data, err)
	}
	if left := w.keys("registry/uploads/"); len(left) != 0 {
		t.Errorf("chunks left: %v", left)
	}
	if left := w.keys("registry/staging/"); len(left) != 0 {
		t.Errorf("staging left: %v", left)
	}
}

func TestContentWithTheWrongDigestIsNotKept(t *testing.T) {
	w := open(t)
	alice := w.person("alice", false)
	r := w.do(alice, http.MethodPost, "/v2/alice/app/blobs/uploads/?digest="+sha([]byte("claimed")), []byte("actual"))
	if r.StatusCode != http.StatusBadRequest {
		t.Errorf("status %d", r.StatusCode)
	}
	if left := w.keys("registry/"); len(left) != 0 {
		t.Errorf("kept: %v", left)
	}
}

func TestMoveFromDir(t *testing.T) {
	w := open(t)
	dir := t.TempDir()
	content := []byte("a layer from a directory")
	hex := sha(content)[len("sha256:"):]
	path := filepath.Join(dir, "blobs", "sha256", hex[:2], hex)
	os.MkdirAll(filepath.Dir(path), 0o755)
	os.WriteFile(path, content, 0o644)
	os.MkdirAll(filepath.Join(dir, "uploads"), 0o755)
	os.WriteFile(filepath.Join(dir, "uploads", "abandoned"), []byte("x"), 0o644)
	if n, err := w.reg.MoveFromDir(ctx, dir); err != nil || n != 1 {
		t.Fatalf("moved %d, %v", n, err)
	}
	data, err := blob.ReadAll(ctx, w.blobs, "registry/blobs/sha256/"+hex)
	if err != nil || !bytes.Equal(data, content) {
		t.Errorf("kept %q, %v", data, err)
	}
	if _, err := os.Stat(path); !errors.Is(err, os.ErrNotExist) {
		t.Errorf("the file is still there: %v", err)
	}
	if _, err := os.Stat(filepath.Join(dir, "uploads")); !errors.Is(err, os.ErrNotExist) {
		t.Errorf("the uploads are still there: %v", err)
	}
	if n, err := w.reg.MoveFromDir(ctx, dir); err != nil || n != 0 {
		t.Errorf("moved %d again, %v", n, err)
	}
}
