use std::any::{Any, TypeId};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::task::Context;

use futures::executor::block_on;
use futures::future::Future;
use futures::stream::StreamExt;
use futures::Poll;
use tokio_sync::mpsc;
use tokio_threadpool::ThreadPool;

use crate::actor::{Actor, ActorContext, BacklogPolicy, Handle, Message};
#[cfg(feature = "peek")]
use crate::mailbox::PeekGrab;
use crate::mailbox::{Envelope, EnvelopeProxy, Mailbox};
use crate::supervisor::{PanicHookGuard, SupervisorGuard};
use crate::system_context::SystemContext;

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
        // Reset the hook after we're done
        let _hook_guard = PanicHookGuard::new(std::panic::take_hook());

        let sup_guard = self.supervisor.guard();
        let my_id = self.inner.id();
        std::panic::set_hook(Box::new(move |info| {
            println!("Inside panic hook!");
            if let Some(sup) = sup_guard.swap(None) {
                println!("Notifying sup about worker crash!");
                sup.notify_worker_stopped(my_id, Some(info.into()));
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
                    let process_result =
                        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                            msg.accept(&mut self)
                        }));

                    match process_result {
                        Ok(()) => { /* all is well */ }
                        Err(_) => {
                            self.close_and_stop();
                            return Poll::Ready(());
                        }
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

pub(crate) struct ActorBundle<A: Actor> {
    pub(crate) actor: A,
    pub(crate) inner: ActorContext<A>,
    pub(crate) recv: Option<mpsc::UnboundedReceiver<Envelope<A>>>,
    pub(crate) supervisor: SupervisorGuard<A>,
    #[cfg(feature = "peek")]
    pub(crate) listeners: Arc<Mutex<AnyArcMap>>,
}

impl<A> ActorBundle<A>
where
    A: Actor,
{
    #[cfg(feature = "peek")]
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
