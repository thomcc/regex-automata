use std::cell::RefCell;
use std::fmt;
use std::mem;
use std::rc::Rc;

use dense;
use state_id::{StateID, dead_id};

type DFARepr<S> = dense::DenseDFA<Vec<S>, S>;

/// An implementation of Hopcroft's algorithm for minimizing DFAs.
///
/// The algorithm implemented here is mostly taken from Wikipedia:
/// https://en.wikipedia.org/wiki/DFA_minimization#Hopcroft's_algorithm
///
/// This code has had some light optimization attention paid to it,
/// particularly in the form of reducing allocation as much as possible.
/// However, it is still generally slow. Future optimization work should
/// probably focus on the bigger picture rather than micro-optimizations. For
/// example:
///
/// 1. Figure out how to more intelligently create initial partitions. That is,
///    Hopcroft's algorithm starts by creating two partitions of DFA states
///    that are known to NOT be equivalent: match states and non-match states.
///    The algorithm proceeds by progressively refining these partitions into
///    smaller partitions. If we could start with more partitions, then we
///    could reduce the amount of work that Hopcroft's algorithm needs to do.
/// 2. For every partition that we visit, we find all incoming transitions to
///    every state in the partition for *every* element in the alphabet. (This
///    is why using byte classes can significantly decrease minimization times,
///    since byte classes shrink the alphabet.) This is quite costly and there
///    is perhaps some redundant work being performed depending on the specific
///    states in the set. For example, we might be able to only visit some
///    elements of the alphabet based on the transitions.
/// 3. Move parts of minimization into determinization. If minimization has
///    fewer states to deal with, then it should run faster. A prime example
///    of this might be large Unicode classes, which are generated in way that
///    can create a lot of redundant states.
pub struct Minimizer<'a, S> {
    dfa: &'a mut DFARepr<S>,
    in_transitions: Vec<Vec<Vec<S>>>,
    partitions: Vec<StateSet<S>>,
    waiting: Vec<StateSet<S>>,
}

impl<'a, S: StateID> fmt::Debug for Minimizer<'a, S> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_struct("Minimizer")
            .field("dfa", &self.dfa)
            .field("in_transitions", &self.in_transitions)
            .field("partitions", &self.partitions)
            .field("waiting", &self.waiting)
            .finish()
    }
}

/// A set of states. A state set makes up a single partition in Hopcroft's
/// algorithm.
///
/// It is represented by an ordered set of state identifiers. We use shared
/// ownership so that a single state set can be in both the set of partitions
/// and in set of waiting sets simultaneously without an additional allocation.
/// Generally, once a state set is built, it becomes immutable.
///
/// We use this representation because it avoids the overhead of more
/// traditional set data structures (HashSet/BTreeSet), and also because
/// computing intersection/subtraction on this representation is especially
/// fast.
#[derive(Clone, Debug, Eq, PartialEq, PartialOrd, Ord)]
struct StateSet<S>(Rc<RefCell<Vec<S>>>);

impl<'a, S: StateID> Minimizer<'a, S> {
    pub fn new(dfa: &'a mut DFARepr<S>) -> Minimizer<'a, S> {
        assert!(
            !dfa.kind().is_premultiplied(),
            "cannot minimize a premultiplied DenseDFA"
        );

        let in_transitions = Minimizer::incoming_transitions(dfa);
        let partitions = Minimizer::initial_partitions(dfa);
        let waiting = vec![partitions[0].clone()];

        Minimizer { dfa, in_transitions, partitions, waiting }
    }

    pub fn run(mut self) {
        let mut incoming = StateSet::empty();
        let mut scratch1 = StateSet::empty();
        let mut scratch2 = StateSet::empty();
        let mut newparts = vec![];

        while let Some(set) = self.waiting.pop() {
            for b in (0..self.dfa.alphabet_len()).map(|b| b as u8) {
                self.find_incoming_to(b, &set, &mut incoming);

                for p in 0..self.partitions.len() {
                    self.partitions[p].intersection(&incoming, &mut scratch1);
                    if scratch1.is_empty() {
                        newparts.push(self.partitions[p].clone());
                        continue;
                    }

                    self.partitions[p].subtract(&incoming, &mut scratch2);
                    if scratch2.is_empty() {
                        newparts.push(self.partitions[p].clone());
                        continue;
                    }

                    let (x, y) = (scratch1.deep_clone(), scratch2.deep_clone());
                    newparts.push(x.clone());
                    newparts.push(y.clone());
                    match self.find_waiting(&self.partitions[p]) {
                        Some(i) => {
                            self.waiting[i] = x;
                            self.waiting.push(y);
                        }
                        None => {
                            if x.len() <= y.len() {
                                self.waiting.push(x);
                            } else {
                                self.waiting.push(y);
                            }
                        }
                    }
                }
                newparts = mem::replace(&mut self.partitions, newparts);
                newparts.clear();
            }
        }

        let mut state_to_part = vec![dead_id(); self.dfa.len()];
        for p in &self.partitions {
            p.iter(|id| state_to_part[id.to_usize()] = p.min());
        }

        let mut minimal_ids = vec![dead_id(); self.dfa.len()];
        let mut new_id = S::from_usize(0);
        for (id, _) in self.dfa.iter() {
            if state_to_part[id.to_usize()] == id {
                minimal_ids[id.to_usize()] = new_id;
                new_id = S::from_usize(new_id.to_usize() + 1);
            }
        }
        let minimal_count = new_id.to_usize();

        for id in (0..self.dfa.len()).map(S::from_usize) {
            if state_to_part[id.to_usize()] != id {
                continue;
            }
            for (_, next) in self.dfa.get_state_mut(id).iter_mut() {
                *next = minimal_ids[state_to_part[next.to_usize()].to_usize()];
            }
            self.dfa.swap_states(id, minimal_ids[id.to_usize()]);
        }

        let old_start = self.dfa.start();
        self.dfa.set_start_state(
            minimal_ids[state_to_part[old_start.to_usize()].to_usize()],
        );
        self.dfa.truncate_states(minimal_count);

        let old_max = self.dfa.max_match_state();
        for id in (1..self.dfa.len()).map(S::from_usize) {
            if state_to_part[id.to_usize()] > old_max {
                break;
            }
            self.dfa.set_max_match_state(id);
        }
    }

    fn find_waiting(&self, set: &StateSet<S>) -> Option<usize> {
        self.waiting.iter().position(|s| s == set)
    }

    fn find_incoming_to(
        &self,
        b: u8,
        set: &StateSet<S>,
        incoming: &mut StateSet<S>,
    ) {
        incoming.clear();
        set.iter(|id| {
            for &inid in &self.in_transitions[id.to_usize()][b as usize] {
                incoming.add(inid);
            }
        });
        incoming.canonicalize();
    }

    fn initial_partitions(dfa: &DFARepr<S>) -> Vec<StateSet<S>> {
        let mut is_match = StateSet::empty();
        let mut no_match = StateSet::empty();
        for (id, _) in dfa.iter() {
            if dfa.is_match_state(id) {
                is_match.add(id);
            } else {
                no_match.add(id);
            }
        }
        assert!(!is_match.is_empty(), "must have at least one matching state");

        let mut sets = vec![is_match];
        if !no_match.is_empty() {
            sets.push(no_match);
        }
        sets.sort_by_key(|s| s.len());
        sets
    }

    fn incoming_transitions(dfa: &DFARepr<S>) -> Vec<Vec<Vec<S>>> {
        let mut incoming = vec![];
        for _ in dfa.iter() {
            incoming.push(vec![vec![]; dfa.alphabet_len()]);
        }
        for (id, state) in dfa.iter() {
            for (b, next) in state.iter() {
                incoming[next.to_usize()][b as usize].push(id);
            }
        }
        incoming
    }
}

impl<S: StateID> StateSet<S> {
    fn empty() -> StateSet<S> {
        StateSet(Rc::new(RefCell::new(vec![])))
    }

    fn add(&mut self, id: S) {
        self.0.borrow_mut().push(id);
    }

    fn min(&self) -> S {
        self.0.borrow()[0]
    }

    fn canonicalize(&mut self) {
        self.0.borrow_mut().sort();
        self.0.borrow_mut().dedup();
    }

    fn clear(&mut self) {
        self.0.borrow_mut().clear();
    }

    fn len(&self) -> usize {
        self.0.borrow().len()
    }

    fn is_empty(&self) -> bool {
        self.len() == 0
    }

    fn deep_clone(&self) -> StateSet<S> {
        let ids = self.0.borrow().iter().cloned().collect();
        StateSet(Rc::new(RefCell::new(ids)))
    }

    fn iter<F: FnMut(S)>(&self, mut f: F) {
        for &id in self.0.borrow().iter() {
            f(id);
        }
    }

    fn intersection(&self, other: &StateSet<S>, dest: &mut StateSet<S>) {
        dest.clear();
        if self.is_empty() || other.is_empty() {
            return;
        }

        let (seta, setb) = (self.0.borrow(), other.0.borrow());
        let (mut ita, mut itb) = (seta.iter().cloned(), setb.iter().cloned());
        let (mut a, mut b) = (ita.next().unwrap(), itb.next().unwrap());
        loop {
            if a == b {
                dest.add(a);
                a = match ita.next() {
                    None => break,
                    Some(a) => a,
                };
                b = match itb.next() {
                    None => break,
                    Some(b) => b,
                };
            } else if a < b {
                a = match ita.next() {
                    None => break,
                    Some(a) => a,
                };
            } else {
                b = match itb.next() {
                    None => break,
                    Some(b) => b,
                };
            }
        }
    }

    fn subtract(&self, other: &StateSet<S>, dest: &mut StateSet<S>) {
        dest.clear();
        if self.is_empty() || other.is_empty() {
            self.iter(|s| dest.add(s));
            return;
        }

        let (seta, setb) = (self.0.borrow(), other.0.borrow());
        let (mut ita, mut itb) = (seta.iter().cloned(), setb.iter().cloned());
        let (mut a, mut b) = (ita.next().unwrap(), itb.next().unwrap());
        loop {
            if a == b {
                a = match ita.next() {
                    None => break,
                    Some(a) => a,
                };
                b = match itb.next() {
                    None => { dest.add(a); break; }
                    Some(b) => b,
                };
            } else if a < b {
                dest.add(a);
                a = match ita.next() {
                    None => break,
                    Some(a) => a,
                };
            } else {
                b = match itb.next() {
                    None => { dest.add(a); break; }
                    Some(b) => b,
                };
            }
        }
        for a in ita {
            dest.add(a);
        }
    }
}
