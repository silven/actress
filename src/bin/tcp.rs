#![feature(async_await)]

use std::any::TypeId;
use std::collections::HashMap;
use std::error::Error;
use std::future::Future;
use std::hash::Hash;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::Poll;

use hyper::client::HttpConnector;
use hyper::header::{HeaderValue, CONTENT_LENGTH};
use hyper::service::{make_service_fn, service_fn};
use hyper::{Body, Client, HeaderMap, Method, Request, Response, Server};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use actress::{Actor, ActorContext, AsyncResponse, Handle, Mailbox, Message, SyncResponse, System};

#[derive(Serialize, Deserialize, Debug)]
struct Foo(u64);

#[derive(Serialize, Deserialize, Debug)]
enum Choice {
    A,
    B,
    C,
}

#[derive(Serialize, Deserialize, Debug)]
struct Resp {
    x: usize,
    y: Option<isize>,
    nom: String,
    foo: Choice,
}

impl Message for Foo {
    type Result = Resp;
}

struct Webby;

impl Actor for Webby {}

impl Handle<Foo> for Webby {
    type Response = AsyncResponse<Foo>;

    fn accept(&mut self, msg: Foo, cx: &mut ActorContext<Self>) -> Self::Response {
        println!("Inside Handle<Foo>");
        let fibber = cx.spawn_actor(Fibber::new()).unwrap();

        AsyncResponse::from_future(async move {
            Resp {
                x: fibber.ask_async(FibRequest(msg.0)).await.unwrap() as usize,
                y: Some(-1),
                nom: "tomten".to_string(),
                foo: Choice::A,
            }
        })
    }
}

struct Fibber {
    count: u64,
}

impl Fibber {
    fn new() -> Self {
        Fibber { count: 0 }
    }
}

impl Actor for Fibber {}

#[derive(Debug)]
struct FibRequest(u64);

impl Message for FibRequest {
    type Result = u64;
}

impl Handle<FibRequest> for Fibber {
    type Response = AsyncResponse<FibRequest>;

    fn accept(&mut self, msg: FibRequest, cx: &mut ActorContext<Self>) -> Self::Response {
        let slave = cx.spawn_actor(Fibber::new()).unwrap();

        AsyncResponse::from_future(async move {
            if msg.0 < 2 {
                msg.0
            } else {
                let f1 = slave.ask_async(FibRequest(msg.0 - 1));
                let f2 = slave.ask_async(FibRequest(msg.0 - 2));

                f1.await.unwrap() + f2.await.unwrap()
            }
        })
    }
}

fn main() {
    let mut system = System::new();

    let mb = system.start(Webby {});
    system.serve::<Foo, _>("/foo", mb);

    /*
    mb.grab::<Foo, _>(|foo| Resp {
        x: 0,
        y: Some(-1),
        nom: "tomten".to_string(),
        foo: Choice::A,
    });
    */

    /*
    let fibber = system.start(Fibber{ count: 0 });
    system.spawn_future(async move {
        match fibber.ask(FibRequest(30)) {
            Ok(fib) => println!("The fib({}) = {}", 30, fib),
            Err(e) => println!("Could not ask: {:?}", e),
        };
    });
    */
    system.run_until_completion();
}
