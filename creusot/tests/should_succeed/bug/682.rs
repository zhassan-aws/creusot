extern crate creusot_contracts;
use creusot_contracts::*;

#[requires(*a <= u64::MAX / 2u64)]
#[ensures(^a > *a)]
fn add_some(a: &mut u64) {
    *a += 1;
}

#[requires(*a == 3u64)]
#[ensures(^a > *a)]
pub fn foo(a: &mut u64) {
    let a_p: Ghost<u64> = ghost!(*a);
    add_some(a);
    assert!(*a > *a_p);
}
