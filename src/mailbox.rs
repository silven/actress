use std::any::{Any, TypeId};
use std::collections::HashMap;
use std::marker::PhantomData;
use std::sync::{Arc, Mutex, Weak};

use futures::executor::block_on;
use tokio_sync::{mpsc, oneshot};

use crate::actor::{Actor, Handle, Message};
use crate::response::Response;
use crate::system::ActorBundle;

type AnyMap = HashMap<TypeId, Arc<dyn Any + Send + Sync>>;

pub(crate) trait EnvelopeProxy {
    type Actor: Actor;

    fn accept(&mut self, actor: &mut ActorBundle<Self::Actor>);
    fn reject(&mut self);
}

pub(crate) struct Envelope<A: Actor>(Box<dyn EnvelopeProxy<Actor = A>>);

// This is safe because the Actor isn't really there.
unsafe impl<A> Send for Envelope<A> where A: Actor {}
unsafe impl<A> Sync for Envelope<A> where A: Actor {}

struct EnvelopeInner<A, M>
where
    M: Message,
    A: Actor + Handle<M>,
{
    act: PhantomData<*const A>,
    msg: Option<M>,
    reply: Option<oneshot::Sender<Option<M::Result>>>,
}

#[cfg(feature = "peek")]
pub(crate) enum PeekGrab<M: Message> {
    Peek(Box<dyn Fn(&M) + Send + Sync + 'static>),
    Alter(Box<dyn Fn(M) -> M + Send + Sync + 'static>),
    Grab(Box<dyn Fn(M) -> M::Result + Send + Sync + 'static>),
}

impl<A, M> EnvelopeProxy for EnvelopeInner<A, M>
where
    A: Actor + Handle<M>,
    M: Message + Send + Sync,
    M::Result: Send + Sync,
{
    type Actor = A;

    fn accept(&mut self, actor: &mut ActorBundle<Self::Actor>) {
        let mut msg = self.msg.take().unwrap();

        #[cfg(feature = "peek")]
        {
            if let Some(arc) = actor.get_listener::<M>() {
                match *arc {
                    PeekGrab::Peek(ref peek) => {
                        peek(&msg);
                    }
                    PeekGrab::Alter(ref alter) => {
                        msg = alter(msg);
                    }
                    PeekGrab::Grab(ref grab) => {
                        let result = grab(msg);
                        if let Some(tx) = self.reply.take() {
                            tx.send(Some(result));
                        }
                        return;
                    }
                }
            }
        }

        let result = <Self::Actor as Handle<M>>::accept(&mut actor.actor, msg, &mut actor.inner);
        println!("Got a result from the actor");
        result.handle(actor.inner.system.spawner.clone(), self.reply.take());
    }

    fn reject(&mut self) {
        if let Some(tx) = self.reply.take() {
            tx.send(None); // TODO; What to do if we can't send a reply?
        }
    }
}

impl<A> Envelope<A>
where
    A: Actor,
{
    fn new<M>(msg: M) -> Self
    where
        A: Handle<M>,
        M: Message + Send + Sync,
        M::Result: Send + Sync,
    {
        Envelope(Box::new(EnvelopeInner {
            act: PhantomData,
            msg: Some(msg),
            reply: None,
        }))
    }

    fn with_reply<M>(msg: M, reply_to: oneshot::Sender<Option<M::Result>>) -> Self
    where
        A: Handle<M>,
        M: Message + Send + Sync,
        M::Result: Send + Sync,
    {
        Envelope(Box::new(EnvelopeInner {
            act: PhantomData,
            msg: Some(msg),
            reply: Some(reply_to),
        }))
    }
}

impl<A> EnvelopeProxy for Envelope<A>
where
    A: Actor,
{
    type Actor = A;

    fn accept(&mut self, actor: &mut ActorBundle<Self::Actor>) {
        self.0.accept(actor);
    }

    fn reject(&mut self) {
        self.0.reject();
    }
}

pub struct Mailbox<A>
where
    A: Actor,
{
    actor_id: usize,
    tx: mpsc::UnboundedSender<Envelope<A>>,
    #[cfg(feature = "peek")]
    listeners: Weak<Mutex<AnyMap>>,
}

#[derive(Debug)]
pub enum MailboxSendError {
    CouldNotSend,
}

#[derive(Debug)]
pub enum MailboxAskError {
    CouldNotSend,
    CouldNotRecv,
    MessageDropped,
}

impl<A> Mailbox<A>
where
    A: Actor,
{
    #[cfg(feature = "peek")]
    pub(crate) fn new(
        actor_id: usize,
        inbox: mpsc::UnboundedSender<Envelope<A>>,
        listeners: Weak<Mutex<AnyMap>>,
    ) -> Self {
        Mailbox {
            actor_id: actor_id,
            tx: inbox,
            listeners: listeners,
        }
    }

    pub fn id(&self) -> usize {
        self.actor_id
    }

    #[cfg(not(feature = "peek"))]
    pub(crate) fn new(actor_id: usize, inbox: mpsc::UnboundedSender<Envelope<A>>) -> Self {
        Mailbox {
            actor_id: actor_id,
            tx: inbox,
        }
    }

    #[cfg(feature = "peek")]
    pub fn copy(&self) -> Self {
        Mailbox {
            actor_id: self.actor_id,
            tx: self.tx.clone(),
            listeners: self.listeners.clone(),
        }
    }

    #[cfg(not(feature = "peek"))]
    pub fn copy(&self) -> Self {
        Mailbox {
            actor_id: self.actor_id,
            tx: self.tx.clone(),
        }
    }

    pub fn send<M>(&self, msg: M) -> Result<(), MailboxSendError>
    where
        A: Actor + Handle<M>,
        M: Message + Send + Sync,
        M::Result: Send + Sync,
    {
        let env = Envelope::new(msg);
        let mut tx = self.tx.clone();
        match tx.try_send(env) {
            Ok(()) => Ok(()),
            Err(_) => Err(MailboxSendError::CouldNotSend),
        }
    }

    #[cfg(feature = "peek")]
    pub fn peek<M, F>(&self, handler: F)
    where
        A: Actor + Handle<M>,
        M: Message,
        F: Fn(&M) + Send + Sync + 'static,
    {
        self.add_listener(PeekGrab::Peek(Box::new(handler)));
    }

    #[cfg(feature = "peek")]
    pub fn alter<M, F>(&self, handler: F)
    where
        A: Actor + Handle<M>,
        M: Message,
        F: Fn(M) -> M + Send + Sync + 'static,
    {
        self.add_listener(PeekGrab::Alter(Box::new(handler)));
    }

    #[cfg(feature = "peek")]
    pub fn grab<M, F>(&self, handler: F)
    where
        A: Actor + Handle<M>,
        M: Message,
        F: Fn(M) -> M::Result + Send + Sync + 'static,
    {
        self.add_listener(PeekGrab::Grab(Box::new(handler)));
    }

    #[cfg(feature = "peek")]
    pub fn clear_listener<M>(&self)
    where
        A: Actor + Handle<M>,
        M: Message,
    {
        if let Some(arc) = self.listeners.upgrade() {
            if let Ok(mut map) = arc.lock() {
                map.remove(&TypeId::of::<M>());
            }
        }
    }

    #[cfg(feature = "peek")]
    fn add_listener<M>(&self, listener: PeekGrab<M>)
    where
        A: Actor + Handle<M>,
        M: Message,
    {
        if let Some(arc) = self.listeners.upgrade() {
            if let Ok(mut map) = arc.lock() {
                map.insert(TypeId::of::<M>(), Arc::new(listener));
            }
        }
    }

    pub fn ask<M>(&self, msg: M) -> Result<M::Result, MailboxAskError>
    where
        A: Actor + Handle<M>,
        M: Message + Send + Sync,
        M::Result: Send + Sync,
    {
        let (reply_tx, reply_rx) = oneshot::channel();
        let env = Envelope::with_reply(msg, reply_tx);
        let mut request_tx = self.tx.clone();

        return match request_tx.try_send(env) {
            Ok(()) => match block_on(reply_rx) {
                Ok(Some(response)) => Ok(response),
                Ok(None) => Err(MailboxAskError::MessageDropped),
                Err(_) => Err(MailboxAskError::CouldNotRecv),
            },
            Err(_) => Err(MailboxAskError::CouldNotSend),
        };
    }

    pub(crate) fn ask_future<M>(
        &self,
        msg: M,
    ) -> Result<oneshot::Receiver<Option<M::Result>>, MailboxAskError>
    where
        A: Actor + Handle<M>,
        M: Message + Send + Sync,
        M::Result: Send + Sync,
    {
        let (reply_tx, reply_rx) = oneshot::channel();
        let env = Envelope::with_reply(msg, reply_tx);
        let mut request_tx = self.tx.clone();

        return match request_tx.try_send(env) {
            Ok(()) => Ok(reply_rx),
            Err(_) => Err(MailboxAskError::CouldNotSend),
        };
    }

    pub async fn ask_async<M>(&self, msg: M) -> Result<M::Result, MailboxAskError>
    where
        A: Actor + Handle<M>,
        M: Message + Send + Sync,
        M::Result: Send + Sync,
    {
        let (reply_tx, reply_rx) = oneshot::channel();
        let env = Envelope::with_reply(msg, reply_tx);
        let mut request_tx = self.tx.clone();

        return match request_tx.try_send(env) {
            Ok(()) => match reply_rx.await {
                Ok(Some(response)) => Ok(response),
                Ok(None) => Err(MailboxAskError::MessageDropped),
                Err(_) => Err(MailboxAskError::CouldNotRecv),
            },
            Err(_) => Err(MailboxAskError::CouldNotSend),
        };
    }
}
