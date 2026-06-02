use std::thread;
use std::time::Duration;

fn main() {
    tracing_subscriber::fmt::init();
    let _server = colorsky_udp::server::EnetServer::start(9999);
    loop {
        thread::sleep(Duration::from_secs(1));
    }
}
