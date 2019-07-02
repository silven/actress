use crate::mailbox::Mailbox;
use crate::system::{SystemContext, ActorBundle, Supervises};

use futures::future::FutureObj;
use std::future::Future;
use std::marker::PhantomData;
use std::pin::Pin;
use tokio_sync::{oneshot, mpsc};
use tokio_threadpool::Sender;

use crate::response::Response;
use std::sync::{Arc, Mutex};
use crate::Supervisor;
use std::collections::HashMap;

pub trait Message: Send + 'static {
    type Result;
}

pub trait Handle<M>
where
    Self: Actor,
    M: Message,
{
    type Response: Response<M>;

    fn accept(&mut self, msg: M, cx: &mut ActorContext<Self>) -> Self::Response;
}

/// Decide what to do in the event of being stopped before the message backlog is empty
pub enum BacklogPolicy {
    Flush,
    Reject,
}

// TODO: Why did adding <A> to ActorContext introduce a requirement on Sized in Handle<M>?
pub trait Actor: Sized + Send + 'static {
    fn starting(&mut self) {}
    fn started(&mut self) {}

    fn stopping(&mut self) {}
    fn stopped(&mut self) {}

    fn backlog_policy(&self) -> BacklogPolicy {
        BacklogPolicy::Flush
    }
}

#[derive(PartialEq)]
pub(crate) enum ActorState {
    Started,
    Stopping,
    Stopped,
}

pub struct ActorContext<A> where A: Actor {
    id: usize,
    state: ActorState,
    mailbox: Mailbox<A>,
    pub(crate) system: SystemContext,
}

impl<Me> ActorContext<Me> where Me: Actor {
    pub(crate) fn new(id: usize, mailbox: Mailbox<Me>, system: SystemContext) -> Self {
        ActorContext {
            id: id,
            state: ActorState::Started,
            mailbox: mailbox,
            system: system,
        }
    }

    pub fn id(&self) -> usize {
        self.id
    }

    pub(crate) fn is_stopping(&self) -> bool {
        self.state == ActorState::Stopping
    }

    pub fn mailbox(&self) -> Mailbox<Me> {
        self.mailbox.copy()
    }

    pub fn spawn_actor<A>(&mut self, actor: A) -> Option<Mailbox<A>>
    where
        A: Actor,
    {
        match self.system.spawn_actor(actor, None) {
            Ok(mailbox) => Some(mailbox),
            Err(_) => None,
        }
    }

    pub fn spawn_child<W>(&mut self, actor: W) -> Option<Mailbox<W>>
        where
            W: Actor,
            Me: Supervisor<W>,
    {
        match self.system.spawn_actor(actor, Some(Box::new(self.mailbox()))) {
            Ok(mailbox) => Some(mailbox),
            Err(_) => None,
        }
    }

    pub fn register<A>(&mut self, name: &str, actor: A)
    where
        A: Actor,
    {
        self.system.register(name, actor);
    }

    pub fn find<A>(&self, name: &str) -> Option<Mailbox<A>>
    where
        A: Actor,
    {
        self.system.find(name)
    }

    pub fn stop(&mut self) {
        self.state = ActorState::Stopping
    }
}
