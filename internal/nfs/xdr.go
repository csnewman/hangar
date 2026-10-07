package nfs

import (
	"encoding/binary"
	"errors"
	"math"
)

// errXDR is a message that does not decode: too short, or a length past
// what is left of it.
var errXDR = errors.New("nfs: malformed XDR")

// reader decodes XDR (RFC 4506). A decoding error sticks: every read after
// it returns zero values, and err says what went wrong first.
type reader struct {
	b   []byte
	err error
}

func (r *reader) fail() {
	if r.err == nil {
		r.err = errXDR
	}
	r.b = nil
}

func (r *reader) uint32() uint32 {
	if len(r.b) < 4 {
		r.fail()
		return 0
	}
	v := binary.BigEndian.Uint32(r.b)
	r.b = r.b[4:]
	return v
}

func (r *reader) uint64() uint64 {
	if len(r.b) < 8 {
		r.fail()
		return 0
	}
	v := binary.BigEndian.Uint64(r.b)
	r.b = r.b[8:]
	return v
}

func (r *reader) int64() int64 { return int64(r.uint64()) }

func (r *reader) bool() bool { return r.uint32() != 0 }

// fixed reads n bytes of fixed-length opaque data, padded to four.
func (r *reader) fixed(n int) []byte {
	padded := (n + 3) &^ 3
	if n < 0 || len(r.b) < padded {
		r.fail()
		return nil
	}
	v := r.b[:n:n]
	r.b = r.b[padded:]
	return v
}

// opaque reads variable-length opaque data, at most max bytes.
func (r *reader) opaque(max int) []byte {
	n := r.uint32()
	if r.err != nil || n > uint32(max) {
		r.fail()
		return nil
	}
	return r.fixed(int(n))
}

func (r *reader) string(max int) string { return string(r.opaque(max)) }

// count reads an array's length, at most max.
func (r *reader) count(max int) int {
	n := r.uint32()
	if r.err != nil || n > uint32(max) {
		r.fail()
		return 0
	}
	return int(n)
}

// bitmap reads a bitmap4.
func (r *reader) bitmap() bitmap {
	n := r.count(8)
	b := make(bitmap, n)
	for i := range b {
		b[i] = r.uint32()
	}
	return b
}

// writer encodes XDR.
type writer struct {
	b []byte
}

func (w *writer) uint32(v uint32) { w.b = binary.BigEndian.AppendUint32(w.b, v) }
func (w *writer) uint64(v uint64) { w.b = binary.BigEndian.AppendUint64(w.b, v) }
func (w *writer) int64(v int64)   { w.uint64(uint64(v)) }

func (w *writer) bool(v bool) {
	if v {
		w.uint32(1)
	} else {
		w.uint32(0)
	}
}

func (w *writer) fixed(v []byte) {
	w.b = append(w.b, v...)
	for len(w.b)%4 != 0 {
		w.b = append(w.b, 0)
	}
}

func (w *writer) opaque(v []byte) {
	if len(v) > math.MaxUint32 {
		panic("nfs: opaque data too long")
	}
	w.uint32(uint32(len(v)))
	w.fixed(v)
}

func (w *writer) string(v string) { w.opaque([]byte(v)) }

func (w *writer) bitmap(b bitmap) {
	w.uint32(uint32(len(b)))
	for _, v := range b {
		w.uint32(v)
	}
}

// bitmap is a bitmap4: bit n is word n/32's bit n%32.
type bitmap []uint32

func (b bitmap) has(n int) bool {
	return n/32 < len(b) && b[n/32]&(1<<(n%32)) != 0
}

func (b *bitmap) set(n int) {
	for len(*b) <= n/32 {
		*b = append(*b, 0)
	}
	(*b)[n/32] |= 1 << (n % 32)
}

// and is the bits set in both.
func (b bitmap) and(o bitmap) bitmap {
	out := make(bitmap, min(len(b), len(o)))
	for i := range out {
		out[i] = b[i] & o[i]
	}
	return out.trim()
}

// trim drops trailing empty words.
func (b bitmap) trim() bitmap {
	for len(b) > 0 && b[len(b)-1] == 0 {
		b = b[:len(b)-1]
	}
	return b
}
