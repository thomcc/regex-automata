use core::{
    borrow::Borrow,
    fmt::Debug,
    panic::{RefUnwindSafe, UnwindSafe},
};

use alloc::{sync::Arc, vec, vec::Vec};

use regex_syntax::hir::{self, literal, Hir};

use crate::{
    meta::{
        error::BuildError,
        regex::{Cache, RegexInfo},
        wrappers,
    },
    nfa::thompson::{self, pikevm::PikeVM, NFA},
    util::{
        captures::{Captures, GroupInfo},
        prefilter::{self, Prefilter, PrefilterI},
        primitives::{NonMaxUsize, PatternID},
        search::{
            Anchored, HalfMatch, Input, Match, MatchError, MatchKind,
            PatternSet,
        },
        syntax::Literals,
    },
};

#[cfg(feature = "dfa-onepass")]
use crate::dfa::onepass;
#[cfg(feature = "hybrid")]
use crate::hybrid;
#[cfg(feature = "nfa-backtrack")]
use crate::nfa::thompson::backtrack;

pub(crate) trait Strategy:
    Debug + Send + Sync + RefUnwindSafe + UnwindSafe + 'static
{
    fn create_captures(&self) -> Captures;

    fn create_cache(&self) -> Cache;

    fn reset_cache(&self, cache: &mut Cache);

    fn try_search(
        &self,
        cache: &mut Cache,
        input: &Input<'_>,
    ) -> Result<Option<Match>, MatchError>;

    fn try_search_half(
        &self,
        cache: &mut Cache,
        input: &Input<'_>,
    ) -> Result<Option<HalfMatch>, MatchError>;

    fn try_search_slots(
        &self,
        cache: &mut Cache,
        input: &Input<'_>,
        slots: &mut [Option<NonMaxUsize>],
    ) -> Result<Option<PatternID>, MatchError>;

    fn try_which_overlapping_matches(
        &self,
        cache: &mut Cache,
        input: &Input<'_>,
        patset: &mut PatternSet,
    ) -> Result<(), MatchError>;
}

// Implement strategy for anything that implements prefilter.
//
// Note that this must only be used for regexes of length 1. Multi-regexes
// don't work here. The prefilter interface only provides the span of a match
// and not the pattern ID. (I did consider making it more expressive, but I
// couldn't figure out how to tie everything together elegantly.) Thus, so long
// as the regex only contains one pattern, we can simply assume that a match
// corresponds to PatternID::ZERO. And indeed, that's what we do here.
//
// In practice, since this impl is used to report matches directly and thus
// completely bypasses the regex engine, we only wind up using this under the
// following restrictions:
//
// * There must be only one pattern. As explained above.
// * The literal sequence must be finite and only contain exact literals.
// * There must not be any look-around assertions. If there are, the literals
// extracted might be exact, but a match doesn't necessarily imply an overall
// match. As a trivial example, 'foo\bbar' does not match 'foobar'.
// * The pattern must not have any explicit capturing groups. If it does, the
// caller might expect them to be resolved. e.g., 'foo(bar)'.
//
// So when all of those things are true, we use a prefilter directly as a
// strategy.
//
// In the case where the number of patterns is more than 1, we don't use this
// but do use a special Aho-Corasick strategy if all of the regexes are just
// simple literals or alternations of literals. (We also use the Aho-Corasick
// strategy when len(patterns)==1 if the number of literals is large. In that
// case, literal extraction gives up and will return an infinite set.)
impl<T: PrefilterI> Strategy for T {
    fn create_captures(&self) -> Captures {
        // The only thing we support here is the start and end of the overall
        // match for a single pattern. In other words, exactly one implicit
        // capturing group. In theory, capturing groups should never be used
        // for this regex because the only way this impl gets used is if there
        // are no explicit capturing groups. Thus, asking to resolve capturing
        // groups is always wasteful.
        let info = GroupInfo::new(vec![vec![None::<&str>]]).unwrap();
        Captures::matches(info)
    }

    fn create_cache(&self) -> Cache {
        Cache {
            capmatches: self.create_captures(),
            pikevm: wrappers::PikeVMCache::none(),
            backtrack: wrappers::BoundedBacktrackerCache::none(),
            onepass: wrappers::OnePassCache::none(),
            hybrid: wrappers::HybridCache::none(),
        }
    }

    fn reset_cache(&self, cache: &mut Cache) {}

    fn try_search(
        &self,
        cache: &mut Cache,
        input: &Input<'_>,
    ) -> Result<Option<Match>, MatchError> {
        if input.is_done() {
            return Ok(None);
        }
        if input.get_anchored().is_anchored() {
            return Ok(self
                .prefix(input.haystack(), input.get_span())
                .map(|sp| Match::new(PatternID::ZERO, sp)));
        }
        Ok(self
            .find(input.haystack(), input.get_span())
            .map(|sp| Match::new(PatternID::ZERO, sp)))
    }

    fn try_search_half(
        &self,
        cache: &mut Cache,
        input: &Input<'_>,
    ) -> Result<Option<HalfMatch>, MatchError> {
        Ok(self
            .try_search(cache, input)?
            .map(|m| HalfMatch::new(m.pattern(), m.end())))
    }

    fn try_search_slots(
        &self,
        cache: &mut Cache,
        input: &Input<'_>,
        slots: &mut [Option<NonMaxUsize>],
    ) -> Result<Option<PatternID>, MatchError> {
        let m = match self.try_search(cache, input)? {
            None => return Ok(None),
            Some(m) => m,
        };
        if let Some(slot) = slots.get_mut(0) {
            *slot = NonMaxUsize::new(m.start());
        }
        if let Some(slot) = slots.get_mut(1) {
            *slot = NonMaxUsize::new(m.end());
        }
        Ok(Some(m.pattern()))
    }

    fn try_which_overlapping_matches(
        &self,
        cache: &mut Cache,
        input: &Input<'_>,
        patset: &mut PatternSet,
    ) -> Result<(), MatchError> {
        if self.try_search(cache, input)?.is_some() {
            patset.insert(PatternID::ZERO);
        }
        Ok(())
    }
}

pub(super) fn new(
    info: &RegexInfo,
    hirs: &[&Hir],
) -> Result<Arc<dyn Strategy>, BuildError> {
    let kind = info.config().get_match_kind();
    let lits = Literals::new(kind, hirs);
    // Check to see if our prefixes are exact, which means we might be able to
    // bypass the regex engine entirely and just rely on literal searches. We
    // need to meet a few criteria that basically lets us implement the full
    // regex API. So for example, we can implement "ask for capturing groups"
    // so long as they are no capturing groups in the regex.
    //
    // We also require that we have a single regex pattern. Namely, we reuse
    // the prefilter infrastructure to implement search and prefilters only
    // report spans. Prefilters don't know about pattern IDs. The multi-regex
    // case isn't a lost cause, we might still use Aho-Corasick and we might
    // still just use a regular prefilter, but that's done below.
    //
    // If we do have only one pattern, then we also require that it has zero
    // look-around assertions. Namely, literal extraction treats look-around
    // assertions as if they match *every* empty string. But of course, that
    // isn't true. So for example, 'foo\bquux' never matches anything, but
    // 'fooquux' is extracted from that as an exact literal. Such cases should
    // just run the regex engine. 'fooquux' will be used as a normal prefilter,
    // and then the regex engine will try to look for an actual match.
    //
    // Finally, currently, our prefilters are all oriented around
    // leftmost-first match semantics, so don't try to use them if the caller
    // asked for anything else.
    //
    // This seems like a lot of requirements to meet, but it applies to a lot
    // of cases. 'foo', '[abc][123]' and 'foo|bar|quux' all meet the above
    // criteria, for example.
    //
    // Note that this is effectively a latency optimization. If we didn't
    // do this, then the extracted literals would still get bundled into a
    // prefilter, and every regex engine capable of running unanchored searches
    // supports prefilters. So this optimization merely sidesteps having to run
    // the regex engine at all to confirm the match. Thus, it decreases the
    // latency of a match.
    if lits.prefixes().is_exact()
        && hirs.len() == 1
        && info.props()[0].look_set().is_empty()
        && info.props()[0].captures_len() == 0
        // We require this because our prefilters can't currently handle
        // assuming the responsibility of being the regex engine in all
        // cases. For example, when running a leftmost search with 'All'
        // match semantics for the regex 'foo|foobar', the prefilter will
        // currently report 'foo' as a match against 'foobar'. 'foo' is a
        // correct candidate, but it is not the correct leftmost match in this
        // circumstance, since the 'all' semantic demands that the search
        // continue until a dead state is reached.
        && info.config().get_match_kind() == MatchKind::LeftmostFirst
    {
        // OK because we know the set is exact and thus finite.
        let prefixes = lits.prefixes().literals().unwrap();
        debug!(
            "trying to bypass regex engine by creating \
             prefilter from {} literals: {:?}",
            prefixes.len(),
            prefixes,
        );
        if let Some(pre) = prefilter::new_as_strategy(kind, prefixes) {
            return Ok(pre);
        }
        debug!("regex bypass failed because no prefilter could be built");
    }
    // This now attempts another short-circuit of the regex engine: if we
    // have a huge alternation of just plain literals, then we can just use
    // Aho-Corasick for that and avoid the regex engine entirely.
    #[cfg(feature = "perf-literal-multisubstring")]
    if let Some(ac) = alternation_literals_to_aho_corasick(info, hirs) {
        return Ok(ac);
    }

    // At this point, we're committed to a regex engine of some kind. So pull
    // out a prefilter if we can, which will feed to each of the constituent
    // regex engines.
    let pre = if info.is_always_anchored_start() {
        // TODO: I'm not sure we necessarily want to do this... We may want to
        // run a prefilter for quick rejecting in some cases. This might mean
        // having a notion of whether a prefilter is "fast"? Or maybe it just
        // depends on haystack length? Or both?
        None
    } else if let Some(pre) = info.config().get_prefilter() {
        Some(pre.clone())
    } else if info.config().get_auto_prefilter() {
        lits.prefixes().literals().and_then(|strings| {
            debug!(
                "creating prefilter from {} literals: {:?}",
                strings.len(),
                strings,
            );
            Prefilter::new(kind, strings)
        })
    } else {
        None
    };
    let core = Core::new(info.clone(), pre.clone(), hirs)?;
    // Now that we have our core regex engines built, there are a few cases
    // where we can do a little bit better than just a normal "search forward
    // and maybe use a prefilter when in a start state." However, these cases
    // may not always work or otherwise build on top of the Core searcher.
    // For example, the anchored reverse optimization seems like it might
    // always work, but only the DFAs support reverse searching and the DFAs
    // might give up or quit for reasons. If we had, e.g., a PikeVM that
    // supported reverse searching, then we could avoid building a full Core
    // engine for this case.
    let core = match AnchoredReverse::new(core) {
        Err(core) => core,
        Ok(ar) => return Ok(Arc::new(ar)),
    };
    Ok(Arc::new(core))
}

#[derive(Debug)]
struct Core {
    info: RegexInfo,
    nfa: NFA,
    nfarev: Option<NFA>,
    pikevm: wrappers::PikeVM,
    backtrack: wrappers::BoundedBacktracker,
    onepass: wrappers::OnePass,
    hybrid: wrappers::Hybrid,
    dfa: wrappers::DFA,
}

impl Core {
    fn new(
        info: RegexInfo,
        pre: Option<Prefilter>,
        hirs: &[&Hir],
    ) -> Result<Core, BuildError> {
        let thompson_config = thompson::Config::new()
            .utf8(info.config().get_utf8())
            .nfa_size_limit(info.config().get_nfa_size_limit())
            .shrink(false)
            .captures(true);
        let nfa = thompson::Compiler::new()
            .configure(thompson_config.clone())
            .build_many_from_hir(hirs)
            .map_err(BuildError::nfa)?;
        // It's possible for the PikeVM or the BB to fail to build, even though
        // at this point, we already have a full NFA in hand. They can fail
        // when a Unicode word boundary is used but where Unicode word boundary
        // support is disabled at compile time, thus making it impossible to
        // match. (Construction can also fail if the NFA was compiled without
        // captures, but we always enable that above.)
        let pikevm = wrappers::PikeVM::new(&info, pre.clone(), &nfa)?;
        let backtrack =
            wrappers::BoundedBacktracker::new(&info, pre.clone(), &nfa)?;
        // The onepass engine can of course fail to build, but we expect it to
        // fail in many cases because it is an optimization that doesn't apply
        // to all regexes. The 'OnePass' wrapper encapsulates this failure (and
        // logs a message if it occurs).
        let onepass = wrappers::OnePass::new(&info, &nfa);
        // We try to encapsulate whether a particular regex engine should be
        // used within each respective wrapper, but the DFAs need a reverse NFA
        // to build itself, and we really do not want to build a reverse NFA if
        // we know we aren't going to use the lazy DFA. So we do a config check
        // up front, which is in practice the only way we won't try to use the
        // DFA.
        let (nfarev, hybrid, dfa) =
            if !info.config().get_hybrid() && !info.config().get_dfa() {
                (None, wrappers::Hybrid::none(), wrappers::DFA::none())
            } else {
                // FIXME: Technically, we don't quite yet KNOW that we need
                // a reverse NFA. It's possible for the DFAs below to both
                // fail to build just based on the forward NFA. In which case,
                // building the reverse NFA was totally wasted work. But...
                // fixing this requires breaking DFA construction apart into
                // two pieces: one for the forward part and another for the
                // reverse part. Quite annoying. Making it worse, when building
                // both DFAs fails, it's quite likely that the NFA is large and
                // that it will take quite some time to build the reverse NFA
                // too. So... it's really probably worth it to do this!
                let nfarev = thompson::Compiler::new()
                    // Currently, reverse NFAs don't support capturing groups,
                    // so we MUST disable them. But even if we didn't have to,
                    // we would, because nothing in this crate does anything
                    // useful with capturing groups in reverse. And of course,
                    // the lazy DFA ignores capturing groups in all cases.
                    .configure(
                        thompson_config.clone().captures(false).reverse(true),
                    )
                    .build_many_from_hir(hirs)
                    .map_err(BuildError::nfa)?;
                let dfa = if !info.config().get_dfa() {
                    wrappers::DFA::none()
                } else {
                    wrappers::DFA::new(&info, pre.clone(), &nfa, &nfarev)
                };
                let hybrid = if !info.config().get_hybrid() {
                    wrappers::Hybrid::none()
                } else if dfa.is_some() {
                    debug!("skipping lazy DFA because we have a full DFA");
                    wrappers::Hybrid::none()
                } else {
                    wrappers::Hybrid::new(&info, pre.clone(), &nfa, &nfarev)
                };
                (Some(nfarev), hybrid, dfa)
            };
        Ok(Core { info, nfa, nfarev, pikevm, backtrack, onepass, hybrid, dfa })
    }

    #[inline(always)]
    fn try_search_mayfail(
        &self,
        cache: &mut Cache,
        input: &Input<'_>,
    ) -> Result<Option<Match>, Option<MatchError>> {
        let err = if let Some(e) = self.dfa.get(input) {
            trace!("using full DFA for search at {:?}", input.get_span());
            match e.try_search(input) {
                Ok(m) => return Ok(m),
                Err(err) => err,
            }
        } else if let Some(e) = self.hybrid.get(input) {
            trace!("using lazy DFA for search at {:?}", input.get_span());
            match e.try_search(&mut cache.hybrid, input) {
                Ok(m) => return Ok(m),
                Err(err) => err,
            }
        } else {
            return Err(None);
        };
        if !is_err_quit_or_gaveup(&err) {
            return Err(Some(err));
        }
        trace!("DFA failed in search: {}", err);
        Err(None)
    }

    #[inline(always)]
    fn try_search_half_mayfail(
        &self,
        cache: &mut Cache,
        input: &Input<'_>,
    ) -> Result<Option<HalfMatch>, Option<MatchError>> {
        let err = if let Some(e) = self.dfa.get(input) {
            trace!("using full DFA for half search at {:?}", input.get_span());
            match e.try_search_half(input) {
                Ok(m) => return Ok(m),
                Err(err) => err,
            }
        } else if let Some(e) = self.hybrid.get(input) {
            trace!("using lazy DFA for half search at {:?}", input.get_span());
            match e.try_search_half(&mut cache.hybrid, input) {
                Ok(m) => return Ok(m),
                Err(err) => err,
            }
        } else {
            return Err(None);
        };
        if !is_err_quit_or_gaveup(&err) {
            return Err(Some(err));
        }
        trace!("DFA failed in half search: {}", err);
        Err(None)
    }

    fn try_search_nofail(
        &self,
        cache: &mut Cache,
        input: &Input<'_>,
    ) -> Result<Option<Match>, MatchError> {
        let caps = &mut cache.capmatches;
        caps.set_pattern(None);
        // We manually inline 'try_search_slots_nofail' here because we need to
        // borrow from 'cache.capmatches' in this method, but if we do, then
        // we can't pass 'cache' wholesale to to 'try_slots_no_hybrid'. It's a
        // classic example of how the borrow checker inhibits decomposition.
        // There are of course work-arounds (more types and/or interior
        // mutability), but that's more annoying than this IMO.
        let pid = if let Some(ref e) = self.onepass.get(input) {
            trace!("using OnePass for basic search at {:?}", input.get_span());
            e.try_search_slots(&mut cache.onepass, input, caps.slots_mut())
        } else if let Some(ref e) = self.backtrack.get(input) {
            trace!(
                "using BoundedBacktracker for basic search at {:?}",
                input.get_span()
            );
            e.try_search_slots(&mut cache.backtrack, input, caps.slots_mut())
        } else {
            trace!("using PikeVM for basic search at {:?}", input.get_span());
            let e = self.pikevm.get().expect("PikeVM is always available");
            e.try_search_slots(&mut cache.pikevm, input, caps.slots_mut())
        }?;
        caps.set_pattern(pid);
        Ok(caps.get_match())
    }

    fn try_search_slots_nofail(
        &self,
        cache: &mut Cache,
        input: &Input<'_>,
        slots: &mut [Option<NonMaxUsize>],
    ) -> Result<Option<PatternID>, MatchError> {
        if let Some(ref e) = self.onepass.get(input) {
            trace!(
                "using OnePass for capture search at {:?}",
                input.get_span()
            );
            e.try_search_slots(&mut cache.onepass, input, slots)
        } else if let Some(ref e) = self.backtrack.get(input) {
            trace!(
                "using BoundedBacktracker for capture search at {:?}",
                input.get_span()
            );
            e.try_search_slots(&mut cache.backtrack, input, slots)
        } else {
            trace!(
                "using PikeVM for capture search at {:?}",
                input.get_span()
            );
            let e = self.pikevm.get().expect("PikeVM is always available");
            e.try_search_slots(&mut cache.pikevm, input, slots)
        }
    }

    fn is_capture_search_needed(&self, slots_len: usize) -> bool {
        slots_len > self.nfa.group_info().implicit_slot_len()
    }
}

impl Strategy for Core {
    #[inline(always)]
    fn create_captures(&self) -> Captures {
        Captures::all(self.nfa.group_info().clone())
    }

    #[inline(always)]
    fn create_cache(&self) -> Cache {
        Cache {
            capmatches: self.create_captures(),
            pikevm: self.pikevm.create_cache(),
            backtrack: self.backtrack.create_cache(),
            onepass: self.onepass.create_cache(),
            hybrid: self.hybrid.create_cache(),
        }
    }

    #[inline(always)]
    fn reset_cache(&self, cache: &mut Cache) {
        cache.pikevm.reset(&self.pikevm);
        cache.backtrack.reset(&self.backtrack);
        cache.onepass.reset(&self.onepass);
        cache.hybrid.reset(&self.hybrid);
    }

    #[inline(always)]
    fn try_search(
        &self,
        cache: &mut Cache,
        input: &Input<'_>,
    ) -> Result<Option<Match>, MatchError> {
        match self.try_search_mayfail(cache, input) {
            Ok(x) => Ok(x),
            Err(Some(err)) => Err(err),
            Err(None) => self.try_search_nofail(cache, input),
        }
    }

    #[inline(always)]
    fn try_search_half(
        &self,
        cache: &mut Cache,
        input: &Input<'_>,
    ) -> Result<Option<HalfMatch>, MatchError> {
        // The main difference with 'try_find' is that if we're using a DFA,
        // we can use a single forward scan without needing to run the reverse
        // DFA.
        match self.try_search_half_mayfail(cache, input) {
            Ok(x) => Ok(x),
            Err(Some(err)) => Err(err),
            Err(None) => {
                // Only the lazy/full DFA returns half-matches, since the DFA
                // requires a reverse scan to find the start position. These
                // fallback regex engines can find the start and end in a
                // single pass, so we just do that and throw away the start
                // offset.
                let matched = self.try_search_nofail(cache, input)?;
                Ok(matched.map(|m| HalfMatch::new(m.pattern(), m.end())))
            }
        }
    }

    #[inline(always)]
    fn try_search_slots(
        &self,
        cache: &mut Cache,
        input: &Input<'_>,
        slots: &mut [Option<NonMaxUsize>],
    ) -> Result<Option<PatternID>, MatchError> {
        // Even if the regex has explicit capture groups, if the caller didn't
        // provide any explicit slots, then it doesn't make sense to try and do
        // extra work to get offsets for those slots. Ideally the caller should
        // realize this and not call this routine in the first place, but alas,
        // we try to save the caller from themselves if they do.
        if !self.is_capture_search_needed(slots.len()) {
            trace!("asked for slots unnecessarily, trying fast path");
            let m = match self.try_search(cache, input)? {
                None => return Ok(None),
                Some(m) => m,
            };
            copy_match_to_slots(m, slots);
            return Ok(Some(m.pattern()));
        }
        let m = match self.try_search_mayfail(cache, input) {
            Ok(Some(m)) => m,
            Ok(None) => return Ok(None),
            Err(Some(err)) => return Err(err),
            Err(None) => {
                return self.try_search_slots_nofail(cache, input, slots)
            }
        };
        // At this point, now that we've found the bounds of the
        // match, we need to re-run something that can resolve
        // capturing groups. But we only need to run on it on the
        // match bounds and not the entire haystack.
        trace!(
            "match found at {}..{} in capture search, \
		  	 using another engine to find captures",
            m.start(),
            m.end(),
        );
        let input = input
            .clone()
            .span(m.start()..m.end())
            .anchored(Anchored::Pattern(m.pattern()));
        self.try_search_slots_nofail(cache, &input, slots)
    }

    #[inline(always)]
    fn try_which_overlapping_matches(
        &self,
        cache: &mut Cache,
        input: &Input<'_>,
        patset: &mut PatternSet,
    ) -> Result<(), MatchError> {
        if let Some(e) = self.dfa.get(input) {
            trace!(
                "using full DFA for overlapping search at {:?}",
                input.get_span()
            );
            let err = match e.try_which_overlapping_matches(input, patset) {
                Ok(m) => return Ok(m),
                Err(err) => err,
            };
            if !is_err_quit_or_gaveup(&err) {
                return Err(err);
            }
            trace!(
                "full DFA failed in overlapping search, using fallback: {}",
                err
            );
            // Fallthrough to the fallback.
        } else if let Some(e) = self.hybrid.get(input) {
            trace!(
                "using lazy DFA for overlapping search at {:?}",
                input.get_span()
            );
            let err = match e.try_which_overlapping_matches(
                &mut cache.hybrid,
                input,
                patset,
            ) {
                Ok(m) => return Ok(m),
                Err(err) => err,
            };
            if !is_err_quit_or_gaveup(&err) {
                return Err(err);
            }
            trace!(
                "lazy DFA failed in overlapping search, using fallback: {}",
                err
            );
            // Fallthrough to the fallback.
        }
        let e = self.pikevm.get().expect("PikeVM is always available");
        e.try_which_overlapping_matches(&mut cache.pikevm, input, patset)
    }
}

#[derive(Debug)]
struct AnchoredReverse {
    core: Core,
}

impl AnchoredReverse {
    fn new(core: Core) -> Result<AnchoredReverse, Core> {
        if !core.info.is_always_anchored_end() {
            return Err(core);
        }
        // Only DFAs can do reverse searches (currently), so we need one of
        // them in order to do this optimization. It's possible (although
        // pretty unlikely) that we have neither and need to give up.
        if !core.hybrid.is_some() && !core.dfa.is_some() {
            debug!(
                "skipping anchored reverse optimization because \
				 we don't have a lazy DFA or a full DFA"
            );
            return Err(core);
        }
        Ok(AnchoredReverse { core })
    }

    #[inline(always)]
    fn try_search_half_anchored_rev(
        &self,
        cache: &mut Cache,
        input: &Input<'_>,
    ) -> Result<Option<HalfMatch>, Option<MatchError>> {
        let result = if let Some(e) = self.core.dfa.get(input) {
            trace!(
                "using full DFA for anchored reverse search at {:?}",
                input.get_span()
            );
            e.try_search_half_anchored_rev(input)
        } else if let Some(e) = self.core.hybrid.get(input) {
            trace!(
                "using lazy DFA for anchored reverse search at {:?}",
                input.get_span()
            );
            e.try_search_half_anchored_rev(&mut cache.hybrid, input)
        } else {
            unreachable!("AnchoredReverse always has a DFA")
        };
        match result {
            Ok(x) => Ok(x),
            Err(err) if is_err_quit_or_gaveup(&err) => {
                trace!("anchored reverse scan failed: {}", err);
                Err(None)
            }
            Err(err) => Err(Some(err)),
        }
    }
}

// Note that in this impl, we don't check that 'input.end() ==
// input.haystack().len()'. In particular, when that condition is false, a
// match is always impossible because we know that the regex is always anchored
// at the end (or else 'AnchoredReverse' won't be built). We don't check that
// here because the 'Regex' wrapper actually does that for us in all cases.
// Thus, in this impl, we can actually assume that the end position in 'input'
// is equivalent to the length of the haystack.
impl Strategy for AnchoredReverse {
    #[inline(always)]
    fn create_captures(&self) -> Captures {
        self.core.create_captures()
    }

    #[inline(always)]
    fn create_cache(&self) -> Cache {
        self.core.create_cache()
    }

    #[inline(always)]
    fn reset_cache(&self, cache: &mut Cache) {
        self.core.reset_cache(cache);
    }

    #[inline(always)]
    fn try_search(
        &self,
        cache: &mut Cache,
        input: &Input<'_>,
    ) -> Result<Option<Match>, MatchError> {
        match self.try_search_half_anchored_rev(cache, input) {
            Ok(None) => return Ok(None),
            Ok(Some(hm)) => {
                return Ok(Some(Match::new(
                    hm.pattern(),
                    hm.offset()..input.end(),
                )))
            }
            Err(Some(err)) => return Err(err),
            Err(None) => {
                trace!("using fallback in basic search");
                self.core.try_search(cache, input)
            }
        }
    }

    #[inline(always)]
    fn try_search_half(
        &self,
        cache: &mut Cache,
        input: &Input<'_>,
    ) -> Result<Option<HalfMatch>, MatchError> {
        match self.try_search_half_anchored_rev(cache, input) {
            Ok(None) => return Ok(None),
            Ok(Some(hm)) => {
                // Careful here! 'try_search_half' is a *forward* search that
                // only cares about the *end* position of a match. But
                // 'hm.offset()' is actually the start of the match. So we
                // actually just throw that away here and, since we know we
                // have a match, return the only possible position at which a
                // match can occur: input.end().
                return Ok(Some(HalfMatch::new(hm.pattern(), input.end())));
            }
            Err(Some(err)) => return Err(err),
            Err(None) => {
                trace!("using fallback in half search");
                self.core.try_search_half(cache, input)
            }
        }
    }

    #[inline(always)]
    fn try_search_slots(
        &self,
        cache: &mut Cache,
        input: &Input<'_>,
        slots: &mut [Option<NonMaxUsize>],
    ) -> Result<Option<PatternID>, MatchError> {
        match self.try_search_half_anchored_rev(cache, input) {
            Ok(None) => return Ok(None),
            Ok(Some(hm)) => {
                if !self.core.is_capture_search_needed(slots.len()) {
                    trace!("asked for slots unnecessarily, skipping captures");
                    let m = Match::new(hm.pattern(), hm.offset()..input.end());
                    copy_match_to_slots(m, slots);
                    return Ok(Some(m.pattern()));
                }
                let start = hm.offset();
                let input = input
                    .clone()
                    .span(start..input.end())
                    .anchored(Anchored::Pattern(hm.pattern()));
                return self
                    .core
                    .try_search_slots_nofail(cache, &input, slots);
            }
            Err(Some(err)) => return Err(err),
            Err(None) => {
                trace!("using fallback in capture search");
                self.core.try_search_slots(cache, input, slots)
            }
        }
    }

    #[inline(always)]
    fn try_which_overlapping_matches(
        &self,
        cache: &mut Cache,
        input: &Input<'_>,
        patset: &mut PatternSet,
    ) -> Result<(), MatchError> {
        // It seems like this could probably benefit from an anchored reverse
        // optimization, perhaps by doing an overlapping reverse search (which
        // the DFAs do support). I haven't given it much thought though, and
        // I'm currently focus more on the single pattern case.
        self.core.try_which_overlapping_matches(cache, input, patset)
    }
}

/// Attempts to extract an alternation of literals, and if it's deemed worth
/// doing, returns an Aho-Corasick prefilter as a strategy.
///
/// And currently, this only returns something when 'hirs.len() == 1'. This
/// could in theory do something if there are multiple HIRs where all of them
/// are alternation of literals, but I haven't had the time to go down that
/// path yet.
#[cfg(feature = "perf-literal-multisubstring")]
fn alternation_literals_to_aho_corasick(
    info: &RegexInfo,
    hirs: &[&Hir],
) -> Option<Arc<dyn Strategy>> {
    use crate::util::prefilter::AhoCorasick;

    let lits = alternation_literals(info, hirs)?;
    AhoCorasick::new_as_strategy(MatchKind::LeftmostFirst, &lits)
}

/// Pull out an alternation of literals from the given sequence of HIR
/// expressions.
///
/// There are numerous ways for this to fail. Generally, this only applies
/// to regexes of the form 'foo|bar|baz|...|quux'. It can also fail if there
/// are "too few" alternates, in which case, the regex engine is likely faster.
///
/// And currently, this only returns something when 'hirs.len() == 1'.
#[cfg(feature = "perf-literal-multisubstring")]
fn alternation_literals(
    info: &RegexInfo,
    hirs: &[&Hir],
) -> Option<Vec<Vec<u8>>> {
    use regex_syntax::hir::{HirKind, Literal};

    // This is pretty hacky, but basically, if `is_alternation_literal` is
    // true, then we can make several assumptions about the structure of our
    // HIR. This is what justifies the `unreachable!` statements below.
    //
    // This code should be refactored once we overhaul this crate's
    // optimization pipeline, because this is a terribly inflexible way to go
    // about things.
    if hirs.len() != 1
        || !info.props()[0].look_set().is_empty()
        || info.props()[0].captures_len() > 0
        || !info.props()[0].is_alternation_literal()
        || info.config().get_match_kind() != MatchKind::LeftmostFirst
    {
        return None;
    }
    let hir = &hirs[0];
    let alts = match *hir.kind() {
        HirKind::Alternation(ref alts) => alts,
        _ => return None, // one literal isn't worth it
    };

    let mut lits = vec![];
    for alt in alts {
        let mut lit = vec![];
        match *alt.kind() {
            HirKind::Literal(Literal(ref bytes)) => {
                lit.extend_from_slice(bytes)
            }
            HirKind::Concat(ref exprs) => {
                for e in exprs {
                    match *e.kind() {
                        HirKind::Literal(Literal(ref bytes)) => {
                            lit.extend_from_slice(bytes);
                        }
                        _ => unreachable!("expected literal, got {:?}", e),
                    }
                }
            }
            _ => unreachable!("expected literal or concat, got {:?}", alt),
        }
        lits.push(lit);
    }
    // Why do this? Well, when the number of literals is small, it's likely
    // that we'll use the lazy DFA which is in turn likely to be faster than
    // Aho-Corasick in such cases. Primarily because Aho-Corasick doesn't have
    // a "lazy DFA" but either a contiguous NFA or a full DFA. We rarely use
    // the latter because it is so hungry (in time and space), and the former
    // is decently fast, but not as fast as a well oiled lazy DFA.
    //
    // However, once the number starts getting large, the lazy DFA is likely
    // to start thrashing because of the modest default cache size. When
    // exactly does this happen? Dunno. But at whatever point that is (we make
    // a guess below based on ad hoc benchmarking), we'll want to cut over to
    // Aho-Corasick, where even the contiguous NFA is likely to do much better.
    if lits.len() < 3000 {
        debug!("skipping Aho-Corasick because there are too few literals");
        return None;
    }
    Some(lits)
}

#[inline(always)]
fn copy_match_to_slots(m: Match, slots: &mut [Option<NonMaxUsize>]) {
    let slot_start = m.pattern().as_usize() * 2;
    let slot_end = slot_start + 1;
    if let Some(slot) = slots.get_mut(slot_start) {
        *slot = NonMaxUsize::new(m.start());
    }
    if let Some(slot) = slots.get_mut(slot_end) {
        *slot = NonMaxUsize::new(m.end());
    }
}

/// Returns true only when the given error corresponds to a search that failed
/// quit because it saw a specific byte, or gave up because it thought itself
/// to be too slow.
///
/// This is useful for checking whether an error returned by the lazy DFA
/// should be bubbled up or if it should result in running another regex
/// engine. Errors like "invalid pattern ID" should get bubbled up, while
/// quitting or giving up should result in trying a different engine.
fn is_err_quit_or_gaveup(err: &MatchError) -> bool {
    use crate::MatchErrorKind::*;
    matches!(*err.kind(), Quit { .. } | GaveUp { .. })
}
