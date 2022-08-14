/*!
Types and routines that support the search APIs of most regex engines.
*/

use core::ops::{Range, RangeBounds};

use crate::util::{
    escape::DebugByte, prefilter::Prefilter, primitives::PatternID, utf8,
};

/// The parameters for a regex search.
///
/// While most regex engines in this crate expose a convenience `find`-like
/// routine that accepts a haystack and returns a match if one was found, it
/// turns out that regex searches have a lot of parameters. The `find`-like
/// methods represent the common use case, while this `Input` type represents
/// the full configurability of a regex search. That configurability includes:
///
/// * Search only a substring of a haystack, while taking the broader context
/// into account for resolving look-around assertions.
/// * Whether to use a prefilter for the search or not.
/// * Indicating whether to search for all patterns in a regex object, or to
/// only search for one pattern in particular.
/// * Whether to report a match as early as possible.
/// * Whether to report matches that might split a codepoint in valid UTF-8.
///
/// All of these parameters, except for the haystack, have sensible default
/// values. This means that the minimal search configuration is simply a call
/// to [`Input::new`] with your haystack. Setting any other parameter is
/// optional.
///
/// The API of `Input` is split into a few different parts:
///
/// * A builder-like API that transforms a `Input` by value. Examples:
/// [`Input::span`] and [`Input::prefilter`].
/// * A setter API that permits mutating parameters in place. Examples:
/// [`Input::set_span`] and [`Input::set_prefilter`].
/// * A getter API that permits retrieving any of the search parameters.
/// Examples: [`Input::get_span`] and [`Input::get_prefilter`].
/// * A few convenience getter routines that don't conform to the above naming
/// pattern due to how common they are. Examples: [`Input::haystack`],
/// [`Input::start`] and [`Input::end`].
/// * Miscellaneous predicates and other helper routines that are useful
/// in some contexts. Examples: [`Input::is_char_boundary`].
///
/// A `Input` exposes so much because it is meant to be used by both callers
/// of regex engines _and_ implementors of regex engines. A constraining
/// factor is that regex engines should accept a `&Input`, which means that
/// implementors should only use the "getter" APIs of a `Input`.
///
/// The lifetime parameters have the following meaning:
///
/// * `'h` refers to the lifetime of the haystack.
/// * `'p` refers to the lifetime of the prefilter. Since a prefilter is
/// optional, this defaults to the `'static` lifetime when a prefilter is not
/// present.
///
/// # Regex engine support
///
/// Any regex engine accepting an `Input` must support at least the following
/// things:
///
/// * Searching a `&[u8]` for matches.
/// * Searching a substring of `&[u8]` for a match, such that any match
/// reported must appear entirely within that substring.
/// * A match should never be reported when [`Input::is_done`] returns true.
///
/// Supporting other aspects of an `Input` are optional, but regex engines
/// should panic when something is requested that it cannot fulfill. (See the
/// `Panics` section below.)
///
/// # Panics
///
/// Since `Input` is meant to be a superset of most of the input parameters to
/// a search for any regex engine in this crate, it is possible to enable or
/// disable some options that might not have the intended effect. For this
/// reason, regex engines accepting an `Input` should panic when specific
/// options are set but cannot be provided.
///
/// What follows is a complete set of rules of when a regex engine should panic
/// based on the given `Input` configuration. Every regex engine in this crate
/// follows these rules.
///
/// * An [`Anchored`] setting is provided that isn't supported. For example, a
/// DFA might be compiled with only an unanchored starting state. Therefore,
/// if the caller asked for an [`Anchored::Yes`] search, then the regex engine
/// should panic. (Note though that if the caller asks for an `Anchored::No`
/// search and the regex pattern itself is anchored, then so long as the
/// regex engine can provide a way to search that is unanchored, it should be
/// permitted. That is, panicking should be a property of the regex engine
/// itself and not a property of the regex pattern.)
/// * If [`Input::utf8`] is enabled and the regex engine doesn't support it,
/// then a panic should occur. It is permissible to panic only in cases where
/// the regex engine would return a match inconsistent with the `utf8`
/// setting. (For example, a zero-width match that splits the UTF-8 encoding
/// of a codepoint.) Panicking may be more expansive than this, i.e., any time
/// it's set.
///
/// The following should *not* result in a panic:
///
/// * If a [`Input::prefilter`] is set and the regex engine doesn't support
/// them, then the regex engine is safe to simply ignore the prefilter. The
/// reason for this is that a prefilter is a best effort optimization technique
/// that must never impact the match semantics. Therefore, neglecting it is
/// merely an optimization decision. For example, a regex engine might support
/// prefilters but might decide in some cases not to use a prefilter even if
/// one is given based on some fact about the search.
/// * If [`Input::earliest`] is enabled and the regex engine doesn't support
/// returning the "earliest" match, then the regex engine is safe to simply
/// ignore the option. This is because "earliest" is not defined as a
/// particular match semantic itself, but rather, a mechanism by which a
/// particular regex engine can "return early" *if the opportunity arises*. The
/// "earliest" option is generally intended to be used as an implementation
/// strategy for implementing predicate routines like `is_match` where the
/// specific offset isn't important, but sometimes the offset is useful.
#[derive(Clone)]
pub struct Input<'h, 'p> {
    haystack: &'h [u8],
    span: Span,
    anchored: Anchored,
    pattern: Option<PatternID>,
    prefilter: Option<&'p dyn Prefilter>,
    earliest: bool,
    utf8: bool,
}

impl<'h, 'p> Input<'h, 'p> {
    /// Create a new search configuration for the given haystack.
    #[inline]
    pub fn new<H: ?Sized + AsRef<[u8]>>(
        haystack: &'h H,
    ) -> Input<'h, 'static> {
        Input {
            haystack: haystack.as_ref(),
            span: Span { start: 0, end: haystack.as_ref().len() },
            anchored: Anchored::No,
            pattern: None,
            prefilter: None,
            earliest: false,
            utf8: true,
        }
    }

    /// Set the span for this search.
    ///
    /// This routine does not panic if the span given is not a valid range for
    /// this search's haystack. If this search is run with an invalid range,
    /// then the most likely outcome is that the actual search execution will
    /// panic.
    ///
    /// This routine is generic over how a span is provided. While
    /// a [`Span`] may be given directly, one may also provide a
    /// `std::ops::Range<usize>`. To provide anything supported by range
    /// syntax, use the [`Input::range`] method.
    ///
    /// The default span is the entire haystack.
    ///
    /// Note that [`Input::range`] overrides this method and vice versa.
    ///
    /// # Example
    ///
    /// This example shows how the span of the search can impact whether a
    /// match is reported or not. This is particularly relevant for look-around
    /// operators, which might take things outside of the span into account
    /// when determining whether they match.
    ///
    /// ```
    /// use regex_automata::{
    ///     nfa::thompson::pikevm::PikeVM,
    ///     Match, Input,
    /// };
    ///
    /// // Look for 'at', but as a distinct word.
    /// let re = PikeVM::new(r"\bat\b")?;
    /// let mut cache = re.create_cache();
    /// let mut caps = re.create_captures();
    ///
    /// // Our haystack contains 'at', but not as a distinct word.
    /// let haystack = "batter";
    ///
    /// // A standard search finds nothing, as expected.
    /// let input = Input::new(haystack);
    /// re.search(&mut cache, &input, &mut caps);
    /// assert_eq!(None, caps.get_match());
    ///
    /// // But if we wanted to search starting at position '1', we might
    /// // slice the haystack. If we do this, it's impossible for the \b
    /// // anchors to take the surrounding context into account! And thus,
    /// // a match is produced.
    /// let input = Input::new(&haystack[1..3]);
    /// re.search(&mut cache, &input, &mut caps);
    /// assert_eq!(Some(Match::must(0, 0..2)), caps.get_match());
    ///
    /// // But if we specify the span of the search instead of slicing the
    /// // haystack, then the regex engine can "see" outside of the span
    /// // and resolve the anchors correctly.
    /// let input = Input::new(haystack).span(1..3);
    /// re.search(&mut cache, &input, &mut caps);
    /// assert_eq!(None, caps.get_match());
    ///
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    ///
    /// This may seem a little ham-fisted, but this scenario tends to come up
    /// if some other regex engine found the match span and now you need to
    /// re-process that span to look for capturing groups. (e.g., Run a faster
    /// DFA first, find a match, then run the PikeVM on just the match span to
    /// resolve capturing groups.) In order to implement that sort of logic
    /// correctly, you need to set the span on the search instead of slicing
    /// the haystack directly.
    ///
    /// The other advantage of using this routine to specify the bounds of the
    /// search is that the match offsets are still reported in terms of the
    /// original haystack. For example, the second search in the example above
    /// reported a match at position `0`, even though `at` starts at offset
    /// `1` because we sliced the haystack.
    #[inline]
    pub fn span<S: Into<Span>>(mut self, span: S) -> Input<'h, 'p> {
        self.set_span(span);
        self
    }

    /// Like `Input::span`, but accepts any range instead.
    ///
    /// This routine does not panic if the range given is not a valid range for
    /// this search's haystack. If this search is run with an invalid range,
    /// then the most likely outcome is that the actual search execution will
    /// panic.
    ///
    /// The default range is the entire haystack.
    ///
    /// Note that [`Input::span`] overrides this method and vice versa.
    ///
    /// # Panics
    ///
    /// This routine will panic if the given range could not be converted
    /// to a valid [`Range`]. For example, this would panic when given
    /// `0..=usize::MAX` since it cannot be represented using a half-open
    /// interval in terms of `usize`.
    ///
    /// # Example
    ///
    /// ```
    /// use regex_automata::Input;
    ///
    /// let input = Input::new("foobar");
    /// assert_eq!(0..6, input.get_range());
    ///
    /// let input = Input::new("foobar").range(2..=4);
    /// assert_eq!(2..5, input.get_range());
    /// ```
    #[inline]
    pub fn range<R: RangeBounds<usize>>(mut self, range: R) -> Input<'h, 'p> {
        self.set_range(range);
        self
    }

    /// Sets the anchor mode of a search.
    ///
    /// When a search is anchored (so that's [`Anchored::Yes`] or
    /// [`Anchored::Pattern`]), a match must begin at the start of a search.
    /// When a search is not anchored (that's [`Anchored::No`]), regex engines
    /// will behave as if the pattern started with a `(?:s-u.)*?`. This prefix
    /// permits a match to appear anywhere.
    ///
    /// By default, the anchored mode is [`Anchored::No`].
    ///
    /// **WARNING:** this is subtly different than using a `^` at the start of
    /// your regex. A `^` forces a regex to match exclusively at the start of
    /// a haystack, regardless of where you begin your search. In contrast,
    /// anchoring a search will allow your regex to match anywhere in your
    /// haystack, but the match must start at the beginning of a search.
    /// (Most of the higher level convenience search routines make "start of
    /// haystack" and "start of search" equivalent, but routines that accept an
    /// `Input` permit treating them as orthogonal.)
    ///
    /// For example, consider the haystack `aba` and the following searches:
    ///
    /// 1. The regex `^a` is compiled with `Anchored::No` and searches `aba`
    ///    starting at position `2`. Since `^` requires the match to start at
    ///    the beginning of the haystack and `2 > 0`, no match is found.
    /// 2. The regex `a` is compiled with `Anchored::Yes` and searches `aba`
    ///    starting at position `2`. This reports a match at `[2, 3]` since
    ///    the match starts where the search started. Since there is no `^`,
    ///    there is no requirement for the match to start at the beginning of
    ///    the haystack.
    /// 3. The regex `a` is compiled with `Anchored::Yes` and searches `aba`
    ///    starting at position `1`. Since `b` corresponds to position `1` and
    ///    since the search is anchored, it finds no match. While the regex
    ///    matches at other positions, configuring the search to be anchored
    ///    requires that it only report a match that begins at the same offset
    ///    as the beginning of the search.
    /// 4. The regex `a` is compiled with `Anchored::No` and searches `aba`
    ///    startting at position `1`. Since the search is not anchored and
    ///    the regex does not start with `^`, the search executes as if there
    ///    is a `(?s:.)*?` prefix that permits it to match anywhere. Thus, it
    ///    reports a match at `[2, 3]`.
    ///
    /// Note that the [`Anchored::Pattern`] mode is like `Anchored::Yes`,
    /// except it only reports matches for a particular pattern.
    ///
    /// # Example
    ///
    /// This demonstrates the differences between an anchored search and
    /// a pattern that begins with `^` (as described in the above warning
    /// message).
    ///
    /// ```
    /// use regex_automata::{
    ///     nfa::thompson::pikevm::PikeVM,
    ///     Anchored, Match, Input,
    /// };
    ///
    /// let haystack = "aba";
    ///
    /// let re = PikeVM::new(r"^a")?;
    /// let (mut cache, mut caps) = (re.create_cache(), re.create_captures());
    /// let input = Input::new(haystack).span(2..3).anchored(Anchored::No);
    /// re.search(&mut cache, &input, &mut caps);
    /// // No match is found because 2 is not the beginning of the haystack,
    /// // which is what ^ requires.
    /// assert_eq!(None, caps.get_match());
    ///
    /// let re = PikeVM::new(r"a")?;
    /// let (mut cache, mut caps) = (re.create_cache(), re.create_captures());
    /// let input = Input::new(haystack).span(2..3).anchored(Anchored::Yes);
    /// re.search(&mut cache, &input, &mut caps);
    /// // An anchored search can still match anywhere in the haystack, it just
    /// // must begin at the start of the search which is '2' in this case.
    /// assert_eq!(Some(Match::must(0, 2..3)), caps.get_match());
    ///
    /// let re = PikeVM::new(r"a")?;
    /// let (mut cache, mut caps) = (re.create_cache(), re.create_captures());
    /// let input = Input::new(haystack).span(1..3).anchored(Anchored::Yes);
    /// re.search(&mut cache, &input, &mut caps);
    /// // No match is found since we start searching at offset 1 which
    /// // corresponds to 'b'. Since there is no '(?s:.)*?' prefix, no match
    /// // is found.
    /// assert_eq!(None, caps.get_match());
    ///
    /// let re = PikeVM::new(r"a")?;
    /// let (mut cache, mut caps) = (re.create_cache(), re.create_captures());
    /// let input = Input::new(haystack).span(1..3).anchored(Anchored::No);
    /// re.search(&mut cache, &input, &mut caps);
    /// // Since anchored=no, an implicit '(?s:.)*?' prefix was added to the
    /// // pattern. Even though the search starts at 'b', the 'match anything'
    /// // prefix allows the search to match 'a'.
    /// let expected = Some(Match::must(0, 2..3));
    /// assert_eq!(expected, caps.get_match());
    ///
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    #[inline]
    pub fn anchored(mut self, mode: Anchored) -> Input<'h, 'p> {
        self.set_anchored(mode);
        self
    }

    /// Set the pattern to search for, if supported.
    ///
    /// When given, an anchored search for only the specified pattern will
    /// be executed. If not given, then the search will look for any pattern
    /// that matches. (Whether that search is anchored or not depends on
    /// the configuration of your regex engine and, ultimately, the pattern
    /// itself.)
    ///
    /// If a pattern ID is given and a regex engine doesn't support searching
    /// by a specific pattern, then the regex engine must panic.
    ///
    /// The default is to look for a match for any pattern in a regex object.
    ///
    /// # Example
    ///
    /// This example shows how to search for a specific pattern.
    ///
    /// ```
    /// use regex_automata::{
    ///     nfa::thompson::pikevm::PikeVM,
    ///     Anchored, Match, PatternID, Input,
    /// };
    ///
    /// let re = PikeVM::new_many(&[r"[a-z0-9]{6}", r"[a-z][a-z0-9]{5}"])?;
    /// let (mut cache, mut caps) = (re.create_cache(), re.create_captures());
    ///
    /// // A standard search looks for any pattern.
    /// let input = Input::new("bar foo123");
    /// re.search(&mut cache, &input, &mut caps);
    /// assert_eq!(Some(Match::must(0, 4..10)), caps.get_match());
    ///
    /// // But we can also check whether a specific pattern
    /// // matches at a particular position.
    /// let input = Input::new("bar foo123")
    ///     .range(4..)
    ///     .anchored(Anchored::Pattern(PatternID::must(1)));
    /// re.search(&mut cache, &input, &mut caps);
    /// assert_eq!(Some(Match::must(1, 4..10)), caps.get_match());
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    #[inline]
    pub fn pattern(mut self, pattern: Option<PatternID>) -> Input<'h, 'p> {
        self.set_pattern(pattern);
        self
    }

    #[inline]
    pub fn prefilter(
        mut self,
        prefilter: Option<&'p dyn Prefilter>,
    ) -> Input<'h, 'p> {
        self.set_prefilter(prefilter);
        self
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
    /// a match rather than a consistent semantic unto itself. This is often
    /// useful for implementing "did a match occur or not" predicates, but
    /// sometimes the offset is useful as well.
    ///
    /// This is disabled by default.
    ///
    /// # Example
    ///
    /// This example shows the difference between "earliest" searching and
    /// normal searching.
    ///
    /// ```
    /// use regex_automata::{nfa::thompson::pikevm::PikeVM, Match, Input};
    ///
    /// let re = PikeVM::new(r"foo[0-9]+")?;
    /// let mut cache = re.create_cache();
    /// let mut caps = re.create_captures();
    ///
    /// // A normal search implements greediness like you expect.
    /// let input = Input::new("foo12345");
    /// re.search(&mut cache, &input, &mut caps);
    /// assert_eq!(Some(Match::must(0, 0..8)), caps.get_match());
    ///
    /// // When 'earliest' is enabled and the regex engine supports
    /// // it, the search will bail once it knows a match has been
    /// // found.
    /// let input = Input::new("foo12345").earliest(true);
    /// re.search(&mut cache, &input, &mut caps);
    /// assert_eq!(Some(Match::must(0, 0..4)), caps.get_match());
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    #[inline]
    pub fn earliest(mut self, yes: bool) -> Input<'h, 'p> {
        self.set_earliest(yes);
        self
    }

    /// Whether to enable UTF-8 mode during search or not.
    ///
    /// UTF-8 mode on a `Input` refers to whether a regex engine should
    /// treat the haystack as valid UTF-8 in cases where that could make a
    /// difference.
    ///
    /// An example of this occurs when a regex pattern semantically matches the
    /// empty string. In such cases, the underlying finite state machine will
    /// likely not distiguish between empty strings that do and do not split
    /// codepoints in UTF-8 haystacks. When this option is enabled, the regex
    /// engine will insert higher level code that checks for whether the match
    /// splits a codepoint, and if so, skip that match entirely and look for
    /// the next one.
    ///
    /// In effect, this option is useful to enable when both of the following
    /// are true:
    ///
    /// 1. Your haystack is valid UTF-8.
    /// 2. You never want to report spans that fall on invalid UTF-8
    /// boundaries.
    ///
    /// Typically, this is enabled in concert with
    /// [`syntax::Config::utf8`](crate::util::syntax::Config::utf8).
    ///
    /// This is enabled by default.
    ///
    /// # Example
    ///
    /// This example shows how UTF-8 mode can impact the match spans that may
    /// be reported in certain cases.
    ///
    /// ```
    /// use regex_automata::{
    ///     nfa::thompson::pikevm::PikeVM,
    ///     Match, Input,
    /// };
    ///
    /// let re = PikeVM::new("")?;
    /// let (mut cache, mut caps) = (re.create_cache(), re.create_captures());
    ///
    /// // UTF-8 mode is enabled by default.
    /// let mut input = Input::new("☃");
    /// re.search(&mut cache, &input, &mut caps);
    /// assert_eq!(Some(Match::must(0, 0..0)), caps.get_match());
    ///
    /// // Even though an empty regex matches at 1..1, our next match is
    /// // 3..3 because 1..1 and 2..2 split the snowman codepoint (which is
    /// // three bytes long).
    /// input.set_start(1);
    /// re.search(&mut cache, &input, &mut caps);
    /// assert_eq!(Some(Match::must(0, 3..3)), caps.get_match());
    ///
    /// // But if we disable UTF-8, then we'll get matches at 1..1 and 2..2:
    /// let mut noutf8 = input.clone().utf8(false);
    /// re.search(&mut cache, &noutf8, &mut caps);
    /// assert_eq!(Some(Match::must(0, 1..1)), caps.get_match());
    ///
    /// noutf8.set_start(2);
    /// re.search(&mut cache, &noutf8, &mut caps);
    /// assert_eq!(Some(Match::must(0, 2..2)), caps.get_match());
    ///
    /// noutf8.set_start(3);
    /// re.search(&mut cache, &noutf8, &mut caps);
    /// assert_eq!(Some(Match::must(0, 3..3)), caps.get_match());
    ///
    /// noutf8.set_start(4);
    /// re.search(&mut cache, &noutf8, &mut caps);
    /// assert_eq!(None, caps.get_match());
    ///
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    #[inline]
    pub fn utf8(mut self, yes: bool) -> Input<'h, 'p> {
        self.set_utf8(yes);
        self
    }

    /// Set the span for this search configuration.
    ///
    /// This is like the [`Input::span`] method, except this mutates the
    /// span in place.
    ///
    /// This routine is generic over how a span is provided. While
    /// a [`Span`] may be given directly, one may also provide a
    /// `std::ops::Range<usize>`.
    ///
    /// # Example
    ///
    /// ```
    /// use regex_automata::Input;
    ///
    /// let mut input = Input::new("foobar");
    /// assert_eq!(0..6, input.get_range());
    /// input.set_span(2..4);
    /// assert_eq!(2..4, input.get_range());
    /// ```
    #[inline]
    pub fn set_span<S: Into<Span>>(&mut self, span: S) {
        self.span = span.into();
    }

    /// Set the span for this search configuration given any range.
    ///
    /// This is like the [`Input::range`] method, except this mutates the
    /// span in place.
    ///
    /// This routine does not panic if the range given is not a valid range for
    /// this search's haystack. If this search is run with an invalid range,
    /// then the most likely outcome is that the actual search execution will
    /// panic.
    ///
    /// # Panics
    ///
    /// This routine will panic if the given range could not be converted
    /// to a valid [`Range`]. For example, this would panic when given
    /// `0..=usize::MAX` since it cannot be represented using a half-open
    /// interval in terms of `usize`.
    ///
    /// # Example
    ///
    /// ```
    /// use regex_automata::Input;
    ///
    /// let mut input = Input::new("foobar");
    /// assert_eq!(0..6, input.get_range());
    /// input.set_range(2..=4);
    /// assert_eq!(2..5, input.get_range());
    /// ```
    #[inline]
    pub fn set_range<R: RangeBounds<usize>>(&mut self, range: R) {
        use core::ops::Bound;

        // It's a little weird to convert ranges into spans, and then spans
        // back into ranges when we actually slice the haystack. Because
        // of that process, we always represent everything as a half-open
        // internal. Therefore, handling things like m..=n is a little awkward.
        let start = match range.start_bound() {
            Bound::Included(&i) => i,
            // Can this case ever happen? Range syntax doesn't support it...
            Bound::Excluded(&i) => i.checked_add(1).unwrap(),
            Bound::Unbounded => 0,
        };
        let end = match range.end_bound() {
            Bound::Included(&i) => i.checked_add(1).unwrap(),
            Bound::Excluded(&i) => i,
            Bound::Unbounded => self.haystack().len(),
        };
        self.set_span(Span { start, end });
    }

    /// Set the starting offset for the span for this search configuration.
    ///
    /// This is a convenience routine for only mutating the start of a span
    /// without having to set the entire span.
    ///
    /// # Example
    ///
    /// ```
    /// use regex_automata::Input;
    ///
    /// let mut input = Input::new("foobar");
    /// assert_eq!(0..6, input.get_range());
    /// input.set_start(5);
    /// assert_eq!(5..6, input.get_range());
    /// ```
    #[inline]
    pub fn set_start(&mut self, start: usize) {
        self.span.start = start;
    }

    /// Set the ending offset for the span for this search configuration.
    ///
    /// This is a convenience routine for only mutating the end of a span
    /// without having to set the entire span.
    ///
    /// # Example
    ///
    /// ```
    /// use regex_automata::Input;
    ///
    /// let mut input = Input::new("foobar");
    /// assert_eq!(0..6, input.get_range());
    /// input.set_end(5);
    /// assert_eq!(0..5, input.get_range());
    /// ```
    #[inline]
    pub fn set_end(&mut self, end: usize) {
        self.span.end = end;
    }

    /// Set the anchor mode of a search.
    ///
    /// This is like [`Input::anchored`], except it mutates the search
    /// configuration in place.
    ///
    /// # Example
    ///
    /// ```
    /// use regex_automata::{Anchored, Input, PatternID};
    ///
    /// let mut input = Input::new("foobar");
    /// assert_eq!(Anchored::No, input.get_anchored());
    ///
    /// let pid = PatternID::must(5);
    /// input.set_anchored(Anchored::Pattern(pid));
    /// assert_eq!(Anchored::Pattern(pid), input.get_anchored());
    /// ```
    #[inline]
    pub fn set_anchored(&mut self, mode: Anchored) {
        self.anchored = mode;
    }

    /// Set the pattern to search for.
    ///
    /// This is like [`Input::pattern`], except it mutates the search
    /// configuration in place.
    ///
    /// # Example
    ///
    /// ```
    /// use regex_automata::{PatternID, Input};
    ///
    /// let mut input = Input::new("foobar");
    /// assert_eq!(None, input.get_pattern());
    /// input.set_pattern(Some(PatternID::must(5)));
    /// assert_eq!(Some(PatternID::must(5)), input.get_pattern());
    /// ```
    #[inline]
    pub fn set_pattern(&mut self, pattern: Option<PatternID>) {
        self.pattern = pattern;
    }

    #[inline]
    pub fn set_prefilter(&mut self, prefilter: Option<&'p dyn Prefilter>) {
        self.prefilter = prefilter;
    }

    /// Set whether the search should execute in "earliest" mode or not.
    ///
    /// This is like [`Input::earliest`], except it mutates the search
    /// configuration in place.
    ///
    /// # Example
    ///
    /// ```
    /// use regex_automata::Input;
    ///
    /// let mut input = Input::new("foobar");
    /// assert!(!input.get_earliest());
    /// input.set_earliest(true);
    /// assert!(input.get_earliest());
    /// ```
    #[inline]
    pub fn set_earliest(&mut self, yes: bool) {
        self.earliest = yes;
    }

    /// Set whether the search should execute in UTF-8 mode or not.
    ///
    /// This is like [`Input::utf8`], except it mutates the search
    /// configuration in place.
    ///
    /// # Example
    ///
    /// ```
    /// use regex_automata::Input;
    ///
    /// let mut input = Input::new("foobar");
    /// assert!(input.get_utf8());
    /// input.set_utf8(false);
    /// assert!(!input.get_utf8());
    /// ```
    #[inline]
    pub fn set_utf8(&mut self, yes: bool) {
        self.utf8 = yes;
    }

    /// Return a borrow of the underlying haystack as a slice of bytes.
    ///
    /// # Example
    ///
    /// ```
    /// use regex_automata::Input;
    ///
    /// let input = Input::new("foobar");
    /// assert_eq!(b"foobar", input.haystack());
    /// ```
    #[inline]
    pub fn haystack(&self) -> &[u8] {
        self.haystack
    }

    /// Return the start position of this search.
    ///
    /// This is a convenience routine for `search.get_span().start()`.
    ///
    /// # Example
    ///
    /// ```
    /// use regex_automata::Input;
    ///
    /// let input = Input::new("foobar");
    /// assert_eq!(0, input.start());
    ///
    /// let input = Input::new("foobar").span(2..4);
    /// assert_eq!(2, input.start());
    /// ```
    #[inline]
    pub fn start(&self) -> usize {
        self.get_span().start
    }

    /// Return the end position of this search.
    ///
    /// This is a convenience routine for `search.get_span().end()`.
    ///
    /// # Example
    ///
    /// ```
    /// use regex_automata::Input;
    ///
    /// let input = Input::new("foobar");
    /// assert_eq!(6, input.end());
    ///
    /// let input = Input::new("foobar").span(2..4);
    /// assert_eq!(4, input.end());
    /// ```
    #[inline]
    pub fn end(&self) -> usize {
        self.get_span().end
    }

    /// Return the span for this search configuration.
    ///
    /// If one was not explicitly set, then the span corresponds to the entire
    /// range of the haystack.
    ///
    /// # Example
    ///
    /// ```
    /// use regex_automata::{Input, Span};
    ///
    /// let input = Input::new("foobar");
    /// assert_eq!(Span { start: 0, end: 6 }, input.get_span());
    /// ```
    #[inline]
    pub fn get_span(&self) -> Span {
        self.span
    }

    /// Return the span as a range for this search configuration.
    ///
    /// If one was not explicitly set, then the span corresponds to the entire
    /// range of the haystack.
    ///
    /// # Example
    ///
    /// ```
    /// use regex_automata::Input;
    ///
    /// let input = Input::new("foobar");
    /// assert_eq!(0..6, input.get_range());
    /// ```
    #[inline]
    pub fn get_range(&self) -> Range<usize> {
        self.get_span().range()
    }

    /// Return the anchored mode for this search configuration.
    ///
    /// If no anchored mode was set, then it defaults to [`Anchored::No`].
    ///
    /// # Example
    ///
    /// ```
    /// use regex_automata::{Anchored, Input, PatternID};
    ///
    /// let mut input = Input::new("foobar");
    /// assert_eq!(Anchored::No, input.get_anchored());
    ///
    /// let pid = PatternID::must(5);
    /// input.set_anchored(Anchored::Pattern(pid));
    /// assert_eq!(Anchored::Pattern(pid), input.get_anchored());
    /// ```
    #[inline]
    pub fn get_anchored(&self) -> Anchored {
        self.anchored
    }

    /// Return the pattern ID for this search configuration, if one was set.
    ///
    /// When no pattern is set, the regex engine should look for matches for
    /// any of the patterns that are in the regex object.
    ///
    /// # Example
    ///
    /// ```
    /// use regex_automata::Input;
    ///
    /// let input = Input::new("foobar");
    /// assert_eq!(None, input.get_pattern());
    /// ```
    #[inline]
    pub fn get_pattern(&self) -> Option<PatternID> {
        self.pattern
    }

    #[inline]
    pub fn get_prefilter(&self) -> Option<&'p dyn Prefilter> {
        self.prefilter
    }

    /// Return whether this search should execute in "earliest" mode.
    ///
    /// # Example
    ///
    /// ```
    /// use regex_automata::Input;
    ///
    /// let input = Input::new("foobar");
    /// assert!(!input.get_earliest());
    /// ```
    #[inline]
    pub fn get_earliest(&self) -> bool {
        self.earliest
    }

    /// Return whether this search should execute in UTF-8 mode.
    ///
    /// # Example
    ///
    /// ```
    /// use regex_automata::Input;
    ///
    /// let input = Input::new("foobar");
    /// assert!(input.get_utf8());
    /// ```
    #[inline]
    pub fn get_utf8(&self) -> bool {
        self.utf8
    }

    /// Return true if and only if this search can never return any other
    /// matches.
    ///
    /// For example, if the start position of this search is greater than the
    /// end position of the search.
    ///
    /// # Example
    ///
    /// ```
    /// use regex_automata::Input;
    ///
    /// let mut input = Input::new("foobar");
    /// assert!(!input.is_done());
    /// input.set_start(6);
    /// assert!(!input.is_done());
    /// input.set_start(7);
    /// assert!(input.is_done());
    /// ```
    #[inline]
    pub fn is_done(&self) -> bool {
        self.get_span().start > self.get_span().end
    }

    /// Returns true if and only if the given offset in this search's haystack
    /// falls on a valid UTF-8 encoded codepoint boundary.
    ///
    /// If the haystack is not valid UTF-8, then the behavior of this routine
    /// is unspecified.
    ///
    /// # Example
    ///
    /// This shows where codepoint bounardies do and don't exist in valid
    /// UTF-8.
    ///
    /// ```
    /// use regex_automata::Input;
    ///
    /// let input = Input::new("☃");
    /// assert!(input.is_char_boundary(0));
    /// assert!(!input.is_char_boundary(1));
    /// assert!(!input.is_char_boundary(2));
    /// assert!(input.is_char_boundary(3));
    /// assert!(!input.is_char_boundary(4));
    /// ```
    #[inline]
    pub fn is_char_boundary(&self, offset: usize) -> bool {
        utf8::is_boundary(self.haystack(), offset)
    }

    /// This skips any empty matches that split a codepoint when this search's
    /// "utf8" option is enabled. The match given should be the initial match
    /// found, and 'find' should be a closure that can execute a regex search.
    ///
    /// We don't export this routine because it could be quite confusing. Folks
    /// might use this to call another regex engine's find routine that already
    /// calls this internally. Plus, its implementation can be written entirely
    /// using existing public APIs.
    ///
    /// N.B. This is written as a non-inlineable cold function that accepts
    /// a pre-existing match because it generally leads to better codegen in
    /// my experience. Namely, we could write a routine that doesn't accept
    /// a pre-existing match and just does the initial search for you. But
    /// doing it this way forcefully separates the hot path from the handling
    /// of pathological cases. That is, one can guard calls to this with
    /// 'm.is_empty()', even though it isn't necessary for correctness.
    #[cold]
    #[inline(never)]
    pub(crate) fn skip_empty_utf8_splits<F>(
        &self,
        mut m: Match,
        mut find: F,
    ) -> Result<Option<Match>, MatchError>
    where
        F: FnMut(&Input<'_, '_>) -> Result<Option<Match>, MatchError>,
    {
        if !self.get_utf8() || !m.is_empty() {
            return Ok(Some(m));
        }
        let mut input = self.clone();
        while m.is_empty() && !input.is_char_boundary(m.end()) {
            input.set_start(input.start().checked_add(1).unwrap());
            m = match find(&input)? {
                None => return Ok(None),
                Some(m) => m,
            };
        }
        Ok(Some(m))
    }
}

impl<'h, 'p> core::fmt::Debug for Input<'h, 'p> {
    fn fmt(&self, f: &mut core::fmt::Formatter) -> core::fmt::Result {
        use crate::util::escape::DebugHaystack;

        f.debug_struct("Input")
            .field("haystack", &DebugHaystack(self.haystack()))
            .field("span", &self.span)
            .field("prefilter", &self.prefilter)
            .field("pattern", &self.pattern)
            .field("earliest", &self.earliest)
            .field("utf8", &self.utf8)
            .finish()
    }
}

/// A representation of a span reported by a regex engine.
///
/// A span corresponds to the starting and ending _byte offsets_ of a
/// contiguous region of bytes. The starting offset is inclusive while the
/// ending offset is exclusive. That is, a span is a half-open interval.
///
/// A span is used to report the offsets of a match, but it is also used to
/// convey which region of a haystack should be searched via routines like
/// [`Input::span`].
///
/// This is basically equivalent to a `std::ops::Range<usize>`, except this
/// type implements `Copy` which makes it more ergonomic to use in the context
/// of this crate. Like a range, this implements `Index` for `[u8]` and `str`,
/// and `IndexMut` for `[u8]`. For convenience, this also impls `From<Range>`,
/// which means things like `Span::from(5..10)` work.
#[derive(Clone, Copy, Eq, Hash, PartialEq)]
pub struct Span {
    /// The start offset of the span, inclusive.
    pub start: usize,
    /// The end offset of the span, exclusive.
    pub end: usize,
}

impl Span {
    /// Returns this span as a range.
    #[inline]
    pub fn range(&self) -> Range<usize> {
        Range::from(*self)
    }

    /// Returns true when this span is empty. That is, when `start >= end`.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.start >= self.end
    }

    /// Returns true when the given offset is contained within this span.
    ///
    /// Note that an empty span contains no offsets and will always return
    /// false.
    #[inline]
    pub fn contains(&self, offset: usize) -> bool {
        !self.is_empty() && self.start <= offset && offset <= self.end
    }
}

impl core::fmt::Debug for Span {
    fn fmt(&self, f: &mut core::fmt::Formatter) -> core::fmt::Result {
        write!(f, "{}..{}", self.start, self.end)
    }
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

impl From<Range<usize>> for Span {
    #[inline]
    fn from(range: Range<usize>) -> Span {
        Span { start: range.start, end: range.end }
    }
}

impl From<Span> for Range<usize> {
    #[inline]
    fn from(span: Span) -> Range<usize> {
        Range { start: span.start, end: span.end }
    }
}

impl PartialEq<Range<usize>> for Span {
    #[inline]
    fn eq(&self, range: &Range<usize>) -> bool {
        self.start == range.start && self.end == range.end
    }
}

impl PartialEq<Span> for Range<usize> {
    #[inline]
    fn eq(&self, span: &Span) -> bool {
        self.start == span.start && self.end == span.end
    }
}

/// A representation of "half" of a match reported by a DFA.
///
/// This is called a "half" match because it only includes the end location (or
/// start location for a reverse search) of a match. This corresponds to the
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
    pattern: PatternID,
    /// The offset of the match.
    ///
    /// For forward searches, the offset is exclusive. For reverse searches,
    /// the offset is inclusive.
    offset: usize,
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

/// A representation of a match reported by a regex engine.
///
/// A match has two essential pieces of information: the [`PatternID`] that
/// matches, and the [`Span`] of the match in a haystack.
///
/// The pattern is identified by an ID, which corresponds to its position
/// (starting from `0`) relative to other patterns used to construct the
/// corresponding regex engine. If only a single pattern is provided, then all
/// matches are guaranteed to have a pattern ID of `0`.
///
/// Every match reported by a regex engine guarantees that its span has its
/// start offset as less than or equal to its end offset.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct Match {
    /// The pattern ID.
    pattern: PatternID,
    /// The underlying match span.
    span: Span,
}

impl Match {
    /// Create a new match from a pattern ID and a span.
    ///
    /// This constructor is generic over how a span is provided. While
    /// a [`Span`] may be given directly, one may also provide a
    /// `std::ops::Range<usize>`.
    ///
    /// # Panics
    ///
    /// This panics if `end < start`.
    ///
    /// # Example
    ///
    /// This shows how to create a match for the first pattern in a regex
    /// object using convenient range syntax.
    ///
    /// ```
    /// use regex_automata::{Match, PatternID};
    ///
    /// let m = Match::new(PatternID::ZERO, 5..10);
    /// assert_eq!(0, m.pattern().as_usize());
    /// assert_eq!(5, m.start());
    /// assert_eq!(10, m.end());
    /// ```
    #[inline]
    pub fn new<S: Into<Span>>(pattern: PatternID, span: S) -> Match {
        let span = span.into();
        assert!(span.start <= span.end, "invalid match span");
        Match { pattern, span }
    }

    /// Create a new match from a pattern ID and a byte offset span.
    ///
    /// This constructor is generic over how a span is provided. While
    /// a [`Span`] may be given directly, one may also provide a
    /// `std::ops::Range<usize>`.
    ///
    /// This is like [`Match::new`], but accepts a `usize` instead of a
    /// [`PatternID`]. This panics if the given `usize` is not representable
    /// as a `PatternID`.
    ///
    /// # Panics
    ///
    /// This panics if `end < start` or if `pattern > PatternID::MAX`.
    ///
    /// # Example
    ///
    /// This shows how to create a match for the third pattern in a regex
    /// object using convenient range syntax.
    ///
    /// ```
    /// use regex_automata::Match;
    ///
    /// let m = Match::must(3, 5..10);
    /// assert_eq!(3, m.pattern().as_usize());
    /// assert_eq!(5, m.start());
    /// assert_eq!(10, m.end());
    /// ```
    #[inline]
    pub fn must<S: Into<Span>>(pattern: usize, span: S) -> Match {
        Match::new(PatternID::must(pattern), span)
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
    ///
    /// This is a convenience routine for `Match::span().start`.
    #[inline]
    pub fn start(&self) -> usize {
        self.span().start
    }

    /// The ending position of the match.
    ///
    /// This is a convenience routine for `Match::span().end`.
    #[inline]
    pub fn end(&self) -> usize {
        self.span().end
    }

    /// Returns the match span as a range.
    ///
    /// This is a convenience routine for `Match::span().range()`.
    #[inline]
    pub fn range(&self) -> core::ops::Range<usize> {
        self.span().range()
    }

    /// Returns the span for this match.
    #[inline]
    pub fn span(&self) -> Span {
        self.span
    }

    /// Returns true when the span in this match is empty.
    ///
    /// An empty match can only be returned when the regex itself can match
    /// the empty string.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.span().is_empty()
    }
}

/// A set of `PatternID`s.
///
/// A set of pattern identifiers is useful for recording which patterns have
/// matched a particular haystack. A pattern set _only_ includes pattern
/// identifiers. It does not include offset information.
///
/// # Example
///
/// This shows basic usage of a set.
///
/// ```
/// use regex_automata::{PatternID, PatternSet};
///
/// let pid1 = PatternID::must(5);
/// let pid2 = PatternID::must(8);
/// // Create a new empty set.
/// let mut set = PatternSet::new(10);
/// // Insert pattern IDs.
/// set.insert(pid1);
/// set.insert(pid2);
/// // Test membership.
/// assert!(set.contains(pid1));
/// assert!(set.contains(pid2));
/// // Get all members.
/// assert_eq!(
///     vec![5, 8],
///     set.iter().map(|p| p.as_usize()).collect::<Vec<usize>>(),
/// );
/// // Clear the set.
/// set.clear();
/// // Test that it is indeed empty.
/// assert!(set.is_empty());
/// ```
#[cfg(feature = "alloc")]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PatternSet {
    /// The number of patterns set to 'true' in this set.
    len: usize,
    /// A map from PatternID to boolean of whether a pattern matches or not.
    ///
    /// This should probably be a bitset, but it's probably unlikely to matter
    /// much in practice.
    ///
    /// The main downside of this representation (and similarly for a bitset)
    /// is that iteration scales with the capacity of the set instead of
    /// the length of the set. This doesn't seem likely to be a problem in
    /// practice.
    ///
    /// Another alternative is to just use a 'SparseSet' for this. It does use
    /// more memory (quite a bit more), but that seems fine I think compared
    /// to the memory being used by the regex engine. The real hiccup with
    /// it is that it yields pattern IDs in the order they were inserted.
    /// Which is actually kind of nice, but at the time of writing, pattern
    /// IDs are yielded in ascending order in the regex crate RegexSet API.
    /// If we did change to 'SparseSet', we could provide an additional
    /// 'iter_match_order' iterator, but keep the ascending order one for
    /// compatibility.
    which: alloc::boxed::Box<[bool]>,
}

#[cfg(feature = "alloc")]
impl PatternSet {
    /// Create a new set of pattern identifiers with the given capacity.
    ///
    /// The given capacity typically corresponds to (at least) the number of
    /// patterns in a compiled regex object.
    ///
    /// # Panics
    ///
    /// This panics if the given capacity exceeds [`PatternID::LIMIT`].
    pub fn new(capacity: usize) -> PatternSet {
        assert!(
            capacity <= PatternID::LIMIT,
            "pattern set capacity exceeds limit of {}",
            PatternID::LIMIT,
        );
        PatternSet {
            len: 0,
            which: alloc::vec![false; capacity].into_boxed_slice(),
        }
    }

    /// Clear this set such that it contains no pattern IDs.
    pub fn clear(&mut self) {
        self.len = 0;
        for matched in self.which.iter_mut() {
            *matched = false;
        }
    }

    /// Return true if and only if the given pattern identifier is in this set.
    ///
    /// # Panics
    ///
    /// This panics if `pid` exceeds the capacity of this set.
    pub fn contains(&self, pid: PatternID) -> bool {
        self.which[pid]
    }

    /// Insert the given pattern identifier into this set.
    ///
    /// If the pattern identifier is already in this set, then this is a no-op.
    ///
    /// # Panics
    ///
    /// This panics if `pid` exceeds the capacity of this set.
    pub fn insert(&mut self, pid: PatternID) {
        if self.which[pid] {
            return;
        }
        self.len += 1;
        self.which[pid] = true;
    }

    /*
    // This is currently commented out because it is unused and it is unclear
    // whether it's useful or not. What's the harm in having it? When, if
    // we ever wanted to change our representation to a 'SparseSet', then
    // supporting this method would be a bit tricky. So in order to keep some
    // API evolution flexibility, we leave it out for now.

    /// Remove the given pattern identifier from this set.
    ///
    /// If the pattern identifier was not previously in this set, then this
    /// does not change the set and returns `false`.
    ///
    /// # Panics
    ///
    /// This panics if `pid` exceeds the capacity of this set.
    pub fn remove(&mut self, pid: PatternID) -> bool {
        if !self.which[pid] {
            return false;
        }
        self.len -= 1;
        self.which[pid] = false;
        true
    }
    */

    /// Return true if and only if this set has no pattern identifiers in it.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Return true if and only if this set has the maximum number of pattern
    /// identifiers in the set. This occurs precisely when `PatternSet::len()
    /// == PatternSet::capacity()`.
    ///
    /// This particular property is useful to test because it may allow one to
    /// stop a search earlier than you might otherwise. Namely, if a search is
    /// only reporting which patterns match a haystack and if you know all of
    /// the patterns match at a given point, then there's no new information
    /// that can be learned by continuing the search. (Because a pattern set
    /// does not keep track of offset information.)
    pub fn is_full(&self) -> bool {
        self.len() == self.capacity()
    }

    /// Returns the total number of pattern identifiers in this set.
    pub fn len(&self) -> usize {
        self.len
    }

    /// Returns the total number of pattern identifiers that may be stored
    /// in this set.
    ///
    /// This is guaranteed to be less than or equal to [`PatternID::LIMIT`].
    ///
    /// Typically, the capacity of a pattern set matches the number of patterns
    /// in a regex object with which you are searching.
    pub fn capacity(&self) -> usize {
        self.which.len()
    }

    /// Returns an iterator over all pattern identifiers in this set.
    ///
    /// The iterator yields pattern identifiers in ascending order, starting
    /// at zero.
    pub fn iter(&self) -> PatternSetIter<'_> {
        PatternSetIter { it: self.which.iter().enumerate() }
    }
}

/// An iterator over all pattern identifiers in a [`PatternSet`].
///
/// The lifetime parameter `'a` refers to the lifetime of the pattern set being
/// iterated over.
///
/// This iterator is created by the [`PatternSet::iter`] method.
#[cfg(feature = "alloc")]
#[derive(Debug)]
pub struct PatternSetIter<'a> {
    it: core::iter::Enumerate<core::slice::Iter<'a, bool>>,
}

#[cfg(feature = "alloc")]
impl<'a> Iterator for PatternSetIter<'a> {
    type Item = PatternID;

    fn next(&mut self) -> Option<PatternID> {
        while let Some((index, &yes)) = self.it.next() {
            if yes {
                // Only valid 'PatternID' values can be inserted into the set
                // and construction of the set panics if the capacity would
                // permit storing invalid pattern IDs. Thus, 'yes' is only true
                // precisely when 'index' corresponds to a valid 'PatternID'.
                return Some(PatternID::new_unchecked(index));
            }
        }
        None
    }
}

/// The type of anchored search to perform.
///
/// This is *almost* a boolean option. That is, you can either do an unanchored
/// search for any pattern in a regex, or you can do an anchored search for any
/// pattern in a regex.
///
/// A third option exists that, assuming the regex engine supports it, permits
/// you to do an anchored search for a specific pattern.
///
/// If a regex engine does not support the anchored mode selected, then the
/// regex engine will panic. While any non-trivial regex engine should support
/// at least one of the available anchored modes, there is no singular mode
/// that is guaranteed to be universally supported. Some regex engines might
/// only support unanchored searches (DFAs compiled without anchored starting
/// states) and some regex engines might only support anchored searches (like
/// the one-pass DFA).
///
/// Note that there is no way to run an unanchored search for a specific
/// pattern. If you need that, you'll need to build separate regexes for each
/// pattern.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Anchored {
    /// Run an unanchored search. This means a match may occur anywhere at or
    /// after the start position of the search.
    ///
    /// This search can return a match for any pattern in the regex.
    No,
    /// Run an anchored search. This means that a match must begin at the
    /// start position of the search.
    ///
    /// This search can return a match for any pattern in the regex.
    Yes,
    /// Run an anchored search for a specific pattern. This means that a match
    /// must be for the given pattern and must begin at the start position of
    /// the search.
    Pattern(PatternID),
}

impl Anchored {
    /// Returns true if and only if this anchor mode corresponds to any kind of
    /// anchored search.
    ///
    /// # Example
    ///
    /// This examples shows that both `Anchored::Yes` and `Anchored::Pattern`
    /// are considered anchored searches.
    ///
    /// ```
    /// use regex_automata::{Anchored, PatternID};
    ///
    /// assert!(!Anchored::No.is_anchored());
    /// assert!(Anchored::Yes.is_anchored());
    /// assert!(Anchored::Pattern(PatternID::ZERO).is_anchored());
    /// ```
    pub fn is_anchored(&self) -> bool {
        matches!(*self, Anchored::Yes | Anchored::Pattern(_))
    }
}

/// The kind of match semantics to use for a regex pattern.
///
/// The default match kind is `LeftmostFirst`.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MatchKind {
    /// Report all possible matches.
    All,
    /// Report only the leftmost matches. When multiple leftmost matches exist,
    /// report the match corresponding to the part of the regex that appears
    /// first in the syntax.
    LeftmostFirst,
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

/// An error indicating that a search stopped before reporting whether a
/// match exists or not.
///
/// To be very clear, this error type implies that one cannot assume that no
/// matches occur, since the search stopped before completing. That is, if
/// you're looking for information about where a search determined that no
/// match can occur, then this error type does *not* give you that. (Indeed, at
/// the time of writing, if you need such a thing, you have to write your own
/// search routine.)
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
/// possible for callers to configure regex engines to never give up on a
/// search, and thus never return an error. Indeed, the default configuration
/// for every regex engine in this crate is such that they will never stop
/// searching early. Therefore, the only way to get a match error is if the
/// regex engine is explicitly configured to do so. Options that enable this
/// behavior document the new error conditions they imply.
///
/// For example, the dense and sparse regex engines in the `dfa` sub-module
/// will only report `MatchError::quit` if instructed by either
/// [enabling Unicode word boundaries](crate::dfa::dense::Config::unicode_word_boundary)
/// or by
/// [explicitly specifying one or more quit bytes](crate::dfa::dense::Config::quit).
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct MatchError(
    #[cfg(feature = "alloc")] alloc::boxed::Box<MatchErrorKind>,
    #[cfg(not(feature = "alloc"))] MatchErrorKind,
);

impl MatchError {
    /// Create a new error value with the given kind.
    ///
    /// This is a more verbose version of the kind-specific constructors,
    /// e.g., `MatchError::quit`.
    pub fn new(kind: MatchErrorKind) -> MatchError {
        #[cfg(feature = "alloc")]
        {
            MatchError(alloc::boxed::Box::new(kind))
        }
        #[cfg(not(feature = "alloc"))]
        {
            MatchError(kind)
        }
    }

    /// Returns a reference to the underlying error kind.
    pub fn kind(&self) -> &MatchErrorKind {
        &self.0
    }

    /// Create a new "quit" error. The given `byte` corresponds to the value
    /// that tripped a search's quit condition, and `offset` corresponds to the
    /// location in the haystack at which the search quit.
    ///
    /// This is the same as calling `MatchError::new` with a
    /// [`MatchErrorKind::Quit`] kind.
    pub fn quit(byte: u8, offset: usize) -> MatchError {
        MatchError::new(MatchErrorKind::Quit { byte, offset })
    }

    /// Create a new "gave up" error. The given `offset` corresponds to the
    /// location in the haystack at which the search gave up.
    ///
    /// This is the same as calling `MatchError::new` with a
    /// [`MatchErrorKind::GaveUp`] kind.
    pub fn gave_up(offset: usize) -> MatchError {
        MatchError::new(MatchErrorKind::GaveUp { offset })
    }

    /// Create a new "haystack too long" error. The given `len` corresponds to
    /// the length of the haystack that was problematic.
    ///
    /// This is the same as calling `MatchError::new` with a
    /// [`MatchErrorKind::HaystackTooLong`] kind.
    pub fn haystack_too_long(len: usize) -> MatchError {
        MatchError::new(MatchErrorKind::HaystackTooLong { len })
    }
}

/// The underlying kind of a [`MatchError`].
///
/// This is a **non-exhaustive** enum. That means new variants may be added in
/// a semver-compatible release.
#[non_exhaustive]
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub enum MatchErrorKind {
    // A previous iteration of this error type specifically encoded "did not
    // match" as a None variant. Instead of fallible regex searches returning
    // Result<Option<Match>, MatchError>, they would return the simpler
    // Result<Match, MatchError>. The appeal of this is the simpler return
    // type. The inherent problem, though, is that "did not match" is not
    // actually an error case. It's an expected behavior of a regex search
    // and is therefore typically handled differently than a real error that
    // prevents one from knowing whether a match occurs at all. Thus, the
    // simpler return type often requires explicit case analysis to deal with
    // the None variant. More to the point, the iteration protocol for the
    // simpler return type was quite awkward, because the iteration protocol
    // really wants an Option<Match> and cannot deal with the None variant
    // inside of the error type.
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
    /// This error occurs if the haystack given to the regex engine was too
    /// long to be searched. This occurs, for example, with regex engines
    /// like the bounded backtracker that have a configurable fixed amount of
    /// capacity that is tied to the length of the haystack. Anything beyond
    /// that configured limit will result in an error at search time.
    HaystackTooLong {
        /// The length of the haystack that exceeded the limit.
        len: usize,
    },
}

#[cfg(feature = "std")]
impl std::error::Error for MatchError {}

impl core::fmt::Display for MatchError {
    fn fmt(&self, f: &mut core::fmt::Formatter) -> core::fmt::Result {
        match *self.kind() {
            MatchErrorKind::Quit { byte, offset } => write!(
                f,
                "quit search after observing byte {:?} at offset {}",
                DebugByte(byte),
                offset,
            ),
            MatchErrorKind::GaveUp { offset } => {
                write!(f, "gave up searching at offset {}", offset)
            }
            MatchErrorKind::HaystackTooLong { len } => {
                write!(f, "haystack of length {} is too long", len)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // We test that our 'MatchError' type is the size we expect. This isn't an
    // API guarantee, but if the size increases, we really want to make sure we
    // decide to do that intentionally. So this should be a speed bump. And in
    // general, we should not increase the size without a very good reason.
    //
    // Why? Because low level search APIs return Result<.., MatchError>. When
    // MatchError gets bigger, so to does the Result type.
    //
    // Now, when 'alloc' is enabled, we do box the error, which de-emphasizes
    // the importance of keeping a small error type. But without 'alloc', we
    // still want things to be small.
    #[test]
    fn match_error_size() {
        let err_size = if cfg!(feature = "alloc") {
            core::mem::size_of::<MatchError>()
        } else {
            2 * core::mem::size_of::<MatchError>()
        };
        assert_eq!(core::mem::size_of::<usize>(), err_size);
    }
}
