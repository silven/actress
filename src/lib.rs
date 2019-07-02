#![feature(async_await, arbitrary_self_types, specialization)]
#![allow(unused_imports)]

mod actor;
mod mailbox;
mod response;
mod system;

pub use crate::{
    mailbox::Mailbox,
    actor::{Actor, ActorContext, Handle, Message},
    response::{AsyncResponse, SyncResponse},
    system::{System, Supervisor, PanicData},
};
