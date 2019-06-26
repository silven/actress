
use std::any::{Any, TypeId};
use std::collections::HashMap;
use std::io;
use std::rc::Rc;

use futures::{Async, Poll};

type AnyMap = HashMap<TypeId, Arc<Any + Send + Sync>>;


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


use crate::actor::{Actor, ActorState, ActorContext, Handle, Message};

use crate::mailbox::{Mailbox, Envelope, EnvelopeProxy, PeekGrab};

pub(crate) struct System {
    started: bool,
    threadpool: ThreadPool,
    context: SystemContext,
}

impl<A> Future for ActorBundle<A>
    where
        A: Actor,
{
    type Item = ();
    type Error = ();

    fn poll(&mut self) -> Poll<Self::Item, Self::Error> {
        loop {
            // TODO: this loop shouldnt have to be here?
            match self.recv.poll() {
                Ok(Async::Ready(Some(mut msg))) => {
                    let _ = msg.accept(self);
                    if self.inner.is_stopping() {
                        // Is this how we stop?
                        self.recv.close();
                        break Ok(Async::Ready(()));
                    }
                }

                Ok(Async::Ready(None)) => return Ok(Async::Ready(())),
                _ => return Ok(Async::NotReady),
            }
        }
    }
}


pub(crate) struct ActorBundle<A: Actor> {
    pub(crate) actor: A,
    recv: mpsc::UnboundedReceiver<Envelope<A>>,
    pub(crate) inner: ActorContext,
    listeners: Arc<Mutex<AnyMap>>, // Mailboxes have Weak-pointers to this field
    //children: Vec<Weak<dyn ActorContainer>>,
}

impl<A> ActorBundle<A>
    where
        A: Actor,
{
    pub(crate) fn get_listener<M>(&self) -> Option<Arc<PeekGrab<M>>>
        where
            A: Handle<M>,
            M: Message,
    {
        if let Ok(dict) = self.listeners.lock() {
            dict.get(&TypeId::of::<M>())
                .and_then(|arc| arc.clone().downcast::<PeekGrab<M>>().ok())
        } else {
            None
        }
    }
}


#[derive(Clone)]
pub(crate) struct SystemContext {
    spawner: Sender,
    registry: Arc<Mutex<HashMap<String, Box<dyn Any + Send + 'static>>>>,
}

unsafe impl Send for SystemContext {}

impl SystemContext {
    pub(crate) fn new(spawner: Sender) -> Self {
        SystemContext {
            spawner: spawner,
            registry: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub(crate) fn register<A>(&mut self, name: &str, actor: A)
        where
            A: Actor,
    {
        let mailbox = self.spawn_actor(actor).unwrap();

        if let Ok(mut registry) = self.registry.lock() {
            registry.insert(name.to_string(), Box::new(mailbox));
        }
    }

    pub(crate) fn find<A>(&self, name: &str) -> Option<Mailbox<A>>
        where
            A: Actor,
    {
        if let Ok(registry) = self.registry.lock() {
            if let Some(anybox) = registry.get(name) {
                if let Some(mailbox) = anybox.downcast_ref::<Mailbox<A>>() {
                    return Some(mailbox.copy());
                }
            }
        }
        None
    }

    pub(crate) fn spawn_future<F: Future<Item = (), Error = ()> + Send + 'static>(&self, fut: F) -> bool {
        self.spawner.spawn(fut).is_ok()
    }

    pub(crate) fn spawn_actor<'s, 'a, A: 'a>(&self, actor: A) -> Result<Mailbox<A>, ()>
        where
            A: Actor,
    {
        let (tx, rx) = mpsc::unbounded_channel();

        let listeners = Arc::new(Mutex::new(HashMap::new()));
        let mailbox = Mailbox::<A>::new(tx, Arc::downgrade(&listeners));

        let mut bundle = ActorBundle {
            actor: actor,
            recv: rx,
            listeners: listeners,
            inner: ActorContext::new(self.clone()),
        };

        Actor::started(&mut bundle.actor);

        match self.spawn_future(bundle) {
            true => Ok(mailbox),
            false => Err(()),
        }
    }
}

#[derive(Clone)]
enum SystemMessage {
    Stop,
}

impl Message for SystemMessage {
    type Result = ();
}

impl Handle<SystemMessage> for Actor {
    fn accept(&mut self, msg: SystemMessage, cx: &mut ActorContext) -> () {
        cx.stop()
    }
}

impl System {
    pub fn new() -> Self {
        let pool = ThreadPool::new();
        let spawner = pool.sender().clone();
        System {
            started: false,
            threadpool: pool,
            context: SystemContext::new(spawner),
        }
    }

    pub fn start<A>(&mut self, actor: A) -> Mailbox<A>
        where
            A: Actor,
    {
        self.context.spawn_actor(actor).unwrap()
    }

    pub fn register<A>(&mut self, name: &str, actor: A)
        where
            A: Actor,
    {
        self.context.register(name, actor);
    }

    pub fn find<A>(&self, name: &str) -> Option<Mailbox<A>>
        where
            A: Actor,
    {
        self.context.find(name)
    }

    pub fn run_until_completion(mut self) {
        println!("Waiting for system to stop...");
        self.threadpool.shutdown_on_idle().wait().unwrap();
        println!("Done with system?");
    }

    pub fn spawn_future<F: Future<Item = (), Error = ()> + Send + 'static>(&self, fut: F) {
        self.context.spawn_future(fut);
    }
}
