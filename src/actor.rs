

use crate::system::{SystemContext};
use crate::mailbox::{Mailbox};

pub trait Message: 'static {
    type Result;
}

pub trait Handle<M: Message> {
    fn accept(&mut self, msg: M, cx: &mut ActorContext) -> M::Result;
}

pub trait Actor: Send + 'static {
    fn starting(&mut self) {}
    fn started(&mut self) {}

    fn stopping(&mut self) {}
    fn stopped(&mut self) {}
}


#[derive(PartialEq)]
pub(crate) enum ActorState {
    Started,
    Stopping,
    Stopped,
}

pub struct ActorContext {
    state: ActorState,
    system: SystemContext,
}

impl ActorContext {
    pub(crate) fn new(system: SystemContext) -> Self {
        ActorContext {
            state: ActorState::Started,
            system: system,
        }
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

    fn register<A>(&mut self, name: &str, actor: A)
        where
            A: Actor,
    {
        self.system.register(name, actor);
    }

    fn find<A>(&self, name: &str) -> Option<Mailbox<A>>
        where
            A: Actor,
    {
        self.system.find(name)
    }

    pub(crate) fn is_stopping(&self) -> bool {
        self.state == ActorState::Stopping
    }

    pub fn stop(&mut self) {
        self.state = ActorState::Stopping
    }
}


