//@ run-pass
//@ check-run-results

#![feature(type_info)]
#![allow(dead_code)]

use std::mem::type_info::Type;

// Simple named struct
struct Point {
    x: i32,
    y: i32,
}

// Nested struct
struct Rectangle {
    top_left: Point,
    bottom_right: Point,
}

// Generic struct
struct Container<T> {
    value: T,
}

// Struct with multiple field types
struct Complex {
    id: u64,
    name: &'static str,
    data: [u8; 4],
    optional: Option<i32>,
}

// Empty struct with no fields
struct EmptyNamed {}

macro_rules! dump_types {
    ($($ty:ty),+ $(,)?) => {
        $(println!("{:#?}", const { Type::of::<$ty>() });)+
    };
}

fn main() {
    dump_types! {
        Point,
        Rectangle,
        Container<u32>,
        Complex,
        EmptyNamed,
    }
}
