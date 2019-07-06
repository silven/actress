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
        AsyncResponse::from_future(async move {
            Resp {
                x: msg.0 as usize,
                y: Some(-1),
                nom: "tomten".to_string(),
                foo: Choice::A,
            }
        })
    }
}


fn main() {
    let mut system = System::new();

    let mb = system.start(Webby {});
    system.serve::<Foo, _>("/foo", mb);

    system.run_until_completion();
}
