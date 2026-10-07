//go:build linux

package nfs

import (
	"strconv"
	"strings"
	"time"

	"golang.org/x/sys/unix"
)

const (
	// maxName is the longest name.
	maxName = 255
	// leaseTime is how long a client's state lasts without its renewing
	// it: a SEQUENCE, at least every lease, from a client that is there.
	leaseTime = 30 * time.Second
	// rootFileID is the root's file ID, which no file on disk has.
	rootFileID = 1<<63 - 1
)

// supported are the attributes this server gives.
var supported = func() bitmap {
	var b bitmap
	for _, a := range []int{
		attrSupportedAttrs, attrType, attrFHExpireType, attrChange, attrSize,
		attrLinkSupport, attrSymlinkSupport, attrNamedAttr, attrFSID,
		attrUniqueHandles, attrLeaseTime, attrRdattrError, attrACLSupport,
		attrCanSetTime, attrCaseInsensitive, attrCasePreserving,
		attrChownRestricted, attrFilehandle, attrFileID, attrFilesAvail,
		attrFilesFree, attrFilesTotal, attrHomogeneous, attrMaxFileSize,
		attrMaxLink, attrMaxName, attrMaxRead, attrMaxWrite, attrMode,
		attrNoTrunc, attrNumLinks, attrOwner, attrOwnerGroup, attrRawDev,
		attrSpaceAvail, attrSpaceFree, attrSpaceTotal, attrSpaceUsed,
		attrTimeAccess, attrTimeAccessSet, attrTimeDelta, attrTimeMetadata,
		attrTimeModify, attrTimeModifySet, attrMountedOnFileID,
		attrFSLayoutTypes, attrLayoutBlkSize, attrSuppAttrExclCreat,
	} {
		b.set(a)
	}
	return b
}()

// settable are the attributes a SETATTR, or a create, may give.
var settable = func() bitmap {
	var b bitmap
	for _, a := range []int{attrSize, attrMode, attrOwner, attrOwnerGroup,
		attrTimeAccessSet, attrTimeModifySet} {
		b.set(a)
	}
	return b
}()

// exclCreatable are those an EXCLUSIVE4_1 create may give: all but the
// times, in which some servers keep the verifier.
var exclCreatable = func() bitmap {
	var b bitmap
	for _, a := range []int{attrSize, attrMode, attrOwner, attrOwnerGroup} {
		b.set(a)
	}
	return b
}()

// node is a file's attributes, or the root's.
type node struct {
	root bool
	st   unix.Stat_t
	fh   []byte
}

func ftype(mode uint32) uint32 {
	switch mode & unix.S_IFMT {
	case unix.S_IFDIR:
		return nf4Dir
	case unix.S_IFLNK:
		return nf4Lnk
	case unix.S_IFBLK:
		return nf4Blk
	case unix.S_IFCHR:
		return nf4Chr
	case unix.S_IFSOCK:
		return nf4Sock
	case unix.S_IFIFO:
		return nf4FIFO
	}
	return nf4Reg
}

func timeOf(t unix.Timespec) (int64, uint32) { return t.Sec, uint32(t.Nsec) }

// change is a file's change attribute: its ctime, which every change to it
// moves on.
func change(st *unix.Stat_t) uint64 {
	return uint64(st.Ctim.Sec)<<32 | uint64(uint32(st.Ctim.Nsec))
}

// encodeAttrs gives the requested attributes of n, that this server has.
func (s *Server) encodeAttrs(req bitmap, n *node) (bitmap, []byte) {
	mask := req.and(supported)
	w := &writer{}
	var fs unix.Statfs_t
	needFS := mask.has(attrFilesAvail) || mask.has(attrFilesFree) || mask.has(attrFilesTotal) ||
		mask.has(attrSpaceAvail) || mask.has(attrSpaceFree) || mask.has(attrSpaceTotal)
	if needFS {
		unix.Fstatfs(s.rootFD, &fs)
	}
	st := &n.st
	if n.root {
		st = &unix.Stat_t{Mode: unix.S_IFDIR | 0o555, Nlink: 2, Ino: rootFileID,
			Ctim: s.started, Mtim: s.started, Atim: s.started}
	}
	for a := 0; a < len(mask)*32; a++ {
		if !mask.has(a) {
			continue
		}
		switch a {
		case attrSupportedAttrs:
			w.bitmap(supported)
		case attrType:
			w.uint32(ftype(st.Mode))
		case attrFHExpireType:
			w.uint32(0) // persistent, while the server runs
		case attrChange:
			w.uint64(change(st))
		case attrSize:
			w.uint64(uint64(st.Size))
		case attrLinkSupport, attrSymlinkSupport, attrCanSetTime, attrCasePreserving,
			attrChownRestricted, attrHomogeneous, attrNoTrunc:
			w.bool(true)
		case attrNamedAttr, attrUniqueHandles, attrCaseInsensitive:
			w.bool(false)
		case attrFSID:
			w.uint64(0x48414e47) // "HANG"
			w.uint64(0)
		case attrLeaseTime:
			w.uint32(uint32(leaseTime / time.Second))
		case attrRdattrError:
			w.uint32(nfsOK)
		case attrACLSupport:
			w.uint32(0)
		case attrFilehandle:
			w.opaque(n.fh)
		case attrFileID, attrMountedOnFileID:
			w.uint64(st.Ino)
		case attrFilesAvail:
			w.uint64(fs.Ffree)
		case attrFilesFree:
			w.uint64(fs.Ffree)
		case attrFilesTotal:
			w.uint64(fs.Files)
		case attrMaxFileSize:
			w.uint64(1<<63 - 1)
		case attrMaxLink:
			w.uint32(65000)
		case attrMaxName:
			w.uint32(maxName)
		case attrMaxRead, attrMaxWrite:
			w.uint64(maxIO)
		case attrMode:
			w.uint32(st.Mode &^ unix.S_IFMT)
		case attrNumLinks:
			w.uint32(uint32(st.Nlink))
		case attrOwner:
			w.string(strconv.FormatUint(uint64(s.uid), 10))
		case attrOwnerGroup:
			w.string(strconv.FormatUint(uint64(s.gid), 10))
		case attrRawDev:
			w.uint32(unix.Major(st.Rdev))
			w.uint32(unix.Minor(st.Rdev))
		case attrSpaceAvail:
			w.uint64(fs.Bavail * uint64(fs.Bsize))
		case attrSpaceFree:
			w.uint64(fs.Bfree * uint64(fs.Bsize))
		case attrSpaceTotal:
			w.uint64(fs.Blocks * uint64(fs.Bsize))
		case attrSpaceUsed:
			w.uint64(uint64(st.Blocks) * 512)
		case attrTimeAccess:
			sec, nsec := timeOf(st.Atim)
			w.int64(sec)
			w.uint32(nsec)
		case attrTimeMetadata:
			sec, nsec := timeOf(st.Ctim)
			w.int64(sec)
			w.uint32(nsec)
		case attrTimeModify:
			sec, nsec := timeOf(st.Mtim)
			w.int64(sec)
			w.uint32(nsec)
		case attrTimeDelta:
			w.int64(0)
			w.uint32(1)
		case attrFSLayoutTypes:
			w.uint32(0)
		case attrLayoutBlkSize:
			w.uint32(maxIO)
		case attrSuppAttrExclCreat:
			w.bitmap(exclCreatable)
		case attrTimeAccessSet, attrTimeModifySet:
			// Write-only: a GETATTR asking gets nothing for them.
			mask[a/32] &^= 1 << (a % 32)
		}
	}
	return mask.trim(), w.b
}

// setTime is a time a SETATTR gives: the server's own clock's, or the one
// given.
type setTime struct {
	server bool
	ts     unix.Timespec
}

// setAttrs are what a SETATTR, or a create, gives.
type setAttrs struct {
	mask         bitmap
	size         *uint64
	mode         *uint32
	uid, gid     *uint32
	atime, mtime *setTime
}

// decodeAttrs reads a fattr4 to set. An attribute that cannot be set, or
// is not known, is refused: what follows it could not be read.
func decodeAttrs(r *reader) (setAttrs, uint32) {
	var a setAttrs
	a.mask = r.bitmap()
	vals := &reader{b: r.opaque(maxRecord)}
	if r.err != nil {
		return a, errBadXDR
	}
	for n := 0; n < len(a.mask)*32; n++ {
		if !a.mask.has(n) {
			continue
		}
		if !settable.has(n) {
			if supported.has(n) {
				return a, errInval
			}
			return a, errAttrNotSupp
		}
		switch n {
		case attrSize:
			v := vals.uint64()
			a.size = &v
		case attrMode:
			v := vals.uint32() & 0o7777
			a.mode = &v
		case attrOwner, attrOwnerGroup:
			id, ok := parseID(vals.string(1024))
			if !ok {
				return a, errBadOwner
			}
			if n == attrOwner {
				a.uid = &id
			} else {
				a.gid = &id
			}
		case attrTimeAccessSet, attrTimeModifySet:
			t := &setTime{server: vals.uint32() == 0}
			if !t.server {
				t.ts = unix.Timespec{Sec: vals.int64(), Nsec: int64(vals.uint32())}
			}
			if n == attrTimeAccessSet {
				a.atime = t
			} else {
				a.mtime = t
			}
		}
	}
	if vals.err != nil {
		return a, errBadXDR
	}
	return a, nfsOK
}

// parseID reads an owner or group as AUTH_SYS clients give them: the
// number, with a domain after an @ when one is configured.
func parseID(s string) (uint32, bool) {
	s, _, _ = strings.Cut(s, "@")
	v, err := strconv.ParseUint(s, 10, 32)
	return uint32(v), err == nil
}

// apply sets a's attributes on the file at p, giving which it set.
func (s *Server) apply(p string, a setAttrs) (bitmap, uint32) {
	var set bitmap
	fd, name, err := s.at(p)
	if err != nil {
		return set, errno(err)
	}
	defer unix.Close(fd)
	var st unix.Stat_t
	if err := unix.Fstatat(fd, name, &st, unix.AT_SYMLINK_NOFOLLOW); err != nil {
		return set, errno(err)
	}
	link := st.Mode&unix.S_IFMT == unix.S_IFLNK
	if a.size != nil {
		if st.Mode&unix.S_IFMT != unix.S_IFREG {
			return set, errInval
		}
		f, err := unix.Openat(fd, name, unix.O_WRONLY|unix.O_NOFOLLOW|unix.O_CLOEXEC, 0)
		if err == nil {
			err = unix.Ftruncate(f, int64(*a.size))
			unix.Close(f)
		}
		if err != nil {
			return set, errno(err)
		}
		set.set(attrSize)
	}
	// Every file is the environment's user's (Config.UID): an owner given
	// is taken as given and changes nothing.
	if a.uid != nil {
		set.set(attrOwner)
	}
	if a.gid != nil {
		set.set(attrOwnerGroup)
	}
	if a.mode != nil && !link {
		if err := unix.Fchmodat(fd, name, *a.mode, 0); err != nil {
			return set, errno(err)
		}
		set.set(attrMode)
	}
	if a.atime != nil || a.mtime != nil {
		ts := []unix.Timespec{{Nsec: unix.UTIME_OMIT}, {Nsec: unix.UTIME_OMIT}}
		for i, t := range []*setTime{a.atime, a.mtime} {
			switch {
			case t == nil:
			case t.server:
				ts[i] = unix.Timespec{Nsec: unix.UTIME_NOW}
			default:
				ts[i] = t.ts
			}
		}
		if err := unix.UtimesNanoAt(fd, name, ts, unix.AT_SYMLINK_NOFOLLOW); err != nil {
			return set, errno(err)
		}
		if a.atime != nil {
			set.set(attrTimeAccessSet)
		}
		if a.mtime != nil {
			set.set(attrTimeModifySet)
		}
	}
	return set, nfsOK
}
