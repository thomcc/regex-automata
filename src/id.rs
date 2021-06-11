use core::{
    convert::{Infallible, TryFrom},
    mem, ops,
};

/// An identifier for a regex pattern.
///
/// The identifier for a pattern corresponds to its relative position among
/// other patterns in a single finite state machine. Namely, when building
/// a multi-pattern regex engine, one must supply a sequence of patterns to
/// match. The position (starting at 0) of each pattern in that sequence
/// represents its identifier. This identifier is in turn used to identify and
/// report matches of that pattern in various APIs.
///
/// A pattern ID is guaranteed to be representable by a `usize`. Similarly,
/// the number of patterns in any regex engine in this crate is similarly
/// guaranteed to be representable by a `usize`. This applies to regex engines
/// that have been deserialized; a deserialization error will be returned if
/// it contains pattern IDs that violate these requirements in your current
/// environment.
///
/// # Representation
///
/// This type is always represented internally by a `u32` and is marked as
/// `repr(transparent)`. Thus, this type always has the same representation as
/// a `u32`.
///
/// # Indexing
///
/// For convenience, callers may use a `PatternID` to index slices.
///
/// # Safety
///
/// While a `PatternID` is meant to guarantee that its value fits into `usize`
/// (while using a possibly smaller representation than `usize` on some
/// targets), callers must not rely on this property for safety. Callers may
/// choose to rely on this property for correctness however.
#[repr(transparent)]
#[derive(
    Clone, Copy, Debug, Default, Eq, Hash, PartialEq, PartialOrd, Ord,
)]
pub struct PatternID(u32);

impl PatternID {
    /// The maximum pattern ID value, represented as a `usize`.
    #[cfg(any(target_pointer_width = "32", target_pointer_width = "64"))]
    pub const MAX: PatternID = PatternID(core::u32::MAX - 1);

    /// The maximum pattern ID value, represented as a `usize`.
    #[cfg(target_pointer_width = "16")]
    pub const MAX: PatternID = PatternID(core::usize::MAX as u32 - 1);

    /// The total number of patterns that are allowed in any single regex
    /// engine.
    pub const LIMIT: usize = PatternID::MAX.0 as usize + 1;

    /// The zero pattern ID value.
    pub const ZERO: PatternID = PatternID(0);

    /// The number of bytes that a single `PatternID` uses in memory.
    pub const SIZE: usize = core::mem::size_of::<PatternID>();

    /// Create a new pattern ID.
    ///
    /// If the given identifier exceeds [`PatternID::MAX`], then this returns
    /// an error.
    #[inline]
    pub fn new(id: usize) -> Result<PatternID, PatternIDError> {
        PatternID::try_from(id)
    }

    /// Create a new pattern ID without checking whether the given value
    /// exceeds [`PatternID::MAX`].
    ///
    /// While this is unchecked, providing an incorrect value must never
    /// sacrifice memory safety, as documented above.
    #[inline]
    pub fn new_unchecked(id: usize) -> PatternID {
        PatternID(id as u32)
    }

    /// Like [`PatternID::new`], but panics if the given ID is not valid.
    #[inline]
    pub fn must(id: usize) -> PatternID {
        PatternID::new(id).unwrap()
    }

    /// Return this pattern ID as a `usize`.
    #[inline]
    pub fn as_usize(&self) -> usize {
        self.0 as usize
    }

    /// Return the internal u32 of this pattern ID.
    #[inline]
    pub fn as_u32(&self) -> u32 {
        self.0
    }

    /// Returns one more than this pattern ID as a usize.
    ///
    /// Since a pattern ID has constrains on its maximum value, adding `1` to
    /// it will always fit in a `usize` (and a `u32`).
    #[inline]
    pub fn one_more(&self) -> usize {
        self.as_usize().checked_add(1).unwrap()
    }

    /// Decode this pattern ID from the bytes given using the native endian
    /// byte order for the current target.
    ///
    /// If the decoded integer is not representable as a pattern ID for the
    /// current target, then this returns an error.
    #[inline]
    pub fn from_ne_bytes(bytes: [u8; 4]) -> Result<PatternID, PatternIDError> {
        let id = u32::from_ne_bytes(bytes);
        if id > PatternID::MAX.as_u32() {
            return Err(PatternIDError { attempted: id as u64 });
        }
        Ok(PatternID::new_unchecked(id as usize))
    }

    /// Decode this pattern ID from the bytes given using the native endian
    /// byte order for the current target.
    ///
    /// This is analogous to [`PatternID::new_unchecked`] in that is does not
    /// check whether the decoded integer is representable as a pattern ID.
    #[inline]
    pub fn from_ne_bytes_unchecked(bytes: [u8; 4]) -> PatternID {
        PatternID::new_unchecked(u32::from_ne_bytes(bytes) as usize)
    }

    /// Return the underlying pattern ID integer as raw bytes in native endian
    /// format.
    #[inline]
    pub fn to_ne_bytes(&self) -> [u8; 4] {
        self.0.to_ne_bytes()
    }

    /// Returns an iterator over all pattern IDs from 0 up to and not including
    /// the given length.
    ///
    /// If the given length exceeds [`PatternID::LIMIT`], then this panics.
    pub(crate) fn iter(len: usize) -> PatternIDIter {
        PatternIDIter::new(len)
    }
}

/// This error occurs when a pattern ID could not be constructed.
///
/// This occurs when given an integer exceeding the maximum pattern ID value.
///
/// When the `std` feature is enabled, this implements the `Error` trait.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PatternIDError {
    attempted: u64,
}

impl PatternIDError {
    /// Returns the value that failed to constructed a pattern ID.
    pub fn attempted(&self) -> u64 {
        self.attempted
    }
}

#[cfg(feature = "std")]
impl std::error::Error for PatternIDError {}

impl core::fmt::Display for PatternIDError {
    fn fmt(&self, f: &mut core::fmt::Formatter) -> core::fmt::Result {
        write!(
            f,
            "failed to create PatternID from {:?}, which exceeds {:?}",
            self.attempted(),
            PatternID::MAX,
        )
    }
}

/// An identifier for a state in a regex engine.
///
/// A state ID is guaranteed to be representable by a `usize`. Similarly, the
/// number of states in any regex engine in this crate is similarly guaranteed
/// to be representable by a `usize`. This applies to regex engines that have
/// been deserialized; a deserialization error will be returned if it contains
/// state IDs that violate these requirements in your current environment.
///
/// # Representation
///
/// This type is always represented internally by a `u32` and is marked as
/// `repr(transparent)`. Thus, this type always has the same representation as
/// a `u32`.
///
/// # Indexing
///
/// For convenience, callers may use a `StateID` to index slices.
///
/// # Safety
///
/// While a `StateID` is meant to guarantee that its value fits into `usize`
/// (while using a possibly smaller representation than `usize` on some
/// targets), callers must not rely on this property for safety. Callers may
/// choose to rely on this property for correctness however.
#[repr(transparent)]
#[derive(
    Clone, Copy, Debug, Default, Eq, Hash, PartialEq, PartialOrd, Ord,
)]
pub struct StateID(u32);

impl StateID {
    /// The maximum state ID value.
    #[cfg(any(target_pointer_width = "32", target_pointer_width = "64"))]
    pub const MAX: StateID = StateID(core::u32::MAX - 1);

    /// The maximum state ID value.
    #[cfg(target_pointer_width = "16")]
    pub const MAX: StateID = StateID(core::usize::MAX as u32 - 1);

    /// The total number of states that are allowed in any single regex
    /// engine, represented as a `usize`.
    pub const LIMIT: usize = StateID::MAX.0 as usize + 1;

    /// The zero state ID value.
    pub const ZERO: StateID = StateID(0);

    /// The number of bytes that a single `StateID` uses in memory.
    pub const SIZE: usize = core::mem::size_of::<StateID>();

    /// A unique state ID that always corresponds to the dead state for DFAs.
    pub(crate) const DEAD: StateID = StateID(0);

    /// Create a new state ID.
    ///
    /// If the given identifier exceeds [`StateID::MAX`], then this returns
    /// an error.
    #[inline]
    pub fn new(id: usize) -> Result<StateID, StateIDError> {
        StateID::try_from(id)
    }

    /// Create a new pattern ID without checking whether the given value
    /// exceeds [`PatternID::MAX`].
    ///
    /// While this is unchecked, providing an incorrect value must never
    /// sacrifice memory safety, as documented above.
    #[inline]
    pub fn new_unchecked(id: usize) -> StateID {
        StateID(id as u32)
    }

    /// Like [`StateID::new`], but panics if the given ID is not valid.
    #[inline]
    pub fn must(id: usize) -> StateID {
        StateID::new(id).unwrap()
    }

    /// Return this pattern ID as a `usize`.
    #[inline]
    pub fn as_usize(&self) -> usize {
        self.0 as usize
    }

    /// Return the internal u32 of this state ID.
    #[inline]
    pub fn as_u32(&self) -> u32 {
        self.0
    }

    /// Returns one more than this state ID as a usize.
    ///
    /// Since a state ID has constrains on its maximum value, adding `1` to
    /// it will always fit in a `usize` (and a `u32`).
    #[inline]
    pub fn one_more(&self) -> usize {
        self.as_usize().checked_add(1).unwrap()
    }

    /// Decode this state ID from the bytes given using the native endian byte
    /// order for the current target.
    ///
    /// If the decoded integer is not representable as a state ID for the
    /// current target, then this returns an error.
    #[inline]
    pub fn from_ne_bytes(bytes: [u8; 4]) -> Result<StateID, StateIDError> {
        let id = u32::from_ne_bytes(bytes);
        if id > StateID::MAX.as_u32() {
            return Err(StateIDError { attempted: id as u64 });
        }
        Ok(StateID::new_unchecked(id as usize))
    }

    /// Decode this state ID from the bytes given using the native endian
    /// byte order for the current target.
    ///
    /// This is analogous to [`StateID::new_unchecked`] in that is does not
    /// check whether the decoded integer is representable as a state ID.
    #[inline]
    pub fn from_ne_bytes_unchecked(bytes: [u8; 4]) -> StateID {
        StateID::new_unchecked(u32::from_ne_bytes(bytes) as usize)
    }

    /// Return the underlying state ID integer as raw bytes in native endian
    /// format.
    #[inline]
    pub fn to_ne_bytes(&self) -> [u8; 4] {
        self.0.to_ne_bytes()
    }

    /// Returns an iterator over all state IDs from 0 up to and not including
    /// the given length.
    ///
    /// If the given length exceeds [`StateID::LIMIT`], then this panics.
    #[inline]
    pub(crate) fn iter(len: usize) -> StateIDIter {
        StateIDIter::new(len)
    }
}

/// This error occurs when a state ID could not be constructed.
///
/// This occurs when given an integer exceeding the maximum state ID value.
///
/// When the `std` feature is enabled, this implements the `Error` trait.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StateIDError {
    attempted: u64,
}

impl StateIDError {
    /// Returns the value that failed to constructed a state ID.
    pub fn attempted(&self) -> u64 {
        self.attempted
    }
}

#[cfg(feature = "std")]
impl std::error::Error for StateIDError {}

impl core::fmt::Display for StateIDError {
    fn fmt(&self, f: &mut core::fmt::Formatter) -> core::fmt::Result {
        write!(
            f,
            "failed to create StateID from {:?}, which exceeds {:?}",
            self.attempted(),
            StateID::MAX,
        )
    }
}

/// A macro for defining exactly identical (modulo names) impls for ID types.
macro_rules! impls {
    ($ty:ident, $tyerr:ident, $tyiter:ident) => {
        #[derive(Clone, Debug)]
        pub(crate) struct $tyiter {
            rng: ops::Range<usize>,
        }

        impl $tyiter {
            fn new(len: usize) -> $tyiter {
                assert!(
                    len <= $ty::LIMIT,
                    "cannot create iterator with IDs when number of \
                     elements exceed {:?}",
                    $ty::LIMIT,
                );
                $tyiter { rng: 0..len }
            }
        }

        impl Iterator for $tyiter {
            type Item = $ty;

            fn next(&mut self) -> Option<$ty> {
                if self.rng.start >= self.rng.end {
                    return None;
                }
                let next_id = self.rng.start + 1;
                let id = mem::replace(&mut self.rng.start, next_id);
                // new_unchecked is OK since we asserted that the number of
                // elements in this iterator will fit in an ID at construction.
                Some($ty::new_unchecked(id))
            }
        }

        impl<T> core::ops::Index<$ty> for [T] {
            type Output = T;

            #[inline]
            fn index(&self, index: $ty) -> &T {
                &self[index.as_usize()]
            }
        }

        impl<T> core::ops::IndexMut<$ty> for [T] {
            #[inline]
            fn index_mut(&mut self, index: $ty) -> &mut T {
                &mut self[index.as_usize()]
            }
        }

        #[cfg(feature = "alloc")]
        impl<T> core::ops::Index<$ty> for Vec<T> {
            type Output = T;

            #[inline]
            fn index(&self, index: $ty) -> &T {
                &self[index.as_usize()]
            }
        }

        #[cfg(feature = "alloc")]
        impl<T> core::ops::IndexMut<$ty> for Vec<T> {
            #[inline]
            fn index_mut(&mut self, index: $ty) -> &mut T {
                &mut self[index.as_usize()]
            }
        }

        impl TryFrom<usize> for $ty {
            type Error = $tyerr;

            fn try_from(id: usize) -> Result<$ty, $tyerr> {
                if id > $ty::MAX.as_usize() {
                    return Err($tyerr { attempted: id as u64 });
                }
                Ok($ty::new_unchecked(id))
            }
        }

        impl TryFrom<u8> for $ty {
            type Error = Infallible;

            fn try_from(id: u8) -> Result<$ty, Infallible> {
                Ok($ty::new_unchecked(id as usize))
            }
        }

        impl TryFrom<u16> for $ty {
            type Error = $tyerr;

            fn try_from(id: u16) -> Result<$ty, $tyerr> {
                if id as u32 > $ty::MAX.as_u32() {
                    return Err($tyerr { attempted: id as u64 });
                }
                Ok($ty::new_unchecked(id as usize))
            }
        }

        impl TryFrom<u32> for $ty {
            type Error = $tyerr;

            fn try_from(id: u32) -> Result<$ty, $tyerr> {
                if id > $ty::MAX.as_u32() {
                    return Err($tyerr { attempted: id as u64 });
                }
                Ok($ty::new_unchecked(id as usize))
            }
        }

        impl TryFrom<u64> for $ty {
            type Error = $tyerr;

            fn try_from(id: u64) -> Result<$ty, $tyerr> {
                if id > $ty::MAX.as_u32() as u64 {
                    return Err($tyerr { attempted: id });
                }
                Ok($ty::new_unchecked(id as usize))
            }
        }
    };
}

impls!(PatternID, PatternIDError, PatternIDIter);
impls!(StateID, StateIDError, StateIDIter);

/// A utility trait that defines a couple of adapters for making it convenient
/// to access indices as ID types. We require ExactSizeIterator so that
/// iterator construction can do a single check to make sure the index of each
/// element is representable by its ID type.
pub(crate) trait IteratorIDExt: Iterator {
    fn with_pattern_ids(self) -> WithPatternIDIter<Self>
    where
        Self: Sized + ExactSizeIterator,
    {
        WithPatternIDIter::new(self)
    }

    fn with_state_ids(self) -> WithStateIDIter<Self>
    where
        Self: Sized + ExactSizeIterator,
    {
        WithStateIDIter::new(self)
    }
}

impl<I: Iterator> IteratorIDExt for I {}

macro_rules! iditer {
    ($ty:ident, $iterty:ident, $withiterty:ident) => {
        /// An iterator adapter that is like std::iter::Enumerate, but attaches
        /// IDs. It requires ExactSizeIterator. At construction, it ensures
        /// that the index of each element in the iterator is representable in
        /// the corresponding ID type.
        #[derive(Clone, Debug)]
        pub(crate) struct $withiterty<I> {
            it: I,
            ids: $iterty,
        }

        impl<I: Iterator + ExactSizeIterator> $withiterty<I> {
            fn new(it: I) -> $withiterty<I> {
                let ids = $ty::iter(it.len());
                $withiterty { it, ids }
            }
        }

        impl<I: Iterator + ExactSizeIterator> Iterator for $withiterty<I> {
            type Item = ($ty, I::Item);

            fn next(&mut self) -> Option<($ty, I::Item)> {
                let item = self.it.next()?;
                // Number of elements in this iterator must match, according
                // to contract of ExactSizeIterator.
                let id = self.ids.next().unwrap();
                Some((id, item))
            }
        }
    };
}

iditer!(PatternID, PatternIDIter, WithPatternIDIter);
iditer!(StateID, StateIDIter, WithStateIDIter);
