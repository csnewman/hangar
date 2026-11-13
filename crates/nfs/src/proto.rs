//! NFSv4.1 and 4.2 (RFC 5661, RFC 7862): the operations, statuses and
//! attributes this server knows.

#![allow(dead_code)]

/// The most a READ or WRITE carries.
pub const MAX_IO: u32 = 1 << 20;

// Operations.
pub const OP_ACCESS: u32 = 3;
pub const OP_CLOSE: u32 = 4;
pub const OP_COMMIT: u32 = 5;
pub const OP_CREATE: u32 = 6;
pub const OP_DELEG_PURGE: u32 = 7;
pub const OP_DELEG_RETURN: u32 = 8;
pub const OP_GETATTR: u32 = 9;
pub const OP_GET_FH: u32 = 10;
pub const OP_LINK: u32 = 11;
pub const OP_LOCK: u32 = 12;
pub const OP_LOCK_T: u32 = 13;
pub const OP_LOCK_U: u32 = 14;
pub const OP_LOOKUP: u32 = 15;
pub const OP_LOOKUP_P: u32 = 16;
pub const OP_N_VERIFY: u32 = 17;
pub const OP_OPEN: u32 = 18;
pub const OP_OPEN_ATTR: u32 = 19;
pub const OP_OPEN_CONFIRM: u32 = 20;
pub const OP_OPEN_DOWNGRADE: u32 = 21;
pub const OP_PUT_FH: u32 = 22;
pub const OP_PUT_PUB_FH: u32 = 23;
pub const OP_PUT_ROOT_FH: u32 = 24;
pub const OP_READ: u32 = 25;
pub const OP_READ_DIR: u32 = 26;
pub const OP_READ_LINK: u32 = 27;
pub const OP_REMOVE: u32 = 28;
pub const OP_RENAME: u32 = 29;
pub const OP_RENEW: u32 = 30;
pub const OP_RESTORE_FH: u32 = 31;
pub const OP_SAVE_FH: u32 = 32;
pub const OP_SEC_INFO: u32 = 33;
pub const OP_SETATTR: u32 = 34;
pub const OP_SET_CLIENT_ID: u32 = 35;
pub const OP_SET_CLIENT_ID_CONFIRM: u32 = 36;
pub const OP_VERIFY: u32 = 37;
pub const OP_WRITE: u32 = 38;
pub const OP_RELEASE_LOCK_OWNER: u32 = 39;
pub const OP_BACKCHANNEL_CTL: u32 = 40;
pub const OP_BIND_CONN_TO_SESSION: u32 = 41;
pub const OP_EXCHANGE_ID: u32 = 42;
pub const OP_CREATE_SESSION: u32 = 43;
pub const OP_DESTROY_SESSION: u32 = 44;
pub const OP_FREE_STATE_ID: u32 = 45;
pub const OP_SEC_INFO_NO_NAME: u32 = 52;
pub const OP_SEQUENCE: u32 = 53;
pub const OP_TEST_STATE_ID: u32 = 55;
pub const OP_DESTROY_CLIENT_ID: u32 = 57;
pub const OP_RECLAIM_COMPLETE: u32 = 58;
pub const OP_ALLOCATE: u32 = 59;
pub const OP_DEALLOCATE: u32 = 62;
pub const OP_ILLEGAL: u32 = 10044;

// Statuses.
pub const NFS_OK: u32 = 0;
pub const ERR_PERM: u32 = 1;
pub const ERR_NO_ENT: u32 = 2;
pub const ERR_IO: u32 = 5;
pub const ERR_NXIO: u32 = 6;
pub const ERR_ACCESS: u32 = 13;
pub const ERR_EXIST: u32 = 17;
pub const ERR_X_DEV: u32 = 18;
pub const ERR_NOT_DIR: u32 = 20;
pub const ERR_IS_DIR: u32 = 21;
pub const ERR_INVAL: u32 = 22;
pub const ERR_F_BIG: u32 = 27;
pub const ERR_NO_SPC: u32 = 28;
pub const ERR_ROFS: u32 = 30;
pub const ERR_M_LINK: u32 = 31;
pub const ERR_NAME_TOO_LONG: u32 = 63;
pub const ERR_NOT_EMPTY: u32 = 66;
pub const ERR_D_QUOT: u32 = 69;
pub const ERR_STALE: u32 = 70;
pub const ERR_BAD_HANDLE: u32 = 10001;
pub const ERR_BAD_COOKIE: u32 = 10003;
pub const ERR_NOT_SUPP: u32 = 10004;
pub const ERR_TOO_SMALL: u32 = 10005;
pub const ERR_SERVER_FAULT: u32 = 10006;
pub const ERR_BAD_TYPE: u32 = 10007;
pub const ERR_DELAY: u32 = 10008;
pub const ERR_DENIED: u32 = 10010;
pub const ERR_EXPIRED: u32 = 10011;
pub const ERR_LOCKED: u32 = 10012;
pub const ERR_SHARE_DENIED: u32 = 10015;
pub const ERR_NO_FILE_HANDLE: u32 = 10020;
pub const ERR_MINOR_VERS_MIS: u32 = 10021;
pub const ERR_STALE_CLIENT_ID: u32 = 10022;
pub const ERR_OLD_STATE_ID: u32 = 10024;
pub const ERR_BAD_STATE_ID: u32 = 10025;
pub const ERR_SYMLINK: u32 = 10029;
pub const ERR_RESTORE_FH: u32 = 10030;
pub const ERR_ATTR_NOT_SUPP: u32 = 10032;
pub const ERR_NO_GRACE: u32 = 10033;
pub const ERR_BAD_XDR: u32 = 10036;
pub const ERR_LOCKS_HELD: u32 = 10037;
pub const ERR_OPEN_MODE: u32 = 10038;
pub const ERR_BAD_OWNER: u32 = 10039;
pub const ERR_BAD_NAME: u32 = 10041;
pub const ERR_LOCK_RANGE: u32 = 10028;
pub const ERR_OP_ILLEGAL: u32 = 10044;
pub const ERR_BAD_SESSION: u32 = 10052;
pub const ERR_BAD_SLOT: u32 = 10053;
pub const ERR_COMPLETE_ALREADY: u32 = 10054;
pub const ERR_CONN_NOT_BOUND: u32 = 10055;
pub const ERR_SEQ_MISORDERED: u32 = 10063;
pub const ERR_SEQUENCE_POS: u32 = 10064;
pub const ERR_REQ_TOO_BIG: u32 = 10065;
pub const ERR_REP_TOO_BIG: u32 = 10066;
pub const ERR_RETRY_UNCACHED: u32 = 10068;
pub const ERR_TOO_MANY_OPS: u32 = 10070;
pub const ERR_OP_NOT_IN_SESSION: u32 = 10071;
pub const ERR_CLIENT_ID_BUSY: u32 = 10074;
pub const ERR_SEQ_FALSE_RETRY: u32 = 10076;
pub const ERR_BAD_HIGH_SLOT: u32 = 10077;
pub const ERR_DEAD_SESSION: u32 = 10078;
pub const ERR_NOT_ONLY_OP: u32 = 10081;
pub const ERR_WRONG_TYPE: u32 = 10083;
pub const ERR_STALE_STATE_ID: u32 = 10023;
pub const ERR_LOCK_NOT_SUPP: u32 = 10043;
pub const ERR_NOT_SAME: u32 = 10027;
pub const ERR_WRONG_SEC: u32 = 10016;
pub const ERR_FILE_OPEN: u32 = 10046;
pub const ERR_ADMIN_REVOKED: u32 = 10047;
pub const ERR_DEADLOCK: u32 = 10045;
pub const ERR_BAD_RANGE: u32 = 10042;
pub const ERR_EXPIRED_STATE_ID: u32 = ERR_EXPIRED;
pub const ERR_RECLAIM_BAD: u32 = 10034;
pub const ERR_UNKNOWN_LAYOUT_TYP: u32 = 10062;

// File types.
pub const NF4_REG: u32 = 1;
pub const NF4_DIR: u32 = 2;
pub const NF4_BLK: u32 = 3;
pub const NF4_CHR: u32 = 4;
pub const NF4_LNK: u32 = 5;
pub const NF4_SOCK: u32 = 6;
pub const NF4_FIFO: u32 = 7;

// Attributes.
pub const ATTR_SUPPORTED_ATTRS: u32 = 0;
pub const ATTR_TYPE: u32 = 1;
pub const ATTR_FH_EXPIRE_TYPE: u32 = 2;
pub const ATTR_CHANGE: u32 = 3;
pub const ATTR_SIZE: u32 = 4;
pub const ATTR_LINK_SUPPORT: u32 = 5;
pub const ATTR_SYMLINK_SUPPORT: u32 = 6;
pub const ATTR_NAMED_ATTR: u32 = 7;
pub const ATTR_FSID: u32 = 8;
pub const ATTR_UNIQUE_HANDLES: u32 = 9;
pub const ATTR_LEASE_TIME: u32 = 10;
pub const ATTR_RDATTR_ERROR: u32 = 11;
pub const ATTR_ACL_SUPPORT: u32 = 13;
pub const ATTR_CAN_SET_TIME: u32 = 15;
pub const ATTR_CASE_INSENSITIVE: u32 = 16;
pub const ATTR_CASE_PRESERVING: u32 = 17;
pub const ATTR_CHOWN_RESTRICTED: u32 = 18;
pub const ATTR_FILEHANDLE: u32 = 19;
pub const ATTR_FILE_ID: u32 = 20;
pub const ATTR_FILES_AVAIL: u32 = 21;
pub const ATTR_FILES_FREE: u32 = 22;
pub const ATTR_FILES_TOTAL: u32 = 23;
pub const ATTR_HOMOGENEOUS: u32 = 26;
pub const ATTR_MAX_FILE_SIZE: u32 = 27;
pub const ATTR_MAX_LINK: u32 = 28;
pub const ATTR_MAX_NAME: u32 = 29;
pub const ATTR_MAX_READ: u32 = 30;
pub const ATTR_MAX_WRITE: u32 = 31;
pub const ATTR_MODE: u32 = 33;
pub const ATTR_NO_TRUNC: u32 = 34;
pub const ATTR_NUM_LINKS: u32 = 35;
pub const ATTR_OWNER: u32 = 36;
pub const ATTR_OWNER_GROUP: u32 = 37;
pub const ATTR_RAW_DEV: u32 = 41;
pub const ATTR_SPACE_AVAIL: u32 = 42;
pub const ATTR_SPACE_FREE: u32 = 43;
pub const ATTR_SPACE_TOTAL: u32 = 44;
pub const ATTR_SPACE_USED: u32 = 45;
pub const ATTR_TIME_ACCESS: u32 = 47;
pub const ATTR_TIME_ACCESS_SET: u32 = 48;
pub const ATTR_TIME_DELTA: u32 = 51;
pub const ATTR_TIME_METADATA: u32 = 52;
pub const ATTR_TIME_MODIFY: u32 = 53;
pub const ATTR_TIME_MODIFY_SET: u32 = 54;
pub const ATTR_MOUNTED_ON_FILE_ID: u32 = 55;
pub const ATTR_FS_LAYOUT_TYPES: u32 = 62;
pub const ATTR_LAYOUT_BLK_SIZE: u32 = 65;
pub const ATTR_SUPP_ATTR_EXCL_CREAT: u32 = 75;

// ACCESS bits.
pub const ACCESS_READ: u32 = 0x01;
pub const ACCESS_LOOKUP: u32 = 0x02;
pub const ACCESS_MODIFY: u32 = 0x04;
pub const ACCESS_EXTEND: u32 = 0x08;
pub const ACCESS_DELETE: u32 = 0x10;
pub const ACCESS_EXECUTE: u32 = 0x20;
pub const ACCESS_ALL: u32 = 0x3f;

// OPEN.
pub const OPEN_NO_CREATE: u32 = 0;
pub const OPEN_CREATE: u32 = 1;

pub const CREATE_UNCHECKED: u32 = 0;
pub const CREATE_GUARDED: u32 = 1;
pub const CREATE_EXCLUSIVE: u32 = 2;
pub const CREATE_EXCLUSIVE1: u32 = 3;

pub const CLAIM_NULL: u32 = 0;
pub const CLAIM_PREVIOUS: u32 = 1;
pub const CLAIM_DELEGATE_CUR: u32 = 2;
pub const CLAIM_DELEG_PREV: u32 = 3;
pub const CLAIM_FH: u32 = 4;
pub const CLAIM_DELEG_CUR_FH: u32 = 5;
pub const CLAIM_DELEG_PREV_FH: u32 = 6;

pub const SHARE_ACCESS_READ: u32 = 1;
pub const SHARE_ACCESS_WRITE: u32 = 2;
pub const SHARE_ACCESS_BOTH: u32 = 3;
pub const SHARE_ACCESS_MASK: u32 = 3;

pub const OPEN_RESULT_LOCKTYPE_POSIX: u32 = 4;
pub const DELEGATE_NONE: u32 = 0;

// LOCK types.
pub const LOCK_READ: u32 = 1;
pub const LOCK_WRITE: u32 = 2;
pub const LOCK_READ_W: u32 = 3;
pub const LOCK_WRITE_W: u32 = 4;

// WRITE stability.
pub const UNSTABLE: u32 = 0;
pub const DATA_SYNC: u32 = 1;
pub const FILE_SYNC: u32 = 2;

// EXCHANGE_ID and CREATE_SESSION.
pub const EXCHGID_USE_NON_PNFS: u32 = 0x00010000;
pub const EXCHGID_CONFIRMED_R: u32 = 0x80000000;
pub const EXCHGID_SUPP_MOVED_REF: u32 = 0x00000001;
pub const EXCHGID_SUPP_MOVED_MIG: u32 = 0x00000002;
pub const SP4_NONE: u32 = 0;
pub const SP4_MACH_CRED: u32 = 1;
pub const SP4_SSV: u32 = 2;

pub const CREATE_SESSION_PERSIST: u32 = 0x1;
