package nfs

import (
	"encoding/binary"
	"fmt"
	"io"
)

// ONC RPC (RFC 5531), as NFS carries it: calls and replies on a stream,
// each a record of one or more fragments (record marking).

const (
	rpcCall  = 0
	rpcReply = 1

	rpcVersion = 2
	nfsProgram = 100003
	nfsVersion = 4

	procNull     = 0
	procCompound = 1

	authNone = 0
	authSys  = 1

	msgAccepted = 0
	msgDenied   = 1

	acceptSuccess      = 0
	acceptProgUnavail  = 1
	acceptProgMismatch = 2
	acceptProcUnavail  = 3
	acceptGarbageArgs  = 4

	rejectRPCMismatch = 0

	lastFragment = 1 << 31
	// maxRecord bounds a call: a WRITE of maxIO and its compound around it.
	maxRecord = maxIO + 64<<10
)

// readRecord reads one record, its fragments joined.
func readRecord(r io.Reader) ([]byte, error) {
	var rec []byte
	for {
		var hdr [4]byte
		if _, err := io.ReadFull(r, hdr[:]); err != nil {
			return nil, err
		}
		h := binary.BigEndian.Uint32(hdr[:])
		n := int(h &^ lastFragment)
		if len(rec)+n > maxRecord {
			return nil, fmt.Errorf("nfs: a record of more than %d bytes", maxRecord)
		}
		start := len(rec)
		rec = append(rec, make([]byte, n)...)
		if _, err := io.ReadFull(r, rec[start:]); err != nil {
			return nil, err
		}
		if h&lastFragment != 0 {
			return rec, nil
		}
	}
}

// writeRecord writes b as one record of one fragment.
func writeRecord(w io.Writer, b []byte) error {
	out := make([]byte, 4, 4+len(b))
	binary.BigEndian.PutUint32(out, lastFragment|uint32(len(b)))
	_, err := w.Write(append(out, b...))
	return err
}

// cred is who a call is from, as AUTH_SYS says: the client's word, which
// is all NFS has to go on. One with AUTH_NONE is nobody.
type cred struct {
	uid, gid uint32
	gids     []uint32
}

var nobody = cred{uid: 65534, gid: 65534}

// call is an RPC call's header, and what follows it.
type call struct {
	xid    uint32
	proc   uint32
	cred   cred
	args   []byte
	reject func(w *writer) // a reply to send instead of calling, if set
}

// parseCall reads a call's header. A call to anything but NFSv4 is
// answered here, with reject set.
func parseCall(b []byte) (call, error) {
	r := &reader{b: b}
	c := call{xid: r.uint32()}
	if r.uint32() != rpcCall {
		return c, fmt.Errorf("nfs: not a call")
	}
	rpcvers, prog, vers := r.uint32(), r.uint32(), r.uint32()
	c.proc = r.uint32()
	flavor, body := r.uint32(), r.opaque(400)
	r.uint32() // the verifier's flavor
	r.opaque(400)
	if r.err != nil {
		return c, r.err
	}
	c.cred = nobody
	if flavor == authSys {
		a := &reader{b: body}
		a.uint32()    // stamp
		a.string(255) // machine name
		uid, gid := a.uint32(), a.uint32()
		n := a.count(16)
		gids := make([]uint32, n)
		for i := range gids {
			gids[i] = a.uint32()
		}
		if a.err == nil {
			c.cred = cred{uid: uid, gid: gid, gids: gids}
		}
	}
	c.args = r.b
	switch {
	case rpcvers != rpcVersion:
		c.reject = func(w *writer) {
			w.uint32(msgDenied)
			w.uint32(rejectRPCMismatch)
			w.uint32(rpcVersion)
			w.uint32(rpcVersion)
		}
	case prog != nfsProgram:
		c.reject = func(w *writer) { accepted(w, acceptProgUnavail) }
	case vers != nfsVersion:
		c.reject = func(w *writer) {
			accepted(w, acceptProgMismatch)
			w.uint32(nfsVersion)
			w.uint32(nfsVersion)
		}
	case c.proc != procNull && c.proc != procCompound:
		c.reject = func(w *writer) { accepted(w, acceptProcUnavail) }
	}
	return c, nil
}

// replyHeader starts a reply to xid.
func replyHeader(xid uint32) *writer {
	w := &writer{}
	w.uint32(xid)
	w.uint32(rpcReply)
	return w
}

// accepted writes an accepted reply's status, with no verifier.
func accepted(w *writer, stat uint32) {
	w.uint32(msgAccepted)
	w.uint32(authNone)
	w.uint32(0)
	w.uint32(stat)
}
