pub extern crate bstr;

use std::borrow::Borrow;
use std::collections::HashSet;
use std::fs;
use std::path::Path;

use anyhow::{bail, Context, Result};
use bstr::{BStr, BString, ByteSlice, ByteVec};
use serde::Deserialize;

mod escape;

const ENV_REGEX_TEST: &str = "REGEX_TEST";
const ENV_REGEX_TEST_VERBOSE: &str = "REGEX_TEST_VERBOSE";

/// A collection of regex tests.
#[derive(Clone, Debug, Deserialize)]
pub struct RegexTests {
    tests: Vec<RegexTest>,
    #[serde(skip)]
    seen: HashSet<String>,
}

/// A regex test describes the inputs and expected outputs of a regex match.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RegexTest {
    #[serde(skip)]
    group: String,
    #[serde(default)]
    name: String,
    #[serde(skip)]
    full_name: String,
    regex: Option<BString>,
    regexes: Option<Vec<BString>>,
    input: BString,
    matches: Vec<Captures>,
    // captures: Option<Vec<Captures>>,
    match_limit: Option<usize>,
    #[serde(default = "default_true")]
    compiles: bool,
    #[serde(default)]
    anchored: bool,
    #[serde(default)]
    case_insensitive: bool,
    #[serde(default)]
    unescape: bool,
    #[serde(default = "default_true")]
    unicode: bool,
    #[serde(default = "default_true")]
    utf8: bool,
    #[serde(default)]
    match_kind: MatchKind,
    #[serde(default)]
    search_kind: SearchKind,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "kebab-case")]
pub enum MatchKind {
    All,
    LeftmostFirst,
    LeftmostLongest,
}

impl Default for MatchKind {
    fn default() -> MatchKind {
        MatchKind::LeftmostFirst
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "kebab-case")]
pub enum SearchKind {
    Earliest,
    Leftmost,
    Overlapping,
}

impl Default for SearchKind {
    fn default() -> SearchKind {
        SearchKind::Leftmost
    }
}

/// Span represents a single match span, from start to end, represented via
/// byte offsets.
#[derive(Clone, Copy, Eq, PartialEq)]
pub struct Span {
    /// The starting byte offset of the match.
    pub start: usize,
    /// The ending byte offset of the match.
    pub end: usize,
}

impl std::fmt::Debug for Span {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(f, "({:?}, {:?})", self.start, self.end)
    }
}

/// Match represents a single match span, from start to end, represented via
/// byte offsets.
#[derive(Clone, Copy, Eq, PartialEq)]
pub struct Match {
    pub id: usize,
    pub span: Span,
}

impl std::fmt::Debug for Match {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(f, "({:?}: {:?})", self.id, self.span)
    }
}

/// Captures represents a single group of captured matches from a regex search.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(from = "CapturesFormat")]
pub struct Captures {
    /// The ID of the regex that matched.
    ///
    /// The ID is the index of the regex provided to the regex compiler,
    /// starting from `0`. In the case of a single regex search, the only
    /// possible ID is `0`.
    id: usize,
    /// The capturing groups that matched, along with the match offsets for
    /// each. The first group should always be non-None, as it corresponds to
    /// the overall match.
    ///
    /// This should either have length 1 (when not capturing group offsets are
    /// included in the tes tresult) or it should have length equal to the
    /// number of capturing groups in the regex pattern.
    groups: Vec<Option<Span>>,
}

impl RegexTests {
    /// Create a new empty collection of glob tests.
    pub fn new() -> RegexTests {
        RegexTests { tests: vec![], seen: HashSet::new() }
    }

    /// Loads all of the tests in the given TOML file. The group name assigned
    /// to each test is the stem of the file name. For example, if one loads
    /// `foo/bar.toml`, then the group name for each test will be `bar`.
    pub fn load<P: AsRef<Path>>(&mut self, path: P) -> Result<()> {
        let path = path.as_ref();
        let data = fs::read(path)
            .with_context(|| format!("failed to read {}", path.display()))?;
        let group_name = path
            .file_stem()
            .with_context(|| {
                format!("failed to get file name of {}", path.display())
            })?
            .to_str()
            .with_context(|| {
                format!("invalid UTF-8 found in {}", path.display())
            })?;
        self.load_slice(&group_name, &data)
            .with_context(|| format!("error loading {}", path.display()))?;
        Ok(())
    }

    /// Load all of the TOML encoded tests in `data` into this collection.
    /// The given group name is assigned to all loaded tests.
    pub fn load_slice(&mut self, group_name: &str, data: &[u8]) -> Result<()> {
        let mut index = 1;
        let mut tests: RegexTests =
            toml::from_slice(&data).context("error decoding TOML")?;
        for t in &mut tests.tests {
            t.group = group_name.to_string();
            if t.name.is_empty() {
                t.name = format!("{}", index);
                index += 1;
            }
            t.full_name = format!("{}/{}", t.group, t.name);
            if t.unescape {
                t.input = BString::from(crate::escape::unescape(&t.input));
            }

            t.validate().with_context(|| {
                format!("error loading test '{}'", t.full_name())
            })?;
            if self.seen.contains(t.full_name()) {
                bail!("found duplicate tests for name '{}'", t.full_name());
            }
            self.seen.insert(t.full_name().to_string());
        }
        self.tests.extend(tests.tests);
        Ok(())
    }

    /// Return an iterator over all regex tests that have been loaded. The
    /// order of the iterator corresponds to the order in which the tests were
    /// loaded.
    pub fn iter(&self) -> RegexTestsIter {
        RegexTestsIter { it: self.tests.iter() }
    }
}

impl Captures {
    /// Create a new set of captures for a single match of a regex.
    ///
    /// The iterator should provide items for every capturing group in the
    /// regex, including the 0th capturing group corresponding to the entire
    /// match. If a capturing group did not participate in the match, then a
    /// `None` value should be used. (Consequently, the 0th capturing group
    /// should never be `None`.)
    pub fn new<I: IntoIterator<Item = Option<Span>>>(
        id: usize,
        it: I,
    ) -> Captures {
        Captures { id, groups: it.into_iter().collect() }
    }

    pub fn id(&self) -> usize {
        self.id
    }

    pub fn groups(&self) -> &[Option<Span>] {
        &self.groups
    }

    pub fn len(&self) -> usize {
        self.groups.len()
    }

    pub fn to_match(&self) -> Match {
        Match { id: self.id(), span: self.to_span() }
    }

    pub fn to_span(&self) -> Span {
        // This is OK because a Captures value must always have at least one
        // group where the first group always corresponds to match offsets.
        self.groups[0].unwrap()
    }
}

impl RegexTest {
    fn test(&self, regex: &mut CompiledRegex) -> Vec<TestResult> {
        match regex.match_regex {
            None => vec![TestResult::skip()],
            Some(ref mut match_regex) => match_regex(self),
        }
    }

    fn validate(&self) -> Result<()> {
        if self.regex.is_none() && self.regexes.is_none() {
            bail!("one of 'regex' or 'regexes' must be present");
        } else if self.regex.is_some() && self.regexes.is_some() {
            bail!("only one of 'regex' or 'regexes' can be present");
        }
        Ok(())
    }

    /// Return the group name of this test.
    ///
    /// Usually the group name corresponds to a collection of related tests.
    pub fn group(&self) -> &str {
        &self.group
    }

    /// The name of this test.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The full name of this test, which is formed by joining the group
    /// name with the test name via a `/`.
    pub fn full_name(&self) -> &str {
        &self.full_name
    }

    /// Return all of the regexes that should be matched for this test. This
    /// slice is guaranteed to be non-empty.
    pub fn regexes(&self) -> &[BString] {
        if let Some(ref regex) = self.regex {
            std::slice::from_ref(regex)
        } else {
            self.regexes.as_ref().unwrap()
        }
    }

    /// Return the text on which the regex should be matched.
    pub fn input(&self) -> &BStr {
        self.input.as_bstr()
    }

    /// Return the match semantics required by this test.
    pub fn match_kind(&self) -> MatchKind {
        self.match_kind
    }

    /// Return the search semantics required by this test.
    pub fn search_kind(&self) -> SearchKind {
        self.search_kind
    }

    /// Returns true if and only if this test expects at least one of the
    /// regexes to match the input.
    pub fn is_match(&self) -> bool {
        !self.matches.is_empty()
    }

    /// Returns a slice of regexes that are expected to match the input. The
    /// slice is empty if no match is expected to occur. The indices returned
    /// here correspond to the indices of the slice returned by the `regexes`
    /// method.
    pub fn which_matches(&self) -> Vec<usize> {
        let mut seen = HashSet::new();
        let mut ids = vec![];
        for cap in &self.matches {
            if !seen.contains(&cap.id) {
                seen.insert(cap.id);
                ids.push(cap.id);
            }
        }
        ids
    }

    /// If this test expects all non-overlapping matches (whether capturing
    /// or not), then they are returned. Otherwise, `None` is returned.
    pub fn matches(&self) -> Vec<Match> {
        let mut matches = vec![];
        for cap in &self.matches {
            matches.push(cap.to_match());
        }
        matches
    }

    /// If this test expects all non-overlapping matches as capturing groups,
    /// then they are returned. Otherwise, `None` is returned.
    pub fn captures(&self) -> Vec<Captures> {
        self.matches.clone()
    }

    /// Returns the limit on the number of matches that should be reported,
    /// if specified in the test.
    ///
    /// This is useful for tests that only want to check for the first
    /// match. In which case, the match limit is set to 1.
    pub fn match_limit(&self) -> Option<usize> {
        self.match_limit
    }

    /// Returns true if the regex(es) in this test are expected to compile.
    pub fn compiles(&self) -> bool {
        self.compiles
    }

    /// Whether the regex should only match at the beginning of text or not.
    pub fn anchored(&self) -> bool {
        self.anchored
    }

    /// Returns true if regex matching should be performed without regard to
    /// case.
    pub fn case_insensitive(&self) -> bool {
        self.case_insensitive
    }

    /// Returns true if regex matching should have Unicode mode enabled.
    ///
    /// This is enabled by default.
    pub fn unicode(&self) -> bool {
        self.unicode
    }

    /// Returns true if regex matching should exclusively match valid UTF-8.
    /// When this is disabled, matching on arbitrary bytes is permitted.
    ///
    /// When this is enabled, all regex match substrings should be entirely
    /// valid UTF-8. While parts of the haystack the regex searches through
    /// may not be valid UTF-8, only the portions that are valid UTF-8 may be
    /// reported in match spans.
    ///
    /// This is enabled by default.
    pub fn utf8(&self) -> bool {
        self.utf8
    }
}

/// The result of compiling a regex.
///
/// In many implementations, the act of matching a regex can be separated from
/// the act of compiling a regex. A `CompiledRegex` represents a regex that has
/// been compiled and is ready to be used for matching.
pub struct CompiledRegex {
    match_regex: Option<Box<dyn FnMut(&RegexTest) -> Vec<TestResult>>>,
}

impl CompiledRegex {
    /// Provide a closure that represents the compiled regex and executes a
    /// regex match on any `RegexTest`. The `RegexTest` given to the closure
    /// provided is the exact same `RegexTest` that is used to compile this
    /// regex.
    pub fn compiled<F: FnMut(&RegexTest) -> Vec<TestResult> + 'static>(
        match_regex: F,
    ) -> CompiledRegex {
        CompiledRegex { match_regex: Some(Box::new(match_regex)) }
    }

    /// Indicate that tests on this regex should be skipped. This typically
    /// occurs if the `RegexTest` requires something that an implementation
    /// does not support.
    pub fn skip() -> CompiledRegex {
        CompiledRegex { match_regex: None }
    }
}

impl std::fmt::Debug for CompiledRegex {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        let status = match self.match_regex {
            None => "Skip",
            Some(_) => "Run(...)",
        };
        f.debug_struct("CompiledRegex").field("match_regex", &status).finish()
    }
}

/// The result of executing a single regex match.
///
/// When using the test runner, callers must provide a closure that takes
/// a `RegexTest` and returns a `TestResult`. The `TestResult` is meant to
/// capture the results of matching the input against the regex specified by
/// the `RegexTest`.
#[derive(Debug, Clone)]
pub struct TestResult {
    name: String,
    kind: TestResultKind,
}

#[derive(Debug, Clone)]
enum TestResultKind {
    Match(bool),
    Which(Vec<usize>),
    StartEnd(Vec<Match>),
    Captures(Vec<Captures>),
    Skip,
    /// Occurs when no test result is available. e.g., A regex failed to
    /// compile or something panicked.
    None,
}

impl TestResult {
    /// Create a test result that indicates a match.
    pub fn matched() -> TestResult {
        TestResult { name: "".to_string(), kind: TestResultKind::Match(true) }
    }

    /// Create a test result that indicates the glob did not match.
    pub fn no_match() -> TestResult {
        TestResult { name: "".to_string(), kind: TestResultKind::Match(false) }
    }

    /// Create a test result that indicates which out of possibly many globs
    /// matched the input. If `which` is empty, then this is equivalent to
    /// `TestResult::no_match()`.
    pub fn which(which: Vec<usize>) -> TestResult {
        TestResult { name: "".to_string(), kind: TestResultKind::Which(which) }
    }

    /// Create a test result containing a sequence of all matches in the
    /// test's input string.
    pub fn matches<I: IntoIterator<Item = Match>>(it: I) -> TestResult {
        TestResult {
            name: "".to_string(),
            kind: TestResultKind::StartEnd(it.into_iter().collect()),
        }
    }

    /// Create a test result containing a sequence of all capturing matches
    /// in the test's input string.
    pub fn captures<I: IntoIterator<Item = Captures>>(it: I) -> TestResult {
        TestResult {
            name: "".to_string(),
            kind: TestResultKind::Captures(it.into_iter().collect()),
        }
    }

    /// Indicate that this test should be skipped. It will not be counted as
    /// a failure.
    pub fn skip() -> TestResult {
        TestResult { name: "".to_string(), kind: TestResultKind::Skip }
    }

    /// Indicate that this test has no results.
    pub fn none() -> TestResult {
        TestResult { name: "".to_string(), kind: TestResultKind::None }
    }

    /// Give a name to this test result. This will be included in the output
    /// if the test fails.
    pub fn name(mut self, name: &str) -> TestResult {
        self.name = name.to_string();
        self
    }
}

/// A runner for executing regex tests.
///
/// This runner is intended to be used within a Rust unit test, marked with the
/// `#[test]` attribute.
///
/// A test runner is responsible for running tests against a regex
/// implementation. It contains logic for skipping tests and collects test
/// results. Typical usage corresponds to calling `test_all` on an iterator
/// of `RegexTest`s, and then calling `assert` once done. If any tests failed,
/// then `assert` will panic with an error message containing all test
/// failures. `assert` must be called before the test completes.
///
/// ### Skipping tests
///
/// If the `REGEX_TEST` environment variable is set, then it may contain
/// a comma separated list of substrings. Each substring corresponds to a
/// whitelisted item, unless it starts with a `-`, in which case it corresponds
/// to a blacklisted item.
///
/// If there are any whitelist substring, then a test's full name must contain
/// at least one of the whitelist substrings in order to be run. If there are
/// no whitelist substrings, then a test is run only when it does not match any
/// blacklist substrings.
///
/// The last substring that a test name matches takes precedent.
///
/// Callers may also specify explicit whitelist or blacklist substrings using
/// the corresponding methods on this type.
///
/// Whitelist and blacklist substrings are matched on the full name of each
/// test, which typically looks like `base_file_stem/test_name`.
#[derive(Debug)]
pub struct TestRunner {
    include: Vec<IncludePattern>,
    results: RegexTestResults,
}

#[derive(Debug)]
struct IncludePattern {
    blacklist: bool,
    substring: BString,
}

impl TestRunner {
    /// Create a new runner for executing tests.
    ///
    /// The test runner maintains a full list of tests that have succeeded,
    /// failed or been skipped. Moreover, the test runner may control which
    /// tests get run via its whitelist and blacklist.
    ///
    /// If the `REGEX_TEST` environment variable is set, then it may contain
    /// a comma separated list of substrings. Each substring corresponds to
    /// a whitelisted item, unless it starts with a `-`, in which case it
    /// corresponds to a blacklisted item.
    ///
    /// If there are any whitelisted substrings, then a test's full name must
    /// contain at least one of the whitelist substrings in order to be run. If
    /// there are no whitelisted substrings, then a test is run only when it
    /// does not match any blacklisted substrings.
    ///
    /// The last substring that a test name matches takes precedent.
    ///
    /// If there was a problem reading the environment variable, then an error
    /// is returned.
    pub fn new() -> Result<TestRunner> {
        let mut runner =
            TestRunner { include: vec![], results: RegexTestResults::new() };
        for mut substring in read_env(ENV_REGEX_TEST)?.split(",") {
            substring = substring.trim();
            if substring.is_empty() {
                continue;
            }
            if substring.starts_with("-") {
                runner.blacklist(&substring[1..]);
            } else {
                runner.whitelist(substring);
            }
        }
        Ok(runner)
    }

    /// Assert that all tests run have either passed or have been skipped.
    ///
    /// If any tests have failed, then a panic occurs with a report of all
    /// failures.
    ///
    /// If `REGEX_TEST_VERBOSE` is set to `1`, then a longer report of tests
    /// that passed, failed or skipped is printed.
    pub fn assert(&mut self) {
        self.results.assert();
    }

    /// Whitelist the given substring.
    pub fn whitelist(&mut self, substring: &str) -> &mut TestRunner {
        self.include.push(IncludePattern {
            blacklist: false,
            substring: BString::from(substring),
        });
        self
    }

    /// Blacklist the given substring.
    ///
    /// A blacklisted test is never run, unless a whitelisted substring added
    /// after the blacklisted substring matches it.
    pub fn blacklist(&mut self, substring: &str) -> &mut TestRunner {
        self.include.push(IncludePattern {
            blacklist: true,
            substring: BString::from(substring),
        });
        self
    }

    /// Run all of the given tests.
    pub fn test_iter<I, T>(
        &mut self,
        it: I,
        mut compile: impl FnMut(
            &RegexTest,
            &[BString],
        ) -> Result<
            CompiledRegex,
            Box<dyn std::error::Error>,
        >,
    ) -> &mut TestRunner
    where
        I: IntoIterator<Item = T>,
        T: Borrow<RegexTest>,
    {
        for test in it {
            let test = test.borrow();
            if self.should_skip(test) {
                self.results.skip(test, &TestResult::skip());
                continue;
            }
            self.test(test, |regexes| compile(test, regexes));
        }
        self
    }

    /// Run a single test.
    ///
    /// This records the result of running the test in this runner. This does
    /// not fail the test immediately if the given regex test fails. Instead,
    /// this is only done when the `assert` method is called.
    ///
    /// Note that using this method bypasses any whitelisted substring applied
    /// to this runner. Whitelisted (and blacklisted) substrings are only
    /// applied when using `test_iter`.
    pub fn test(
        &mut self,
        test: &RegexTest,
        mut compile: impl FnMut(
            &[BString],
        ) -> Result<
            CompiledRegex,
            Box<dyn std::error::Error>,
        >,
    ) -> &mut TestRunner {
        let mut compiled = match safe(|| compile(test.regexes())) {
            Err(msg) => {
                // Regex tests should never panic. It's auto-fail if they do.
                self.results.fail(
                    test,
                    &TestResult::none(),
                    RegexTestFailureKind::UnexpectedPanicCompile(msg),
                );
                return self;
            }
            Ok(Ok(compiled)) => compiled,
            Ok(Err(err)) => {
                if !test.compiles() {
                    self.results.pass(test, &TestResult::none());
                } else {
                    self.results.fail(
                        test,
                        &TestResult::none(),
                        RegexTestFailureKind::CompileError { err },
                    );
                }
                return self;
            }
        };
        if !test.compiles() {
            self.results.fail(
                test,
                &TestResult::none(),
                RegexTestFailureKind::NoCompileError,
            );
            return self;
        }
        let results = match safe(|| test.test(&mut compiled)) {
            Ok(results) => results,
            Err(msg) => {
                self.results.fail(
                    test,
                    &TestResult::none(),
                    RegexTestFailureKind::UnexpectedPanicSearch(msg),
                );
                return self;
            }
        };
        for result in results.iter() {
            match result.kind {
                TestResultKind::None => {}
                TestResultKind::Skip => {
                    self.results.skip(test, result);
                }
                TestResultKind::Match(yes) => {
                    if yes == test.is_match() {
                        self.results.pass(test, result);
                    } else {
                        self.results.fail(
                            test,
                            result,
                            RegexTestFailureKind::IsMatch,
                        );
                    }
                }
                TestResultKind::Which(ref which) => {
                    if &**which != test.which_matches() {
                        self.results.fail(
                            test,
                            result,
                            RegexTestFailureKind::Many { got: which.to_vec() },
                        );
                    } else {
                        self.results.pass(test, result);
                    }
                }
                TestResultKind::StartEnd(ref matches) => {
                    let expected = test.matches();
                    if &expected != matches {
                        self.results.fail(
                            test,
                            result,
                            RegexTestFailureKind::StartEnd {
                                got: matches.clone(),
                            },
                        );
                    } else {
                        self.results.pass(test, result);
                    }
                }
                TestResultKind::Captures(ref caps) => {
                    let expected = test.captures();
                    if &expected != caps {
                        self.results.fail(
                            test,
                            result,
                            RegexTestFailureKind::Captures {
                                got: caps.clone(),
                            },
                        );
                    } else {
                        self.results.pass(test, result);
                    }
                }
            }
        }
        self
    }

    /// Return true if and only if the given test should be skipped.
    fn should_skip(&self, test: &RegexTest) -> bool {
        if self.include.is_empty() {
            return false;
        }

        // If we don't have any whitelist patterns, then the test will be run
        // unless it is blacklisted. Otherwise, if there are whitelist
        // patterns, then the test must match at least one of them.
        let mut skip = self.include.iter().any(|pat| !pat.blacklist);
        for pat in &self.include {
            if test.full_name().as_bytes().contains_str(&pat.substring) {
                skip = pat.blacklist;
            }
        }
        skip
    }
}

/// A collection of test results, corresponding to passed, skipped and failed
/// tests.
#[derive(Debug)]
struct RegexTestResults {
    pass: Vec<RegexTestResult>,
    fail: Vec<RegexTestFailure>,
    skip: Vec<RegexTestResult>,
}

/// A test that passed or skipped, along with its specific result.
#[derive(Debug)]
struct RegexTestResult {
    test: RegexTest,
    result: TestResult,
}

/// A test that failed along with the reason why.
#[derive(Debug)]
struct RegexTestFailure {
    test: RegexTest,
    result: TestResult,
    kind: RegexTestFailureKind,
}

/// Describes the nature of the failed test.
#[derive(Debug)]
enum RegexTestFailureKind {
    /// This occurs when the test expected a match (or didn't expect a match),
    /// but the actual regex implementation didn't match (or did match).
    IsMatch,
    /// This occurs when a set of regexes is tested, and the matching regexes
    /// returned by the regex implementation don't match the expected matching
    /// regexes. This error contains the indices of the regexes that matched.
    Many { got: Vec<usize> },
    /// This occurs when a single regex is used to find all non-overlapping
    /// matches in an input string, where the result did not match what was
    /// expected. This reports the incorrect matches returned by the regex
    /// implementation under test.
    StartEnd { got: Vec<Match> },
    /// Like StartEnd, but for capturing groups.
    Captures { got: Vec<Captures> },
    /// This occurs when the test expected the regex to fail to compile, but it
    /// compiled successfully.
    NoCompileError,
    /// This occurs when the test expected the regex to compile successfully,
    /// but it failed to compile.
    CompileError { err: Box<dyn std::error::Error> },
    /// While compiling, a panic occurred. If possible, the panic message
    /// is captured.
    UnexpectedPanicCompile(String),
    /// While searching, a panic occurred. If possible, the panic message
    /// is captured.
    UnexpectedPanicSearch(String),
}

impl RegexTestResults {
    fn new() -> RegexTestResults {
        RegexTestResults { pass: vec![], fail: vec![], skip: vec![] }
    }

    fn pass(&mut self, test: &RegexTest, result: &TestResult) {
        self.pass.push(RegexTestResult {
            test: test.clone(),
            result: result.clone(),
        });
    }

    fn fail(
        &mut self,
        test: &RegexTest,
        result: &TestResult,
        kind: RegexTestFailureKind,
    ) {
        self.fail.push(RegexTestFailure {
            test: test.clone(),
            result: result.clone(),
            kind,
        });
    }

    fn skip(&mut self, test: &RegexTest, result: &TestResult) {
        self.skip.push(RegexTestResult {
            test: test.clone(),
            result: result.clone(),
        });
    }

    fn assert(&self) {
        if read_env(ENV_REGEX_TEST_VERBOSE).map_or(false, |s| s == "1") {
            self.verbose();
        }
        if self.fail.is_empty() {
            return;
        }
        let failures = self
            .fail
            .iter()
            .map(|f| f.to_string())
            .collect::<Vec<String>>()
            .join("\n\n");
        panic!(
            "found {} failures:\n{}\n{}\n{}\n\n\
             Set the REGEX_TEST environment variable to filter tests, \n\
             e.g., REGEX_TEST=foo,-foo2 runs every test whose name contains \n\
             foo but not foo2\n\n",
            self.fail.len(),
            "~".repeat(79),
            failures.trim(),
            "~".repeat(79),
        )
    }

    fn verbose(&self) {
        println!("{}", "~".repeat(79));
        for t in &self.skip {
            println!("skip: {}", t.full_name());
        }
        for t in &self.pass {
            println!("pass: {}", t.full_name());
        }
        for t in &self.fail {
            println!("FAIL: {}", t.test.full_name());
        }
        println!(
            "\npassed: {}, skipped: {}, failed: {}",
            self.pass.len(),
            self.skip.len(),
            self.fail.len()
        );
        println!("{}", "~".repeat(79));
    }
}

impl RegexTestResult {
    fn full_name(&self) -> String {
        if self.result.name.is_empty() {
            self.test.full_name().to_string()
        } else {
            format!("{} ({})", self.test.full_name(), self.result.name)
        }
    }
}

impl RegexTestFailure {
    fn full_name(&self) -> String {
        if self.result.name.is_empty() {
            self.test.full_name().to_string()
        } else {
            format!("{} ({})", self.test.full_name(), self.result.name)
        }
    }
}

impl std::fmt::Display for RegexTestFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(
            f,
            "{}: {}\n\
             pattern:     {:?}\n\
             input:       {:?}",
            self.full_name(),
            self.kind.fmt(&self.test)?,
            self.test.regexes(),
            self.test.input(),
        )?;
        if !self.result.name.is_empty() {
            write!(f, "\ntest result: {:?}", self.result.name)?;
        }
        Ok(())
    }
}

impl RegexTestFailureKind {
    fn fmt(&self, test: &RegexTest) -> Result<String, std::fmt::Error> {
        use std::fmt::Write;

        let mut buf = String::new();
        match *self {
            RegexTestFailureKind::IsMatch => {
                if test.is_match() {
                    write!(buf, "expected match, but none found")?;
                } else {
                    write!(buf, "expected no match, but found a match")?;
                }
            }
            RegexTestFailureKind::Many { ref got } => {
                write!(
                    buf,
                    "expected regexes {:?} to match, but found {:?}",
                    test.which_matches(),
                    got
                )?;
            }
            RegexTestFailureKind::StartEnd { ref got } => {
                write!(
                    buf,
                    "did not find expected matches\n\
                     expected: {:?}\n     \
                     got: {:?}",
                    test.matches(),
                    got,
                )?;
            }
            RegexTestFailureKind::Captures { ref got } => {
                write!(
                    buf,
                    "expected to find {:?} captures, but got {:?}",
                    test.captures(),
                    got,
                )?;
            }
            RegexTestFailureKind::NoCompileError => {
                write!(buf, "expected regex to NOT compile, but it did")?;
            }
            RegexTestFailureKind::CompileError { ref err } => {
                write!(buf, "expected regex to compile, failed: {}", err)?;
            }
            RegexTestFailureKind::UnexpectedPanicCompile(ref msg) => {
                write!(buf, "got unexpected panic while compiling:\n{}", msg)?;
            }
            RegexTestFailureKind::UnexpectedPanicSearch(ref msg) => {
                write!(buf, "got unexpected panic while searching:\n{}", msg)?;
            }
        }
        Ok(buf)
    }
}

/// An iterator over regex tests.
#[derive(Debug)]
pub struct RegexTestsIter<'a> {
    it: std::slice::Iter<'a, RegexTest>,
}

impl<'a> Iterator for RegexTestsIter<'a> {
    type Item = &'a RegexTest;

    fn next(&mut self) -> Option<&'a RegexTest> {
        self.it.next()
    }
}

/// Represents the actual 'captures' key format more faithfully such that
/// Serde can deserialize it. Namely, we need a way to represent a 'None' value
/// inside a TOML array, and TOML has no 'null' value. So we make '[]' be
/// 'None', and we use 'MaybeSpan' to recognize it.
#[derive(Deserialize)]
#[serde(untagged)]
enum CapturesFormat {
    Span([usize; 2]),
    Match { id: usize, offsets: [usize; 2] },
    Spans(Vec<MaybeSpan>),
    Captures { id: usize, groups: Vec<MaybeSpan> },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(untagged)]
enum MaybeSpan {
    None([usize; 0]),
    Some([usize; 2]),
}

impl From<CapturesFormat> for Captures {
    fn from(data: CapturesFormat) -> Captures {
        // TODO: Make this fallible, because we need a non-None 0th group
        // for captures.
        match data {
            CapturesFormat::Span([start, end]) => {
                Captures { id: 0, groups: vec![Some(Span { start, end })] }
            }
            CapturesFormat::Match { id, offsets: [start, end] } => {
                Captures { id, groups: vec![Some(Span { start, end })] }
            }
            CapturesFormat::Spans(groups) => Captures {
                id: 0,
                groups: groups.into_iter().map(|g| g.into_option()).collect(),
            },
            CapturesFormat::Captures { id, groups } => Captures {
                id,
                groups: groups.into_iter().map(|g| g.into_option()).collect(),
            },
        }
    }
}

impl MaybeSpan {
    fn into_option(self) -> Option<Span> {
        match self {
            MaybeSpan::None(_) => None,
            MaybeSpan::Some([start, end]) => Some(Span { start, end }),
        }
    }
}

/// Read the environment variable given. If it doesn't exist, then return an
/// empty string. Otherwise, check that it is valid UTF-8. If it isn't, return
/// a useful error message.
fn read_env(var: &str) -> Result<String> {
    let val = match std::env::var_os(var) {
        None => return Ok("".to_string()),
        Some(val) => val,
    };
    let val = val.into_string().map_err(|os| {
        anyhow::anyhow!(
            "invalid UTF-8 in env var {}={:?}",
            var,
            Vec::from_os_str_lossy(&os)
        )
    })?;
    Ok(val)
}

/// Runs the given closure such that any panics are caught and converted into
/// errors. If the panic'd value could not be converted to a known error type,
/// then a generic string error message is used.
///
/// This is useful for use inside the test runner such that bugs for certain
/// tests don't prevent other tests from running.
fn safe<T, F>(fun: F) -> Result<T, String>
where
    F: FnOnce() -> T,
{
    use std::panic;

    panic::catch_unwind(panic::AssertUnwindSafe(fun)).map_err(|any_err| {
        // Extract common types of panic payload:
        // panic and assert produce &str or String
        if let Some(&s) = any_err.downcast_ref::<&str>() {
            s.to_owned()
        } else if let Some(s) = any_err.downcast_ref::<String>() {
            s.to_owned()
        } else {
            "UNABLE TO SHOW RESULT OF PANIC.".to_owned()
        }
    })
}

/// A function to set some boolean fields to a default of 'true'. We use a
/// function so that we can hand a path to it to Serde.
fn default_true() -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn err_no_regexes() {
        let data = r#"
[[tests]]
name = "foo"
input = "lib.rs"
matches = true
case_insensitive = true
"#;

        let mut tests = RegexTests::new();
        assert!(tests.load_slice("test", data.as_bytes()).is_err());
    }

    #[test]
    fn err_unknown_field() {
        let data = r#"
[[tests]]
name = "foo"
regex = ".*.rs"
input = "lib.rs"
matches = true
something = 0
"#;

        let mut tests = RegexTests::new();
        assert!(tests.load_slice("test", data.as_bytes()).is_err());
    }

    #[test]
    fn err_no_matches() {
        let data = r#"
[[tests]]
name = "foo"
regex = ".*.rs"
input = "lib.rs"
"#;

        let mut tests = RegexTests::new();
        assert!(tests.load_slice("test", data.as_bytes()).is_err());
    }

    #[test]
    fn load_match() {
        let data = r#"
[[tests]]
name = "foo"
regex = ".*.rs"
input = "lib.rs"
matches = [[0, 6]]
compiles = false
anchored = true
case_insensitive = true
unicode = false
utf8 = false
"#;

        let mut tests = RegexTests::new();
        tests.load_slice("test", data.as_bytes()).unwrap();

        let t0 = &tests.tests[0];
        assert_eq!("test", t0.group());
        assert_eq!("foo", t0.name());
        assert_eq!("test/foo", t0.full_name());
        assert_eq!(&[".*.rs"], t0.regexes());
        assert_eq!(true, t0.is_match());
        assert_eq!(vec![0], t0.which_matches());

        assert!(!t0.compiles());
        assert!(t0.anchored());
        assert!(t0.case_insensitive());
        assert!(!t0.unicode());
        assert!(!t0.utf8());
    }

    #[test]
    fn load_which_matches() {
        let data = r#"
[[tests]]
name = "foo"
regexes = [".*.rs", ".*.toml"]
input = "lib.rs"
matches = [
    { id = 0, offsets = [0, 0] },
    { id = 2, offsets = [0, 0] },
    { id = 5, offsets = [0, 0] },
]
"#;

        let mut tests = RegexTests::new();
        tests.load_slice("test", data.as_bytes()).unwrap();

        let t0 = &tests.tests[0];
        assert_eq!(&[".*.rs", ".*.toml"], t0.regexes());
        assert_eq!(true, t0.is_match());
        assert_eq!(vec![0, 2, 5], t0.which_matches());

        assert!(t0.compiles());
        assert!(!t0.anchored());
        assert!(!t0.case_insensitive());
        assert!(t0.unicode());
        assert!(t0.utf8());
    }

    #[test]
    fn load_spans() {
        let data = r#"
[[tests]]
name = "foo"
regex = ".*.rs"
input = "lib.rs"
matches = [[0, 2], [5, 10]]
"#;

        let mut tests = RegexTests::new();
        tests.load_slice("test", data.as_bytes()).unwrap();

        let spans =
            vec![Span { start: 0, end: 2 }, Span { start: 5, end: 10 }];
        let t0 = &tests.tests[0];
        assert_eq!(t0.regexes(), &[".*.rs"]);
        assert_eq!(t0.is_match(), true);
        assert_eq!(t0.which_matches(), &[0]);
        assert_eq!(
            t0.matches(),
            vec![
                Match { id: 0, span: spans[0] },
                Match { id: 0, span: spans[1] },
            ]
        );
        assert_eq!(
            t0.captures(),
            vec![
                Captures::new(0, vec![Some(spans[0])]),
                Captures::new(0, vec![Some(spans[1])]),
            ]
        );
    }

    #[test]
    fn load_capture_spans() {
        let data = r#"
[[tests]]
name = "foo"
regex = ".*.rs"
input = "lib.rs"
matches = [
  [[0, 15], [5, 10], [], [13, 14]],
  [[20, 30], [22, 24], [25, 27], []],
]
"#;

        let mut tests = RegexTests::new();
        tests.load_slice("test", data.as_bytes()).unwrap();

        let t0 = &tests.tests[0];
        assert_eq!(t0.regexes(), &[".*.rs"]);
        assert_eq!(t0.is_match(), true);
        assert_eq!(t0.which_matches(), &[0]);
        assert_eq!(
            t0.matches(),
            vec![
                Match { id: 0, span: Span { start: 0, end: 15 } },
                Match { id: 0, span: Span { start: 20, end: 30 } },
            ]
        );
        assert_eq!(
            t0.captures(),
            vec![
                Captures::new(
                    0,
                    vec![
                        Some(Span { start: 0, end: 15 }),
                        Some(Span { start: 5, end: 10 }),
                        None,
                        Some(Span { start: 13, end: 14 }),
                    ]
                ),
                Captures::new(
                    0,
                    vec![
                        Some(Span { start: 20, end: 30 }),
                        Some(Span { start: 22, end: 24 }),
                        Some(Span { start: 25, end: 27 }),
                        None,
                    ]
                ),
            ]
        );
    }
}
