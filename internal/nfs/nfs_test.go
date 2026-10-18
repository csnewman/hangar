package nfs_test

import (
	"encoding/binary"
	"fmt"
	"io"
	"net"
	"os"
	"path/filepath"
	"syscall"
	"testing"

	"github.com/csnewman/hangar/internal/nfs"
)

// view is a fixed one.
type view struct {
	sets   []string
	hidden map[string]bool // "set/path"
	squash map[string]nfs.Squash
}

func (v view) Sets() []string               { return v.sets }
func (v view) Hidden(set, path string) bool { return v.hidden[set+"/"+path] }
func (v view) Squash(set string) nfs.Squash { return v.squash[set] }

// enc writes XDR, as a client sends it.
type enc struct{ b []byte }

func (e *enc) u32(v uint32) *enc { e.b = binary.BigEndian.AppendUint32(e.b, v); return e }
func (e *enc) u64(v uint64) *enc { e.b = binary.BigEndian.AppendUint64(e.b, v); return e }
func (e *enc) fixed(v []byte) *enc {
	e.b = append(e.b, v...)
	for len(e.b)%4 != 0 {
		e.b = append(e.b, 0)
	}
	return e
}
func (e *enc) opaque(v []byte) *enc { return e.u32(uint32(len(v))).fixed(v) }
func (e *enc) str(v string) *enc    { return e.opaque([]byte(v)) }

// dec reads XDR, as a client reads it.
type dec struct {
	t *testing.T
	b []byte
}

func (d *dec) u32() uint32 {
	d.t.Helper()
	if len(d.b) < 4 {
		d.t.Fatal("a reply too short")
	}
	v := binary.BigEndian.Uint32(d.b)
	d.b = d.b[4:]
	return v
}
func (d *dec) u64() uint64 { return uint64(d.u32())<<32 | uint64(d.u32()) }
func (d *dec) fixed(n int) []byte {
	d.t.Helper()
	p := (n + 3) &^ 3
	if len(d.b) < p {
		d.t.Fatal("a reply too short")
	}
	v := d.b[:n]
	d.b = d.b[p:]
	return v
}
func (d *dec) opaque() []byte { return d.fixed(int(d.u32())) }

// client speaks NFSv4.1 to a server over a pipe, one call at a time.
type client struct {
	t       *testing.T
	conn    net.Conn
	xid     uint32
	session []byte
	seq     uint32
	id      uint64
	// uid is who calls are from, as AUTH_SYS says: 1000 unless set, or
	// root with asRoot.
	uid    uint32
	asRoot bool
}

func connect(t *testing.T, srv *nfs.Server, name string) *client {
	t.Helper()
	a, b := net.Pipe()
	go srv.Serve(b)
	t.Cleanup(func() { a.Close() })
	c := &client{t: t, conn: a}

	// EXCHANGE_ID, then CREATE_SESSION.
	var ops enc
	ops.u32(42).fixed(make([]byte, 8)).str(name).u32(0).u32(0).u32(0)
	d := c.compound(ops.b, 1)
	c.expectOK(d, 42)
	c.id = d.u64()
	seq := d.u32()

	ops = enc{}
	ops.u32(43).u64(c.id).u32(seq).u32(0)
	for range 2 { // fore and back channel attributes
		ops.u32(0).u32(1 << 20).u32(1 << 20).u32(4096).u32(16).u32(8).u32(0)
	}
	ops.u32(0x40000000).u32(1).u32(0) // callback program; AUTH_NONE
	d = c.compound(ops.b, 1)
	c.expectOK(d, 43)
	c.session = append([]byte{}, d.fixed(16)...)
	return c
}

// compound sends ops, n of them, and returns the reply after its status,
// tag and count, checking the count is n when it succeeded.
func (c *client) compound(ops []byte, n uint32) *dec {
	c.t.Helper()
	c.xid++
	var call enc
	call.u32(c.xid).u32(0).u32(2).u32(100003).u32(4).u32(1)
	// AUTH_SYS as uid 1000, and no verifier.
	var cred enc
	uid := c.uid
	if uid == 0 && !c.asRoot {
		uid = 1000
	}
	cred.u32(0).str("test").u32(uid).u32(uid).u32(0)
	call.u32(1).opaque(cred.b).u32(0).u32(0)
	call.str("").u32(1).u32(n)
	call.b = append(call.b, ops...)
	hdr := binary.BigEndian.AppendUint32(nil, 1<<31|uint32(len(call.b)))
	if _, err := c.conn.Write(append(hdr, call.b...)); err != nil {
		c.t.Fatal(err)
	}
	var rh [4]byte
	if _, err := io.ReadFull(c.conn, rh[:]); err != nil {
		c.t.Fatal(err)
	}
	body := make([]byte, binary.BigEndian.Uint32(rh[:])&^(1<<31))
	if _, err := io.ReadFull(c.conn, body); err != nil {
		c.t.Fatal(err)
	}
	d := &dec{t: c.t, b: body}
	if d.u32() != c.xid || d.u32() != 1 || d.u32() != 0 {
		c.t.Fatal("not an accepted reply to the call")
	}
	d.u32()
	d.opaque()
	if st := d.u32(); st != 0 {
		c.t.Fatalf("RPC status %d", st)
	}
	d.u32() // compound status
	d.opaque()
	d.u32()
	return d
}

func (c *client) expectOK(d *dec, op uint32) {
	c.t.Helper()
	if got, st := d.u32(), d.u32(); got != op || st != 0 {
		c.t.Fatalf("op %d: status %d (op %d)", op, st, got)
	}
}

// seqOp is a SEQUENCE, first in a compound.
func (c *client) seqOp(e *enc) {
	c.seq++
	e.u32(53).fixed(c.session).u32(c.seq).u32(0).u32(0).u32(0)
}

// run sends SEQUENCE and ops, and returns the status of the first that
// failed, or 0, with the reply after SEQUENCE.
func (c *client) run(ops func(e *enc) uint32) (uint32, *dec) {
	c.t.Helper()
	var e enc
	c.seqOp(&e)
	n := ops(&e)
	d := c.compound(e.b, n+1)
	c.expectOK(d, 53)
	d.fixed(16)
	for range 5 {
		d.u32()
	}
	return 0, d
}

// status reads one op's result header, giving its status.
func status(d *dec) uint32 {
	d.u32()
	return d.u32()
}

func path(e *enc, parts ...string) {
	e.u32(24) // PUTROOTFH
	for _, p := range parts {
		e.u32(15).str(p) // LOOKUP
	}
}

// makeSet makes a set's directory as the server would: the environments'
// user's, where the test is run as root to act as them.
func makeSet(root, set string) {
	os.MkdirAll(filepath.Join(root, set), 0o755)
	os.Chown(filepath.Join(root, set), 1000, 1000)
}

func newServer(t *testing.T, v view) (*nfs.Server, string) {
	t.Helper()
	root := t.TempDir()
	srv, err := nfs.New(nfs.Config{Root: root, View: v, UID: 1000, GID: 1000})
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(srv.Close)
	return srv, root
}

// open creates and opens name in set for writing, returning its stateid.
func (c *client) open(set, name string) []byte {
	c.t.Helper()
	_, d := c.run(func(e *enc) uint32 {
		path(e, set)
		// OPEN: seqid, both access, no deny, owner, create unchecked with
		// no attributes, by name.
		e.u32(18).u32(0).u32(3).u32(0).u64(c.id).str("owner").u32(1).u32(0).u32(0).u32(0).u32(0).str(name)
		return 3
	})
	if st := status(d); st != 0 {
		c.t.Fatalf("PUTROOTFH: %d", st)
	}
	if st := status(d); st != 0 {
		c.t.Fatalf("LOOKUP %s: %d", set, st)
	}
	if st := status(d); st != 0 {
		c.t.Fatalf("OPEN %s: %d", name, st)
	}
	return append([]byte{}, d.fixed(16)...)
}

func TestFilesThroughTheView(t *testing.T) {
	srv, root := newServer(t, view{sets: []string{"mine"}, hidden: map[string]bool{"mine/secret": true}})
	makeSet(root, "theirs")
	os.WriteFile(filepath.Join(root, "theirs", "f"), []byte("x"), 0o644)
	makeSet(root, "mine")
	os.WriteFile(filepath.Join(root, "mine", "secret"), []byte("x"), 0o644)
	c := connect(t, srv, "a")

	// Another's set, and a hidden file, are not there.
	for _, p := range [][]string{{"theirs"}, {"mine", "secret"}} {
		_, d := c.run(func(e *enc) uint32 { path(e, p...); return uint32(len(p) + 1) })
		status(d)
		for i := range p {
			if st := status(d); i == len(p)-1 && st != 2 {
				t.Errorf("%v: status %d, not NOENT", p, st)
			}
		}
	}

	// Written and read back.
	sid := c.open("mine", "f")
	_, d := c.run(func(e *enc) uint32 {
		path(e, "mine", "f")
		e.u32(38).fixed(sid).u64(0).u32(2).opaque([]byte("hello"))
		e.u32(25).fixed(sid).u64(0).u32(100)
		return 5
	})
	for range 3 {
		status(d)
	}
	if st := status(d); st != 0 {
		t.Fatalf("WRITE: %d", st)
	}
	d.u32()
	d.u32()
	d.fixed(8)
	if st := status(d); st != 0 {
		t.Fatalf("READ: %d", st)
	}
	d.u32()
	if got := string(d.opaque()); got != "hello" {
		t.Errorf("read back %q", got)
	}
	if b, _ := os.ReadFile(filepath.Join(root, "mine", "f")); string(b) != "hello" {
		t.Errorf("the root holds %q", b)
	}
}

// A lock is refused another client while one holds it, and free once that
// one closes the file; making a directory is exclusive too.
func TestLocksBetweenClients(t *testing.T) {
	srv, _ := newServer(t, view{sets: []string{"s"}})
	a, b := connect(t, srv, "a"), connect(t, srv, "b")
	sa, sb := a.open("s", "f"), b.open("s", "f")

	lock := func(c *client, sid []byte) uint32 {
		_, d := c.run(func(e *enc) uint32 {
			path(e, "s", "f")
			// LOCK: write, no reclaim, the whole file, a new lock owner.
			e.u32(12).u32(2).u32(0).u64(0).u64(^uint64(0)).u32(1).u32(0).fixed(sid).u32(0).u64(c.id).str("lockowner")
			return 4
		})
		for range 3 {
			status(d)
		}
		return status(d)
	}
	if st := lock(a, sa); st != 0 {
		t.Fatalf("the first lock: %d", st)
	}
	if st := lock(b, sb); st != 10010 {
		t.Fatalf("a second client's lock: %d, not DENIED", st)
	}
	_, d := a.run(func(e *enc) uint32 {
		path(e, "s", "f")
		e.u32(4).u32(0).fixed(sa)
		return 4
	})
	for range 3 {
		status(d)
	}
	if st := status(d); st != 0 {
		t.Fatalf("CLOSE: %d", st)
	}
	if st := lock(b, sb); st != 0 {
		t.Fatalf("the lock after the holder closed: %d", st)
	}

	mkdir := func(c *client) uint32 {
		_, d := c.run(func(e *enc) uint32 {
			path(e, "s")
			e.u32(6).u32(2).str("lockdir").u32(0).u32(0)
			return 3
		})
		status(d)
		status(d)
		return status(d)
	}
	if st := mkdir(a); st != 0 {
		t.Fatalf("mkdir: %d", st)
	}
	if st := mkdir(b); st != 17 {
		t.Fatalf("a second mkdir: %d, not EXIST", st)
	}
}

// A link made in a set does not lead out of it.
func TestLinksStayInTheRoot(t *testing.T) {
	srv, root := newServer(t, view{sets: []string{"s"}})
	outside := t.TempDir()
	os.WriteFile(filepath.Join(outside, "f"), []byte("outside"), 0o644)
	makeSet(root, "s")
	os.Symlink(outside, filepath.Join(root, "s", "out"))
	c := connect(t, srv, "a")
	_, d := c.run(func(e *enc) uint32 { path(e, "s", "out", "f"); return 4 })
	status(d)
	status(d)
	status(d)
	if st := status(d); st == 0 {
		t.Fatal("looked a file up through a link out of the root")
	}
}

// ALLOCATE grows a file to hold the range, as fallocate does; DEALLOCATE
// punches a hole in it, keeping its size.
func TestAllocate(t *testing.T) {
	srv, root := newServer(t, view{sets: []string{"s"}})
	makeSet(root, "s")
	c := connect(t, srv, "a")
	sid := c.open("s", "f")
	op := func(code uint32, offset, length uint64) uint32 {
		_, d := c.run(func(e *enc) uint32 {
			path(e, "s", "f")
			e.u32(code).fixed(sid).u64(offset).u64(length)
			return 4
		})
		for range 3 {
			status(d)
		}
		return status(d)
	}
	if st := op(59, 0, 8192); st != 0 {
		t.Fatalf("ALLOCATE: %d", st)
	}
	if st, _ := os.Stat(filepath.Join(root, "s", "f")); st.Size() != 8192 {
		t.Errorf("allocated to %d bytes", st.Size())
	}
	if st := op(62, 0, 4096); st != 0 {
		t.Fatalf("DEALLOCATE: %d", st)
	}
	if st, _ := os.Stat(filepath.Join(root, "s", "f")); st.Size() != 8192 {
		t.Errorf("a hole changed the size to %d", st.Size())
	}
}

// Who a set's callers act as: one owner for everything, root as nobody,
// or each as themselves. Only a server run as root acts as anyone else.
func TestSquash(t *testing.T) {
	if os.Geteuid() != 0 {
		t.Skip("acting as another user needs root")
	}
	sets := []string{"all", "root", "none"}
	srv, root := newServer(t, view{sets: append(sets, "fresh"), squash: map[string]nfs.Squash{
		"all": nfs.SquashAll, "root": nfs.SquashRoot, "none": nfs.SquashNone, "fresh": nfs.SquashNone}})
	for _, set := range sets {
		os.MkdirAll(filepath.Join(root, set), 0o777)
		os.Chmod(filepath.Join(root, set), 0o777)
	}
	owner := func(set, name string) uint32 {
		var st syscall.Stat_t
		if err := syscall.Lstat(filepath.Join(root, set, name), &st); err != nil {
			t.Fatal(err)
		}
		return st.Uid
	}
	// chown asks a SETATTR of the owner, and gives its status.
	chown := func(c *client, set, name string, uid uint32) uint32 {
		_, d := c.run(func(e *enc) uint32 {
			path(e, set, name)
			var attrs enc
			attrs.str(fmt.Sprint(uid))
			e.u32(34).fixed(make([]byte, 16)).u32(2).u32(0).u32(1 << (36 - 32)).opaque(attrs.b)
			return 4
		})
		for range 3 {
			status(d)
		}
		return status(d)
	}

	rootCaller := connect(t, srv, "root")
	rootCaller.asRoot = true
	for set, want := range map[string]uint32{"all": 1000, "root": 65534, "none": 0} {
		rootCaller.open(set, "byroot")
		if uid := owner(set, "byroot"); uid != want {
			t.Errorf("%s: a file root made is %d's, want %d's", set, uid, want)
		}
	}
	if st := chown(rootCaller, "none", "byroot", 4321); st != 0 {
		t.Errorf("root changing an owner with no squash: %d", st)
	} else if uid := owner("none", "byroot"); uid != 4321 {
		t.Errorf("the owner is %d after root changed it", uid)
	}
	if st := chown(rootCaller, "all", "byroot", 4321); st != 0 {
		t.Errorf("changing an owner squashed to one: %d, want it taken", st)
	} else if uid := owner("all", "byroot"); uid != 1000 {
		t.Errorf("squashed to one owner, the owner changed to %d", uid)
	}

	// A set with no directory yet has one made, the environments' user's,
	// for a caller to make files in.
	user := connect(t, srv, "user")
	user.open("fresh", "f")
	if uid := owner("fresh", "f"); uid != 1000 {
		t.Errorf("a file made in a new set is %d's", uid)
	}
	if uid := owner("fresh", "."); uid != 1000 {
		t.Errorf("a new set's directory is %d's", uid)
	}

	// A user is refused another's file, with no squash.
	if st := chown(user, "none", "byroot", 1000); st != 1 {
		t.Errorf("a user taking another's file: %d, want PERM", st)
	}
}
