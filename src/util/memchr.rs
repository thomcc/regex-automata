pub use self::inner::*;

#[cfg(feature = "perf-literal-substring")]
pub(super) mod inner {
    #[inline(always)]
    pub(crate) fn memchr(n1: u8, haystack: &[u8]) -> Option<usize> {
        memchr::memchr(n1, haystack)
    }

    #[inline(always)]
    pub(crate) fn memchr2(n1: u8, n2: u8, haystack: &[u8]) -> Option<usize> {
        memchr::memchr2(n1, n2, haystack)
    }

    #[inline(always)]
    pub(crate) fn memchr3(
        n1: u8,
        n2: u8,
        n3: u8,
        haystack: &[u8],
    ) -> Option<usize> {
        memchr::memchr3(n1, n2, n3, haystack)
    }

    #[inline(always)]
    pub(crate) fn memrchr(n1: u8, haystack: &[u8]) -> Option<usize> {
        memchr::memrchr(n1, haystack)
    }

    #[inline(always)]
    pub(crate) fn memrchr2(n1: u8, n2: u8, haystack: &[u8]) -> Option<usize> {
        memchr::memrchr2(n1, n2, haystack)
    }

    #[inline(always)]
    pub(crate) fn memrchr3(
        n1: u8,
        n2: u8,
        n3: u8,
        haystack: &[u8],
    ) -> Option<usize> {
        memchr::memrchr3(n1, n2, n3, haystack)
    }
}

#[cfg(not(feature = "perf-literal-substring"))]
pub(super) mod inner {
    #[inline(always)]
    pub(crate) fn memchr(n1: u8, haystack: &[u8]) -> Option<usize> {
        haystack.iter().position(|&b| b == n1)
    }

    #[inline(always)]
    pub(crate) fn memchr2(n1: u8, n2: u8, haystack: &[u8]) -> Option<usize> {
        haystack.iter().position(|&b| b == n1 || b == n2)
    }

    #[inline(always)]
    pub(crate) fn memchr3(
        n1: u8,
        n2: u8,
        n3: u8,
        haystack: &[u8],
    ) -> Option<usize> {
        haystack.iter().position(|&b| b == n1 || b == n2 || b == n3)
    }

    #[inline(always)]
    pub(crate) fn memrchr(n1: u8, haystack: &[u8]) -> Option<usize> {
        haystack.iter().rposition(|&b| b == n1)
    }

    #[inline(always)]
    pub(crate) fn memrchr2(n1: u8, n2: u8, haystack: &[u8]) -> Option<usize> {
        haystack.iter().rposition(|&b| b == n1 || b == n2)
    }

    #[inline(always)]
    pub(crate) fn memrchr3(
        n1: u8,
        n2: u8,
        n3: u8,
        haystack: &[u8],
    ) -> Option<usize> {
        haystack.iter().rposition(|&b| b == n1 || b == n2 || b == n3)
    }
}