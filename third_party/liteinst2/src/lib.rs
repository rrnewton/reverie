#![deny(warnings)]
#![doc = include_str!("../README.md")]
#![forbid(unsafe_op_in_unsafe_fn)]
#![warn(missing_docs)]

pub mod cache_line;
pub mod patcher;
pub mod planner;
pub mod probe;
pub mod rapid;
pub mod scanner;
pub mod trampoline;
mod trap;
