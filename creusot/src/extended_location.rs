use rustc_middle::mir::Location;
use rustc_mir_dataflow::{self as dataflow, Analysis, Results, ResultsCursor};
use std::borrow::Borrow;

// Dataflow locations
#[derive(Debug, Copy, Clone)]
pub enum ExtendedLocation {
    Start(Location),
    Mid(Location),
}

// Rust hides the real `Direction` trait from me, this hack recreates it
pub trait Dir {
    fn is_forward() -> bool;
}

impl Dir for dataflow::Forward {
    fn is_forward() -> bool {
        true
    }
}

impl Dir for dataflow::Backward {
    fn is_forward() -> bool {
        false
    }
}

impl ExtendedLocation {
    pub(crate) fn is_entry_loc(self) -> bool {
        if let Self::Start(loc) = self {
            loc == Location::START
        } else {
            false
        }
    }

    pub(crate) fn loc(&self) -> &Location {
        match self {
            Self::Start(l) => l,
            Self::Mid(l) => l,
        }
    }

    pub(crate) fn same_block(&self, other: Self) -> bool {
        self.loc().block == other.loc().block
    }

    pub(crate) fn seek_to<'tcx, A, R, D>(self, cursor: &mut ResultsCursor<'_, 'tcx, A, R>)
    where
        A: Analysis<'tcx, Direction = D>,
        D: Dir,
        R: Borrow<Results<'tcx, A>>,
    {
        use ExtendedLocation::*;
        if D::is_forward() {
            match self {
                Start(loc) => cursor.seek_before_primary_effect(loc),
                Mid(loc) => cursor.seek_after_primary_effect(loc),
            }
        } else {
            match self {
                Start(loc) => cursor.seek_after_primary_effect(loc),
                Mid(loc) => cursor.seek_before_primary_effect(loc),
            }
        }
    }
}
