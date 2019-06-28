use std::any::{Any, TypeId};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use futures::future::Future;
use futures::executor::block_on;
use futures::stream::StreamExt;

use futures::Poll;

use tokio_sync::mpsc;

use tokio_threadpool::{Sender, ThreadPool};

use mopa;
use mopa::mopafy;

use crate::actor::{Actor, ActorContext, BacklogPolicy, Handle, Message};
use crate::mailbox::{Envelope, EnvelopeProxy, Mailbox, PeekGrab};

use std::task::Context;

type AnyArcMap = HashMap<TypeId, Arc<dyn Any + Send + Sync>>;

pub struct System {
    threadpool: ThreadPool,
    context: SystemContext,
}

// TODO: Is this safe? It should be, the actor bundle itself should never move.
impl<A> Unpin for ActorBundle<A> where A: Actor {}

impl<A> Future for ActorBundle<A>
where
    A: Actor,
{
    type Output = ();

    fn poll(mut self: std::pin::Pin<&mut Self>, cx: &mut Context) -> Poll<()> {
        // TODO: this loop shouldn't have to be here?
        loop {
            if self.recv.is_none() {
                panic!("Poll called after channel closed!");
            }

            match self.recv.as_mut().unwrap().poll_recv(cx) {
                Poll::Ready(Some(mut msg)) => {
                    msg.accept(&mut self);
                    if self.inner.is_stopping() {
                        // Is this how we stop?
                        let mut rx = self.recv.take().unwrap();
                        rx.close();

                        // Only blocks a finite amount of time, since the channel is closed.
                        let backlog: Vec<_> = block_on(rx.collect());
                        println!("Stopping actor {} with {} messages left in backlog", self.inner.id(), backlog.len());

                        // Bikeshedding name of this mechanism
                        match self.actor.backlog_policy() {
                            BacklogPolicy::Reject => backlog.into_iter()
                                .for_each(|mut m| m.reject()),
                            BacklogPolicy::Flush => backlog.into_iter()
                                .for_each(|mut m| m.accept(&mut self)),
                        };

                        return Poll::Ready(());
                    }
                }

                Poll::Ready(None) => return Poll::Ready(()),
                _ => return Poll::Pending,
            }
        }
    }
}

pub(crate) struct ActorBundle<A: Actor> {
    pub(crate) actor: A,
    pub(crate) inner: ActorContext,

    recv: Option<mpsc::UnboundedReceiver<Envelope<A>>>,
    listeners: Arc<Mutex<AnyArcMap>>, // Mailboxes have Weak-pointers to this field
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
    registry: Arc<Mutex<HashMap<String, Box<dyn AcceptsSystemMessage>>>>,
    id_counter: usize,
}

impl SystemContext {
    pub(crate) fn new(spawner: Sender) -> Self {
        SystemContext {
            spawner: spawner,
            registry: Arc::new(Mutex::new(HashMap::new())),
            id_counter: 0,
        }
    }

    pub(crate) fn register<A>(&mut self, name: &str, actor: A) -> Mailbox<A>
    where
        A: Actor,
    {
        let mailbox = self.spawn_actor(actor).unwrap();

        if let Ok(mut registry) = self.registry.lock() {
            registry.insert(name.to_string(), Box::new(mailbox.copy()));
        }

        mailbox
    }

    pub(crate) fn find<A>(&self, name: &str) -> Option<Mailbox<A>>
    where
        A: Actor,
    {
        if let Ok(registry) = self.registry.lock() {
            if let Some(mailboxbox) = registry.get(name) {
                if let Some(mailbox) = mailboxbox.downcast_ref::<Mailbox<A>>() {
                    return Some(mailbox.copy());
                }
            }
        }
        None
    }

    pub(crate) fn spawn_future<F: Future<Output=()> + Send + 'static>(
        &self,
        fut: F,
    ) -> bool {
        self.spawner.spawn(fut).is_ok()
    }

    pub(crate) fn spawn_actor<A>(&mut self, actor: A) -> Result<Mailbox<A>, ()>
    where
        A: Actor,
    {
        let (tx, rx) = mpsc::unbounded_channel();

        let listeners = Arc::new(Mutex::new(HashMap::new()));
        let mailbox = Mailbox::<A>::new(tx, Arc::downgrade(&listeners));

        self.id_counter += 1;
        let mut bundle = ActorBundle {
            actor: actor,
            recv: Some(rx),
            listeners: listeners,
            inner: ActorContext::new(self.id_counter, self.clone()),
        };

        Actor::started(&mut bundle.actor);

        match self.spawn_future(bundle) {
            true => Ok(mailbox),
            false => Err(()),
        }
    }
}

#[derive(Clone)]
pub enum SystemMessage {
    Stop,
}

impl Message for SystemMessage {
    type Result = ();
}

impl<A> Handle<SystemMessage> for A where A: Actor {
    fn accept(&mut self, msg: SystemMessage, cx: &mut ActorContext) {
        println!("Actor {} handling system message.", cx.id());
        match msg {
            SystemMessage::Stop => { cx.stop() },
        }
    }
}

// Kind-of-Hack to be able to send certain generic messages to all mailboxes
trait AcceptsSystemMessage: mopa::Any + Send + 'static {
    fn system_message(&self, msg: SystemMessage);
}
mopafy!(AcceptsSystemMessage);


impl<A> AcceptsSystemMessage for Mailbox<A> where A: Actor {
    fn system_message(&self, msg: SystemMessage) {
        self.send(msg);
    }
}

impl System {
    pub fn new() -> Self {
        let pool = ThreadPool::new();
        let spawner = pool.sender().clone();
        System {
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

    pub fn register<A>(&mut self, name: &str, actor: A) -> Mailbox<A>
    where
        A: Actor,
    {
        self.context.register(name, actor)
    }

    pub fn find<A>(&self, name: &str) -> Option<Mailbox<A>>
    where
        A: Actor,
    {
        self.context.find(name)
    }

    pub fn run_until_completion(self) {
        println!("Waiting for system to stop...");

        match self.context.registry.lock() {
            Ok(registry ) => {
                for service in registry.values() {
                    service.system_message(SystemMessage::Stop);
                }
            },
            Err(_) => panic!("Could not terminate services..."),
        }

        self.threadpool.shutdown_on_idle().wait();
        println!("Done with system?");
    }

    pub fn spawn_future<F: Future<Output=()> + Send + 'static>(&self, fut: F) {
        self.context.spawn_future(fut);
    }
}
