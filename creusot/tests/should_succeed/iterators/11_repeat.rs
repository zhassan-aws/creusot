extern crate creusot_contracts;

use creusot_contracts::*;

mod common;
use common::Iterator;

pub struct Repeat<A> {
    element: A,
}

impl<A: Clone> Iterator for Repeat<A> {
    type Item = A;

    #[predicate]
    fn completed(&mut self) -> bool {
        pearlite! { false }
    }

    #[predicate]
    fn produces(self, visited: Seq<Self::Item>, o: Self) -> bool {
        pearlite! {
            self == o &&
            forall<i: Int> 0 <= i && i < visited.len() ==> visited[i] == self.element
        }
    }

    #[law]
    #[requires(a.invariant())]
    #[ensures(a.produces(Seq::EMPTY, a))]
    fn produces_refl(a: Self) {}

    #[law]
    #[requires(a.invariant())]
    #[requires(b.invariant())]
    #[requires(c.invariant())]
    #[requires(a.produces(ab, b))]
    #[requires(b.produces(bc, c))]
    #[ensures(a.produces(ab.concat(bc), c))]
    fn produces_trans(a: Self, ab: Seq<Self::Item>, b: Self, bc: Seq<Self::Item>, c: Self) {}

    #[predicate]
    fn invariant(self) -> bool {
        pearlite! { true }
    }

    #[maintains((mut self).invariant())]
    #[ensures(match result {
      None => self.completed(),
      Some(v) => (*self).produces(Seq::singleton(v), ^self)
    })]
    fn next(&mut self) -> Option<A> {
        Some(self.element.clone())
    }
}
