#![feature(async_await, arbitrary_self_types, weak_counts)]

pub mod actor;
mod mailbox;
pub mod system;

pub use crate::{
    actor::{Actor, ActorContext, Handle, Message},
    system::System,
};