#![feature(async_await, arbitrary_self_types, specialization)]
#![deny(unused_imports)]

mod actor;
mod http;
mod internal_handlers;
mod mailbox;
mod response;
mod supervisor;
mod system;
mod system_context;

pub use crate::{
    actor::{Actor, ActorContext, Handle, Message},
    mailbox::Mailbox,
    response::{AsyncResponse, SyncResponse},
    supervisor::{PanicData, Supervisor},
    system::System,
};
