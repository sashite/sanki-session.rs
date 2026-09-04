#![doc = include_str!("../README.md")]
#![forbid(unsafe_code)]
#![cfg_attr(not(test), warn(missing_docs))]

pub mod event;
pub mod implicit;
pub mod natural_state;
pub mod selection;
pub mod session;
pub mod timing;
pub mod verdict;
