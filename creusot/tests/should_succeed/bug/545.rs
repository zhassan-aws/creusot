extern crate creusot_contracts;
use creusot_contracts::*;

pub fn negative_is_negative() {
    proof_assert!(0i32 > -100i32);
}
