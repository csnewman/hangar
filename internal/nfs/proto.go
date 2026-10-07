package nfs

// NFSv4.1 and 4.2 (RFC 5661, RFC 7862): the operations, statuses and
// attributes this server knows.

// maxIO is the most a READ or WRITE carries.
const maxIO = 1 << 20

// Operations.
const (
	opAccess             = 3
	opClose              = 4
	opCommit             = 5
	opCreate             = 6
	opDelegPurge         = 7
	opDelegReturn        = 8
	opGetattr            = 9
	opGetFH              = 10
	opLink               = 11
	opLock               = 12
	opLockT              = 13
	opLockU              = 14
	opLookup             = 15
	opLookupP            = 16
	opNVerify            = 17
	opOpen               = 18
	opOpenAttr           = 19
	opOpenConfirm        = 20
	opOpenDowngrade      = 21
	opPutFH              = 22
	opPutPubFH           = 23
	opPutRootFH          = 24
	opRead               = 25
	opReadDir            = 26
	opReadLink           = 27
	opRemove             = 28
	opRename             = 29
	opRenew              = 30
	opRestoreFH          = 31
	opSaveFH             = 32
	opSecInfo            = 33
	opSetattr            = 34
	opSetClientID        = 35
	opSetClientIDConfirm = 36
	opVerify             = 37
	opWrite              = 38
	opReleaseLockOwner   = 39
	opBackchannelCtl     = 40
	opBindConnToSession  = 41
	opExchangeID         = 42
	opCreateSession      = 43
	opDestroySession     = 44
	opFreeStateID        = 45
	opSecInfoNoName      = 52
	opSequence           = 53
	opTestStateID        = 55
	opDestroyClientID    = 57
	opReclaimComplete    = 58
	opIllegal            = 10044
)

// Statuses.
const (
	nfsOK               = 0
	errPerm             = 1
	errNoEnt            = 2
	errIO               = 5
	errNXIO             = 6
	errAccess           = 13
	errExist            = 17
	errXDev             = 18
	errNotDir           = 20
	errIsDir            = 21
	errInval            = 22
	errFBig             = 27
	errNoSpc            = 28
	errROFS             = 30
	errMLink            = 31
	errNameTooLong      = 63
	errNotEmpty         = 66
	errDQuot            = 69
	errStale            = 70
	errBadHandle        = 10001
	errBadCookie        = 10003
	errNotSupp          = 10004
	errTooSmall         = 10005
	errServerFault      = 10006
	errBadType          = 10007
	errDelay            = 10008
	errDenied           = 10010
	errExpired          = 10011
	errLocked           = 10012
	errShareDenied      = 10015
	errNoFileHandle     = 10020
	errMinorVersMis     = 10021
	errStaleClientID    = 10022
	errOldStateID       = 10024
	errBadStateID       = 10025
	errSymlink          = 10029
	errRestoreFH        = 10030
	errAttrNotSupp      = 10032
	errNoGrace          = 10033
	errBadXDR           = 10036
	errLocksHeld        = 10037
	errOpenMode         = 10038
	errBadOwner         = 10039
	errBadName          = 10041
	errLockRange        = 10028
	errOpIllegal        = 10044
	errBadSession       = 10052
	errBadSlot          = 10053
	errCompleteAlready  = 10054
	errConnNotBound     = 10055
	errSeqMisordered    = 10063
	errSequencePos      = 10064
	errReqTooBig        = 10065
	errRepTooBig        = 10066
	errRetryUncached    = 10068
	errTooManyOps       = 10070
	errOpNotInSession   = 10071
	errClientIDBusy     = 10074
	errSeqFalseRetry    = 10076
	errBadHighSlot      = 10077
	errDeadSession      = 10078
	errNotOnlyOp        = 10081
	errWrongType        = 10083
	errStaleStateID     = 10023
	errLockNotSupp      = 10043
	errNotSame          = 10027
	errWrongSec         = 10016
	errFileOpen         = 10046
	errAdminRevoked     = 10047
	errDeadlock         = 10045
	errBadRange         = 10042
	errExpiredStateID   = errExpired
	errReclaimBad       = 10034
	errUnknownLayoutTyp = 10062
)

// File types.
const (
	nf4Reg  = 1
	nf4Dir  = 2
	nf4Blk  = 3
	nf4Chr  = 4
	nf4Lnk  = 5
	nf4Sock = 6
	nf4FIFO = 7
)

// Attributes.
const (
	attrSupportedAttrs    = 0
	attrType              = 1
	attrFHExpireType      = 2
	attrChange            = 3
	attrSize              = 4
	attrLinkSupport       = 5
	attrSymlinkSupport    = 6
	attrNamedAttr         = 7
	attrFSID              = 8
	attrUniqueHandles     = 9
	attrLeaseTime         = 10
	attrRdattrError       = 11
	attrACLSupport        = 13
	attrCanSetTime        = 15
	attrCaseInsensitive   = 16
	attrCasePreserving    = 17
	attrChownRestricted   = 18
	attrFilehandle        = 19
	attrFileID            = 20
	attrFilesAvail        = 21
	attrFilesFree         = 22
	attrFilesTotal        = 23
	attrHomogeneous       = 26
	attrMaxFileSize       = 27
	attrMaxLink           = 28
	attrMaxName           = 29
	attrMaxRead           = 30
	attrMaxWrite          = 31
	attrMode              = 33
	attrNoTrunc           = 34
	attrNumLinks          = 35
	attrOwner             = 36
	attrOwnerGroup        = 37
	attrRawDev            = 41
	attrSpaceAvail        = 42
	attrSpaceFree         = 43
	attrSpaceTotal        = 44
	attrSpaceUsed         = 45
	attrTimeAccess        = 47
	attrTimeAccessSet     = 48
	attrTimeDelta         = 51
	attrTimeMetadata      = 52
	attrTimeModify        = 53
	attrTimeModifySet     = 54
	attrMountedOnFileID   = 55
	attrFSLayoutTypes     = 62
	attrLayoutBlkSize     = 65
	attrSuppAttrExclCreat = 75
)

// ACCESS bits.
const (
	accessRead    = 0x01
	accessLookup  = 0x02
	accessModify  = 0x04
	accessExtend  = 0x08
	accessDelete  = 0x10
	accessExecute = 0x20
	accessAll     = 0x3f
)

// OPEN.
const (
	openNoCreate = 0
	openCreate   = 1

	createUnchecked  = 0
	createGuarded    = 1
	createExclusive  = 2
	createExclusive1 = 3

	claimNull        = 0
	claimPrevious    = 1
	claimDelegateCur = 2
	claimDelegPrev   = 3
	claimFH          = 4
	claimDelegCurFH  = 5
	claimDelegPrevFH = 6

	shareAccessRead  = 1
	shareAccessWrite = 2
	shareAccessBoth  = 3
	shareAccessMask  = 3

	openResultLocktypePosix = 4
	delegateNone            = 0
)

// LOCK types.
const (
	lockRead   = 1
	lockWrite  = 2
	lockReadW  = 3
	lockWriteW = 4
)

// WRITE stability.
const (
	unstable = 0
	dataSync = 1
	fileSync = 2
)

// EXCHANGE_ID and CREATE_SESSION.
const (
	exchgidUseNonPNFS   = 0x00010000
	exchgidConfirmedR   = 0x80000000
	exchgidSuppMovedRef = 0x00000001
	exchgidSuppMovedMig = 0x00000002
	sp4None             = 0
	sp4MachCred         = 1
	sp4SSV              = 2

	createSessionPersist = 0x1
)
