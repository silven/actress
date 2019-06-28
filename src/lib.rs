#![feature(arbitrary_self_types)]

pub mod actor;
mod mailbox;
pub mod system;

pub use crate::{
    actor::{Actor, ActorContext, Handle, Message},
    system::System,
};
