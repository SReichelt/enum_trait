use enum_trait::meta;

use crate::{meta_bool::*, meta_num::*, optional_type::*};

meta! {
    pub enum trait TypeList<trait ItemBound: ?Sized> {
        Empty,
        NonEmpty<Head: ItemBound, Tail: TypeList<ItemBound>>,
    }

    trait impl<trait ItemBound: ?Sized> TypeList<ItemBound> {
        pub type IsEmpty: MetaBool = match <Self> {
            Empty => True,
            NonEmpty<Head: ItemBound, Tail: TypeList<ItemBound>> => False,
        };

        pub type Len: MetaNum = match <Self> {
            Empty => Zero,
            NonEmpty<Head: ItemBound, Tail: TypeList<ItemBound>> => Succ<Tail::Len>,
        };

        pub type Get<I: ValidIndex<ItemBound, Self>>: ItemBound = match <Self, I> {
            NonEmpty<Head: ItemBound, Tail: TypeList<ItemBound>>, Zero => Head,
            NonEmpty<Head: ItemBound, Tail: TypeList<ItemBound>>, Succ<P: ValidIndex<ItemBound, Tail>> =>
                Tail::Get<P>,
        };

        pub type GetOpt<I: ExtendedIndex<ItemBound, Self>>: OptionalType<ItemBound> = match <Self, I> {
            Empty, Zero => NoType,
            NonEmpty<Head: ItemBound, Tail: TypeList<ItemBound>>, Zero => SomeType<Head>,
            NonEmpty<Head: ItemBound, Tail: TypeList<ItemBound>>, Succ<P: ExtendedIndex<ItemBound, Tail>> =>
                Tail::GetOpt<P>,
        };

        /// Equivalent to `GetOpt` followed by `UnwrapOr`, but avoids the type-erasure problem of
        /// `GetOpt`.
        pub type GetOr<I: ExtendedIndex<ItemBound, Self>, X: ItemBound>: ItemBound = match <Self, I> {
            Empty, Zero => X,
            NonEmpty<Head: ItemBound, Tail: TypeList<ItemBound>>, Zero => Head,
            NonEmpty<Head: ItemBound, Tail: TypeList<ItemBound>>, Succ<P: ExtendedIndex<ItemBound, Tail>> =>
                Tail::GetOr<P, X>,
        };

        pub type Append<T: ItemBound>: TypeList<ItemBound> = match <Self> {
            Empty => NonEmpty<T, Empty>,
            NonEmpty<Head: ItemBound, Tail: TypeList<ItemBound>> => NonEmpty<Head, Tail::Append<T>>,
        };

        pub type AppendAll<List: TypeList<ItemBound>>: TypeList<ItemBound> = match <Self> {
            Empty => List,
            NonEmpty<Head: ItemBound, Tail: TypeList<ItemBound>> => NonEmpty<Head, Tail::AppendAll<List>>,
        };
    }

    pub trait ValidIndex<trait ItemBound: ?Sized, List: TypeList<ItemBound>> =
        MetaNumLessThan<List::Len>;

    pub trait ExtendedIndex<trait ItemBound: ?Sized, List: TypeList<ItemBound>> =
        MetaNumLessOrEqual<List::Len>;

    /*
    pub trait SizedTypeList<trait ItemBound: Sized> = TypeList<ItemBound>;

    pub type NestedTupleWith<trait ItemBound: Sized, List: SizedTypeList<ItemBound>, T: Sized>: Sized =
        match <List> {
            Empty => T,
            NonEmpty<Head: ItemBound, Tail: SizedTypeList<ItemBound>> =>
                (Head, NestedTupleWith<ItemBound, List, T>),
        };

    pub type NestedTuple<trait ItemBound: Sized, List: SizedTypeList<ItemBound>>: Sized =
        NestedTupleWith<ItemBound, List, ()>;

    pub fn get_nested_tuple_item<
        trait ItemBound: Sized,
        List: SizedTypeList<ItemBound>,
        T: Sized,
        I: ExtendedIndex<ItemBound, List>
    >(
        tuple: &NestedTupleWith<ItemBound, List, T>,
    ) -> &<List::GetOr<I, T> {
        match <List, I> {
            Empty, Zero => tuple,
            NonEmpty<Head: ItemBound, Tail: TypeList<ItemBound>>, Zero => &tuple.0,
            NonEmpty<Head: ItemBound, Tail: TypeList<ItemBound>>, Succ<P: ExtendedIndex<ItemBound, Tail>> =>
                get_nested_tuple_item<ItemBound, Tail, T, P>(&tuple.1),
        }
    }
    */

    /*pub type MapToRefs<'a, trait ItemBound: ?Sized + 'a, List: TypeList<ItemBound>>: SizedTypeList = match <List> {
        Empty => Empty,
        NonEmpty<Head: ItemBound, Tail: TypeList<ItemBound>> => NonEmpty<&'a Head, MapToRefs<'a, ItemBound, Tail>>,
    };

    pub type MapToMutRefs<'a, trait ItemBound: ?Sized + 'a, List: TypeList<ItemBound>>: SizedTypeList = match <List> {
        Empty => Empty,
        NonEmpty<Head: ItemBound, Tail: TypeList<ItemBound>> => NonEmpty<&'a mut Head, MapToMutRefs<'a, ItemBound, Tail>>,
    };*/
}

#[macro_export]
macro_rules! type_list {
    [] => ($crate::type_list::Empty);
    [..$List:ty] => ($List);
    [$Head:ty $(, $($Tail:tt)*)?] => (
        $crate::type_list::NonEmpty<$Head, $crate::type_list![$($($Tail)*)?]>
    );
    [$Item:ty; $n:literal] => (
        enum_trait::iterate!(
            $n,
            $crate::type_list::Empty,
            |<List: TypeList>| $crate::type_list::NonEmpty<$Item, List>,
        )
    );
}

pub use type_list;

#[cfg(test)]
mod tests {
    use enum_trait::const_test;

    use super::*;

    type EmptyTypeList = type_list![];
    type TwoItemTypeList = type_list![&'static str, u8];
    type ThreeItemTypeList = type_list![bool; 3];

    #[const_test]
    const fn properties() {
        // Note: Currently `assert_eq!` cannot be used in const fns.
        assert!(<EmptyTypeList as TypeList>::IsEmpty::VALUE);
        assert!(<EmptyTypeList as TypeList>::Len::VALUE == 0);
        assert!(!<TwoItemTypeList as TypeList>::IsEmpty::VALUE);
        assert!(<TwoItemTypeList as TypeList>::Len::VALUE == 2);
        assert!(!<ThreeItemTypeList as TypeList>::IsEmpty::VALUE);
        assert!(<ThreeItemTypeList as TypeList>::Len::VALUE == 3);
    }

    #[const_test]
    const fn getters() {
        let _: <TwoItemTypeList as TypeList>::Get<meta_num!(0)> = "test";
        let _: <TwoItemTypeList as TypeList>::Get<meta_num!(1)> = 42;

        let _: <TwoItemTypeList as TypeList>::GetOr<meta_num!(0), bool> = "test";
        let _: <TwoItemTypeList as TypeList>::GetOr<meta_num!(1), bool> = 42;
        let _: <TwoItemTypeList as TypeList>::GetOr<meta_num!(2), bool> = true;
    }
}
