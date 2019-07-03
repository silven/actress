#![feature(async_await, arbitrary_self_types, specialization)]

mod actor;
mod mailbox;
mod response;
mod system;
mod internal_handlers;
mod supervisor;
mod system_context;

pub use crate::{
    mailbox::Mailbox,
    actor::{Actor, ActorContext, Handle, Message},
    response::{AsyncResponse, SyncResponse},
    system::{System},
    supervisor::{Supervisor, PanicData},
};
