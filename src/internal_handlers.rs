use std::marker::PhantomData;

use mopa::mopafy;

use crate::{Actor, ActorContext, Handle, Mailbox, Message, PanicData, Supervisor};

// All actors can be stopped by a StopActor message, sent by the system.
struct StopActor;

impl Message for StopActor {
    type Result = ();
}

impl<A> Handle<StopActor> for A
where
    A: Actor,
{
    type Response = ();
    fn accept(&mut self, _msg: StopActor, cx: &mut ActorContext<A>) {
        cx.stop();
    }
}

// Kind-of-Hack to be able to use this stop function as well as being able to downcast from inside
// the registry
pub(crate) trait StoppableActor: mopa::Any + Send + 'static {
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

// All Supvervisors get a message when a worker it it supvervising has stopped.

/// If there is panic data, there was a crash. If there is none, it stopped gracefully.
pub(crate) struct WorkerStopped<W: Actor>(u64, Option<PanicData>, PhantomData<*const W>);
// Send is safe because the actor isn't really there.
unsafe impl<W> Send for WorkerStopped<W> where W: Actor {}
//unsafe impl<W> Sync for WorkerStopped<W> where W: Actor {}

impl<W> Message for WorkerStopped<W>
where
    W: Actor,
{
    type Result = ();
}

pub(crate) trait Supervises<A>: Send + 'static
where
    A: Actor,
{
    fn notify_worker_stopped(&self, worker_id: u64, info: Option<PanicData>);
}

impl<S, W> Handle<WorkerStopped<W>> for S
where
    S: Supervisor<W>,
    W: Actor,
{
    type Response = ();

    fn accept(&mut self, msg: WorkerStopped<W>, _: &mut ActorContext<S>) {
        self.worker_stopped(msg.0, msg.1)
    }
}

impl<S, W> Supervises<W> for Mailbox<S>
where
    S: Supervisor<W> + Handle<WorkerStopped<W>>,
    W: Actor,
{
    fn notify_worker_stopped(&self, worker_id: u64, info: Option<PanicData>) {
        self.send(WorkerStopped(worker_id, info, PhantomData::<*const W>));
    }
}

// We also notify workers when their supervisor has stopped

pub(crate) struct SupervisorStopped;
impl Message for SupervisorStopped {
    type Result = ();
}

pub(crate) trait SupervisedBy<A>: Send + 'static
where
    A: Actor,
{
    fn notify_supervisor_stopped(&self);
}

impl<A> Handle<SupervisorStopped> for A
where
    A: Actor,
{
    type Response = ();

    fn accept(&mut self, _msg: SupervisorStopped, cx: &mut ActorContext<A>) {
        self.supervisor_stopped(cx);
    }
}

impl<S, W> SupervisedBy<S> for Mailbox<W>
where
    S: Supervisor<W>,
    W: Actor + Handle<SupervisorStopped>,
{
    fn notify_supervisor_stopped(&self) {
        self.send(SupervisorStopped);
    }
}
