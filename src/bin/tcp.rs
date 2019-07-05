#![feature(async_await)]

use hyper::service::{make_service_fn, service_fn};
use hyper::{Body, Client, Method, Request, Response, Server};

use actress::{Actor, ActorContext, Handle, Mailbox, Message, SyncResponse, System, AsyncResponse};
use hyper::client::HttpConnector;
use std::collections::HashMap;
use std::error::Error;
use std::future::Future;
use std::hash::Hash;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use std::any::TypeId;
use std::task::Poll;

#[derive(Serialize, Deserialize, Debug)]
struct Foo(u64);

impl Message for Foo {
    type Result = Foo;
}
type PBF<T> = Pin<Box<dyn Future<Output=T> + Send + Sync + 'static>>;

trait HttpMailbox: Send + Sync + 'static {
    fn handle_bytes(&self, req: serde_json::Value) -> PBF<Result<serde_json::Value, &'static str>>;
}


fn ff<R, F: Future<Output = R> + Send + Sync + 'static>(fut: F) -> PBF<R> {
    Box::pin(fut)
}

trait AcceptsHttp<M>
where
    M: Message + DeserializeOwned,
    M::Result: Send + Serialize,
{
    fn handle(&self, msg: M) -> PBF<Result<M::Result, &'static str>>;
}

impl<M> HttpMailbox for Box<(dyn AcceptsHttp<M> + Send + Sync + 'static)>
where
    M: Message + DeserializeOwned + Send,
    M::Result: Send + Serialize,
{
    fn handle_bytes(&self, json: serde_json::Value) -> PBF<Result<serde_json::Value, &'static str>> {
        println!("Inside HttpMailbox");
        match serde_json::from_value(json) {
            Ok(data) => {
                let fut = self.handle(data);

                ff(async move {
                    match fut.await {
                        Ok(m) => match serde_json::to_value(m) {
                            Ok(value) => Ok(value),
                            Err(_) => Err("Could not deserialize"),
                        },
                        Err(e) => Err(e)
                    }

                })
            },
            Err(_) => Box::pin(futures::future::err("Could not deserialize")),
        }
    }
}

use futures::future::poll_fn;

impl<A, M> AcceptsHttp<M> for Mailbox<A>
where
    M: Message + DeserializeOwned + Send + Sync,
    M::Result: Send + Sync + Serialize,
    A: Actor + Handle<M>,
{
    fn handle(&self, msg: M) -> PBF<Result<M::Result, &'static str>> {

        println!("Inside AcceptsHttp for {:?}", TypeId::of::<M>());
        let fut = self.ask_nicely(msg).unwrap();
        ff(async move {
            match fut.await {
                Ok(Some(r)) => Ok(r),
                Ok(None) => Err("Message was dropped"),
                Err(_) => Err("Could not recv"),
            }
        })

        /*
        match self.ask_nicely(msg) {
            Ok(mut fut) => ff(async move {
               Err("oh noes!")
            }),
            Err(_) => Box::pin(futures::future::err("Could not send msg")),
        }
        */
    }
}

type Handler = Box<dyn HttpMailbox>;

struct Webby;

impl Actor for Webby {}

impl Handle<Foo> for Webby {
    type Response = AsyncResponse<Foo>;

    fn accept(&mut self, msg: Foo, cx: &mut ActorContext<Self>) -> Self::Response {
        println!("Inside Handle<Foo>");
        AsyncResponse::from_future(async move {
            Foo(msg.0 + 1)
        })
    }
}

fn webbox<A, M>(mailbox: Mailbox<A>) -> Handler
where
    A: Actor + Handle<M>,
    M: Message + DeserializeOwned + Send + Sync,
    M::Result: Send + Serialize + Sync,
{
    // Sad about the double dynamic dispatch..
    Box::new(Box::new(mailbox) as Box<(dyn AcceptsHttp<M> + Send + Sync + 'static)>)
}

fn main() {
    let mut system = System::new();

    let addr = "127.0.0.1:12345".parse().unwrap();

    let mb = system.start(Webby {});
    system.spawn_future(async move {
        let mk_service = make_service_fn(move |t| {
            println!("Got a target: {:?}", t);
            let mb = mb.copy();
            async move {
                let mb = mb.copy();
                Ok::<_, hyper::Error>(service_fn(move |req: Request<Body>| {
                    println!("Got a request: {:?}", req);
                    let routes = {
                        let mut tmp: HashMap<String, Handler> = HashMap::new();
                        tmp.insert("/".to_owned(), webbox::<_, Foo>(mb.copy()));
                        tmp
                    };

                    async move {
                        let (parts, mut body) = req.into_parts();

                        let mut buff = Vec::with_capacity(512);

                        while let Some(next) = body.next().await {
                            match next {
                                Ok(chunk) => {
                                    println!("Read a chunk: {:?}", chunk);
                                    buff.extend_from_slice(chunk.as_ref())
                                },
                                Err(err) => {
                                    eprintln!("Body Error: {}", err);
                                    return Ok::<_, hyper::Error>(Response::new(Body::from(
                                        "Error reading body!",
                                    )));
                                }
                            }
                        }
                        println!("Read a buffer: {:?}", buff);

                        let resp_body = if let Ok(json_value) = serde_json::from_slice(&buff) {
                            println!("Got a json value: {:?}", json_value);
                            match routes.get(parts.uri.path()) {
                                Some(handler) => {
                                    let mut fut = handler.handle_bytes(json_value);
                                    //Body::from("this is amazing!")
                                    //let pinned = Pin::new(&mut fut);
                                    match fut.await {
                                        Ok(json) => Body::from(json.to_string()),
                                        Err(msg) => Body::from(msg),
                                    }

                                },
                                None => Body::from("Detta gick inte"),
                            }
                        } else {
                            Body::from("Could not deserialize!")
                        };

                        return Ok::<_, hyper::Error>(Response::new(resp_body));
                    }
                }))
            }
        });

        Server::bind(&addr).serve(mk_service).await.expect("omfg");
    });

    /*
    system.spawn_future(async move {
        let mut inc = listener.incoming();
        while let Some(sock) = inc.next().await {
            match sock {
                Ok(s) => { println!("sock? {:?}", s) },
                Err(e) => { println!("no sock? {:?}", e) },
            }
        }
    });
    */

    system.run_until_completion();
}
