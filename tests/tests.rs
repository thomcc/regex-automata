mod dfa;
mod hybrid;
mod meta;
mod nfa;

#[cfg(not(miri))]
type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

#[cfg(not(miri))]
fn suite() -> Result<ret::RegexTests> {
    let _ = env_logger::try_init();

    let mut tests = ret::RegexTests::new();
    macro_rules! load {
        ($name:expr) => {{
            const DATA: &[u8] =
                include_bytes!(concat!("data/", $name, ".toml"));
            tests.load_slice($name, DATA)?;
        }};
    }

    load!("anchored");
    load!("bytes");
    load!("crazy");
    load!("earliest");
    load!("empty");
    load!("expensive");
    load!("flags");
    load!("iter");
    load!("leftmost-all");
    load!("misc");
    load!("multiline");
    load!("no-unicode");
    load!("overlapping");
    load!("regression");
    load!("set");
    load!("substring");
    load!("unicode");
    load!("word-boundary");
    load!("fowler/basic");
    load!("fowler/nullsubexpr");
    load!("fowler/repetition");

    Ok(tests)
}

/// Configure a regex_automata::Input with the given test configuration.
#[cfg(not(miri))]
fn create_input<'a, 'b>(
    test: &'a ret::RegexTest,
    create: impl Fn(&'a [u8]) -> regex_automata::Input<'a, 'b>,
) -> regex_automata::Input<'a, 'b> {
    use regex_automata::Anchored;

    let bounds = test.bounds();
    let anchored = if test.anchored() { Anchored::Yes } else { Anchored::No };
    create(test.input()).range(bounds.start..bounds.end).anchored(anchored)
}

/// Convert capture matches into the test suite's capture values.
///
/// The given captures must represent a valid match, where the first capturing
/// group has a non-None span. Otherwise this panics.
#[cfg(not(miri))]
fn testify_captures(
    caps: &regex_automata::util::captures::Captures,
) -> ret::Captures {
    assert!(caps.is_match(), "expected captures to represent a match");
    let spans = caps
        .iter()
        .map(|group| group.map(|m| ret::Span { start: m.start, end: m.end }));
    // These unwraps are OK because we assume our 'caps' represents a match,
    // and a match always gives a non-zero number of groups with the first
    // group being non-None.
    ret::Captures::new(caps.pattern().unwrap().as_usize(), spans).unwrap()
}
