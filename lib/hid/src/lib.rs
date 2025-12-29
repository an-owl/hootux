#![feature(variant_count)]
#![cfg_attr(not(test), no_std)]

pub mod fstreams;
pub mod keyboard;
pub mod query;

pub const KEYBOARD: u32 = 0;
pub const RODENT: u32 = 1;
