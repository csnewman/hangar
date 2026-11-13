//! XDR (RFC 4506), as NFS encodes its messages.

/// Decodes XDR. A decoding error sticks: every read after it returns zero
/// values, and failed says one went wrong.
pub struct Reader<'a> {
    pub b: &'a [u8],
    pub failed: bool,
}

impl<'a> Reader<'a> {
    pub fn new(b: &'a [u8]) -> Reader<'a> {
        Reader { b, failed: false }
    }

    fn fail(&mut self) {
        self.failed = true;
        self.b = &[];
    }

    pub fn u32(&mut self) -> u32 {
        if self.b.len() < 4 {
            self.fail();
            return 0;
        }
        let v = u32::from_be_bytes(self.b[..4].try_into().unwrap());
        self.b = &self.b[4..];
        v
    }

    pub fn u64(&mut self) -> u64 {
        if self.b.len() < 8 {
            self.fail();
            return 0;
        }
        let v = u64::from_be_bytes(self.b[..8].try_into().unwrap());
        self.b = &self.b[8..];
        v
    }

    pub fn i64(&mut self) -> i64 {
        self.u64() as i64
    }

    pub fn bool(&mut self) -> bool {
        self.u32() != 0
    }

    /// n bytes of fixed-length opaque data, padded to four.
    pub fn fixed(&mut self, n: usize) -> &'a [u8] {
        let padded = (n + 3) & !3;
        if self.b.len() < padded {
            self.fail();
            return &[];
        }
        let v = &self.b[..n];
        self.b = &self.b[padded..];
        v
    }

    /// Variable-length opaque data, at most max bytes.
    pub fn opaque(&mut self, max: usize) -> &'a [u8] {
        let n = self.u32();
        if self.failed || n as usize > max {
            self.fail();
            return &[];
        }
        self.fixed(n as usize)
    }

    pub fn string(&mut self, max: usize) -> Vec<u8> {
        self.opaque(max).to_vec()
    }

    /// An array's length, at most max.
    pub fn count(&mut self, max: usize) -> usize {
        let n = self.u32();
        if self.failed || n as usize > max {
            self.fail();
            return 0;
        }
        n as usize
    }

    pub fn bitmap(&mut self) -> Bitmap {
        let n = self.count(8);
        Bitmap((0..n).map(|_| self.u32()).collect())
    }
}

/// Encodes XDR.
#[derive(Default)]
pub struct Writer {
    pub b: Vec<u8>,
}

impl Writer {
    pub fn u32(&mut self, v: u32) {
        self.b.extend_from_slice(&v.to_be_bytes());
    }

    pub fn u64(&mut self, v: u64) {
        self.b.extend_from_slice(&v.to_be_bytes());
    }

    pub fn i64(&mut self, v: i64) {
        self.u64(v as u64);
    }

    pub fn bool(&mut self, v: bool) {
        self.u32(v as u32);
    }

    pub fn fixed(&mut self, v: &[u8]) {
        self.b.extend_from_slice(v);
        while !self.b.len().is_multiple_of(4) {
            self.b.push(0);
        }
    }

    pub fn opaque(&mut self, v: &[u8]) {
        self.u32(v.len() as u32);
        self.fixed(v);
    }

    pub fn bitmap(&mut self, b: &Bitmap) {
        self.u32(b.0.len() as u32);
        for v in &b.0 {
            self.u32(*v);
        }
    }
}

/// A bitmap4: bit n is word n/32's bit n%32.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Bitmap(pub Vec<u32>);

impl Bitmap {
    pub fn has(&self, n: usize) -> bool {
        n / 32 < self.0.len() && self.0[n / 32] & (1 << (n % 32)) != 0
    }

    pub fn set(&mut self, n: usize) {
        while self.0.len() <= n / 32 {
            self.0.push(0);
        }
        self.0[n / 32] |= 1 << (n % 32);
    }

    /// The bits set in both.
    pub fn and(&self, o: &Bitmap) -> Bitmap {
        let mut out = Bitmap(self.0.iter().zip(&o.0).map(|(a, b)| a & b).collect());
        out.trim();
        out
    }

    /// Drops trailing empty words.
    pub fn trim(&mut self) {
        while self.0.last() == Some(&0) {
            self.0.pop();
        }
    }
}
