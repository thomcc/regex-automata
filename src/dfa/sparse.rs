/*!
Types and routines specific to sparse DFAs.

This module is the home of [`sparse::DFA`](DFA).

Unlike the [`dense`](super::dense) module, this module does not contain a
builder or configuration specific for sparse DFAs. Instead, the intended
way to build a sparse DFA is either by using a default configuration with
its constructor [`sparse::DFA::new`](DFA::new), or by first configuring the
construction of a dense DFA with [`dense::Builder`](super::dense::Builder)
and then calling [`dense::DFA::to_sparse`](super::dense::DFA::to_sparse). For
example, this configures a sparse DFA to do an overlapping search:

```
use regex_automata::{
    dfa::{Automaton, OverlappingState, dense},
    HalfMatch, MatchKind, Input,
};

let dense_re = dense::Builder::new()
    .configure(dense::Config::new().match_kind(MatchKind::All))
    .build(r"Samwise|Sam")?;
let sparse_re = dense_re.to_sparse()?;

// Setup our haystack and initial start state.
let input = Input::new("Samwise");
let mut state = OverlappingState::start();

// First, 'Sam' will match.
sparse_re.try_search_overlapping_fwd(&input, &mut state)?;
assert_eq!(Some(HalfMatch::must(0, 3)), state.get_match());

// And now 'Samwise' will match.
sparse_re.try_search_overlapping_fwd(&input, &mut state)?;
assert_eq!(Some(HalfMatch::must(0, 7)), state.get_match());
# Ok::<(), Box<dyn std::error::Error>>(())
```
*/

#[cfg(feature = "alloc")]
use core::iter;
use core::{
    convert::{TryFrom, TryInto},
    fmt,
    mem::size_of,
};

#[cfg(feature = "alloc")]
use alloc::{collections::BTreeSet, vec, vec::Vec};

#[cfg(feature = "alloc")]
use crate::dfa::{dense, error::Error};
use crate::{
    dfa::{
        automaton::{fmt_state_indicator, Automaton},
        special::Special,
        DEAD,
    },
    util::{
        alphabet::ByteClasses,
        bytes::{self, DeserializeError, Endian, SerializeError},
        escape::DebugByte,
        id::{PatternID, StateID},
        search::Input,
        start::Start,
    },
};

const LABEL: &str = "rust-regex-automata-dfa-sparse";
const VERSION: u32 = 2;

/// A sparse deterministic finite automaton (DFA) with variable sized states.
///
/// In contrast to a [dense::DFA](crate::dfa::dense::DFA), a sparse DFA uses
/// a more space efficient representation for its transitions. Consequently,
/// sparse DFAs may use much less memory than dense DFAs, but this comes at a
/// price. In particular, reading the more space efficient transitions takes
/// more work, and consequently, searching using a sparse DFA is typically
/// slower than a dense DFA.
///
/// A sparse DFA can be built using the default configuration via the
/// [`DFA::new`] constructor. Otherwise, one can configure various aspects
/// of a dense DFA via [`dense::Builder`](crate::dfa::dense::Builder),
/// and then convert a dense DFA to a sparse DFA using
/// [`dense::DFA::to_sparse`](crate::dfa::dense::DFA::to_sparse).
///
/// In general, a sparse DFA supports all the same search operations as a dense
/// DFA.
///
/// Making the choice between a dense and sparse DFA depends on your specific
/// work load. If you can sacrifice a bit of search time performance, then a
/// sparse DFA might be the best choice. In particular, while sparse DFAs are
/// probably always slower than dense DFAs, you may find that they are easily
/// fast enough for your purposes!
///
/// # Type parameters
///
/// A `DFA` has one type parameter, `T`, which is used to represent the parts
/// of a sparse DFA. `T` is typically a `Vec<u8>` or a `&[u8]`.
///
/// # The `Automaton` trait
///
/// This type implements the [`Automaton`] trait, which means it can be used
/// for searching. For example:
///
/// ```
/// use regex_automata::{
///     dfa::{Automaton, sparse::DFA},
///     HalfMatch,
/// };
///
/// let dfa = DFA::new("foo[0-9]+")?;
/// let expected = HalfMatch::must(0, 8);
/// assert_eq!(Some(expected), dfa.try_find_fwd(b"foo12345")?);
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[derive(Clone)]
pub struct DFA<T> {
    // When compared to a dense DFA, a sparse DFA *looks* a lot simpler
    // representation-wise. In reality, it is perhaps more complicated. Namely,
    // in a dense DFA, all information needs to be very cheaply accessible
    // using only state IDs. In a sparse DFA however, each state uses a
    // variable amount of space because each state encodes more information
    // than just its transitions. Each state also includes an accelerator if
    // one exists, along with the matching pattern IDs if the state is a match
    // state.
    //
    // That is, a lot of the complexity is pushed down into how each state
    // itself is represented.
    trans: Transitions<T>,
    starts: StartTable<T>,
    special: Special,
}

#[cfg(feature = "alloc")]
impl DFA<Vec<u8>> {
    /// Parse the given regular expression using a default configuration and
    /// return the corresponding sparse DFA.
    ///
    /// If you want a non-default configuration, then use
    /// the [`dense::Builder`](crate::dfa::dense::Builder)
    /// to set your own configuration, and then call
    /// [`dense::DFA::to_sparse`](crate::dfa::dense::DFA::to_sparse) to create
    /// a sparse DFA.
    ///
    /// # Example
    ///
    /// ```
    /// use regex_automata::{
    ///     dfa::{Automaton, sparse},
    ///     HalfMatch,
    /// };
    ///
    /// let dfa = sparse::DFA::new("foo[0-9]+bar")?;
    ///
    /// let expected = HalfMatch::must(0, 11);
    /// assert_eq!(Some(expected), dfa.try_find_fwd(b"foo12345bar")?);
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn new(pattern: &str) -> Result<DFA<Vec<u8>>, Error> {
        dense::Builder::new()
            .build(pattern)
            .and_then(|dense| dense.to_sparse())
    }

    /// Parse the given regular expressions using a default configuration and
    /// return the corresponding multi-DFA.
    ///
    /// If you want a non-default configuration, then use
    /// the [`dense::Builder`](crate::dfa::dense::Builder)
    /// to set your own configuration, and then call
    /// [`dense::DFA::to_sparse`](crate::dfa::dense::DFA::to_sparse) to create
    /// a sparse DFA.
    ///
    /// # Example
    ///
    /// ```
    /// use regex_automata::{
    ///     dfa::{Automaton, sparse},
    ///     HalfMatch,
    /// };
    ///
    /// let dfa = sparse::DFA::new_many(&["[0-9]+", "[a-z]+"])?;
    /// let expected = HalfMatch::must(1, 3);
    /// assert_eq!(Some(expected), dfa.try_find_fwd(b"foo12345bar")?);
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn new_many<P: AsRef<str>>(
        patterns: &[P],
    ) -> Result<DFA<Vec<u8>>, Error> {
        dense::Builder::new()
            .build_many(patterns)
            .and_then(|dense| dense.to_sparse())
    }
}

#[cfg(feature = "alloc")]
impl DFA<Vec<u8>> {
    /// Create a new DFA that matches every input.
    ///
    /// # Example
    ///
    /// ```
    /// use regex_automata::{
    ///     dfa::{Automaton, sparse},
    ///     HalfMatch,
    /// };
    ///
    /// let dfa = sparse::DFA::always_match()?;
    ///
    /// let expected = HalfMatch::must(0, 0);
    /// assert_eq!(Some(expected), dfa.try_find_fwd(b"")?);
    /// assert_eq!(Some(expected), dfa.try_find_fwd(b"foo")?);
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn always_match() -> Result<DFA<Vec<u8>>, Error> {
        dense::DFA::always_match()?.to_sparse()
    }

    /// Create a new sparse DFA that never matches any input.
    ///
    /// # Example
    ///
    /// ```
    /// use regex_automata::dfa::{Automaton, sparse};
    ///
    /// let dfa = sparse::DFA::never_match()?;
    /// assert_eq!(None, dfa.try_find_fwd(b"")?);
    /// assert_eq!(None, dfa.try_find_fwd(b"foo")?);
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn never_match() -> Result<DFA<Vec<u8>>, Error> {
        dense::DFA::never_match()?.to_sparse()
    }

    /// The implementation for constructing a sparse DFA from a dense DFA.
    pub(crate) fn from_dense<T: AsRef<[u32]> + Send + Sync>(
        dfa: &dense::DFA<T>,
    ) -> Result<DFA<Vec<u8>>, Error> {
        // In order to build the transition table, we need to be able to write
        // state identifiers for each of the "next" transitions in each state.
        // Our state identifiers correspond to the byte offset in the
        // transition table at which the state is encoded. Therefore, we do not
        // actually know what the state identifiers are until we've allocated
        // exactly as much space as we need for each state. Thus, construction
        // of the transition table happens in two passes.
        //
        // In the first pass, we fill out the shell of each state, which
        // includes the transition length, the input byte ranges and
        // zero-filled space for the transitions and accelerators, if present.
        // In this first pass, we also build up a map from the state identifier
        // index of the dense DFA to the state identifier in this sparse DFA.
        //
        // In the second pass, we fill in the transitions based on the map
        // built in the first pass.

        // The capacity given here reflects a minimum. (Well, the true minimum
        // is likely even bigger, but hopefully this saves a few reallocs.)
        let mut sparse = Vec::with_capacity(StateID::SIZE * dfa.state_len());
        // This maps state indices from the dense DFA to StateIDs in the sparse
        // DFA. We build out this map on the first pass, and then use it in the
        // second pass to back-fill our transitions.
        let mut remap: Vec<StateID> = vec![DEAD; dfa.state_len()];
        for state in dfa.states() {
            let pos = sparse.len();

            remap[dfa.to_index(state.id())] =
                StateID::new(pos).map_err(|_| Error::too_many_states())?;
            // zero-filled space for the transition length
            sparse.push(0);
            sparse.push(0);

            let mut transition_len = 0;
            for (unit1, unit2, _) in state.sparse_transitions() {
                match (unit1.as_u8(), unit2.as_u8()) {
                    (Some(b1), Some(b2)) => {
                        transition_len += 1;
                        sparse.push(b1);
                        sparse.push(b2);
                    }
                    (None, None) => {}
                    (Some(_), None) | (None, Some(_)) => {
                        // can never occur because sparse_transitions never
                        // groups EOI with any other transition.
                        unreachable!()
                    }
                }
            }
            // Add dummy EOI transition. This is never actually read while
            // searching, but having space equivalent to the total number
            // of transitions is convenient. Otherwise, we'd need to track
            // a different number of transitions for the byte ranges as for
            // the 'next' states.
            //
            // N.B. The loop above is not guaranteed to yield the EOI
            // transition, since it may point to a DEAD state. By putting
            // it here, we always write the EOI transition, and thus
            // guarantee that our transition length is >0. Why do we always
            // need the EOI transition? Because in order to implement
            // Automaton::next_eoi_state, this lets us just ask for the last
            // transition. There are probably other/better ways to do this.
            transition_len += 1;
            sparse.push(0);
            sparse.push(0);

            // Check some assumptions about transition length.
            assert_ne!(
                transition_len, 0,
                "transition length should be non-zero",
            );
            assert!(
                transition_len <= 257,
                "expected transition length {} to be <= 257",
                transition_len,
            );

            // Fill in the transition length.
            // Since transition length is always <= 257, we use the most
            // significant bit to indicate whether this is a match state or
            // not.
            let ntrans = if dfa.is_match_state(state.id()) {
                transition_len | (1 << 15)
            } else {
                transition_len
            };
            bytes::NE::write_u16(ntrans, &mut sparse[pos..]);

            // zero-fill the actual transitions.
            // Unwraps are OK since transition_length <= 257 and our minimum
            // support usize size is 16-bits.
            let zeros = usize::try_from(transition_len)
                .unwrap()
                .checked_mul(StateID::SIZE)
                .unwrap();
            sparse.extend(iter::repeat(0).take(zeros));

            // If this is a match state, write the pattern IDs matched by this
            // state.
            if dfa.is_match_state(state.id()) {
                let plen = dfa.match_pattern_len(state.id());
                // Write the actual pattern IDs with a u32 length prefix.
                // First, zero-fill space.
                let mut pos = sparse.len();
                // Unwraps are OK since it's guaranteed that plen <=
                // PatternID::LIMIT, which is in turn guaranteed to fit into a
                // u32.
                let zeros = size_of::<u32>()
                    .checked_mul(plen)
                    .unwrap()
                    .checked_add(size_of::<u32>())
                    .unwrap();
                sparse.extend(iter::repeat(0).take(zeros));

                // Now write the length prefix.
                bytes::NE::write_u32(
                    // Will never fail since u32::MAX is invalid pattern ID.
                    // Thus, the number of pattern IDs is representable by a
                    // u32.
                    plen.try_into().expect("pattern ID length fits in u32"),
                    &mut sparse[pos..],
                );
                pos += size_of::<u32>();

                // Now write the pattern IDs.
                for &pid in dfa.pattern_id_slice(state.id()) {
                    pos += bytes::write_pattern_id::<bytes::NE>(
                        pid,
                        &mut sparse[pos..],
                    );
                }
            }

            // And now add the accelerator, if one exists. An accelerator is
            // at most 4 bytes and at least 1 byte. The first byte is the
            // length, N. N bytes follow the length. The set of bytes that
            // follow correspond (exhaustively) to the bytes that must be seen
            // to leave this state.
            let accel = dfa.accelerator(state.id());
            sparse.push(accel.len().try_into().unwrap());
            sparse.extend_from_slice(accel);
        }

        let mut new = DFA {
            trans: Transitions {
                sparse,
                classes: dfa.byte_classes().clone(),
                len: dfa.state_len(),
                pattern_len: dfa.pattern_len(),
            },
            starts: StartTable::from_dense_dfa(dfa, &remap)?,
            special: dfa.special().remap(|id| remap[dfa.to_index(id)]),
        };
        // And here's our second pass. Iterate over all of the dense states
        // again, and update the transitions in each of the states in the
        // sparse DFA.
        for old_state in dfa.states() {
            let new_id = remap[dfa.to_index(old_state.id())];
            let mut new_state = new.trans.state_mut(new_id);
            let sparse = old_state.sparse_transitions();
            for (i, (_, _, next)) in sparse.enumerate() {
                let next = remap[dfa.to_index(next)];
                new_state.set_next_at(i, next);
            }
        }
        trace!(
            "created sparse DFA, memory usage: {} (dense memory usage: {})",
            new.memory_usage(),
            dfa.memory_usage(),
        );
        Ok(new)
    }
}

impl<T: AsRef<[u8]> + Send + Sync> DFA<T> {
    /// Cheaply return a borrowed version of this sparse DFA. Specifically, the
    /// DFA returned always uses `&[u8]` for its transitions.
    pub fn as_ref<'a>(&'a self) -> DFA<&'a [u8]> {
        DFA {
            trans: self.trans.as_ref(),
            starts: self.starts.as_ref(),
            special: self.special,
        }
    }

    /// Return an owned version of this sparse DFA. Specifically, the DFA
    /// returned always uses `Vec<u8>` for its transitions.
    ///
    /// Effectively, this returns a sparse DFA whose transitions live on the
    /// heap.
    #[cfg(feature = "alloc")]
    pub fn to_owned(&self) -> DFA<Vec<u8>> {
        DFA {
            trans: self.trans.to_owned(),
            starts: self.starts.to_owned(),
            special: self.special,
        }
    }

    /// Returns the memory usage, in bytes, of this DFA.
    ///
    /// The memory usage is computed based on the number of bytes used to
    /// represent this DFA.
    ///
    /// This does **not** include the stack size used up by this DFA. To
    /// compute that, use `std::mem::size_of::<sparse::DFA>()`.
    pub fn memory_usage(&self) -> usize {
        self.trans.memory_usage() + self.starts.memory_usage()
    }

    /// Returns true only if this DFA has starting states for each pattern.
    ///
    /// When a DFA has starting states for each pattern, then a search with the
    /// DFA can be configured to only look for anchored matches of a specific
    /// pattern. Specifically, APIs like [`Automaton::find_earliest_fwd_at`]
    /// can accept a non-None `pattern_id` if and only if this method returns
    /// true. Otherwise, calling `find_earliest_fwd_at` will panic.
    ///
    /// Note that if the DFA is empty, this always returns false.
    pub fn has_starts_for_each_pattern(&self) -> bool {
        self.starts.pattern_len > 0
    }
}

/// Routines for converting a sparse DFA to other representations, such as raw
/// bytes suitable for persistent storage.
impl<T: AsRef<[u8]> + Send + Sync> DFA<T> {
    /// Serialize this DFA as raw bytes to a `Vec<u8>` in little endian
    /// format.
    ///
    /// The written bytes are guaranteed to be deserialized correctly and
    /// without errors in a semver compatible release of this crate by a
    /// `DFA`'s deserialization APIs (assuming all other criteria for the
    /// deserialization APIs has been satisfied):
    ///
    /// * [`DFA::from_bytes`]
    /// * [`DFA::from_bytes_unchecked`]
    ///
    /// Note that unlike a [`dense::DFA`](crate::dfa::dense::DFA)'s
    /// serialization methods, this does not add any initial padding to the
    /// returned bytes. Padding isn't required for sparse DFAs since they have
    /// no alignment requirements.
    ///
    /// # Example
    ///
    /// This example shows how to serialize and deserialize a DFA:
    ///
    /// ```
    /// use regex_automata::{
    ///     dfa::{Automaton, sparse::DFA},
    ///     HalfMatch,
    /// };
    ///
    /// // Compile our original DFA.
    /// let original_dfa = DFA::new("foo[0-9]+")?;
    ///
    /// // N.B. We use native endianness here to make the example work, but
    /// // using to_bytes_little_endian would work on a little endian target.
    /// let buf = original_dfa.to_bytes_native_endian();
    /// // Even if buf has initial padding, DFA::from_bytes will automatically
    /// // ignore it.
    /// let dfa: DFA<&[u8]> = DFA::from_bytes(&buf)?.0;
    ///
    /// let expected = HalfMatch::must(0, 8);
    /// assert_eq!(Some(expected), dfa.try_find_fwd(b"foo12345")?);
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    #[cfg(feature = "alloc")]
    pub fn to_bytes_little_endian(&self) -> Vec<u8> {
        self.to_bytes::<bytes::LE>()
    }

    /// Serialize this DFA as raw bytes to a `Vec<u8>` in big endian
    /// format.
    ///
    /// The written bytes are guaranteed to be deserialized correctly and
    /// without errors in a semver compatible release of this crate by a
    /// `DFA`'s deserialization APIs (assuming all other criteria for the
    /// deserialization APIs has been satisfied):
    ///
    /// * [`DFA::from_bytes`]
    /// * [`DFA::from_bytes_unchecked`]
    ///
    /// Note that unlike a [`dense::DFA`](crate::dfa::dense::DFA)'s
    /// serialization methods, this does not add any initial padding to the
    /// returned bytes. Padding isn't required for sparse DFAs since they have
    /// no alignment requirements.
    ///
    /// # Example
    ///
    /// This example shows how to serialize and deserialize a DFA:
    ///
    /// ```
    /// use regex_automata::{
    ///     dfa::{Automaton, sparse::DFA},
    ///     HalfMatch,
    /// };
    ///
    /// // Compile our original DFA.
    /// let original_dfa = DFA::new("foo[0-9]+")?;
    ///
    /// // N.B. We use native endianness here to make the example work, but
    /// // using to_bytes_big_endian would work on a big endian target.
    /// let buf = original_dfa.to_bytes_native_endian();
    /// // Even if buf has initial padding, DFA::from_bytes will automatically
    /// // ignore it.
    /// let dfa: DFA<&[u8]> = DFA::from_bytes(&buf)?.0;
    ///
    /// let expected = HalfMatch::must(0, 8);
    /// assert_eq!(Some(expected), dfa.try_find_fwd(b"foo12345")?);
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    #[cfg(feature = "alloc")]
    pub fn to_bytes_big_endian(&self) -> Vec<u8> {
        self.to_bytes::<bytes::BE>()
    }

    /// Serialize this DFA as raw bytes to a `Vec<u8>` in native endian
    /// format.
    ///
    /// The written bytes are guaranteed to be deserialized correctly and
    /// without errors in a semver compatible release of this crate by a
    /// `DFA`'s deserialization APIs (assuming all other criteria for the
    /// deserialization APIs has been satisfied):
    ///
    /// * [`DFA::from_bytes`]
    /// * [`DFA::from_bytes_unchecked`]
    ///
    /// Note that unlike a [`dense::DFA`](crate::dfa::dense::DFA)'s
    /// serialization methods, this does not add any initial padding to the
    /// returned bytes. Padding isn't required for sparse DFAs since they have
    /// no alignment requirements.
    ///
    /// Generally speaking, native endian format should only be used when
    /// you know that the target you're compiling the DFA for matches the
    /// endianness of the target on which you're compiling DFA. For example,
    /// if serialization and deserialization happen in the same process or on
    /// the same machine. Otherwise, when serializing a DFA for use in a
    /// portable environment, you'll almost certainly want to serialize _both_
    /// a little endian and a big endian version and then load the correct one
    /// based on the target's configuration.
    ///
    /// # Example
    ///
    /// This example shows how to serialize and deserialize a DFA:
    ///
    /// ```
    /// use regex_automata::{
    ///     dfa::{Automaton, sparse::DFA},
    ///     HalfMatch,
    /// };
    ///
    /// // Compile our original DFA.
    /// let original_dfa = DFA::new("foo[0-9]+")?;
    ///
    /// let buf = original_dfa.to_bytes_native_endian();
    /// // Even if buf has initial padding, DFA::from_bytes will automatically
    /// // ignore it.
    /// let dfa: DFA<&[u8]> = DFA::from_bytes(&buf)?.0;
    ///
    /// let expected = HalfMatch::must(0, 8);
    /// assert_eq!(Some(expected), dfa.try_find_fwd(b"foo12345")?);
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    #[cfg(feature = "alloc")]
    pub fn to_bytes_native_endian(&self) -> Vec<u8> {
        self.to_bytes::<bytes::NE>()
    }

    /// The implementation of the public `to_bytes` serialization methods,
    /// which is generic over endianness.
    #[cfg(feature = "alloc")]
    fn to_bytes<E: Endian>(&self) -> Vec<u8> {
        let mut buf = vec![0; self.write_to_len()];
        // This should always succeed since the only possible serialization
        // error is providing a buffer that's too small, but we've ensured that
        // `buf` is big enough here.
        self.write_to::<E>(&mut buf).unwrap();
        buf
    }

    /// Serialize this DFA as raw bytes to the given slice, in little endian
    /// format. Upon success, the total number of bytes written to `dst` is
    /// returned.
    ///
    /// The written bytes are guaranteed to be deserialized correctly and
    /// without errors in a semver compatible release of this crate by a
    /// `DFA`'s deserialization APIs (assuming all other criteria for the
    /// deserialization APIs has been satisfied):
    ///
    /// * [`DFA::from_bytes`]
    /// * [`DFA::from_bytes_unchecked`]
    ///
    /// # Errors
    ///
    /// This returns an error if the given destination slice is not big enough
    /// to contain the full serialized DFA. If an error occurs, then nothing
    /// is written to `dst`.
    ///
    /// # Example
    ///
    /// This example shows how to serialize and deserialize a DFA without
    /// dynamic memory allocation.
    ///
    /// ```
    /// use regex_automata::{
    ///     dfa::{Automaton, sparse::DFA},
    ///     HalfMatch,
    /// };
    ///
    /// // Compile our original DFA.
    /// let original_dfa = DFA::new("foo[0-9]+")?;
    ///
    /// // Create a 4KB buffer on the stack to store our serialized DFA.
    /// let mut buf = [0u8; 4 * (1<<10)];
    /// // N.B. We use native endianness here to make the example work, but
    /// // using write_to_little_endian would work on a little endian target.
    /// let written = original_dfa.write_to_native_endian(&mut buf)?;
    /// let dfa: DFA<&[u8]> = DFA::from_bytes(&buf[..written])?.0;
    ///
    /// let expected = HalfMatch::must(0, 8);
    /// assert_eq!(Some(expected), dfa.try_find_fwd(b"foo12345")?);
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn write_to_little_endian(
        &self,
        dst: &mut [u8],
    ) -> Result<usize, SerializeError> {
        self.write_to::<bytes::LE>(dst)
    }

    /// Serialize this DFA as raw bytes to the given slice, in big endian
    /// format. Upon success, the total number of bytes written to `dst` is
    /// returned.
    ///
    /// The written bytes are guaranteed to be deserialized correctly and
    /// without errors in a semver compatible release of this crate by a
    /// `DFA`'s deserialization APIs (assuming all other criteria for the
    /// deserialization APIs has been satisfied):
    ///
    /// * [`DFA::from_bytes`]
    /// * [`DFA::from_bytes_unchecked`]
    ///
    /// # Errors
    ///
    /// This returns an error if the given destination slice is not big enough
    /// to contain the full serialized DFA. If an error occurs, then nothing
    /// is written to `dst`.
    ///
    /// # Example
    ///
    /// This example shows how to serialize and deserialize a DFA without
    /// dynamic memory allocation.
    ///
    /// ```
    /// use regex_automata::{
    ///     dfa::{Automaton, sparse::DFA},
    ///     HalfMatch,
    /// };
    ///
    /// // Compile our original DFA.
    /// let original_dfa = DFA::new("foo[0-9]+")?;
    ///
    /// // Create a 4KB buffer on the stack to store our serialized DFA.
    /// let mut buf = [0u8; 4 * (1<<10)];
    /// // N.B. We use native endianness here to make the example work, but
    /// // using write_to_big_endian would work on a big endian target.
    /// let written = original_dfa.write_to_native_endian(&mut buf)?;
    /// let dfa: DFA<&[u8]> = DFA::from_bytes(&buf[..written])?.0;
    ///
    /// let expected = HalfMatch::must(0, 8);
    /// assert_eq!(Some(expected), dfa.try_find_fwd(b"foo12345")?);
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn write_to_big_endian(
        &self,
        dst: &mut [u8],
    ) -> Result<usize, SerializeError> {
        self.write_to::<bytes::BE>(dst)
    }

    /// Serialize this DFA as raw bytes to the given slice, in native endian
    /// format. Upon success, the total number of bytes written to `dst` is
    /// returned.
    ///
    /// The written bytes are guaranteed to be deserialized correctly and
    /// without errors in a semver compatible release of this crate by a
    /// `DFA`'s deserialization APIs (assuming all other criteria for the
    /// deserialization APIs has been satisfied):
    ///
    /// * [`DFA::from_bytes`]
    /// * [`DFA::from_bytes_unchecked`]
    ///
    /// Generally speaking, native endian format should only be used when
    /// you know that the target you're compiling the DFA for matches the
    /// endianness of the target on which you're compiling DFA. For example,
    /// if serialization and deserialization happen in the same process or on
    /// the same machine. Otherwise, when serializing a DFA for use in a
    /// portable environment, you'll almost certainly want to serialize _both_
    /// a little endian and a big endian version and then load the correct one
    /// based on the target's configuration.
    ///
    /// # Errors
    ///
    /// This returns an error if the given destination slice is not big enough
    /// to contain the full serialized DFA. If an error occurs, then nothing
    /// is written to `dst`.
    ///
    /// # Example
    ///
    /// This example shows how to serialize and deserialize a DFA without
    /// dynamic memory allocation.
    ///
    /// ```
    /// use regex_automata::{
    ///     dfa::{Automaton, sparse::DFA},
    ///     HalfMatch,
    /// };
    ///
    /// // Compile our original DFA.
    /// let original_dfa = DFA::new("foo[0-9]+")?;
    ///
    /// // Create a 4KB buffer on the stack to store our serialized DFA.
    /// let mut buf = [0u8; 4 * (1<<10)];
    /// let written = original_dfa.write_to_native_endian(&mut buf)?;
    /// let dfa: DFA<&[u8]> = DFA::from_bytes(&buf[..written])?.0;
    ///
    /// let expected = HalfMatch::must(0, 8);
    /// assert_eq!(Some(expected), dfa.try_find_fwd(b"foo12345")?);
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn write_to_native_endian(
        &self,
        dst: &mut [u8],
    ) -> Result<usize, SerializeError> {
        self.write_to::<bytes::NE>(dst)
    }

    /// The implementation of the public `write_to` serialization methods,
    /// which is generic over endianness.
    fn write_to<E: Endian>(
        &self,
        dst: &mut [u8],
    ) -> Result<usize, SerializeError> {
        let mut nw = 0;
        nw += bytes::write_label(LABEL, &mut dst[nw..])?;
        nw += bytes::write_endianness_check::<E>(&mut dst[nw..])?;
        nw += bytes::write_version::<E>(VERSION, &mut dst[nw..])?;
        nw += {
            // Currently unused, intended for future flexibility
            E::write_u32(0, &mut dst[nw..]);
            size_of::<u32>()
        };
        nw += self.trans.write_to::<E>(&mut dst[nw..])?;
        nw += self.starts.write_to::<E>(&mut dst[nw..])?;
        nw += self.special.write_to::<E>(&mut dst[nw..])?;
        Ok(nw)
    }

    /// Return the total number of bytes required to serialize this DFA.
    ///
    /// This is useful for determining the size of the buffer required to pass
    /// to one of the serialization routines:
    ///
    /// * [`DFA::write_to_little_endian`]
    /// * [`DFA::write_to_big_endian`]
    /// * [`DFA::write_to_native_endian`]
    ///
    /// Passing a buffer smaller than the size returned by this method will
    /// result in a serialization error.
    ///
    /// # Example
    ///
    /// This example shows how to dynamically allocate enough room to serialize
    /// a sparse DFA.
    ///
    /// ```
    /// use regex_automata::{
    ///     dfa::{Automaton, sparse::DFA},
    ///     HalfMatch,
    /// };
    ///
    /// // Compile our original DFA.
    /// let original_dfa = DFA::new("foo[0-9]+")?;
    ///
    /// let mut buf = vec![0; original_dfa.write_to_len()];
    /// let written = original_dfa.write_to_native_endian(&mut buf)?;
    /// let dfa: DFA<&[u8]> = DFA::from_bytes(&buf[..written])?.0;
    ///
    /// let expected = HalfMatch::must(0, 8);
    /// assert_eq!(Some(expected), dfa.try_find_fwd(b"foo12345")?);
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn write_to_len(&self) -> usize {
        bytes::write_label_len(LABEL)
        + bytes::write_endianness_check_len()
        + bytes::write_version_len()
        + size_of::<u32>() // unused, intended for future flexibility
        + self.trans.write_to_len()
        + self.starts.write_to_len()
        + self.special.write_to_len()
    }
}

impl<'a> DFA<&'a [u8]> {
    /// Safely deserialize a sparse DFA with a specific state identifier
    /// representation. Upon success, this returns both the deserialized DFA
    /// and the number of bytes read from the given slice. Namely, the contents
    /// of the slice beyond the DFA are not read.
    ///
    /// Deserializing a DFA using this routine will never allocate heap memory.
    /// For safety purposes, the DFA's transitions will be verified such that
    /// every transition points to a valid state. If this verification is too
    /// costly, then a [`DFA::from_bytes_unchecked`] API is provided, which
    /// will always execute in constant time.
    ///
    /// The bytes given must be generated by one of the serialization APIs
    /// of a `DFA` using a semver compatible release of this crate. Those
    /// include:
    ///
    /// * [`DFA::to_bytes_little_endian`]
    /// * [`DFA::to_bytes_big_endian`]
    /// * [`DFA::to_bytes_native_endian`]
    /// * [`DFA::write_to_little_endian`]
    /// * [`DFA::write_to_big_endian`]
    /// * [`DFA::write_to_native_endian`]
    ///
    /// The `to_bytes` methods allocate and return a `Vec<u8>` for you. The
    /// `write_to` methods do not allocate and write to an existing slice
    /// (which may be on the stack). Since deserialization always uses the
    /// native endianness of the target platform, the serialization API you use
    /// should match the endianness of the target platform. (It's often a good
    /// idea to generate serialized DFAs for both forms of endianness and then
    /// load the correct one based on endianness.)
    ///
    /// # Errors
    ///
    /// Generally speaking, it's easier to state the conditions in which an
    /// error is _not_ returned. All of the following must be true:
    ///
    /// * The bytes given must be produced by one of the serialization APIs
    ///   on this DFA, as mentioned above.
    /// * The endianness of the target platform matches the endianness used to
    ///   serialized the provided DFA.
    ///
    /// If any of the above are not true, then an error will be returned.
    ///
    /// Note that unlike deserializing a
    /// [`dense::DFA`](crate::dfa::dense::DFA), deserializing a sparse DFA has
    /// no alignment requirements. That is, an alignment of `1` is valid.
    ///
    /// # Panics
    ///
    /// This routine will never panic for any input.
    ///
    /// # Example
    ///
    /// This example shows how to serialize a DFA to raw bytes, deserialize it
    /// and then use it for searching.
    ///
    /// ```
    /// use regex_automata::{
    ///     dfa::{Automaton, sparse::DFA},
    ///     HalfMatch,
    /// };
    ///
    /// let initial = DFA::new("foo[0-9]+")?;
    /// let bytes = initial.to_bytes_native_endian();
    /// let dfa: DFA<&[u8]> = DFA::from_bytes(&bytes)?.0;
    ///
    /// let expected = HalfMatch::must(0, 8);
    /// assert_eq!(Some(expected), dfa.try_find_fwd(b"foo12345")?);
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    ///
    /// # Example: loading a DFA from static memory
    ///
    /// One use case this library supports is the ability to serialize a
    /// DFA to disk and then use `include_bytes!` to store it in a compiled
    /// Rust program. Those bytes can then be cheaply deserialized into a
    /// `DFA` structure at runtime and used for searching without having to
    /// re-compile the DFA (which can be quite costly).
    ///
    /// We can show this in two parts. The first part is serializing the DFA to
    /// a file:
    ///
    /// ```no_run
    /// use regex_automata::dfa::{Automaton, sparse::DFA};
    ///
    /// let dfa = DFA::new("foo[0-9]+")?;
    ///
    /// // Write a big endian serialized version of this DFA to a file.
    /// let bytes = dfa.to_bytes_big_endian();
    /// std::fs::write("foo.bigendian.dfa", &bytes)?;
    ///
    /// // Do it again, but this time for little endian.
    /// let bytes = dfa.to_bytes_little_endian();
    /// std::fs::write("foo.littleendian.dfa", &bytes)?;
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    ///
    /// And now the second part is embedding the DFA into the compiled program
    /// and deserializing it at runtime on first use. We use conditional
    /// compilation to choose the correct endianness. As mentioned above, we
    /// do not need to employ any special tricks to ensure a proper alignment,
    /// since a sparse DFA has no alignment requirements.
    ///
    /// ```no_run
    /// use regex_automata::{
    ///     dfa::{Automaton, sparse},
    ///     HalfMatch,
    /// };
    ///
    /// type DFA = sparse::DFA<&'static [u8]>;
    ///
    /// fn get_foo() -> &'static DFA {
    ///     use std::cell::Cell;
    ///     use std::mem::MaybeUninit;
    ///     use std::sync::Once;
    ///
    ///     # const _: &str = stringify! {
    ///     #[cfg(target_endian = "big")]
    ///     static BYTES: &[u8] = include_bytes!("foo.bigendian.dfa");
    ///     #[cfg(target_endian = "little")]
    ///     static BYTES: &[u8] = include_bytes!("foo.littleendian.dfa");
    ///     # };
    ///     # static BYTES: &[u8] = b"";
    ///
    ///     struct Lazy(Cell<MaybeUninit<DFA>>);
    ///     // SAFETY: This is safe because DFA impls Sync.
    ///     unsafe impl Sync for Lazy {}
    ///
    ///     static INIT: Once = Once::new();
    ///     static DFA: Lazy = Lazy(Cell::new(MaybeUninit::uninit()));
    ///
    ///     INIT.call_once(|| {
    ///         let (dfa, _) = DFA::from_bytes(BYTES)
    ///             .expect("serialized DFA should be valid");
    ///         // SAFETY: This is guaranteed to only execute once, and all
    ///         // we do with the pointer is write the DFA to it.
    ///         unsafe {
    ///             (*DFA.0.as_ptr()).as_mut_ptr().write(dfa);
    ///         }
    ///     });
    ///     // SAFETY: DFA is guaranteed to by initialized via INIT and is
    ///     // stored in static memory.
    ///     unsafe {
    ///         let dfa = (*DFA.0.as_ptr()).as_ptr();
    ///         std::mem::transmute::<*const DFA, &'static DFA>(dfa)
    ///     }
    /// }
    ///
    /// let dfa = get_foo();
    /// let expected = HalfMatch::must(0, 8);
    /// assert_eq!(Ok(Some(expected)), dfa.try_find_fwd(b"foo12345"));
    /// ```
    ///
    /// Alternatively, consider using
    /// [`lazy_static`](https://crates.io/crates/lazy_static)
    /// or
    /// [`once_cell`](https://crates.io/crates/once_cell),
    /// which will guarantee safety for you.
    pub fn from_bytes(
        slice: &'a [u8],
    ) -> Result<(DFA<&'a [u8]>, usize), DeserializeError> {
        // SAFETY: This is safe because we validate both the sparse transitions
        // (by trying to decode every state) and start state ID list below. If
        // either validation fails, then we return an error.
        let (dfa, nread) = unsafe { DFA::from_bytes_unchecked(slice)? };
        dfa.trans.validate()?;
        dfa.starts.validate(&dfa.trans)?;
        // N.B. dfa.special doesn't have a way to do unchecked deserialization,
        // so it has already been validated.
        Ok((dfa, nread))
    }

    /// Deserialize a DFA with a specific state identifier representation in
    /// constant time by omitting the verification of the validity of the
    /// sparse transitions.
    ///
    /// This is just like [`DFA::from_bytes`], except it can potentially return
    /// a DFA that exhibits undefined behavior if its transitions contains
    /// invalid state identifiers.
    ///
    /// This routine is useful if you need to deserialize a DFA cheaply and
    /// cannot afford the transition validation performed by `from_bytes`.
    ///
    /// # Safety
    ///
    /// This routine is unsafe because it permits callers to provide
    /// arbitrary transitions with possibly incorrect state identifiers. While
    /// the various serialization routines will never return an incorrect
    /// DFA, there is no guarantee that the bytes provided here
    /// are correct. While `from_bytes_unchecked` will still do several forms
    /// of basic validation, this routine does not check that the transitions
    /// themselves are correct. Given an incorrect transition table, it is
    /// possible for the search routines to access out-of-bounds memory because
    /// of explicit bounds check elision.
    ///
    /// # Example
    ///
    /// ```
    /// use regex_automata::{
    ///     dfa::{Automaton, sparse::DFA},
    ///     HalfMatch,
    /// };
    ///
    /// let initial = DFA::new("foo[0-9]+")?;
    /// let bytes = initial.to_bytes_native_endian();
    /// // SAFETY: This is guaranteed to be safe since the bytes given come
    /// // directly from a compatible serialization routine.
    /// let dfa: DFA<&[u8]> = unsafe { DFA::from_bytes_unchecked(&bytes)?.0 };
    ///
    /// let expected = HalfMatch::must(0, 8);
    /// assert_eq!(Some(expected), dfa.try_find_fwd(b"foo12345")?);
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub unsafe fn from_bytes_unchecked(
        slice: &'a [u8],
    ) -> Result<(DFA<&'a [u8]>, usize), DeserializeError> {
        let mut nr = 0;

        nr += bytes::read_label(&slice[nr..], LABEL)?;
        nr += bytes::read_endianness_check(&slice[nr..])?;
        nr += bytes::read_version(&slice[nr..], VERSION)?;

        let _unused = bytes::try_read_u32(&slice[nr..], "unused space")?;
        nr += size_of::<u32>();

        let (trans, nread) = Transitions::from_bytes_unchecked(&slice[nr..])?;
        nr += nread;

        let (starts, nread) = StartTable::from_bytes_unchecked(&slice[nr..])?;
        nr += nread;

        let (special, nread) = Special::from_bytes(&slice[nr..])?;
        nr += nread;
        if special.max.as_usize() >= trans.sparse().len() {
            return Err(DeserializeError::generic(
                "max should not be greater than or equal to sparse bytes",
            ));
        }

        Ok((DFA { trans, starts, special }, nr))
    }
}

impl<T: AsRef<[u8]> + Send + Sync> fmt::Debug for DFA<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "sparse::DFA(")?;
        for state in self.trans.states() {
            fmt_state_indicator(f, self, state.id())?;
            writeln!(f, "{:06?}: {:?}", state.id(), state)?;
        }
        writeln!(f, "")?;
        for (i, (start_id, sty, pid)) in self.starts.iter().enumerate() {
            if i % self.starts.stride == 0 {
                match pid {
                    None => writeln!(f, "START-GROUP(ALL)")?,
                    Some(pid) => {
                        writeln!(f, "START_GROUP(pattern: {:?})", pid)?
                    }
                }
            }
            writeln!(f, "  {:?} => {:06?}", sty, start_id.as_usize())?;
        }
        writeln!(f, "state length: {:?}", self.trans.len)?;
        writeln!(f, ")")?;
        Ok(())
    }
}

unsafe impl<T: AsRef<[u8]> + Send + Sync> Automaton for DFA<T> {
    #[inline]
    fn is_special_state(&self, id: StateID) -> bool {
        self.special.is_special_state(id)
    }

    #[inline]
    fn is_dead_state(&self, id: StateID) -> bool {
        self.special.is_dead_state(id)
    }

    #[inline]
    fn is_quit_state(&self, id: StateID) -> bool {
        self.special.is_quit_state(id)
    }

    #[inline]
    fn is_match_state(&self, id: StateID) -> bool {
        self.special.is_match_state(id)
    }

    #[inline]
    fn is_start_state(&self, id: StateID) -> bool {
        self.special.is_start_state(id)
    }

    #[inline]
    fn is_accel_state(&self, id: StateID) -> bool {
        self.special.is_accel_state(id)
    }

    // This is marked as inline to help dramatically boost sparse searching,
    // which decodes each state it enters to follow the next transition.
    #[inline(always)]
    fn next_state(&self, current: StateID, input: u8) -> StateID {
        let input = self.trans.classes.get(input);
        self.trans.state(current).next(input)
    }

    #[inline]
    unsafe fn next_state_unchecked(
        &self,
        current: StateID,
        input: u8,
    ) -> StateID {
        self.next_state(current, input)
    }

    #[inline]
    fn next_eoi_state(&self, current: StateID) -> StateID {
        self.trans.state(current).next_eoi()
    }

    #[inline]
    fn pattern_len(&self) -> usize {
        self.trans.pattern_len
    }

    #[inline]
    fn match_len(&self, id: StateID) -> usize {
        self.trans.state(id).pattern_len()
    }

    #[inline]
    fn match_pattern(&self, id: StateID, match_index: usize) -> PatternID {
        // This is an optimization for the very common case of a DFA with a
        // single pattern. This conditional avoids a somewhat more costly path
        // that finds the pattern ID from the state machine, which requires
        // a bit of slicing/pointer-chasing. This optimization tends to only
        // matter when matches are frequent.
        if self.trans.pattern_len == 1 {
            return PatternID::ZERO;
        }
        self.trans.state(id).pattern_id(match_index)
    }

    #[inline]
    fn start_state_forward(&self, input: &Input<'_, '_>) -> StateID {
        let index = Start::from_position_fwd(&input);
        self.starts.start(index, input.get_pattern())
    }

    #[inline]
    fn start_state_reverse(&self, input: &Input<'_, '_>) -> StateID {
        let index = Start::from_position_rev(&input);
        self.starts.start(index, input.get_pattern())
    }

    #[inline]
    fn accelerator(&self, id: StateID) -> &[u8] {
        self.trans.state(id).accelerator()
    }
}

/// The transition table portion of a sparse DFA.
///
/// The transition table is the core part of the DFA in that it describes how
/// to move from one state to another based on the input sequence observed.
///
/// Unlike a typical dense table based DFA, states in a sparse transition
/// table have variable size. That is, states with more transitions use more
/// space than states with fewer transitions. This means that finding the next
/// transition takes more work than with a dense DFA, but also typically uses
/// much less space.
#[derive(Clone)]
struct Transitions<T> {
    /// The raw encoding of each state in this DFA.
    ///
    /// Each state has the following information:
    ///
    /// * A set of transitions to subsequent states. Transitions to the dead
    ///   state are omitted.
    /// * If the state can be accelerated, then any additional accelerator
    ///   information.
    /// * If the state is a match state, then the state contains all pattern
    ///   IDs that match when in that state.
    ///
    /// To decode a state, use Transitions::state.
    ///
    /// In practice, T is either Vec<u8> or &[u8].
    sparse: T,
    /// A set of equivalence classes, where a single equivalence class
    /// represents a set of bytes that never discriminate between a match
    /// and a non-match in the DFA. Each equivalence class corresponds to a
    /// single character in this DFA's alphabet, where the maximum number of
    /// characters is 257 (each possible value of a byte plus the special
    /// EOI transition). Consequently, the number of equivalence classes
    /// corresponds to the number of transitions for each DFA state. Note
    /// though that the *space* used by each DFA state in the transition table
    /// may be larger. The total space used by each DFA state is known as the
    /// stride and is documented above.
    ///
    /// The only time the number of equivalence classes is fewer than 257 is
    /// if the DFA's kind uses byte classes which is the default. Equivalence
    /// classes should generally only be disabled when debugging, so that
    /// the transitions themselves aren't obscured. Disabling them has no
    /// other benefit, since the equivalence class map is always used while
    /// searching. In the vast majority of cases, the number of equivalence
    /// classes is substantially smaller than 257, particularly when large
    /// Unicode classes aren't used.
    ///
    /// N.B. Equivalence classes aren't particularly useful in a sparse DFA
    /// in the current implementation, since equivalence classes generally tend
    /// to correspond to continuous ranges of bytes that map to the same
    /// transition. So in a sparse DFA, equivalence classes don't really lead
    /// to a space savings. In the future, it would be good to try and remove
    /// them from sparse DFAs entirely, but requires a bit of work since sparse
    /// DFAs are built from dense DFAs, which are in turn built on top of
    /// equivalence classes.
    classes: ByteClasses,
    /// The total number of states in this DFA. Note that a DFA always has at
    /// least one state---the dead state---even the empty DFA. In particular,
    /// the dead state always has ID 0 and is correspondingly always the first
    /// state. The dead state is never a match state.
    len: usize,
    /// The total number of unique patterns represented by these match states.
    pattern_len: usize,
}

impl<'a> Transitions<&'a [u8]> {
    unsafe fn from_bytes_unchecked(
        mut slice: &'a [u8],
    ) -> Result<(Transitions<&'a [u8]>, usize), DeserializeError> {
        let slice_start = slice.as_ptr() as usize;

        let (state_len, nr) =
            bytes::try_read_u32_as_usize(&slice, "state length")?;
        slice = &slice[nr..];

        let (pattern_len, nr) =
            bytes::try_read_u32_as_usize(&slice, "pattern length")?;
        slice = &slice[nr..];

        let (classes, nr) = ByteClasses::from_bytes(&slice)?;
        slice = &slice[nr..];

        let (len, nr) =
            bytes::try_read_u32_as_usize(&slice, "sparse transitions length")?;
        slice = &slice[nr..];

        bytes::check_slice_len(slice, len, "sparse states byte length")?;
        let sparse = &slice[..len];
        slice = &slice[len..];

        let trans = Transitions {
            sparse,
            classes,
            len: state_len,
            pattern_len: pattern_len,
        };
        Ok((trans, slice.as_ptr() as usize - slice_start))
    }
}

impl<T: AsRef<[u8]>> Transitions<T> {
    /// Writes a serialized form of this transition table to the buffer given.
    /// If the buffer is too small, then an error is returned. To determine
    /// how big the buffer must be, use `write_to_len`.
    fn write_to<E: Endian>(
        &self,
        mut dst: &mut [u8],
    ) -> Result<usize, SerializeError> {
        let nwrite = self.write_to_len();
        if dst.len() < nwrite {
            return Err(SerializeError::buffer_too_small(
                "sparse transition table",
            ));
        }
        dst = &mut dst[..nwrite];

        // write state length
        E::write_u32(u32::try_from(self.len).unwrap(), dst);
        dst = &mut dst[size_of::<u32>()..];

        // write pattern length
        E::write_u32(u32::try_from(self.pattern_len).unwrap(), dst);
        dst = &mut dst[size_of::<u32>()..];

        // write byte class map
        let n = self.classes.write_to(dst)?;
        dst = &mut dst[n..];

        // write number of bytes in sparse transitions
        E::write_u32(u32::try_from(self.sparse().len()).unwrap(), dst);
        dst = &mut dst[size_of::<u32>()..];

        // write actual transitions
        dst.copy_from_slice(self.sparse());
        Ok(nwrite)
    }

    /// Returns the number of bytes the serialized form of this transition
    /// table will use.
    fn write_to_len(&self) -> usize {
        size_of::<u32>()   // state length
        + size_of::<u32>() // pattern length
        + self.classes.write_to_len()
        + size_of::<u32>() // sparse transitions length
        + self.sparse().len()
    }

    /// Validates that every state ID in this transition table is valid.
    ///
    /// That is, every state ID can be used to correctly index a state in this
    /// table.
    fn validate(&self) -> Result<(), DeserializeError> {
        // In order to validate everything, we not only need to make sure we
        // can decode every state, but that every transition in every state
        // points to a valid state. There are many duplicative transitions, so
        // we record state IDs that we've verified so that we don't redo the
        // decoding work.
        //
        // Except, when in no_std mode, we don't have dynamic memory allocation
        // available to us, so we skip this optimization. It's not clear
        // whether doing something more clever is worth it just yet. If you're
        // profiling this code and need it to run faster, please file an issue.
        //
        // ---AG
        struct Seen {
            #[cfg(feature = "alloc")]
            set: BTreeSet<StateID>,
            #[cfg(not(feature = "alloc"))]
            set: core::marker::PhantomData<StateID>,
        }

        #[cfg(feature = "alloc")]
        impl Seen {
            fn new() -> Seen {
                Seen { set: BTreeSet::new() }
            }
            fn insert(&mut self, id: StateID) {
                self.set.insert(id);
            }
            fn contains(&self, id: &StateID) -> bool {
                self.set.contains(id)
            }
        }

        #[cfg(not(feature = "alloc"))]
        impl Seen {
            fn new() -> Seen {
                Seen { set: core::marker::PhantomData }
            }
            fn insert(&mut self, _id: StateID) {}
            fn contains(&self, _id: &StateID) -> bool {
                false
            }
        }

        let mut verified: Seen = Seen::new();
        // We need to make sure that we decode the correct number of states.
        // Otherwise, an empty set of transitions would validate even if the
        // recorded state length is non-empty.
        let mut len = 0;
        // We can't use the self.states() iterator because it assumes the state
        // encodings are valid. It could panic if they aren't.
        let mut id = DEAD;
        while id.as_usize() < self.sparse().len() {
            let state = self.try_state(id)?;
            verified.insert(id);
            // The next ID should be the offset immediately following `state`.
            id = StateID::new(bytes::add(
                id.as_usize(),
                state.bytes_len(),
                "next state ID offset",
            )?)
            .map_err(|err| {
                DeserializeError::state_id_error(err, "next state ID offset")
            })?;
            len += 1;

            // Now check that all transitions in this state are correct.
            for i in 0..state.ntrans {
                let to = state.next_at(i);
                if verified.contains(&to) {
                    continue;
                }
                let _ = self.try_state(to)?;
                verified.insert(id);
            }
        }
        if len != self.len {
            return Err(DeserializeError::generic(
                "mismatching sparse state length",
            ));
        }
        Ok(())
    }

    /// Converts these transitions to a borrowed value.
    fn as_ref(&self) -> Transitions<&'_ [u8]> {
        Transitions {
            sparse: self.sparse(),
            classes: self.classes.clone(),
            len: self.len,
            pattern_len: self.pattern_len,
        }
    }

    /// Converts these transitions to an owned value.
    #[cfg(feature = "alloc")]
    fn to_owned(&self) -> Transitions<Vec<u8>> {
        Transitions {
            sparse: self.sparse().to_vec(),
            classes: self.classes.clone(),
            len: self.len,
            pattern_len: self.pattern_len,
        }
    }

    /// Return a convenient representation of the given state.
    ///
    /// This panics if the state is invalid.
    ///
    /// This is marked as inline to help dramatically boost sparse searching,
    /// which decodes each state it enters to follow the next transition. Other
    /// functions involved are also inlined, which should hopefully eliminate
    /// a lot of the extraneous decoding that is never needed just to follow
    /// the next transition.
    #[inline(always)]
    fn state(&self, id: StateID) -> State<'_> {
        let mut state = &self.sparse()[id.as_usize()..];
        let mut ntrans = bytes::read_u16(&state) as usize;
        let is_match = (1 << 15) & ntrans != 0;
        ntrans &= !(1 << 15);
        state = &state[2..];

        let (input_ranges, state) = state.split_at(ntrans * 2);
        let (next, state) = state.split_at(ntrans * StateID::SIZE);
        let (pattern_ids, state) = if is_match {
            let npats = bytes::read_u32(&state) as usize;
            state[4..].split_at(npats * 4)
        } else {
            (&[][..], state)
        };

        let accel_len = usize::from(state[0]);
        let accel = &state[1..accel_len + 1];
        State { id, is_match, ntrans, input_ranges, next, pattern_ids, accel }
    }

    /// Like `state`, but will return an error if the state encoding is
    /// invalid. This is useful for verifying states after deserialization,
    /// which is required for a safe deserialization API.
    ///
    /// Note that this only verifies that this state is decodable and that
    /// all of its data is consistent. It does not verify that its state ID
    /// transitions point to valid states themselves, nor does it verify that
    /// every pattern ID is valid.
    fn try_state(&self, id: StateID) -> Result<State<'_>, DeserializeError> {
        if id.as_usize() > self.sparse().len() {
            return Err(DeserializeError::generic("invalid sparse state ID"));
        }
        let mut state = &self.sparse()[id.as_usize()..];
        // Encoding format starts with a u16 that stores the total number of
        // transitions in this state.
        let (mut ntrans, _) =
            bytes::try_read_u16_as_usize(state, "state transition length")?;
        let is_match = ((1 << 15) & ntrans) != 0;
        ntrans &= !(1 << 15);
        state = &state[2..];
        if ntrans > 257 || ntrans == 0 {
            return Err(DeserializeError::generic(
                "invalid transition length",
            ));
        }

        // Each transition has two pieces: an inclusive range of bytes on which
        // it is defined, and the state ID that those bytes transition to. The
        // pairs come first, followed by a corresponding sequence of state IDs.
        let input_ranges_len = ntrans.checked_mul(2).unwrap();
        bytes::check_slice_len(state, input_ranges_len, "sparse byte pairs")?;
        let (input_ranges, state) = state.split_at(input_ranges_len);
        // Every range should be of the form A-B, where A<=B.
        for pair in input_ranges.chunks(2) {
            let (start, end) = (pair[0], pair[1]);
            if start > end {
                return Err(DeserializeError::generic("invalid input range"));
            }
        }

        // And now extract the corresponding sequence of state IDs. We leave
        // this sequence as a &[u8] instead of a &[S] because sparse DFAs do
        // not have any alignment requirements.
        let next_len = ntrans
            .checked_mul(self.id_len())
            .expect("state size * #trans should always fit in a usize");
        bytes::check_slice_len(state, next_len, "sparse trans state IDs")?;
        let (next, state) = state.split_at(next_len);
        // We can at least verify that every state ID is in bounds.
        for idbytes in next.chunks(self.id_len()) {
            let (id, _) =
                bytes::read_state_id(idbytes, "sparse state ID in try_state")?;
            bytes::check_slice_len(
                self.sparse(),
                id.as_usize(),
                "invalid sparse state ID",
            )?;
        }

        // If this is a match state, then read the pattern IDs for this state.
        // Pattern IDs is a u32-length prefixed sequence of native endian
        // encoded 32-bit integers.
        let (pattern_ids, state) = if is_match {
            let (npats, nr) =
                bytes::try_read_u32_as_usize(state, "pattern ID length")?;
            let state = &state[nr..];

            let pattern_ids_len =
                bytes::mul(npats, 4, "sparse pattern ID byte length")?;
            bytes::check_slice_len(
                state,
                pattern_ids_len,
                "sparse pattern IDs",
            )?;
            let (pattern_ids, state) = state.split_at(pattern_ids_len);
            for patbytes in pattern_ids.chunks(PatternID::SIZE) {
                bytes::read_pattern_id(
                    patbytes,
                    "sparse pattern ID in try_state",
                )?;
            }
            (pattern_ids, state)
        } else {
            (&[][..], state)
        };

        // Now read this state's accelerator info. The first byte is the length
        // of the accelerator, which is typically 0 (for no acceleration) but
        // is no bigger than 3. The length indicates the number of bytes that
        // follow, where each byte corresponds to a transition out of this
        // state.
        if state.is_empty() {
            return Err(DeserializeError::generic("no accelerator length"));
        }
        let (accel_len, state) = (usize::from(state[0]), &state[1..]);

        if accel_len > 3 {
            return Err(DeserializeError::generic(
                "sparse invalid accelerator length",
            ));
        }
        bytes::check_slice_len(
            state,
            accel_len,
            "sparse corrupt accelerator length",
        )?;
        let (accel, _) = (&state[..accel_len], &state[accel_len..]);

        Ok(State {
            id,
            is_match,
            ntrans,
            input_ranges,
            next,
            pattern_ids,
            accel,
        })
    }

    /// Return an iterator over all of the states in this DFA.
    ///
    /// The iterator returned yields tuples, where the first element is the
    /// state ID and the second element is the state itself.
    fn states(&self) -> StateIter<'_, T> {
        StateIter { trans: self, id: DEAD.as_usize() }
    }

    /// Returns the sparse transitions as raw bytes.
    fn sparse(&self) -> &[u8] {
        self.sparse.as_ref()
    }

    /// Returns the number of bytes represented by a single state ID.
    fn id_len(&self) -> usize {
        StateID::SIZE
    }

    /// Return the memory usage, in bytes, of these transitions.
    ///
    /// This does not include the size of a `Transitions` value itself.
    fn memory_usage(&self) -> usize {
        self.sparse().len()
    }
}

#[cfg(feature = "alloc")]
impl<T: AsMut<[u8]>> Transitions<T> {
    /// Return a convenient mutable representation of the given state.
    /// This panics if the state is invalid.
    fn state_mut(&mut self, id: StateID) -> StateMut<'_> {
        let mut state = &mut self.sparse_mut()[id.as_usize()..];
        let mut ntrans = bytes::read_u16(&state) as usize;
        let is_match = (1 << 15) & ntrans != 0;
        ntrans &= !(1 << 15);
        state = &mut state[2..];

        let (input_ranges, state) = state.split_at_mut(ntrans * 2);
        let (next, state) = state.split_at_mut(ntrans * StateID::SIZE);
        let (pattern_ids, state) = if is_match {
            let npats = bytes::read_u32(&state) as usize;
            state[4..].split_at_mut(npats * 4)
        } else {
            (&mut [][..], state)
        };

        let accel_len = usize::from(state[0]);
        let accel = &mut state[1..accel_len + 1];
        StateMut {
            id,
            is_match,
            ntrans,
            input_ranges,
            next,
            pattern_ids,
            accel,
        }
    }

    /// Returns the sparse transitions as raw mutable bytes.
    fn sparse_mut(&mut self) -> &mut [u8] {
        self.sparse.as_mut()
    }
}

/// The set of all possible starting states in a DFA.
///
/// See the eponymous type in the `dense` module for more details. This type
/// is very similar to `dense::StartTable`, except that its underlying
/// representation is `&[u8]` instead of `&[S]`. (The latter would require
/// sparse DFAs to be aligned, which is explicitly something we do not require
/// because we don't really need it.)
#[derive(Clone)]
struct StartTable<T> {
    /// The initial start state IDs as a contiguous table of native endian
    /// encoded integers, represented by `S`.
    ///
    /// In practice, T is either Vec<u8> or &[u8] and has no alignment
    /// requirements.
    ///
    /// The first `stride` (currently always 4) entries always correspond to
    /// the start states for the entire DFA. After that, there are
    /// `stride * patterns` state IDs, where `patterns` may be zero in the
    /// case of a DFA with no patterns or in the case where the DFA was built
    /// without enabling starting states for each pattern.
    table: T,
    /// The number of starting state IDs per pattern.
    stride: usize,
    /// The total number of patterns for which starting states are encoded.
    /// This may be zero for non-empty DFAs when the DFA was built without
    /// start states for each pattern.
    pattern_len: usize,
}

#[cfg(feature = "alloc")]
impl StartTable<Vec<u8>> {
    fn new(patterns: usize) -> StartTable<Vec<u8>> {
        let stride = Start::len();
        // This is OK since the only way we're here is if a dense DFA could be
        // constructed successfully, which uses the same space.
        let len = stride
            .checked_mul(patterns)
            .unwrap()
            .checked_add(stride)
            .unwrap()
            .checked_mul(StateID::SIZE)
            .unwrap();
        StartTable { table: vec![0; len], stride, pattern_len: patterns }
    }

    fn from_dense_dfa<T: AsRef<[u32]> + Send + Sync>(
        dfa: &dense::DFA<T>,
        remap: &[StateID],
    ) -> Result<StartTable<Vec<u8>>, Error> {
        // Unless the DFA has start states compiled for each pattern, then
        // as far as the starting state table is concerned, there are zero
        // patterns to account for. It will instead only store starting states
        // for the entire DFA.
        let start_pattern_len = if dfa.has_starts_for_each_pattern() {
            dfa.pattern_len()
        } else {
            0
        };
        let mut sl = StartTable::new(start_pattern_len);
        for (old_start_id, sty, pid) in dfa.starts() {
            let new_start_id = remap[dfa.to_index(old_start_id)];
            sl.set_start(sty, pid, new_start_id);
        }
        Ok(sl)
    }
}

impl<'a> StartTable<&'a [u8]> {
    unsafe fn from_bytes_unchecked(
        mut slice: &'a [u8],
    ) -> Result<(StartTable<&'a [u8]>, usize), DeserializeError> {
        let slice_start = slice.as_ptr() as usize;

        let (stride, nr) =
            bytes::try_read_u32_as_usize(slice, "sparse start table stride")?;
        slice = &slice[nr..];

        let (pattern_len, nr) = bytes::try_read_u32_as_usize(
            slice,
            "sparse start table patterns",
        )?;
        slice = &slice[nr..];

        if stride != Start::len() {
            return Err(DeserializeError::generic(
                "invalid sparse starting table stride",
            ));
        }
        if pattern_len > PatternID::LIMIT {
            return Err(DeserializeError::generic(
                "sparse invalid number of patterns",
            ));
        }
        let pattern_table_size =
            bytes::mul(stride, pattern_len, "sparse invalid pattern length")?;
        // Our start states always start with a single stride of start states
        // for the entire automaton which permit it to match any pattern. What
        // follows it are an optional set of start states for each pattern.
        let start_state_len = bytes::add(
            stride,
            pattern_table_size,
            "sparse invalid 'any' pattern starts size",
        )?;
        let table_bytes_len = bytes::mul(
            start_state_len,
            StateID::SIZE,
            "sparse pattern table bytes length",
        )?;
        bytes::check_slice_len(
            slice,
            table_bytes_len,
            "sparse start ID table",
        )?;
        let table_bytes = &slice[..table_bytes_len];
        slice = &slice[table_bytes_len..];

        let sl = StartTable { table: table_bytes, stride, pattern_len };
        Ok((sl, slice.as_ptr() as usize - slice_start))
    }
}

impl<T: AsRef<[u8]>> StartTable<T> {
    fn write_to<E: Endian>(
        &self,
        mut dst: &mut [u8],
    ) -> Result<usize, SerializeError> {
        let nwrite = self.write_to_len();
        if dst.len() < nwrite {
            return Err(SerializeError::buffer_too_small(
                "sparse starting table ids",
            ));
        }
        dst = &mut dst[..nwrite];

        // write stride
        E::write_u32(u32::try_from(self.stride).unwrap(), dst);
        dst = &mut dst[size_of::<u32>()..];
        // write pattern length
        E::write_u32(u32::try_from(self.pattern_len).unwrap(), dst);
        dst = &mut dst[size_of::<u32>()..];
        // write start IDs
        dst.copy_from_slice(self.table());
        Ok(nwrite)
    }

    /// Returns the number of bytes the serialized form of this transition
    /// table will use.
    fn write_to_len(&self) -> usize {
        size_of::<u32>() // stride
        + size_of::<u32>() // # patterns
        + self.table().len()
    }

    /// Validates that every starting state ID in this table is valid.
    ///
    /// That is, every starting state ID can be used to correctly decode a
    /// state in the DFA's sparse transitions.
    fn validate(
        &self,
        trans: &Transitions<T>,
    ) -> Result<(), DeserializeError> {
        for (id, _, _) in self.iter() {
            let _ = trans.try_state(id)?;
        }
        Ok(())
    }

    /// Converts this start list to a borrowed value.
    fn as_ref(&self) -> StartTable<&'_ [u8]> {
        StartTable {
            table: self.table(),
            stride: self.stride,
            pattern_len: self.pattern_len,
        }
    }

    /// Converts this start list to an owned value.
    #[cfg(feature = "alloc")]
    fn to_owned(&self) -> StartTable<Vec<u8>> {
        StartTable {
            table: self.table().to_vec(),
            stride: self.stride,
            pattern_len: self.pattern_len,
        }
    }

    /// Return the start state for the given index and pattern ID. If the
    /// pattern ID is None, then the corresponding start state for the entire
    /// DFA is returned. If the pattern ID is not None, then the corresponding
    /// starting state for the given pattern is returned. If this start table
    /// does not have individual starting states for each pattern, then this
    /// panics.
    fn start(&self, index: Start, pattern_id: Option<PatternID>) -> StateID {
        let start_index = index.as_usize();
        let index = match pattern_id {
            None => start_index,
            Some(pid) => {
                let pid = pid.as_usize();
                assert!(
                    pid < self.pattern_len,
                    "invalid pattern ID {:?}",
                    pid
                );
                self.stride
                    .checked_mul(pid)
                    .unwrap()
                    .checked_add(self.stride)
                    .unwrap()
                    .checked_add(start_index)
                    .unwrap()
            }
        };
        let start = index * StateID::SIZE;
        // This OK since we're allowed to assume that the start table contains
        // valid StateIDs.
        bytes::read_state_id_unchecked(&self.table()[start..]).0
    }

    /// Return an iterator over all start IDs in this table.
    fn iter(&self) -> StartStateIter<'_, T> {
        StartStateIter { st: self, i: 0 }
    }

    /// Returns the total number of start state IDs in this table.
    fn len(&self) -> usize {
        self.table().len() / StateID::SIZE
    }

    /// Returns the table as a raw slice of bytes.
    fn table(&self) -> &[u8] {
        self.table.as_ref()
    }

    /// Return the memory usage, in bytes, of this start list.
    ///
    /// This does not include the size of a `StartTable` value itself.
    fn memory_usage(&self) -> usize {
        self.table().len()
    }
}

#[cfg(feature = "alloc")]
impl<T: AsMut<[u8]>> StartTable<T> {
    /// Set the start state for the given index and pattern.
    ///
    /// If the pattern ID or state ID are not valid, then this will panic.
    fn set_start(
        &mut self,
        index: Start,
        pattern_id: Option<PatternID>,
        id: StateID,
    ) {
        let start_index = index.as_usize();
        let index = match pattern_id {
            None => start_index,
            Some(pid) => {
                let pid = pid.as_usize();
                assert!(
                    pid < self.pattern_len,
                    "invalid pattern ID {:?}",
                    pid
                );
                self.stride
                    .checked_mul(pid)
                    .unwrap()
                    .checked_add(self.stride)
                    .unwrap()
                    .checked_add(start_index)
                    .unwrap()
            }
        };
        let start = index * StateID::SIZE;
        let end = start + StateID::SIZE;
        bytes::write_state_id::<bytes::NE>(
            id,
            &mut self.table.as_mut()[start..end],
        );
    }
}

/// An iterator over all state state IDs in a sparse DFA.
struct StartStateIter<'a, T> {
    st: &'a StartTable<T>,
    i: usize,
}

impl<'a, T: AsRef<[u8]>> Iterator for StartStateIter<'a, T> {
    type Item = (StateID, Start, Option<PatternID>);

    fn next(&mut self) -> Option<(StateID, Start, Option<PatternID>)> {
        let i = self.i;
        if i >= self.st.len() {
            return None;
        }
        self.i += 1;

        // This unwrap is okay since the stride of any DFA must always match
        // the number of start state types.
        let start_type = Start::from_usize(i % self.st.stride).unwrap();
        let pid = if i < self.st.stride {
            // This means we don't have start states for each pattern.
            None
        } else {
            // These unwraps are OK since we may assume our table and stride
            // is correct.
            let pid = i
                .checked_sub(self.st.stride)
                .unwrap()
                .checked_div(self.st.stride)
                .unwrap();
            Some(PatternID::new(pid).unwrap())
        };
        let start = i * StateID::SIZE;
        let end = start + StateID::SIZE;
        let bytes = self.st.table()[start..end].try_into().unwrap();
        // This is OK since we're allowed to assume that any IDs in this start
        // table are correct and valid for this DFA.
        let id = StateID::from_ne_bytes_unchecked(bytes);
        Some((id, start_type, pid))
    }
}

impl<'a, T> fmt::Debug for StartStateIter<'a, T> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_struct("StartStateIter").field("i", &self.i).finish()
    }
}

/// An iterator over all states in a sparse DFA.
///
/// This iterator yields tuples, where the first element is the state ID and
/// the second element is the state itself.
struct StateIter<'a, T> {
    trans: &'a Transitions<T>,
    id: usize,
}

impl<'a, T: AsRef<[u8]>> Iterator for StateIter<'a, T> {
    type Item = State<'a>;

    fn next(&mut self) -> Option<State<'a>> {
        if self.id >= self.trans.sparse().len() {
            return None;
        }
        let state = self.trans.state(StateID::new_unchecked(self.id));
        self.id = self.id + state.bytes_len();
        Some(state)
    }
}

impl<'a, T> fmt::Debug for StateIter<'a, T> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_struct("StateIter").field("id", &self.id).finish()
    }
}

/// A representation of a sparse DFA state that can be cheaply materialized
/// from a state identifier.
#[derive(Clone)]
struct State<'a> {
    /// The identifier of this state.
    id: StateID,
    /// Whether this is a match state or not.
    is_match: bool,
    /// The number of transitions in this state.
    ntrans: usize,
    /// Pairs of input ranges, where there is one pair for each transition.
    /// Each pair specifies an inclusive start and end byte range for the
    /// corresponding transition.
    input_ranges: &'a [u8],
    /// Transitions to the next state. This slice contains native endian
    /// encoded state identifiers, with `S` as the representation. Thus, there
    /// are `ntrans * size_of::<S>()` bytes in this slice.
    next: &'a [u8],
    /// If this is a match state, then this contains the pattern IDs that match
    /// when the DFA is in this state.
    ///
    /// This is a contiguous sequence of 32-bit native endian encoded integers.
    pattern_ids: &'a [u8],
    /// An accelerator for this state, if present. If this state has no
    /// accelerator, then this is an empty slice. When non-empty, this slice
    /// has length at most 3 and corresponds to the exhaustive set of bytes
    /// that must be seen in order to transition out of this state.
    accel: &'a [u8],
}

impl<'a> State<'a> {
    /// Searches for the next transition given an input byte. If no such
    /// transition could be found, then a dead state is returned.
    ///
    /// This is marked as inline to help dramatically boost sparse searching,
    /// which decodes each state it enters to follow the next transition.
    #[inline(always)]
    fn next(&self, input: u8) -> StateID {
        // This straight linear search was observed to be much better than
        // binary search on ASCII haystacks, likely because a binary search
        // visits the ASCII case last but a linear search sees it first. A
        // binary search does do a little better on non-ASCII haystacks, but
        // not by much. There might be a better trade off lurking here.
        for i in 0..(self.ntrans - 1) {
            let (start, end) = self.range(i);
            if start <= input && input <= end {
                return self.next_at(i);
            }
            // We could bail early with an extra branch: if input < b1, then
            // we know we'll never find a matching transition. Interestingly,
            // this extra branch seems to not help performance, or will even
            // hurt it. It's likely very dependent on the DFA itself and what
            // is being searched.
        }
        DEAD
    }

    /// Returns the next state ID for the special EOI transition.
    fn next_eoi(&self) -> StateID {
        self.next_at(self.ntrans - 1)
    }

    /// Returns the identifier for this state.
    fn id(&self) -> StateID {
        self.id
    }

    /// Returns the inclusive input byte range for the ith transition in this
    /// state.
    fn range(&self, i: usize) -> (u8, u8) {
        (self.input_ranges[i * 2], self.input_ranges[i * 2 + 1])
    }

    /// Returns the next state for the ith transition in this state.
    fn next_at(&self, i: usize) -> StateID {
        let start = i * StateID::SIZE;
        let end = start + StateID::SIZE;
        let bytes = self.next[start..end].try_into().unwrap();
        StateID::from_ne_bytes_unchecked(bytes)
    }

    /// Returns the pattern ID for the given match index. If the match index
    /// is invalid, then this panics.
    fn pattern_id(&self, match_index: usize) -> PatternID {
        let start = match_index * PatternID::SIZE;
        bytes::read_pattern_id_unchecked(&self.pattern_ids[start..]).0
    }

    /// Returns the total number of pattern IDs for this state. This is always
    /// zero when `is_match` is false.
    fn pattern_len(&self) -> usize {
        assert_eq!(0, self.pattern_ids.len() % 4);
        self.pattern_ids.len() / 4
    }

    /// Return the total number of bytes that this state consumes in its
    /// encoded form.
    fn bytes_len(&self) -> usize {
        let mut len = 2
            + (self.ntrans * 2)
            + (self.ntrans * StateID::SIZE)
            + (1 + self.accel.len());
        if self.is_match {
            len += size_of::<u32>() + self.pattern_ids.len();
        }
        len
    }

    /// Return an accelerator for this state.
    fn accelerator(&self) -> &'a [u8] {
        self.accel
    }
}

impl<'a> fmt::Debug for State<'a> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut printed = false;
        for i in 0..(self.ntrans - 1) {
            let next = self.next_at(i);
            if next == DEAD {
                continue;
            }

            if printed {
                write!(f, ", ")?;
            }
            let (start, end) = self.range(i);
            if start == end {
                write!(f, "{:?} => {:?}", DebugByte(start), next)?;
            } else {
                write!(
                    f,
                    "{:?}-{:?} => {:?}",
                    DebugByte(start),
                    DebugByte(end),
                    next,
                )?;
            }
            printed = true;
        }
        let eoi = self.next_at(self.ntrans - 1);
        if eoi != DEAD {
            if printed {
                write!(f, ", ")?;
            }
            write!(f, "EOI => {:?}", eoi)?;
        }
        Ok(())
    }
}

/// A representation of a mutable sparse DFA state that can be cheaply
/// materialized from a state identifier.
#[cfg(feature = "alloc")]
struct StateMut<'a> {
    /// The identifier of this state.
    id: StateID,
    /// Whether this is a match state or not.
    is_match: bool,
    /// The number of transitions in this state.
    ntrans: usize,
    /// Pairs of input ranges, where there is one pair for each transition.
    /// Each pair specifies an inclusive start and end byte range for the
    /// corresponding transition.
    input_ranges: &'a mut [u8],
    /// Transitions to the next state. This slice contains native endian
    /// encoded state identifiers, with `S` as the representation. Thus, there
    /// are `ntrans * size_of::<S>()` bytes in this slice.
    next: &'a mut [u8],
    /// If this is a match state, then this contains the pattern IDs that match
    /// when the DFA is in this state.
    ///
    /// This is a contiguous sequence of 32-bit native endian encoded integers.
    pattern_ids: &'a [u8],
    /// An accelerator for this state, if present. If this state has no
    /// accelerator, then this is an empty slice. When non-empty, this slice
    /// has length at most 3 and corresponds to the exhaustive set of bytes
    /// that must be seen in order to transition out of this state.
    accel: &'a mut [u8],
}

#[cfg(feature = "alloc")]
impl<'a> StateMut<'a> {
    /// Sets the ith transition to the given state.
    fn set_next_at(&mut self, i: usize, next: StateID) {
        let start = i * StateID::SIZE;
        let end = start + StateID::SIZE;
        bytes::write_state_id::<bytes::NE>(next, &mut self.next[start..end]);
    }
}

#[cfg(feature = "alloc")]
impl<'a> fmt::Debug for StateMut<'a> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let state = State {
            id: self.id,
            is_match: self.is_match,
            ntrans: self.ntrans,
            input_ranges: self.input_ranges,
            next: self.next,
            pattern_ids: self.pattern_ids,
            accel: self.accel,
        };
        fmt::Debug::fmt(&state, f)
    }
}

/// A binary search routine specialized specifically to a sparse DFA state's
/// transitions. Specifically, the transitions are defined as a set of pairs
/// of input bytes that delineate an inclusive range of bytes. If the input
/// byte is in the range, then the corresponding transition is a match.
///
/// This binary search accepts a slice of these pairs and returns the position
/// of the matching pair (the ith transition), or None if no matching pair
/// could be found.
///
/// Note that this routine is not currently used since it was observed to
/// either decrease performance when searching ASCII, or did not provide enough
/// of a boost on non-ASCII haystacks to be worth it. However, we leave it here
/// for posterity in case we can find a way to use it.
///
/// In theory, we could use the standard library's search routine if we could
/// cast a `&[u8]` to a `&[(u8, u8)]`, but I don't believe this is currently
/// guaranteed to be safe and is thus UB (since I don't think the in-memory
/// representation of `(u8, u8)` has been nailed down). One could define a
/// repr(C) type, but the casting doesn't seem justified.
#[allow(dead_code)]
#[inline(always)]
fn binary_search_ranges(ranges: &[u8], needle: u8) -> Option<usize> {
    debug_assert!(ranges.len() % 2 == 0, "ranges must have even length");
    debug_assert!(ranges.len() <= 512, "ranges should be short");

    let (mut left, mut right) = (0, ranges.len() / 2);
    while left < right {
        let mid = (left + right) / 2;
        let (b1, b2) = (ranges[mid * 2], ranges[mid * 2 + 1]);
        if needle < b1 {
            right = mid;
        } else if needle > b2 {
            left = mid + 1;
        } else {
            return Some(mid);
        }
    }
    None
}
