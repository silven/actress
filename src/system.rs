use std::any::{Any, TypeId};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use futures::executor::block_on;
use futures::future::Future;
use futures::stream::StreamExt;

use futures::Poll;

use tokio_sync::mpsc;

use tokio_threadpool::{Sender, ThreadPool, Worker};

use crossbeam::atomic::AtomicCell;

use mopa;
use mopa::mopafy;

use crate::actor::{Actor, ActorContext, BacklogPolicy, Handle, Message};
use crate::mailbox::{Envelope, EnvelopeProxy, Mailbox, PeekGrab};

use std::task::Context;
use crate::actor::ActorState::Stopping;
use std::marker::PhantomData;
use std::panic::PanicInfo;
use std::rc::Rc;
use std::cell::Cell;
use std::sync::atomic::{AtomicPtr, Ordering};

type AnyArcMap = HashMap<TypeId, Arc<dyn Any + Send + Sync>>;

pub struct System {
    threadpool: ThreadPool,
    context: SystemContext,
}

#[derive(Debug)]
pub struct PanicData {
    message: Option<String>,
    file: String,
    line: u32,
    column: u32,
}

impl From<&std::panic::PanicInfo<'_>> for PanicData {
    fn from(value: &std::panic::PanicInfo) -> Self {
        // Current impl always returns Some
        let loc = value.location().unwrap();
        PanicData {
            message: value.payload().downcast_ref::<&str>().map(ToString::to_string),
            file: loc.file().to_owned(),
            line: loc.line(),
            column: loc.column(),
        }

    }
}

struct PanicHookGuard(Option<Box<dyn Fn(&PanicInfo) + Sync + Send + 'static>>);
impl Drop for PanicHookGuard {
    fn drop(&mut self) {
        // Never none
        std::panic::set_hook(self.0.take().unwrap());
    }
}

// TODO: Is this safe? It should be, the actor bundle itself should never move.
impl<A> Unpin for ActorBundle<A> where A: Actor {}

impl<A> Future for ActorBundle<A>
where
    A: Actor,
{
    type Output = ();

    fn poll(mut self: std::pin::Pin<&mut Self>, cx: &mut Context) -> Poll<()> {
        // Reset the hook after we're done
        let _hook_guard = PanicHookGuard(Some(std::panic::take_hook()));

        let sup_guard = self.supervisor.guard();
        let my_id = self.inner.id();
        std::panic::set_hook(Box::new(move |info| {
            println!("Inside panic hook!");
            if let Some(sup) = sup_guard.swap(None) {
                println!("Notifying sup about worker crash!");
                sup.notify_worker_stopped(my_id, Some(From::from(info)));
            };
        }));

        // TODO: this loop shouldn't have to be here?
        loop {
            if self.recv.is_none() {
                panic!("Poll called after channel closed! This should never happen!");
            }

            match self.recv.as_mut().unwrap().poll_recv(cx) {
                Poll::Ready(Some(mut msg)) => {
                    // TODO: Is this really safe?
                    let process_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        msg.accept(&mut self)
                    }));

                    match process_result {
                        Ok(()) => { /* all is well */},
                        Err(_) => {
                            self.close_and_stop();
                            return Poll::Ready(());
                        },
                    }

                    if self.inner.is_stopping() {
                        // Only blocks a finite amount of time, since the rx channel is closed.
                        let backlog = self.close_and_stop();
                        println!(
                            "Stopping actor {} with {} messages left in backlog",
                            self.inner.id(),
                            backlog.len()
                        );

                        // Bikeshedding name of this mechanism
                        match self.actor.backlog_policy() {
                            BacklogPolicy::Reject => {
                                backlog.into_iter().for_each(|mut m| m.reject())
                            }
                            BacklogPolicy::Flush => {
                                backlog.into_iter().for_each(|mut m| m.accept(&mut self))
                            }
                        };
                        break;
                    }
                }
                Poll::Ready(None) => break,
                Poll::Pending => return Poll::Pending,
            }
        }

        Actor::stopped(&mut self.actor);
        return Poll::Ready(());
    }
}

pub(crate) struct ChildGuard<A>(Vec<Box<dyn SupervisedBy<A>>>) where A: Actor;

impl<A> ChildGuard<A> where A: Actor {
    pub(crate) fn new() -> Self {
        ChildGuard(Vec::new())
    }

    pub(crate) fn push<W>(&mut self, mailbox: Mailbox<W>) where A: Supervisor<W>, W: Actor {
        self.0.push(Box::new(mailbox));
    }
}

impl<A> Drop for ChildGuard<A> where A: Actor {
    fn drop(&mut self) {
        for child in &self.0 {
            child.notify_supervisor_stopped();
        }
    }
}

pub(crate) struct SupervisorGuard<A> where A: Actor { id: usize, inner: Arc<AtomicCell<Option<Box<dyn Supervises<A>>>>> }

impl<A> SupervisorGuard<A> where A: Actor {
    pub(crate) fn new(id: usize, sup: Option<Box<dyn Supervises<A>>>) -> Self {
        SupervisorGuard { id, inner: Arc::new(AtomicCell::new(sup)) }
    }

    pub(crate) fn is_some(&self) -> bool {
        let x = self.inner.swap(None);
        let some = x.is_some();
        self.inner.swap(x);
        some
    }

    pub(crate) fn guard(&self) -> Arc<AtomicCell<Option<Box<dyn Supervises<A>>>>> {
        self.inner.clone()
    }
}

impl<A> Drop for SupervisorGuard<A> where A: Actor {
    fn drop(&mut self) {
        println!("Dropping supervisor guard");
        if let Some(sup) = self.inner.swap(None) {
            println!("Notifying supvervisor that worker stopped");
            sup.notify_worker_stopped(self.id, None);
        }
    }
}

pub(crate) struct ActorBundle<A: Actor> {
    pub(crate) actor: A,
    pub(crate) inner: ActorContext<A>,

    supervisor: SupervisorGuard<A>,
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

    fn close_and_stop(&mut self) -> Vec<Envelope<A>> {
        let mut rx = self.recv.take().unwrap();
        rx.close();
        // This means your channel was closed. Do we want to allow for a way to resume?
        Actor::stopping(&mut self.actor);

        return block_on(rx.collect());
    }
}

#[derive(Clone)]
pub(crate) struct SystemContext {
    pub(crate) spawner: Sender,
    registry: Arc<Mutex<HashMap<String, Box<dyn StoppableActor>>>>,
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
        let mailbox = self.spawn_actor(actor, None).unwrap();

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

    pub(crate) fn spawn_future<F: Future<Output = ()> + Send + 'static>(&self, fut: F) -> bool {
        self.spawner.spawn(fut).is_ok() // TODO; Better way of handling spawn errors?
    }

    pub(crate) fn spawn_actor<A>(&mut self, actor: A, sup: Option<Box<dyn Supervises<A>>>) -> Result<Mailbox<A>, ()>
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
            supervisor: SupervisorGuard::new(self.id_counter, sup),
            inner: ActorContext::new(self.id_counter, mailbox.copy(), self.clone()),
        };

        // TODO; Figure out a way to move this into the true-branch below
        Actor::started(&mut bundle.actor);

        match self.spawn_future(bundle) {
            true => Ok(mailbox),
            false => Err(()),
        }
    }
}

struct StopActor;

impl Message for StopActor {
    type Result = ();
}

impl<A> Handle<StopActor> for A where A: Actor {
    type Response = ();
    fn accept(&mut self, _msg: StopActor, cx: &mut ActorContext<A>) {
        cx.stop();
    }
}

/// If there is panic data, there was a crash. If there is none, it stopped gracefully.
pub(crate) struct WorkerStopped<W: Actor>(usize, Option<PanicData>, PhantomData<*const W>);
unsafe impl<W> Send for WorkerStopped<W> where W: Actor {}

impl<W> Message for WorkerStopped<W> where W: Actor {
    type Result = ();
}

pub trait Supervisor<W>: Actor where W: Actor {
    fn worker_stopped(&mut self, worker_id: usize, info: Option<PanicData>);
}

pub(crate) trait Supervises<A>: Send + Sync + 'static where A: Actor {
    fn notify_worker_stopped(&self, worker_id: usize, info: Option<PanicData>);
}

impl<S, W> Handle<WorkerStopped<W>> for S where S: Supervisor<W>, W: Actor {
    type Response = ();

    fn accept(&mut self, msg: WorkerStopped<W>, _: &mut ActorContext<S>) {
        self.worker_stopped(msg.0, msg.1)
    }
}

impl<S, W> Supervises<W> for Mailbox<S> where S: Supervisor<W> + Handle<WorkerStopped<W>>, W: Actor
{
    fn notify_worker_stopped(&self, worker_id: usize, info: Option<PanicData>) {
        self.send(WorkerStopped(worker_id, info, PhantomData::<*const W>));
    }
}

pub(crate) struct SupervisorStopped;
impl Message for SupervisorStopped {
    type Result = ();
}

pub(crate) trait SupervisedBy<A>: Send + Sync + 'static where A: Actor {
    fn notify_supervisor_stopped(&self);
}

impl<A> Handle<SupervisorStopped> for A where A: Actor,  {
    type Response = ();

    fn accept(&mut self, _msg: SupervisorStopped, cx: &mut ActorContext<A>) {
        self.supervisor_stopped(cx);
    }
}

impl<S, W> SupervisedBy<S> for Mailbox<W> where S: Supervisor<W>, W: Actor + Handle<SupervisorStopped> {
    fn notify_supervisor_stopped(&self) {
        self.send(SupervisorStopped);
    }
}

// Kind-of-Hack to be able to send certain generic messages to all mailboxes
trait StoppableActor: mopa::Any + Send + 'static {
    fn stop_me(&self);
}
mopafy!(StoppableActor);

impl<A> StoppableActor for Mailbox<A>
where
    A: Actor,
{
    fn stop_me(&self) {
        self.send(StopActor);
    }
}

impl System {
    pub fn new() -> Self {
        let pool = tokio_threadpool::Builder::new().pool_size(8).build();

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
        self.context.spawn_actor(actor, None).unwrap()
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
            Ok(registry) => {
                for service in registry.values() {
                    service.stop_me();
                }
            }
            Err(_) => panic!("Could not terminate services..."),
        }

        self.threadpool.shutdown_on_idle().wait();
        println!("Done with system?");
    }

    pub fn spawn_future<F: Future<Output = ()> + Send + 'static>(&self, fut: F) {
        self.context.spawn_future(fut);
    }
}
