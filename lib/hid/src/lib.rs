#![feature(variant_count)]
#![cfg_attr(not(test), no_std)]

pub mod fstreams;
pub mod keyboard;
pub mod query;

const KEYBOARD: u64 = 0;
const RODENT: u64 = 1;
