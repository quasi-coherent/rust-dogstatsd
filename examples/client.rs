// Load the crate
extern crate statsd;

// Import the client object.
use statsd::client::Client;

fn main() {
    let client = Client::new("127.0.0.1:8125", "myapp").unwrap();
    client.incr("some.counter", None);
    println!("Sent a counter!");

    client.gauge("some.gauge", 124.0, None);
    println!("Set a gauge!");

    client.timer("timer.duration", 182.1, None);
    println!("Set a timer!");

    client.time("closure.duration", None, || {
        println!("Timing a closure");
    });
}
