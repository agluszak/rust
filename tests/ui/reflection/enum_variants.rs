//@ run-pass
//@ check-run-results

#![feature(type_info)]
#![allow(dead_code)]

use std::mem::type_info::Type;

enum E {
    S { r: u8, g: u8, b: u8 },
    T(u8, u8),
}

macro_rules! dump_types {
    ($($ty:ty),+ $(,)?) => {
        $(println!("{:#?}", const { Type::of::<$ty>() });)+
    };
}

fn main() {
    dump_types! {
        E,
    }
}
