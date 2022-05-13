use crate::util::id::PatternID;

#[derive(Clone)]
pub struct Search<T> {
    haystack: T,
    span: Span,
    pattern: Option<PatternID>,
    earliest: bool,
    utf8: bool,
}

impl<T: AsRef<[u8]>> Search<T> {
    /// Create a new search configuration for the given haystack.
    #[inline]
    pub fn new(haystack: T) -> Search<T> {
        let span = Span::new(0, haystack.as_ref().len());
        Search { haystack, span, pattern: None, earliest: false, utf8: true }
    }

    /// Set the span for this search.
    ///
    /// This routine does not panic if the span given is not a valid range for
    /// this search's haystack. If this search is run with an invalid range,
    /// then the most likely outcome is that the actual execution will panic.
    #[inline]
    pub fn span(self, span: Span) -> Search<T> {
        Search { span, ..self }
    }

    /// Like `Search::span`, but accepts any range instead.
    ///
    /// This routine does not panic if the span given is not a valid range for
    /// this search's haystack. If this search is run with an invalid range,
    /// then the most likely outcome is that the actual execution will panic.
    ///
    /// # Panics
    ///
    /// This routine will panic if the given range could not be converted to a
    /// valid [`core::ops::Range`]. For example, this would panic when given
    /// `0..=usize::MAX` since it cannot be represented using a half-open
    /// interval.
    #[inline]
    pub fn range<R: core::ops::RangeBounds<usize>>(
        self,
        range: R,
    ) -> Search<T> {
        use core::ops::Bound;

        // It's a little weird to convert ranges into spans, and then spans
        // back into ranges when we actually slice the haystack. Because
        // of that process, we always represent everything as a `Range`.
        // Therefore, handling things like m..=n is a little awkward. (We would
        // use core::ops::Range inside of Span if we could, but it isn't Copy
        // and it's too inconvenient for a Span to not by Copy.)
        let start = match range.start_bound() {
            Bound::Included(&i) => i,
            // Can this case ever happen? Range syntax doesn't support it...
            Bound::Excluded(&i) => i.checked_add(1).unwrap(),
            Bound::Unbounded => 0,
        };
        let end = match range.end_bound() {
            Bound::Included(&i) => i.checked_add(1).unwrap(),
            Bound::Excluded(&i) => i,
            Bound::Unbounded => 0,
        };
        self.span(Span::new(start, end))
    }

    /// Set the pattern to search for, if supported.
    ///
    /// When given, the an anchored search for only the specified pattern will
    /// be executed. If not given, then the search will look for any pattern
    /// that matches. (Whether that search is anchored or not depends on the
    /// configuration of your regex engine and, ultimately, the pattern
    /// itself.)
    ///
    /// If a pattern ID is given and a regex engine doesn't support searching
    /// by a specific pattern, then the regex engine must panic.
    #[inline]
    pub fn pattern(self, pattern: Option<PatternID>) -> Search<T> {
        Search { pattern, ..self }
    }

    /// Whether to execute an "earliest" search or not.
    ///
    /// When running a non-overlapping search, an "earliest" search will return
    /// the match location as early as possible. For example, given a pattern
    /// of `foo[0-9]+` and a haystack of `foo12345`, a normal leftmost search
    /// will return `foo12345` as a match. But an "earliest" search for regex
    /// engines that support "earliest" semantics will return `foo1` as a
    /// match, since as soon as the first digit following `foo` is seen, it is
    /// known to have found a match.
    ///
    /// Note that "earliest" semantics generally depend on the regex engine.
    /// Different regex engines may determine there is a match at different
    /// points. So there is no guarantee that "earliest" matches will always
    /// return the same offsets for all regex engines. The "earliest" notion
    /// is really about when the particular regex engine determines there is
    /// a match. This is often useful for implementing "did a match occur or
    /// not" predicates, but sometimes the offset is useful as well.
    ///
    /// This is disabled by default.
    #[inline]
    pub fn earliest(self, yes: bool) -> Search<T> {
        Search { earliest: yes, ..self }
    }

    #[inline]
    pub fn utf8(self, yes: bool) -> Search<T> {
        Search { utf8: yes, ..self }
    }

    /// Return the haystack for this search as bytes.
    #[inline]
    pub fn bytes(&self) -> &[u8] {
        self.haystack.as_ref()
    }

    /// Return a borrow of the underlying haystack.
    #[inline]
    pub fn haystack(&self) -> &T {
        &self.haystack
    }

    /// Consume this search and return the haystack inside of it.
    #[inline]
    pub fn into_haystack(self) -> T {
        self.haystack
    }

    /// Set the span for this search configuration.
    ///
    /// This is like the [`Search::span`] method, except this mutates the
    /// span in place.
    #[inline]
    pub fn set_span(&mut self, span: Span) {
        self.span = span;
    }

    /// Set the starting offset for the span for this search configuration.
    ///
    /// This is a convenience routine for only mutating the start of a span
    /// without having to set the entire span.
    #[inline]
    pub fn set_start(&mut self, start: usize) {
        self.span.set_start(start);
    }

    /// Set the ending offset for the span for this search configuration.
    ///
    /// This is a convenience routine for only mutating the end of a span
    /// without having to set the entire span.
    #[inline]
    pub fn set_end(&mut self, end: usize) {
        self.span.set_end(end);
    }

    /// Step the search ahead by one "unit."
    ///
    /// A unit is either a byte (when [`Search::utf8`] is disabled) or a
    /// UTF-8 encoding of a Unicode scalar value (when `Search::utf8` is
    /// enabled). The latter moves ahead at most 4 bytes, depending on the
    /// length of next encoded codepoint.
    ///
    /// Stepping this search may cause the start offset to be greater than the
    /// end offset, thus resulting in [`Search::is_done`] returning `true`.
    ///
    /// # Panics
    ///
    /// This panics if this would otherwise overflow a `usize`.
    #[inline]
    pub fn step(&mut self) {
        self.set_start(if self.utf8 {
            crate::util::next_utf8(self.bytes(), self.get_span().start())
        } else {
            self.get_span().start().checked_add(1).unwrap()
        });
    }

    /// Return the span for this search configuration.
    ///
    /// If one was not explicitly set, then the span corresponds to the entire
    /// range of the haystack.
    #[inline]
    pub fn get_span(&self) -> Span {
        self.span
    }

    /// Return the pattern ID for this search configuration, if one was set.
    #[inline]
    pub fn get_pattern(&self) -> Option<PatternID> {
        self.pattern
    }

    /// Return whether this search should execute in "earliest" mode.
    #[inline]
    pub fn get_earliest(&self) -> bool {
        self.earliest
    }

    /// Return whether this search should execute in "UTF-8" mode.
    #[inline]
    pub fn get_utf8(&self) -> bool {
        self.utf8
    }

    /// Return true if and only if this search can never return any other
    /// matches.
    ///
    /// For example, if the start position of this search is greater than the
    /// end position of the search.
    #[inline]
    pub fn is_done(&self) -> bool {
        self.get_span().start() > self.get_span().end()
    }

    /// Returns true if and only if the given offset in this search's haystack
    /// falls on a valid UTF-8 encoded codepoint boundary.
    ///
    /// If the haystack is not valid UTF-8, then the behavior of this routine
    /// is unspecified.
    #[inline]
    pub fn is_char_boundary(&self, offset: usize) -> bool {
        crate::util::is_char_boundary(self.bytes(), offset)
    }
}

impl<T: AsRef<[u8]>> core::fmt::Debug for Search<T> {
    fn fmt(&self, f: &mut core::fmt::Formatter) -> core::fmt::Result {
        use crate::util::escape::DebugHaystack;

        f.debug_struct("Search")
            .field("span", &self.span)
            .field("pattern", &self.pattern)
            .field("earliest", &self.earliest)
            .field("utf8", &self.utf8)
            .field("haystack", &DebugHaystack(self.bytes()))
            .finish()
    }
}

/// A representation of a match reported by a regex engine.
///
/// A match records the start and end offsets of the match in the haystack.
///
/// Every match guarantees that `start <= end`.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct Span {
    /// The start offset of the match, inclusive.
    start: usize,
    /// The end offset of the match, exclusive.
    end: usize,
}

impl Span {
    /// Create a new match from a byte offset span.
    #[inline]
    pub fn new(start: usize, end: usize) -> Span {
        Span { start, end }
    }

    /// The starting position of the match.
    #[inline]
    pub fn start(&self) -> usize {
        self.start
    }

    /// The ending position of the match.
    #[inline]
    pub fn end(&self) -> usize {
        self.end
    }

    /// Returns the match location as a range.
    #[inline]
    pub fn range(&self) -> core::ops::Range<usize> {
        self.start..self.end
    }

    /// Returns true if and only if this match is empty. That is, when
    /// `start() == end()`.
    ///
    /// An empty match can only be returned when the empty string matches the
    /// corresponding regex.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.start == self.end
    }

    /// Set the starting offset for this span.
    #[inline]
    pub fn set_start(&mut self, start: usize) {
        self.start = start;
    }

    /// Set the ending offset for this span.
    #[inline]
    pub fn set_end(&mut self, end: usize) {
        self.end = end;
    }

    /*
    /// Return a new span with the given offset added to each bound.
    ///
    /// # Panics
    ///
    /// This panics if `end < start` after adding the given offset to the
    /// start bound.
    #[inline]
    pub fn add(&self, offset: usize) -> Span {
        Span::new(self.start + offset, self.end + offset)
    }

    /// Return a new span with the given offset added to the start bound.
    ///
    /// # Panics
    ///
    /// This panics if `end < start` after adding the given offset to the
    /// start bound.
    #[inline]
    pub fn add_start(&self, offset: usize) -> Span {
        Span::new(self.start + offset, self.end)
    }

    /// Return a new span with the given offset added to the end bound.
    ///
    /// # Panics
    ///
    /// This panics if `end < start` after adding the given offset to the
    /// end bound.
    #[inline]
    pub fn add_end(&self, offset: usize) -> Span {
        Span::new(self.start, self.end + offset)
    }
    */
}

impl core::ops::Index<Span> for [u8] {
    type Output = [u8];

    #[inline]
    fn index(&self, index: Span) -> &[u8] {
        &self[index.range()]
    }
}

impl core::ops::IndexMut<Span> for [u8] {
    #[inline]
    fn index_mut(&mut self, index: Span) -> &mut [u8] {
        &mut self[index.range()]
    }
}

impl core::ops::Index<Span> for str {
    type Output = str;

    #[inline]
    fn index(&self, index: Span) -> &str {
        &self[index.range()]
    }
}

/// The kind of match semantics to use for a regex pattern.
///
/// The default match kind is `LeftmostFirst`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MatchKind {
    /// Report all possible matches.
    All,
    /// Report only the leftmost matches. When multiple leftmost matches exist,
    /// report the match corresponding to the part of the regex that appears
    /// first in the syntax.
    LeftmostFirst,
    /// Hints that destructuring should not be exhaustive.
    ///
    /// This enum may grow additional variants, so this makes sure clients
    /// don't count on exhaustive matching. (Otherwise, adding a new variant
    /// could break existing code.)
    #[doc(hidden)]
    __Nonexhaustive,
    // There is prior art in RE2 that shows that we should be able to add
    // LeftmostLongest too. The tricky part of it is supporting ungreedy
    // repetitions. Instead of treating all NFA states as having equivalent
    // priority (as in 'All') or treating all NFA states as having distinct
    // priority based on order (as in 'LeftmostFirst'), we instead group NFA
    // states into sets, and treat members of each set as having equivalent
    // priority, but having greater priority than all following members
    // of different sets.
    //
    // However, it's not clear whether it's really worth adding this. After
    // all, leftmost-longest can be emulated when using literals by using
    // leftmost-first and sorting the literals by length in descending order.
    // However, this won't work for arbitrary regexes. e.g., `\w|\w\w` will
    // always match `a` in `ab` when using leftmost-first, but leftmost-longest
    // would match `ab`.
}

impl MatchKind {
    #[cfg(feature = "alloc")]
    pub(crate) fn continue_past_first_match(&self) -> bool {
        *self == MatchKind::All
    }
}

impl Default for MatchKind {
    fn default() -> MatchKind {
        MatchKind::LeftmostFirst
    }
}

/// A representation of a match reported by a DFA.
///
/// This is called a "half" match because it only includes the end location
/// (or start location for a reverse match) of a match. This corresponds to the
/// information that a single DFA scan can report. Getting the other half of
/// the match requires a second scan with a reversed DFA.
///
/// A half match also includes the pattern that matched. The pattern is
/// identified by an ID, which corresponds to its position (starting from `0`)
/// relative to other patterns used to construct the corresponding DFA. If only
/// a single pattern is provided to the DFA, then all matches are guaranteed to
/// have a pattern ID of `0`.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct HalfMatch {
    /// The pattern ID.
    pub(crate) pattern: PatternID,
    /// The offset of the match.
    ///
    /// For forward searches, the offset is exclusive. For reverse searches,
    /// the offset is inclusive.
    pub(crate) offset: usize,
}

impl HalfMatch {
    /// Create a new half match from a pattern ID and a byte offset.
    #[inline]
    pub fn new(pattern: PatternID, offset: usize) -> HalfMatch {
        HalfMatch { pattern, offset }
    }

    /// Create a new half match from a pattern ID and a byte offset.
    ///
    /// This is like [`HalfMatch::new`], but accepts a `usize` instead of a
    /// [`PatternID`]. This panics if the given `usize` is not representable
    /// as a `PatternID`.
    #[inline]
    pub fn must(pattern: usize, offset: usize) -> HalfMatch {
        HalfMatch::new(PatternID::new(pattern).unwrap(), offset)
    }

    /// Returns the ID of the pattern that matched.
    ///
    /// The ID of a pattern is derived from the position in which it was
    /// originally inserted into the corresponding DFA. The first pattern has
    /// identifier `0`, and each subsequent pattern is `1`, `2` and so on.
    #[inline]
    pub fn pattern(&self) -> PatternID {
        self.pattern
    }

    /// The position of the match.
    ///
    /// If this match was produced by a forward search, then the offset is
    /// exclusive. If this match was produced by a reverse search, then the
    /// offset is inclusive.
    #[inline]
    pub fn offset(&self) -> usize {
        self.offset
    }
}

/// A representation of a multi match reported by a regex engine.
///
/// A multi match has two essential pieces of information: the identifier of
/// the pattern that matched, along with the start and end offsets of the match
/// in the haystack.
///
/// The pattern is identified by an ID, which corresponds to its position
/// (starting from `0`) relative to other patterns used to construct the
/// corresponding regex engine. If only a single pattern is provided, then all
/// multi matches are guaranteed to have a pattern ID of `0`.
///
/// Every multi match guarantees that `start <= end`.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct Match {
    /// The pattern ID.
    pattern: PatternID,
    /// The underlying match span.
    span: Span,
}

impl Match {
    /// Create a new match from a pattern ID and a byte offset span.
    ///
    /// # Panics
    ///
    /// This panics if `end < start`.
    #[inline]
    pub fn new(pattern: PatternID, start: usize, end: usize) -> Match {
        Match { pattern, span: Span::new(start, end) }
    }

    /// Create a new match from a pattern ID and a byte offset span.
    ///
    /// This is like [`Match::new`], but accepts a `usize` instead of a
    /// [`PatternID`]. This panics if the given `usize` is not representable
    /// as a `PatternID`.
    ///
    /// # Panics
    ///
    /// This panics if `end < start` or if `pattern > PatternID::MAX`.
    #[inline]
    pub fn must(pattern: usize, start: usize, end: usize) -> Match {
        Match::new(PatternID::new(pattern).unwrap(), start, end)
    }

    /// Returns the ID of the pattern that matched.
    ///
    /// The ID of a pattern is derived from the position in which it was
    /// originally inserted into the corresponding regex engine. The first
    /// pattern has identifier `0`, and each subsequent pattern is `1`, `2` and
    /// so on.
    #[inline]
    pub fn pattern(&self) -> PatternID {
        self.pattern
    }

    /// The starting position of the match.
    #[inline]
    pub fn start(&self) -> usize {
        self.span().start()
    }

    /// The ending position of the match.
    #[inline]
    pub fn end(&self) -> usize {
        self.span().end()
    }

    /// Returns the match location as a range.
    #[inline]
    pub fn range(&self) -> core::ops::Range<usize> {
        self.span().range()
    }

    /// Returns the span for this match.
    #[inline]
    fn span(&self) -> &Span {
        // Should we export this method? Returning an &Span makes sense if
        // we keep our match types non-Copy. But if we do make Span satisfy
        // Copy, then we should probably just return Span.
        &self.span
    }

    /// Returns true if and only if this match is empty. That is, when
    /// `start() == end()`.
    ///
    /// An empty match can only be returned when the empty string was among
    /// the patterns used to build the Aho-Corasick automaton.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.span().is_empty()
    }
}

/// An error type indicating that a search stopped prematurely without finding
/// a match.
///
/// This error type implies that one cannot assume that no matches occur, since
/// the search stopped before completing.
///
/// Normally, when one searches for something, the response is either an
/// affirmative "it was found at this location" or a negative "not found at
/// all." However, in some cases, a regex engine can be configured to stop its
/// search before concluding whether a match exists or not. When this happens,
/// it may be important for the caller to know why the regex engine gave up and
/// where in the input it gave up at. This error type exposes the 'why' and the
/// 'where.'
///
/// For example, the DFAs provided by this library generally cannot correctly
/// implement Unicode word boundaries. Instead, they provide an option to
/// eagerly support them on ASCII text (since Unicode word boundaries are
/// equivalent to ASCII word boundaries when searching ASCII text), but will
/// "give up" if a non-ASCII byte is seen. In such cases, one is usually
/// required to either report the failure to the caller (unergonomic) or
/// otherwise fall back to some other regex engine (ergonomic, but potentially
/// costly).
///
/// More generally, some regex engines offer the ability for callers to specify
/// certain bytes that will trigger the regex engine to automatically quit if
/// they are seen.
///
/// Still yet, there may be other reasons for a failed match. For example,
/// the hybrid DFA provided by this crate can be configured to give up if it
/// believes that it is not efficient. This in turn permits callers to choose a
/// different regex engine.
///
/// # Advice
///
/// While this form of error reporting adds complexity, it is generally
/// possible for callers to configure regex engines to never give up a search,
/// and thus never return an error. Indeed, the default configuration for every
/// regex engine in this crate is such that they will never stop searching
/// early. Therefore, the only way to get a match error is if the regex engine
/// is explicitly configured to do so. Options that enable this behavior
/// document the new error conditions they imply.
///
/// Regex engines for which no errors are possible for any configuration will
/// return the normal `Option<Match>` and not use this error type at all.
///
/// For example, regex engines in the `dfa` sub-module will only report
/// `MatchError::Quit` if instructed by either
/// [enabling Unicode word boundaries](crate::dfa::dense::Config::unicode_word_boundary)
/// or by
/// [explicitly specifying one or more quit bytes](crate::dfa::dense::Config::quit).
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub enum MatchError {
    // Note that the first version of this type was called `SearchError` and it
    // included a third `None` variant to indicate that the search completed
    // and no match was found. However, this was problematic for iterator
    // APIs where the `None` sentinel for stopping iteration corresponds
    // precisely to the "match not found" case. The fact that the `None`
    // variant was buried inside this type was in turn quite awkward. So
    // instead, I removed the `None` variant, renamed the type and used
    // `Result<Option<Match>, MatchError>` in non-iterator APIs instead of the
    // conceptually simpler `Result<Match, MatchError>`. However, we "regain"
    // ergonomics by only putting the more complex API in the `try_` variants
    // ("fallible") of search methods. The infallible APIs will instead just
    // return `Option<Match>` and panic on error.
    /// The search saw a "quit" byte at which it was instructed to stop
    /// searching.
    Quit {
        /// The "quit" byte that was observed that caused the search to stop.
        byte: u8,
        /// The offset at which the quit byte was observed.
        offset: usize,
    },
    /// The search, based on heuristics, determined that it would be better
    /// to stop, typically to provide the caller an opportunity to use an
    /// alternative regex engine.
    ///
    /// Currently, the only way for this to occur is via the lazy DFA and
    /// only when it is configured to do so (it will not return this error by
    /// default).
    GaveUp {
        /// The offset at which the search stopped. This corresponds to the
        /// position immediately following the last byte scanned.
        offset: usize,
    },
}

#[cfg(feature = "std")]
impl std::error::Error for MatchError {}

impl core::fmt::Display for MatchError {
    fn fmt(&self, f: &mut core::fmt::Formatter) -> core::fmt::Result {
        match *self {
            MatchError::Quit { byte, offset } => write!(
                f,
                "quit search after observing byte \\x{:02X} at offset {}",
                byte, offset,
            ),
            MatchError::GaveUp { offset } => {
                write!(f, "gave up searching at offset {}", offset)
            }
        }
    }
}
