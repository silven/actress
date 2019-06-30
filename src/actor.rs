use crate::mailbox::Mailbox;
use crate::system::SystemContext;

use futures::future::FutureObj;
use tokio_sync::oneshot;
use tokio_threadpool::Sender;
use std::future::Future;
use std::pin::Pin;
use std::marker::PhantomData;

use crate::response::Response;

pub trait Message: 'static {
    type Result;
}

pub trait Handle<M>
where
    Self: Actor,
    M: Message,
{
    type Response: Response<M>;

    fn accept(&mut self, msg: M, cx: &mut ActorContext) -> Self::Response;
}

pub enum BacklogPolicy {
    Flush,
    Reject,
}

pub trait Actor: Send + 'static {
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

pub struct ActorContext {
    id: usize,
    state: ActorState,
    pub(crate) system: SystemContext,
}

impl ActorContext {
    pub(crate) fn new(id: usize, system: SystemContext) -> Self {
        ActorContext {
            id: id,
            state: ActorState::Started,
            system: system,
        }
    }

    pub fn id(&self) -> usize {
        self.id
    }

    pub(crate) fn is_stopping(&self) -> bool {
        self.state == ActorState::Stopping
    }

    pub fn spawn_actor<A>(&mut self, actor: A) -> Option<Mailbox<A>>
    where
        A: Actor,
    {
        match self.system.spawn_actor(actor) {
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
