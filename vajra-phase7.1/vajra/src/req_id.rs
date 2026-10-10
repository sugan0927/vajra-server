//! Request-ID tracing.
//!
//! The current request's ID lives in a thread-local: one worker owns each
//! thread and processes requests serially, so no request can see another's.
//! This keeps the ID out of every function signature — nothing in the
//! existing API changes.

use std::cell::RefCell;
use std::fmt;

pub const MAX_LEN: usize = 128;
const UUID_LEN: usize = 36;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IdKind { Client, Generated }

#[derive(Clone)]
pub struct RequestId {
    bytes: [u8; MAX_LEN],
    len: u8,
    kind: IdKind,
}

impl RequestId {
    pub fn from_client(s: &str) -> Option<Self> {
        let b = s.as_bytes();
        if b.is_empty() || b.len() > MAX_LEN { return None; }
        if !b.iter().all(|&c| c.is_ascii_alphanumeric() || c == b'-') { return None; }
        let mut bytes = [0u8; MAX_LEN];
        bytes[..b.len()].copy_from_slice(b);
        Some(Self { bytes, len: b.len() as u8, kind: IdKind::Client })
    }

    pub fn generate() -> Self {
        let mut r = [0u8; 16];
        getrandom::getrandom(&mut r).expect("getrandom");
        r[6] = (r[6] & 0x0f) | 0x40;
        r[8] = (r[8] & 0x3f) | 0x80;
        let hex = b"0123456789abcdef";
        let mut bytes = [0u8; MAX_LEN];
        let mut j = 0usize;
        for (i, &b) in r.iter().enumerate() {
            if matches!(i, 4 | 6 | 8 | 10) { bytes[j] = b'-'; j += 1; }
            bytes[j] = hex[(b >> 4) as usize];
            bytes[j + 1] = hex[(b & 0x0f) as usize];
            j += 2;
        }
        debug_assert_eq!(j, UUID_LEN);
        Self { bytes, len: UUID_LEN as u8, kind: IdKind::Generated }
    }

    #[inline]
    pub fn as_str(&self) -> &str {
        unsafe { std::str::from_utf8_unchecked(&self.bytes[..self.len as usize]) }
    }
    #[inline] pub fn len(&self) -> usize { self.len as usize }
    #[inline] pub fn kind(&self) -> IdKind { self.kind }
}

impl AsRef<str> for RequestId {
    #[inline] fn as_ref(&self) -> &str { self.as_str() }
}
impl fmt::Display for RequestId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { f.write_str(self.as_str()) }
}
impl fmt::Debug for RequestId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "RequestId({}, {:?})", self.as_str(), self.kind)
    }
}

thread_local! {
    static CURRENT: RefCell<Option<RequestId>> = const { RefCell::new(None) };
}

pub fn set_opt(id: Option<RequestId>) { CURRENT.with(|c| *c.borrow_mut() = id); }
pub fn clear() { set_opt(None); }
pub fn current() -> Option<RequestId> { CURRENT.with(|c| c.borrow().clone()) }

pub fn extract_from_h1(headers: &[httparse::Header<'_>]) -> Option<RequestId> {
    for h in headers {
        if h.name.eq_ignore_ascii_case("x-request-id") {
            let s = std::str::from_utf8(h.value).ok()?;
            return RequestId::from_client(s);
        }
    }
    None
}

pub fn extract_from_h2(headers: &[(Vec<u8>, Vec<u8>)]) -> Option<RequestId> {
    for (n, v) in headers {
        if n.eq_ignore_ascii_case(b"x-request-id") {
            let s = std::str::from_utf8(v).ok()?;
            return RequestId::from_client(s);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn accepts_valid() {
        let id = RequestId::from_client("abc-123-XYZ").unwrap();
        assert_eq!(id.as_str(), "abc-123-XYZ");
        assert_eq!(id.kind(), IdKind::Client);
    }
    #[test] fn rejects_bad() {
        for s in ["", "a b", "a/b", "a\nb", "a\"b", "café", "a\0b"] {
            assert!(RequestId::from_client(s).is_none(), "{s:?}");
        }
        assert!(RequestId::from_client(&"a".repeat(MAX_LEN + 1)).is_none());
    }
    #[test] fn generated_is_uuidv4() {
        let id = RequestId::generate();
        let s = id.as_str();
        assert_eq!(s.len(), UUID_LEN);
        for i in [8, 13, 18, 23] { assert_eq!(&s[i..i+1], "-"); }
        assert_eq!(&s[14..15], "4");
        assert!(matches!(&s[19..20], "8" | "9" | "a" | "b"));
    }
    #[test] fn thread_local_round_trip() {
        set_opt(RequestId::from_client("t-1"));
        assert_eq!(current().unwrap().as_str(), "t-1");
        clear();
        assert!(current().is_none());
    }
}
