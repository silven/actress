#![feature(async_await)]

use tokio_io::AsyncRead;
use tokio_tcp::TcpListener;

use std::future::Future;
use futures::stream::Stream;

use actress::System;
use futures::stream::StreamExt;


fn main() {
    let mut system = System::new();

    let addr = "127.0.0.1:12345".parse().unwrap();
    let listener = TcpListener::bind(&addr)
        .expect("unable to bind TCP listener");

    system.spawn_future(async move {
        let mut inc = listener.incoming();
        while let Some(sock) = inc.next().await {
            match sock {
                Ok(s) => { println!("sock? {:?}", s) },
                Err(e) => { println!("no sock? {:?}", e) },
            }
        }

    });

    system.run_until_completion();
}
