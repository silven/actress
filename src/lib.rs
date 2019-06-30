#![feature(async_await, arbitrary_self_types, specialization)]
#![allow(unused_imports)]

mod actor;
mod mailbox;
mod response;
mod system;

pub use crate::{
    actor::{Actor, ActorContext, Handle, Message},
    system::System,
    response::{AsyncResponse, SyncResponse},
};
