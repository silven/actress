
use std::any::{Any, TypeId};
use std::collections::HashMap;
use std::io;
use std::rc::Rc;

use futures::{Async, Poll};

use futures::executor;
use futures::future::lazy;
use futures::future::Future;
use futures::stream::Stream;
use futures::task::Spawn;

use tokio_sync::{mpsc, oneshot};
use tokio_threadpool::{Sender, ThreadPool};

use std::pin::Pin;
use std::task::Context;
use std::time::Duration;

use std::marker::PhantomData;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, Weak};


use crate::actor::{Actor, ActorContext, Message, Handle};

use crate::system::{ActorBundle};

pub(crate) trait EnvelopeProxy {
    type Actor: Actor;

    fn accept(&mut self, actor: &mut ActorBundle<Self::Actor>);
}

pub(crate) struct Envelope<A: Actor>(Box<EnvelopeProxy<Actor = A>>);

unsafe impl<A> Send for Envelope<A> where A: Actor {}

struct EnvelopeInner<A, M>
    where
        M: Message,
        A: Actor + Handle<M>,
{
    act: PhantomData<*const A>,
    msg: Option<M>,
    reply: Option<oneshot::Sender<M::Result>>,
}

pub(crate) enum PeekGrab<M: Message> {
    Peek(Box<Fn(&M) + Send + Sync + 'static>),
    Alter(Box<Fn(M) -> M + Send + Sync + 'static>),
    Grab(Box<Fn(M) -> M::Result + Send + Sync + 'static>),
}

impl<A, M> EnvelopeProxy for EnvelopeInner<A, M>
    where
        A: Actor + Handle<M>,
        M: Message,
{
    type Actor = A;

    fn accept(&mut self, actor: &mut ActorBundle<A>) {
        let mut msg = self.msg.take().unwrap();
        let listener = actor.get_listener::<M>();

        if let Some(arc) = listener {
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
                        tx.send(result); // TODO; What to do if we can't send a reply?
                    }
                    return;
                }
            }
        }

        let result = <Self::Actor as Handle<M>>::accept(&mut actor.actor, msg, &mut actor.inner);

        if let Some(tx) = self.reply.take() {
            tx.send(result); // TODO; What to do if we can't send a reply?
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
            M: Message,
    {
        Envelope(Box::new(EnvelopeInner {
            act: PhantomData,
            msg: Some(msg),
            reply: None,
        }))
    }

    fn with_reply<M>(msg: M, reply_to: oneshot::Sender<M::Result>) -> Self
        where
            A: Handle<M>,
            M: Message,
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
}


type AnyMap = HashMap<TypeId, Arc<Any + Send + Sync>>;

pub(crate) struct Mailbox<A>
    where
        A: Actor,
{
    tx: mpsc::UnboundedSender<Envelope<A>>,
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
}

impl<A> Mailbox<A>
    where
        A: Actor,
{
    pub(crate) fn new(inbox: mpsc::UnboundedSender<Envelope<A>>, listeners: Weak<Mutex<AnyMap>>) -> Self {
        Mailbox {
            tx: inbox,
            listeners: listeners,
        }
    }

    // Can't use Clone for &Mailbox due to blanket impl
    pub(crate) fn copy(&self) -> Self {
        Mailbox {
            tx: self.tx.clone(),
            listeners: self.listeners.clone(),
        }
    }

    pub fn send<M>(&self, msg: M) -> Result<(), MailboxSendError>
        where
            A: Actor + Handle<M>,
            M: Message,
    {
        let env = Envelope::new(msg);
        let mut tx = self.tx.clone();
        match tx.try_send(env) {
            Ok(()) => Ok(()),
            Err(_) => Err(MailboxSendError::CouldNotSend),
        }
    }

    pub fn peek<M, F>(&self, handler: F)
        where
            A: Actor + Handle<M>,
            M: Message,
            F: Fn(&M) + Send + Sync + 'static,
    {
        self.add_listener(PeekGrab::Peek(Box::new(handler)));
    }

    pub fn alter<M, F>(&self, handler: F)
        where
            A: Actor + Handle<M>,
            M: Message,
            F: Fn(M) -> M + Send + Sync + 'static,
    {
        self.add_listener(PeekGrab::Alter(Box::new(handler)));
    }

    pub fn grab<M, F>(&self, handler: F)
        where
            A: Actor + Handle<M>,
            M: Message,
            F: Fn(M) -> M::Result + Send + Sync + 'static,
    {
        self.add_listener(PeekGrab::Grab(Box::new(handler)));
    }

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
            M: Message,
    {
        let (tx, rx) = oneshot::channel();
        let env = Envelope::with_reply(msg, tx);
        let mut tx = self.tx.clone();

        return match tx.try_send(env) {
            Ok(()) => match rx.wait() {
                // TODO Can we do without the wait() call?
                Ok(response) => Ok(response),
                Err(_) => Err(MailboxAskError::CouldNotRecv),
            },
            Err(_) => Err(MailboxAskError::CouldNotSend),
        };
    }
}