# Rust DogStatsd

[![Build Status](https://travis-ci.org/minato128/rust-dogstatsd.svg?branch=master)](https://travis-ci.org/minato128/rust-dogstatsd)

A DogStatsD client implementation of statsd in rust. 

forked from https://github.com/markstory/rust-statsd

## Using the client library

Add the `datadog-statsd` package as a dependency in your `Cargo.toml` file:

```toml
[dependencies]
datadog-statsd = "^0.1.0"
```

You need rustc >= 1.8.0 for statsd to work.

You can then get a client instance and start tracking metrics:

```rust
// Load the crate
extern crate datadog_statsd;

// Import the client object.
use datadog_statsd::Client;

// Get a client with the prefix of `myapp`. The host should be the
// IP:port of your statsd daemon.
let client = Client::new("127.0.0.1:8125", "myapp").unwrap();
```

## Tracking Metrics

Once you've created a client, you can track timers and metrics:

```rust
// Increment a counter by 1
client.incr("some.counter");

// Decrement a counter by 1
client.decr("some.counter");

// Update a gauge
client.gauge("some.value", 12.0);

// Modify a counter by an arbitrary float.
client.count("some.counter", 511.0);

// Send a histogram value as a float.
client.histogram("some.histogram", 511.0);
```

### Tracking Timers

Timers can be updated using `timer()` and `time()`:

```rust
// Update a timer based on a calculation you've done.
client.timer("operation.duration", 13.4);

// Time a closure
client.time("operation.duration", || {
	// Do something expensive.
});
```

### Pipeline

Multiple metrics can be sent to StatsD once using pipeline:

```rust
let mut pipe = client.pipeline():

// Increment a counter by 1
pipe.incr("some.counter");

// Decrement a counter by 1
pipe.decr("some.counter");

// Update a gauge
pipe.gauge("some.value", 12.0);

// Modify a counter by an arbitrary float.
pipe.count("some.counter", 511.0);

// Send a histogram value as a float.
pipe.histogram("some.histogram", 511.0);

// Set max UDP packet size if you wish, default is 512
pipe.set_max_udp_size(128);

// Send to StatsD
pipe.send(&client);
```

Pipelines are also helpful to make functions simpler to test, as you can
pass a pipeline and be confident that no UDP packets will be sent.


## License

Licenesed under the [MIT License](LICENSE.txt).
