use std::future::Future;
use std::pin::Pin;

use tokio_sync::oneshot;
//use tokio_threadpool::Sender;
use tokio::runtime::TaskExecutor;

use crate::actor::Message;

//use tokio::runtime::current_thread::Handle;

pub trait Response<M: Message> {
    fn handle(self, spawner: TaskExecutor, reply_to: Option<oneshot::Sender<Option<M::Result>>>);
}

pub struct SyncResponse<M: Message>(M::Result);
pub struct AsyncResponse<M: Message>(Pin<Box<dyn Future<Output = M::Result> + Send>>);

impl<M: Message> SyncResponse<M> {
    pub fn new(value: M::Result) -> Self {
        SyncResponse(value)
    }
}

impl<M: Message> AsyncResponse<M> {
    pub fn from_future<F: Future<Output = M::Result> + Send + 'static>(fut: F) -> Self {
        AsyncResponse(Box::pin(fut))
    }
}

impl<M> Response<M> for AsyncResponse<M>
where
    M: Message,
    M::Result: Send,
{
    fn handle(self, spawner: TaskExecutor, reply_to: Option<oneshot::Sender<Option<M::Result>>>) {
        spawner.spawn(async move {
            let result: <M as Message>::Result = self.0.await;
            if let Some(tx) = reply_to {
                tx.send(Some(result));
            }
        });
    }
}

impl<M> Response<M> for SyncResponse<M>
where
    M: Message,
{
    fn handle(self, _: TaskExecutor, reply_to: Option<oneshot::Sender<Option<M::Result>>>) {
        if let Some(tx) = reply_to {
            tx.send(Some(self.0));
        }
    }
}

macro_rules! simple_response {
    ($type:ty) => {
        impl<M: Message> Response<M> for $type
        where
            M: Message<Result = $type>,
        {
            fn handle(self, _: TaskExecutor, reply_to: Option<oneshot::Sender<Option<M::Result>>>) {
                if let Some(tx) = reply_to {
                    tx.send(Some(self));
                }
            }
        }
    };
}

//Unfortunate that we can't blanket impl these
simple_response!(());

simple_response!(bool);

simple_response!(f32);
simple_response!(f64);

simple_response!(u8);
simple_response!(u16);
simple_response!(u32);
simple_response!(u64);
simple_response!(u128);
simple_response!(usize);

simple_response!(i8);
simple_response!(i16);
simple_response!(i32);
simple_response!(i64);
simple_response!(i128);
simple_response!(isize);

simple_response!(char);
simple_response!(String);
