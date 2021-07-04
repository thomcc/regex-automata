use core::{borrow::Borrow, iter, mem::size_of};

use crate::{
    dfa::HalfMatch,
    hybrid::{
        error::{BuildError, CacheError},
        id::LazyStateID,
        search, Config,
    },
    nfa::thompson,
    util::{
        alphabet::{self, ByteClasses, ByteSet},
        determinize::{
            self, Start, State, StateBuilderEmpty, StateBuilderNFA,
        },
        id::{PatternID, StateID as NFAStateID},
        matchtypes::{MatchError, MatchKind},
        sparse_set::SparseSets,
    },
};

#[derive(Clone, Debug)]
pub struct InertDFA<N> {
    nfa: N,
    stride2: usize,
    classes: ByteClasses,
    quit: ByteSet,
    anchored: bool,
    match_kind: MatchKind,
    starts_for_each_pattern: bool,
    cache_capacity: usize,
    minimum_cache_flush_count: Option<usize>,
    bytes_per_state: usize,
}

impl<N: Borrow<thompson::NFA>> InertDFA<N> {
    pub(crate) fn new(
        config: &Config,
        nfa: N,
    ) -> Result<InertDFA<N>, BuildError> {
        let mut quit = config.quit.unwrap_or(ByteSet::empty());
        if nfa.borrow().has_word_boundary_unicode() {
            if config.get_unicode_word_boundary() {
                for b in 0x80..=0xFF {
                    quit.add(b);
                }
            } else {
                // If heuristic support for Unicode word boundaries wasn't
                // enabled, then we can still check if our quit set is correct.
                // If the caller set their quit bytes in a way that causes the
                // DFA to quit on at least all non-ASCII bytes, then that's all
                // we need for heuristic support to work.
                if !quit.contains_range(0x80, 0xFF) {
                    return Err(
                        BuildError::unsupported_dfa_word_boundary_unicode(),
                    );
                }
            }
        }
        let classes = if !config.get_byte_classes() {
            // The lazy DFA will always use the equivalence class map, but
            // enabling this option is useful for debugging. Namely, this will
            // cause all transitions to be defined over their actual bytes
            // instead of an opaque equivalence class identifier. The former is
            // much easier to grok as a human.
            ByteClasses::singletons()
        } else {
            let mut set = nfa.borrow().byte_class_set().clone();
            // It is important to distinguish any "quit" bytes from all other
            // bytes. Otherwise, a non-quit byte may end up in the same class
            // as a quit byte, and thus cause the DFA stop when it shouldn't.
            if !quit.is_empty() {
                set.add_set(&quit);
            }
            set.byte_classes()
        };
        let min_cache = minimum_cache_capacity(
            nfa.borrow(),
            &classes,
            config.get_starts_for_each_pattern(),
        );
        if config.get_cache_capacity() < min_cache {
            return Err(BuildError::insufficient_cache_capacity(
                min_cache,
                config.get_cache_capacity(),
            ));
        }
        let stride2 = classes.stride2();
        let inert = InertDFA {
            nfa,
            stride2,
            classes,
            quit,
            anchored: config.get_anchored(),
            match_kind: config.get_match_kind(),
            starts_for_each_pattern: config.get_starts_for_each_pattern(),
            cache_capacity: config.get_cache_capacity(),
            minimum_cache_flush_count: config.get_minimum_cache_flush_count(),
            bytes_per_state: config.get_bytes_per_state(),
        };
        Ok(inert)
    }

    /// Returns the number of patterns in this DFA. (It is possible for this
    /// to be zero, for a DFA that never matches anything.)
    pub fn pattern_count(&self) -> usize {
        self.nfa.borrow().match_len()
    }

    pub fn as_ref(&self) -> InertDFA<&thompson::NFA> {
        InertDFA {
            nfa: self.nfa.borrow(),
            stride2: self.stride2,
            classes: self.classes.clone(),
            quit: self.quit.clone(),
            anchored: self.anchored,
            match_kind: self.match_kind,
            starts_for_each_pattern: self.starts_for_each_pattern,
            cache_capacity: self.cache_capacity,
            minimum_cache_flush_count: self.minimum_cache_flush_count,
            bytes_per_state: self.bytes_per_state,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Cache {
    trans: Vec<LazyStateID>,
    starts: Vec<LazyStateID>,
    states: Vec<State>,
    states_to_id: StateMap,
    sparses: SparseSets,
    stack: Vec<NFAStateID>,
    scratch_state_builder: StateBuilderEmpty,
}

impl Cache {
    pub fn new(inert: &InertDFA<&thompson::NFA>) -> Cache {
        let mut starts_len = Start::count();
        if inert.starts_for_each_pattern {
            starts_len += Start::count() * inert.pattern_count();
        }
        let mut cache = Cache {
            trans: vec![],
            starts: vec![],
            states: vec![],
            states_to_id: StateMap::new(),
            sparses: SparseSets::new(inert.nfa.borrow().len()),
            stack: vec![],
            scratch_state_builder: StateBuilderEmpty::new(),
        };
        let mut dfa = DFA { inert, cache: &mut cache };
        dfa.cache
            .starts
            .extend(iter::repeat(dfa.unknown_id()).take(starts_len));
        // This sets up some states that we use as sentinels that are present
        // in every DFA. While it would be technically possible to implement
        // this DFA without explicitly putting these states in the transition
        // table, this is convenient to do to make `next_state` correct for all
        // valid state IDs without needing explicit conditionals to special
        // case these sentinel states.
        //
        // All three of these states are "dead" states. That is, all of
        // them transition only to themselves. So once you enter one of
        // these states, it's impossible to leave them. Thus, any correct
        // search routine must explicitly check for these state types. (Sans
        // `unknown`, since that is only used internally to represent missing
        // states.)
        let unknown = dfa.add_empty_state().unwrap().to_unknown();
        let dead = dfa.add_empty_state().unwrap().to_dead();
        let quit = dfa.add_empty_state().unwrap().to_quit();
        assert_eq!(unknown, dfa.unknown_id());
        assert_eq!(dead, dfa.dead_id());
        assert_eq!(quit, dfa.quit_id());
        // We transition "unknown" states to dead states to uphold the property
        // that `next_state` should never return an "unknown" state ID. This is
        // somewhat of a pathological case, since it implies the caller has
        // probably gone out of their way to pass an unknown state ID to
        // `next_state`.
        dfa.set_all_transitions(unknown, dead);
        dfa.set_all_transitions(dead, dead);
        dfa.set_all_transitions(quit, quit);
        // We make room for all three of these states so that we maintain the
        // relationship between the index into 'states' and the premultiplied
        // state identifier that points into the transition table.
        dfa.cache.states.push(State::dead());
        dfa.cache.states.push(State::dead());
        dfa.cache.states.push(State::dead());
        // All of these states are equivalent, so putting all three of them in
        // the cache isn't possible. Moreover, we wouldn't want to do that.
        // Unknown and quit states are special in that they are artificial
        // constructions this implementation. But dead states are a natural
        // part of determinization. When you reach a point in the NFA where you
        // cannot go anywhere else, a dead state will naturally arise and we
        // MUST reuse the canonical dead state that we've created here. Why?
        // Because it is the state ID that tells the search routine whether a
        // state is dead or not, and thus, whether to stop the search.
        dfa.cache.states_to_id.insert(State::dead(), dead);
        cache
    }
}

/// A map from states to state identifiers. When using std, we use a standard
/// hashmap, since it's a bit faster for this use case. (Other maps, like
/// one's based on FNV, have not yet been benchmarked.)
///
/// The main purpose of this map is to reuse states where possible. This won't
/// fully minimize the DFA, but it works well in a lot of cases.
#[cfg(feature = "std")]
type StateMap = std::collections::HashMap<State, LazyStateID>;
#[cfg(not(feature = "std"))]
type StateMap = BTreeMap<State, LazyStateID>;

#[derive(Debug)]
pub struct DFA<'i, 'c> {
    inert: &'i InertDFA<&'i thompson::NFA>,
    cache: &'c mut Cache,
}

impl<'i, 'c> DFA<'i, 'c> {
    pub fn new(
        inert: &'i InertDFA<&'i thompson::NFA>,
        cache: &'c mut Cache,
    ) -> DFA<'i, 'c> {
        DFA { inert, cache }
    }

    pub fn find_leftmost_fwd(
        &mut self,
        pattern_id: Option<PatternID>,
        bytes: &[u8],
    ) -> Result<Option<HalfMatch>, MatchError> {
        self.find_leftmost_fwd_at(pattern_id, bytes, 0, bytes.len())
    }

    pub fn find_leftmost_rev(
        &mut self,
        pattern_id: Option<PatternID>,
        bytes: &[u8],
    ) -> Result<Option<HalfMatch>, MatchError> {
        self.find_leftmost_rev_at(pattern_id, bytes, 0, bytes.len())
    }

    pub fn find_leftmost_fwd_at(
        &mut self,
        pattern_id: Option<PatternID>,
        bytes: &[u8],
        start: usize,
        end: usize,
    ) -> Result<Option<HalfMatch>, MatchError> {
        search::find_fwd(false, self, pattern_id, bytes, start, end)
    }

    pub fn find_leftmost_rev_at(
        &mut self,
        pattern_id: Option<PatternID>,
        bytes: &[u8],
        start: usize,
        end: usize,
    ) -> Result<Option<HalfMatch>, MatchError> {
        search::find_rev(false, self, pattern_id, bytes, start, end)
    }

    pub fn next_state(
        &mut self,
        current: LazyStateID,
        input: u8,
    ) -> Result<LazyStateID, CacheError> {
        let input = self.inert.classes.get(input);
        let offset = current.as_usize_unmasked() + usize::from(input);
        let sid = self.cache.trans[offset];
        if !sid.is_unknown() {
            return Ok(sid);
        }
        self.cache_next_state(current, alphabet::Unit::u8(input))
    }

    pub fn next_eoi_state(
        &mut self,
        current: LazyStateID,
    ) -> Result<LazyStateID, CacheError> {
        let eoi = self.inert.classes.eoi().as_usize();
        let offset = current.as_usize_unchecked() + eoi;
        let sid = self.cache.trans[offset];
        if !sid.is_unknown() {
            return Ok(sid);
        }
        self.cache_next_state(current, self.inert.classes.eoi())
    }

    pub fn start_state_forward(
        &mut self,
        pattern_id: Option<PatternID>,
        bytes: &[u8],
        start: usize,
        end: usize,
    ) -> Result<LazyStateID, CacheError> {
        let start_type = Start::from_position_fwd(bytes, start, end);
        let sid = self.get_cached_start(pattern_id, start_type);
        if !sid.is_unknown() {
            return Ok(sid);
        }
        self.cache_start_group(pattern_id, start_type)
    }

    pub fn start_state_reverse(
        &mut self,
        pattern_id: Option<PatternID>,
        bytes: &[u8],
        start: usize,
        end: usize,
    ) -> Result<LazyStateID, CacheError> {
        let start_type = Start::from_position_rev(bytes, start, end);
        let sid = self.get_cached_start(pattern_id, start_type);
        if !sid.is_unknown() {
            return Ok(sid);
        }
        self.cache_start_group(pattern_id, start_type)
    }

    /// Returns the number of patterns in this DFA. (It is possible for this
    /// to be zero, for a DFA that never matches anything.)
    pub fn pattern_count(&self) -> usize {
        self.inert.pattern_count()
    }

    pub fn match_count(&self, id: LazyStateID) -> usize {
        assert!(id.is_match());
        self.get_cached_state(id).match_count()
    }

    pub fn match_pattern(
        &self,
        id: LazyStateID,
        match_index: usize,
    ) -> PatternID {
        // This is an optimization for the very common case of a DFA with a
        // single pattern. This conditional avoids a somewhat more costly path
        // that finds the pattern ID from the corresponding `State`, which
        // requires a bit of slicing/pointer-chasing. This optimization tends
        // to only matter when matches are frequent.
        if self.pattern_count() == 1 {
            return PatternID::ZERO;
        }
        self.get_cached_state(id).match_pattern(match_index)
    }

    fn cache_next_state(
        &mut self,
        current: LazyStateID,
        unit: alphabet::Unit,
    ) -> Result<LazyStateID, CacheError> {
        let stride2 = self.stride2();
        let empty_builder = self.get_state_builder();
        let builder = determinize::next(
            self.inert.nfa.borrow(),
            self.inert.match_kind,
            &mut self.cache.sparses,
            &mut self.cache.stack,
            &self.cache.states[current.as_usize_unmasked() >> stride2],
            unit,
            empty_builder,
        );
        let next =
            self.maybe_add_state(builder, |sid| sid).map(|(sid, _)| sid)?;
        // This is the payoff. The next time 'next_state' is called with this
        // state and alphabet unit, it will find this transition and avoid
        // having to re-determinize this transition.
        self.set_transition(current, unit, next);
        Ok(next)
    }

    fn get_cached_start(
        &self,
        pattern_id: Option<PatternID>,
        start: Start,
    ) -> LazyStateID {
        let start_index = start.as_usize();
        let index = match pattern_id {
            None => start_index,
            Some(pid) => {
                let pid = pid.as_usize();
                assert!(
                    pid < self.pattern_count(),
                    "invalid pattern ID: {:?}",
                    pid
                );
                Start::count() + (Start::count() * pid) + start_index
            }
        };
        self.cache.starts[index]
    }

    fn get_cached_state(&self, sid: LazyStateID) -> &State {
        let index = sid.as_usize_unmasked() >> self.stride2();
        &self.cache.states[index]
    }

    fn cache_start_group(
        &mut self,
        pattern_id: Option<PatternID>,
        start: Start,
    ) -> Result<LazyStateID, CacheError> {
        let nfa = self.inert.nfa.borrow();
        let nfa_start_id = match pattern_id {
            Some(pid) => nfa.start_pattern(pid),
            None if self.inert.anchored => nfa.start_anchored(),
            None => nfa.start_unanchored(),
        };

        let id = self.cache_start_one(nfa_start_id, start)?;
        self.set_start_state(pattern_id, start, id);
        Ok(id)
    }

    fn cache_start_one(
        &mut self,
        nfa_start_id: NFAStateID,
        start: Start,
    ) -> Result<LazyStateID, CacheError> {
        let mut builder_matches = self.get_state_builder().into_matches();
        start.set_state(&mut builder_matches);
        determinize::epsilon_closure(
            self.inert.nfa.borrow(),
            nfa_start_id,
            *builder_matches.look_have(),
            &mut self.cache.stack,
            &mut self.cache.sparses.set1,
        );
        let mut builder = builder_matches.into_nfa();
        determinize::add_nfa_states(
            self.inert.nfa.borrow(),
            &self.cache.sparses.set1,
            &mut builder,
        );
        self.maybe_add_state(builder, |id| id.to_start()).map(|(sid, _)| sid)
    }

    fn maybe_add_state(
        &mut self,
        builder: StateBuilderNFA,
        idmap: impl Fn(LazyStateID) -> LazyStateID,
    ) -> Result<(LazyStateID, bool), CacheError> {
        if let Some(&cached_id) =
            self.cache.states_to_id.get(builder.as_bytes())
        {
            // Since we have a cached state, put the constructed state's
            // memory back into our scratch space, so that it can be reused.
            self.put_state_builder(builder);
            return Ok((cached_id, false));
        }
        self.add_state(builder, idmap).map(|sid| (sid, true))
    }

    fn add_state(
        &mut self,
        builder: StateBuilderNFA,
        idmap: impl Fn(LazyStateID) -> LazyStateID,
    ) -> Result<LazyStateID, CacheError> {
        let mut id = idmap(self.add_empty_state()?);
        if builder.is_match() {
            id = id.to_match();
        }
        if !self.inert.quit.is_empty() {
            let quit_id = self.quit_id();
            for b in self.inert.quit.iter() {
                self.set_transition(id, alphabet::Unit::u8(b), quit_id);
            }
        }
        let state = builder.to_state();
        self.cache.states.push(state.clone());
        self.cache.states_to_id.insert(state, id);
        self.put_state_builder(builder);
        Ok(id)
    }

    fn add_empty_state(&mut self) -> Result<LazyStateID, CacheError> {
        let next = self.cache.trans.len();
        // TODO: Attempt a cache reset here if allocating a new ID fails.
        let sid = LazyStateID::new(next)
            .map_err(|_| CacheError::too_many_cache_resets())?;
        self.cache
            .trans
            .extend(iter::repeat(self.unknown_id()).take(self.stride()));
        Ok(sid)
    }

    fn set_all_transitions(&mut self, from: LazyStateID, to: LazyStateID) {
        for unit in self.inert.classes.representatives() {
            self.set_transition(from, unit, to);
        }
    }

    fn set_transition(
        &mut self,
        from: LazyStateID,
        unit: alphabet::Unit,
        to: LazyStateID,
    ) {
        assert!(self.is_valid(from));
        assert!(self.is_valid(to));
        let offset =
            from.as_usize_unmasked() + self.inert.classes.get_by_unit(unit);
        self.cache.trans[offset] = to;
    }

    fn set_start_state(
        &mut self,
        pattern_id: Option<PatternID>,
        start: Start,
        id: LazyStateID,
    ) {
        assert!(self.is_valid(id));
        let start_index = start.as_usize();
        let index = match pattern_id {
            None => start_index,
            Some(pid) => {
                let pid = pid.as_usize();
                Start::count() + (Start::count() * pid) + start_index
            }
        };
        self.cache.starts[index] = id;
    }

    fn unknown_id(&self) -> LazyStateID {
        // This unwrap is OK since 0 is always a valid state ID.
        LazyStateID::new(0).unwrap()
    }

    fn dead_id(&self) -> LazyStateID {
        // This unwrap is OK since the maximum value here is 1 * 512 = 512,
        // which is <= 2047 (the maximum state ID on 16-bit systems). Where
        // 512 is the worst case for our equivalence classes (every byte is a
        // distinct class).
        LazyStateID::new(1 << self.stride2()).unwrap()
    }

    fn quit_id(&self) -> LazyStateID {
        // This unwrap is OK since the maximum value here is 2 * 512 = 1024,
        // which is <= 2047 (the maximum state ID on 16-bit systems). Where
        // 512 is the worst case for our equivalence classes (every byte is a
        // distinct class).
        LazyStateID::new(2 << self.stride2()).unwrap()
    }

    fn is_valid(&self, id: LazyStateID) -> bool {
        let id = id.as_usize_unmasked();
        id < self.cache.trans.len() && id % self.stride() == 0
    }

    /// Returns the stride, as a base-2 exponent, required for these
    /// equivalence classes.
    ///
    /// The stride is always the smallest power of 2 that is greater than or
    /// equal to the alphabet length. This is done so that converting between
    /// state IDs and indices can be done with shifts alone, which is much
    /// faster than integer division.
    fn stride2(&self) -> usize {
        self.inert.stride2
    }

    /// Returns the total stride for every state in this lazy DFA. This
    /// corresponds to the total number of transitions used by each state in
    /// this DFA's transition table.
    fn stride(&self) -> usize {
        1 << self.stride2()
    }

    /// Returns a state builder from this DFA that might have existing
    /// capacity. This helps avoid allocs in cases where a state is built that
    /// turns out to already be cached.
    ///
    /// Callers must put the state builder back with 'put_state_builder',
    /// otherwise the allocation reuse won't work.
    fn get_state_builder(&mut self) -> StateBuilderEmpty {
        core::mem::replace(
            &mut self.cache.scratch_state_builder,
            StateBuilderEmpty::new(),
        )
    }

    /// Puts the given state builder back into this DFA for reuse.
    ///
    /// Note that building a 'State' from a builder always creates a new alloc,
    /// so callers should always put the builder back.
    fn put_state_builder(&mut self, builder: StateBuilderNFA) {
        let _ = core::mem::replace(
            &mut self.cache.scratch_state_builder,
            builder.clear(),
        );
    }
}

fn minimum_cache_capacity(
    nfa: &thompson::NFA,
    classes: &ByteClasses,
    starts_for_each_pattern: bool,
) -> usize {
    const ID_SIZE: usize = size_of::<LazyStateID>();
    const MIN_STATES: usize = 5;
    let stride = 1 << classes.stride2();

    let sparses = 2 * nfa.len() * NFAStateID::SIZE;
    let trans = MIN_STATES * stride * ID_SIZE;

    let mut starts = Start::count() * ID_SIZE;
    if starts_for_each_pattern {
        starts += (Start::count() * nfa.match_len()) * ID_SIZE;
    }

    // Every `State` has three bytes for flags, 4 bytes for the number of
    // patterns, followed by 32-bit encodings of patterns and then varint
    // encodings of NFA state IDs. We use the worst case (which isn't
    // technically possible) of 5 bytes for each NFA state ID.
    let max_state_size = 3 + 4 + (nfa.match_len() * 4) + (nfa.len() * 5);
    let states = MIN_STATES * (size_of::<State>() + max_state_size);
    let states_to_sid = states + (MIN_STATES * ID_SIZE);
    let stack = nfa.len() * NFAStateID::SIZE;
    let scratch_state_builder = max_state_size;

    sparses
        + trans
        + starts
        + states
        + states_to_sid
        + stack
        + scratch_state_builder
}
