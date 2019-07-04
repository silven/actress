#![feature(async_await)]

use hyper::service::{make_service_fn, service_fn};
use hyper::{Body, Client, Method, Request, Response, Server};

use actress::System;
use hyper::client::HttpConnector;
use std::error::Error;
use std::future::Future;
use std::pin::Pin;

type MyResponse = Result<Response<Body>, Box<dyn Error + Send + Sync>>;
type ResponseFuture = Pin<Box<dyn Future<Output = MyResponse> + Send>>;

fn f<T: Into<Body> + Send + 'static>(t: T) -> ResponseFuture {
    Box::pin(async move { Ok(Response::new(t.into())) })
}

fn r<T: Into<Body> + Send + 'static>(t: T) -> MyResponse {
    Ok(Response::new(t.into()))
}
/*
fn route(req: Request<Body>, client: Client<HttpConnector>) -> ResponseFuture {
    match (req.method(), req.uri().path()) {
        (&Method::GET, "/") => {
            f("Welcome!")
        },
        (_, path) => {
            f(format!("Hello World to you! {}", path))
        }
    }
}
*/

async fn route(req: Request<Body>) -> MyResponse {
    match (req.method(), req.uri().path()) {
        (&Method::GET, "/") => r("Welcome!"),
        (_, path) => r(format!("Hello World to you! {}", path)),
    }
}

fn main() {
    let mut system = System::new();

    let addr = "127.0.0.1:12345".parse().unwrap();
    //let listener = TcpListener::bind(&addr).expect("unable to bind TCP listener");

    system.spawn_future(async move {
        Server::bind(&addr)
            .serve(make_service_fn(|_| {
                async { Ok::<_, hyper::Error>(service_fn(route)) }
            })).await;
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
