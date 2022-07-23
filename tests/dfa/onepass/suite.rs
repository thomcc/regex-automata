use regex_automata::{
    dfa::onepass::{self, OnePass},
    nfa::thompson,
    util::iter,
    MatchKind, SyntaxConfig,
};

use ret::{
    bstr::{BString, ByteSlice},
    CompiledRegex, RegexTest, TestResult, TestRunner,
};

use crate::{suite, testify_captures, Result};

const EXPANSIONS: &[&str] = &["is_match", "find", "captures"];

/// Tests the default configuration of the hybrid NFA/DFA.
#[test]
fn default() -> Result<()> {
    let builder = OnePass::builder();
    TestRunner::new()?
        .expand(EXPANSIONS, |t| t.compiles())
        .test_iter(suite()?.iter(), compiler(builder))
        .assert();
    Ok(())
}

/// Tests the hybrid NFA/DFA when 'starts_for_each_pattern' is enabled for all
/// tests.
#[test]
fn starts_for_each_pattern() -> Result<()> {
    let mut builder = OnePass::builder();
    builder.configure(OnePass::config().starts_for_each_pattern(true));
    TestRunner::new()?
        .expand(EXPANSIONS, |t| t.compiles())
        .test_iter(suite()?.iter(), compiler(builder))
        .assert();
    Ok(())
}

/// Tests the hybrid NFA/DFA when byte classes are disabled.
///
/// N.B. Disabling byte classes doesn't avoid any indirection at search time.
/// All it does is cause every byte value to be its own distinct equivalence
/// class.
#[test]
fn no_byte_classes() -> Result<()> {
    let mut builder = OnePass::builder();
    builder.configure(OnePass::config().byte_classes(false));
    TestRunner::new()?
        .expand(EXPANSIONS, |t| t.compiles())
        .test_iter(suite()?.iter(), compiler(builder))
        .assert();
    Ok(())
}

fn compiler(
    mut builder: onepass::Builder,
) -> impl FnMut(&RegexTest, &[BString]) -> Result<CompiledRegex> {
    move |test, regexes| {
        let regexes = regexes
            .iter()
            .map(|r| r.to_str().map(|s| s.to_string()))
            .collect::<std::result::Result<Vec<String>, _>>()?;

        // Check if our regex contains things that aren't supported by DFAs.
        // That is, Unicode word boundaries when searching non-ASCII text.
        if !configure_onepass_builder(test, &mut builder) {
            return Ok(CompiledRegex::skip());
        }
        let re = match builder.build_many(&regexes) {
            Ok(re) => re,
            Err(err) => {
                let msg = err.to_string();
                if test.compiles() && msg.contains("not one-pass") {
                    return Ok(CompiledRegex::skip());
                }
                return Err(err.into());
            }
        };
        let mut cache = re.create_cache();
        Ok(CompiledRegex::compiled(move |test| -> TestResult {
            run_test(&re, &mut cache, test)
        }))
    }
}

fn run_test(
    re: &OnePass,
    cache: &mut onepass::Cache,
    test: &RegexTest,
) -> TestResult {
    match test.additional_name() {
        "is_match" => TestResult::matched(re.is_match(cache, test.input())),
        "find" => match test.search_kind() {
            ret::SearchKind::Earliest => {
                let input = re.create_input(test.input()).earliest(true);
                let mut caps = re.create_captures();
                let it = iter::Searcher::new(input)
                    .into_matches_iter(|input| {
                        re.search(cache, input, &mut caps);
                        Ok(caps.get_match())
                    })
                    .infallible()
                    .take(test.match_limit().unwrap_or(std::usize::MAX))
                    .map(|m| ret::Match {
                        id: m.pattern().as_usize(),
                        span: ret::Span { start: m.start(), end: m.end() },
                    });
                TestResult::matches(it)
            }
            ret::SearchKind::Leftmost => {
                /*
                let it = re
                    .find_iter(cache, test.input())
                    .take(test.match_limit().unwrap_or(std::usize::MAX))
                    .map(|m| ret::Match {
                        id: m.pattern().as_usize(),
                        span: ret::Span { start: m.start(), end: m.end() },
                    });
                TestResult::matches(it)
                */
                let input = re.create_input(test.input()).earliest(false);
                let mut caps = re.create_captures();
                let it = iter::Searcher::new(input)
                    .into_matches_iter(|input| {
                        re.search(cache, input, &mut caps);
                        Ok(caps.get_match())
                    })
                    .infallible()
                    .take(test.match_limit().unwrap_or(std::usize::MAX))
                    .map(|m| ret::Match {
                        id: m.pattern().as_usize(),
                        span: ret::Span { start: m.start(), end: m.end() },
                    });
                TestResult::matches(it)
            }
            ret::SearchKind::Overlapping => {
                // The one-pass DFA does not support any kind of overlapping
                // search. This is not just a matter of not having the API.
                // It's fundamentally incompatible with the one-pass concept.
                // If overlapping matches were possible, then the one-pass DFA
                // would fail to build.
                TestResult::skip()
            }
        },
        "captures" => match test.search_kind() {
            ret::SearchKind::Earliest => {
                let input = re.create_input(test.input()).earliest(true);
                let it = iter::Searcher::new(input)
                    .into_captures_iter(re.create_captures(), |input, caps| {
                        Ok(re.search(cache, input, caps))
                    })
                    .infallible()
                    .take(test.match_limit().unwrap_or(std::usize::MAX))
                    .map(|caps| testify_captures(&caps));
                TestResult::captures(it)
            }
            ret::SearchKind::Leftmost => {
                /*
                let it = re
                    .captures_iter(cache, test.input())
                    .take(test.match_limit().unwrap_or(std::usize::MAX))
                    .map(|caps| testify_captures(&caps));
                TestResult::captures(it)
                */
                let input = re.create_input(test.input()).earliest(false);
                let it = iter::Searcher::new(input)
                    .into_captures_iter(re.create_captures(), |input, caps| {
                        Ok(re.search(cache, input, caps))
                    })
                    .infallible()
                    .take(test.match_limit().unwrap_or(std::usize::MAX))
                    .map(|caps| testify_captures(&caps));
                TestResult::captures(it)
            }
            ret::SearchKind::Overlapping => {
                // The one-pass DFA does not support any kind of overlapping
                // search. This is not just a matter of not having the API.
                // It's fundamentally incompatible with the one-pass concept.
                // If overlapping matches were possible, then the one-pass DFA
                // would fail to build.
                TestResult::skip()
            }
        },
        name => TestResult::fail(&format!("unrecognized test name: {}", name)),
    }
}

/// Configures the given regex builder with all relevant settings on the given
/// regex test.
///
/// If the regex test has a setting that is unsupported, then this returns
/// false (implying the test should be skipped).
fn configure_onepass_builder(
    test: &RegexTest,
    builder: &mut onepass::Builder,
) -> bool {
    if !test.anchored() {
        return false;
    }
    let match_kind = match test.match_kind() {
        ret::MatchKind::All => MatchKind::All,
        ret::MatchKind::LeftmostFirst => MatchKind::LeftmostFirst,
        ret::MatchKind::LeftmostLongest => return false,
    };

    let config = OnePass::config().match_kind(match_kind).utf8(test.utf8());
    builder
        .configure(config)
        .syntax(config_syntax(test))
        .thompson(config_thompson(test));
    true
}

/// Configuration of a Thompson NFA compiler from a regex test.
fn config_thompson(_test: &RegexTest) -> thompson::Config {
    thompson::Config::new()
}

/// Configuration of the regex parser from a regex test.
fn config_syntax(test: &RegexTest) -> SyntaxConfig {
    SyntaxConfig::new()
        .case_insensitive(test.case_insensitive())
        .unicode(test.unicode())
        .utf8(test.utf8())
}
