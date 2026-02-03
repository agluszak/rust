//@ run-pass
//@ check-run-results

#![feature(type_info)]
#![allow(dead_code)]

use std::mem::type_info::Type;

// Unit struct
struct UnitStruct;

// Tuple struct
struct TupleStruct(u32, String);

// Named struct
struct NamedStruct {
    a: u32,
    b: String,
}

// Enum with different variant kinds
enum MyEnum {
    Unit,
    Tuple(u32),
    Named { x: i32, y: i32 },
}

// Union
union MyUnion {
    a: u32,
    b: f32,
}

macro_rules! dump_types {
    ($($ty:ty),+ $(,)?) => {
        $(println!("{:#?}", const { Type::of::<$ty>() });)+
    };
}

fn main() {
    dump_types! {
        UnitStruct,
        TupleStruct,
        NamedStruct,
        MyEnum,
        MyUnion,
    }
}
