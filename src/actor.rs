use crate::mailbox::Mailbox;
use crate::system::SystemContext;

use tokio_sync::oneshot;
use tokio_threadpool::Sender;
use futures::future::{FutureObj};

pub trait Message: 'static {
    type Result;
}

pub trait Response<M: Message> {
    fn handle(self, spawner: Sender, reply_to: Option<oneshot::Sender<Option<M::Result>>>);
}

impl<M: Message> Response<M> for std::pin::Pin<Box<dyn std::future::Future<Output=M::Result> + Send>>
where
    M::Result: Send,
{
    fn handle(self, spawner: Sender, reply_to: Option<oneshot::Sender<Option<M::Result>>>) {
        println!("This is the future!");

        spawner.spawn(async move {
            let result: <M as Message>::Result = self.await;
            if let Some(tx) = reply_to {
                tx.send(Some(result));
            }
        });
    }
}

macro_rules! simple_result {
    ($type:ty) => {
        impl<M: Message> Response<M> for $type
        where
            M: Message<Result = $type>,
        {
            fn handle(self, _: Sender, reply_to: Option<oneshot::Sender<Option<M::Result>>>) {
                if let Some(tx) = reply_to {
                    tx.send(Some(self));
                }
            }
        }
    };
}

simple_result!(());
simple_result!(u64);

/*
impl<M, T> Response<M> for T
    where
        M: Message<Result=Self>
{
    fn handle(self, reply_to: Option<oneshot::Sender<Option<M::Result>>>) {
        if let Some(tx) = reply_to {
            tx.send(Some(self));
        }
    }
}
*/


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
