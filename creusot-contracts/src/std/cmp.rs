use crate::{logic::OrdLogic, *};
pub use ::std::cmp::*;

#[cfg(feature = "contracts")]
pub use creusot_contracts_proc::PartialEq;

extern_spec! {
    mod std {
        mod cmp {
            trait PartialEq<Rhs> {
                #[ensures(result == (self.deep_model() == rhs.deep_model()))]
                fn eq(&self, rhs: &Rhs) -> bool
                where
                    Self: DeepModel,
                    Rhs: DeepModel<DeepModelTy = Self::DeepModelTy>;
            }

            // TODO: for now, we only support total orders
            trait PartialOrd<Rhs>
                where Self: DeepModel,
                      Rhs: DeepModel<DeepModelTy = Self::DeepModelTy>,
                      Self::DeepModelTy: OrdLogic
            {
                #[ensures(result == Some((*self).deep_model().cmp_log((*rhs).deep_model())))]
                fn partial_cmp(&self, rhs: &Rhs) -> Option<Ordering>;

                #[ensures(result == (self.deep_model() < other.deep_model()))]
                fn lt(&self, other: &Rhs) -> bool;

                #[ensures(result == (self.deep_model() <= other.deep_model()))]
                fn le(&self, other: &Rhs) -> bool;

                #[ensures(result == (self.deep_model() > other.deep_model()))]
                fn gt(&self, other: &Rhs) -> bool;

                #[ensures(result == (self.deep_model() >= other.deep_model()))]
                fn ge(&self, other: &Rhs) -> bool;
            }

            trait Ord
                where Self: DeepModel,
                      Self::DeepModelTy: OrdLogic
            {
                #[ensures(result == (*self).deep_model().cmp_log((*rhs).deep_model()))]
                fn cmp(&self, rhs: &Self) -> Ordering;

                #[ensures(result.deep_model() >= self.deep_model())]
                #[ensures(result.deep_model() >= o.deep_model())]
                #[ensures(result == self || result == o)]
                #[ensures(self.deep_model() <= o.deep_model() ==> result == o)]
                #[ensures(o.deep_model() < self.deep_model() ==> result == self)]
                fn max(self, o: Self) -> Self;
            }
        }
    }
}

impl<T: DeepModel> DeepModel for Reverse<T> {
    type DeepModelTy = Reverse<T::DeepModelTy>;

    #[logic]
    fn deep_model(self) -> Self::DeepModelTy {
        pearlite! { Reverse(self.0.deep_model()) }
    }
}

impl<T: OrdLogic> OrdLogic for Reverse<T> {
    #[logic]
    fn cmp_log(self, o: Self) -> Ordering {
        match self.0.cmp_log(o.0) {
            Ordering::Equal => Ordering::Equal,
            Ordering::Less => Ordering::Greater,
            Ordering::Greater => Ordering::Less,
        }
    }

    #[law]
    #[ensures(x.le_log(y) == (x.cmp_log(y) != Ordering::Greater))]
    fn cmp_le_log(x: Self, y: Self) {}

    #[law]
    #[ensures(x.lt_log(y) == (x.cmp_log(y) == Ordering::Less))]
    fn cmp_lt_log(x: Self, y: Self) {}

    #[law]
    #[ensures(x.ge_log(y) == (x.cmp_log(y) != Ordering::Less))]
    fn cmp_ge_log(x: Self, y: Self) {}

    #[law]
    #[ensures(x.gt_log(y) == (x.cmp_log(y) == Ordering::Greater))]
    fn cmp_gt_log(x: Self, y: Self) {}

    #[law]
    #[ensures(x.cmp_log(x) == Ordering::Equal)]
    fn refl(x: Self) {}

    #[law]
    #[requires(x.cmp_log(y) == o)]
    #[requires(y.cmp_log(z) == o)]
    #[ensures(x.cmp_log(z) == o)]
    fn trans(x: Self, y: Self, z: Self, o: Ordering) {}

    #[law]
    #[requires(x.cmp_log(y) == Ordering::Less)]
    #[ensures(y.cmp_log(x) == Ordering::Greater)]
    fn antisym1(x: Self, y: Self) {}

    #[law]
    #[requires(x.cmp_log(y) == Ordering::Greater)]
    #[ensures(y.cmp_log(x) == Ordering::Less)]
    fn antisym2(x: Self, y: Self) {}

    #[law]
    #[ensures((x == y) == (x.cmp_log(y) == Ordering::Equal))]
    fn eq_cmp(x: Self, y: Self) {}
}
