use std::marker::PhantomData;

use mopa::{Any, mopafy};

use crate::{Actor, ActorContext, Handle, Mailbox, Message, PanicData, Supervisor};

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
