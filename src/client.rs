use futures::Future;
use rand;
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::fmt;
use std::net::{SocketAddr, ToSocketAddrs, UdpSocket};
use std::sync::Arc;
use std::time;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum StatsdError {
    #[error("io error: {0}")]
    IoError(#[from] std::io::Error),
    #[error("{0}")]
    AddrParseError(String),
}

/// A config to build a statsd Client.  The address field should implement `std::net::ToSocketAddrs`.
/// See https://doc.rust-lang.org/std/net/trait.ToSocketAddrs.html.
///
/// This type admits a builder pattern that's used like this:
///
/// ```ignore
/// use datadog_statsd::ClientConfig;
///
/// let config: ClientConfig =
///     ClientConfig::builder(("127.0.0.1", 8125))
///         .prefix("some.prefix")
///         .constant_tags(vec!["tag1", "tag2"])
///         .build();
/// ...
///
/// ```
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ClientConfig<T> {
    pub address: T,
    pub prefix: Option<String>,
    pub constant_tags: Option<Vec<String>>,
}

impl<T> ClientConfig<T> {
    pub fn builder(address: T) -> ClientConfigBuilder<T> {
        ClientConfigBuilder::new(address)
    }

    pub fn to_socket_addr(&self) -> Result<SocketAddr, StatsdError>
    where
        T: ToSocketAddrs,
    {
        self.address
            .to_socket_addrs()?
            .next()
            .ok_or_else(|| StatsdError::AddrParseError("could not parse address".to_string()))
    }
}

pub struct ClientConfigBuilder<T> {
    address: T,
    prefix: Option<String>,
    constant_tags: Option<Vec<String>>,
}

impl<T> ClientConfigBuilder<T> {
    pub fn new(address: T) -> Self {
        Self {
            address,
            prefix: None,
            constant_tags: None,
        }
    }

    pub fn prefix(mut self, prefix: &str) -> Self {
        self.prefix = Some(prefix.into());
        self
    }

    pub fn constant_tags(mut self, constant_tags: Vec<&str>) -> Self {
        self.constant_tags = Some(constant_tags.iter().map(|t| t.to_string()).collect());
        self
    }

    pub fn build(self) -> ClientConfig<T> {
        ClientConfig {
            address: self.address,
            prefix: self.prefix,
            constant_tags: self.constant_tags,
        }
    }
}

struct InternalClient {
    socket: UdpSocket,
    socket_addr: SocketAddr,
    prefix: String,
    constant_tags: Vec<String>,
}

/// Client socket for statsd servers.
///
/// After creating a metric you can use `Client`
/// to send metrics to the configured statsd server
///
/// # Example
///
/// Creating a client and sending metrics is easy.
///
/// ```ignore
/// use datadog_statsd::client::Client;
///
/// let client = Client::new("127.0.0.1:8125", "myapp", tags);
/// client.incr("some.metric.completed");
/// ```
pub struct Client {
    client: Arc<InternalClient>,
}

impl Clone for Client {
    fn clone(&self) -> Self {
        Self {
            client: Arc::clone(&self.client),
        }
    }
}

impl Client {
    /// Construct a new statsd client given a client config
    pub fn new<T: ToSocketAddrs>(client_config: &ClientConfig<T>) -> Result<Client, StatsdError> {
        let socket_addr = client_config.to_socket_addr()?;

        // Bind to a generic port as we'll only be writing on this
        // socket.
        let socket = if socket_addr.is_ipv4() {
            UdpSocket::bind("0.0.0.0:0")?
        } else {
            UdpSocket::bind("[::]:0")?
        };
        let internal_client = InternalClient {
            socket,
            socket_addr,
            prefix: match &client_config.prefix {
                Some(prefix) => prefix.to_string(),
                _ => "".into(),
            },
            constant_tags: match &client_config.constant_tags {
                Some(tags) => tags.iter().map(|x| x.to_string()).collect(),
                None => vec![],
            },
        };
        Ok(Client {
            client: Arc::new(internal_client),
        })
    }

    /// Increment a metric by 1
    ///
    /// ```ignore
    /// # Increment a given metric by 1.
    /// client.incr("metric.completed", tags);
    /// ```
    ///
    /// This modifies a counter with an effective sampling
    /// rate of 1.0.
    pub fn incr(&self, metric: &str, tags: Option<Vec<&str>>) {
        self.count(metric, 1.0, tags);
    }

    /// Decrement a metric by -1
    ///
    /// ```ignore
    /// # Decrement a given metric by 1
    /// client.decr("metric.completed", tags);
    /// ```
    ///
    /// This modifies a counter with an effective sampling
    /// rate of 1.0.
    pub fn decr(&self, metric: &str, tags: Option<Vec<&str>>) {
        self.count(metric, -1.0, tags);
    }

    /// Modify a counter by `value`.
    ///
    /// Will increment or decrement a counter by `value` with
    /// a sampling rate of 1.0.
    ///
    /// ```ignore
    /// // Increment by 12
    /// client.count("metric.completed", 12.0, tags);
    /// ```
    pub fn count(&self, metric: &str, value: f64, tags: Option<Vec<&str>>) {
        let data = self.prepare_with_tags(format!("{}:{}|c", metric, value), tags);
        self.send(data);
    }

    /// Modify a counter by `value` only x% of the time.
    ///
    /// Will increment or decrement a counter by `value` with
    /// a custom sampling rate.
    ///
    ///
    /// ```ignore
    /// // Increment by 4 50% of the time.
    /// client.sampled_count("metric.completed", 4, 0.5, tags);
    /// ```
    pub fn sampled_count(&self, metric: &str, value: f64, rate: f64, tags: Option<Vec<&str>>) {
        if rand::random::<f64>() >= rate {
            return;
        }
        let data = self.prepare_with_tags(format!("{}:{}|c|@{}", metric, value, rate), tags);
        self.send(data);
    }

    /// Set a gauge value.
    ///
    /// ```ignore
    /// // set a gauge to 9001
    /// client.gauge("power_level.observed", 9001.0, tags);
    /// ```
    pub fn gauge(&self, metric: &str, value: f64, tags: Option<Vec<&str>>) {
        let data = self.prepare_with_tags(format!("{}:{}|g", metric, value), tags);
        self.send(data);
    }

    /// Send a timer value.
    ///
    /// The value is expected to be in ms.
    ///
    /// ```ignore
    /// // pass a duration value
    /// client.timer("response.duration", 10.123, tags);
    /// ```
    pub fn timer(&self, metric: &str, value: f64, tags: Option<Vec<&str>>) {
        let data = self.prepare_with_tags(format!("{}:{}|ms", metric, value), tags);
        self.send(data);
    }

    /// Time a block of code.
    ///
    /// The passed closure will be timed and executed. The block's
    /// duration will be sent as a metric.
    ///
    /// ```ignore
    /// // pass a duration value
    /// client.time("response.duration", tags, || {
    ///   // Your code here.
    /// });
    /// ```
    pub fn time<F, R>(&self, metric: &str, tags: Option<Vec<&str>>, callable: F) -> R
    where
        F: FnOnce() -> R,
    {
        let start = time::Instant::now();
        let return_val = callable();
        let used = start.elapsed();
        let data = self.prepare_with_tags(format!("{}:{}|ms", metric, used.as_millis()), tags);
        self.send(data);
        return_val
    }

    /// Time an async block of code.
    /// The passed future will be `await`ed on, timed, and the result returned, the time
    /// having passed being sent as a "time" metric.
    pub async fn time_async<F, O>(&self, metric: &str, tags: Option<Vec<&str>>, f: F) -> O
    where
        F: Future<Output = O>,
    {
        let start = time::Instant::now();
        let return_val = f.await;
        let used = start.elapsed();
        let data = self.prepare_with_tags(format!("{}:{}|ms", metric, used.as_millis()), tags);
        self.send(data);
        return_val
    }

    fn prepare<T: AsRef<str>>(&self, data: T) -> String {
        if self.client.prefix.is_empty() {
            data.as_ref().to_string()
        } else {
            format!("{}.{}", self.client.prefix, data.as_ref())
        }
    }

    fn prepare_with_tags<T: AsRef<str>>(&self, data: T, tags: Option<Vec<&str>>) -> String {
        self.append_tags(self.prepare(data), tags)
    }

    fn append_tags<T: AsRef<str>>(&self, data: T, tags: Option<Vec<&str>>) -> String {
        if self.client.constant_tags.is_empty() && tags.is_none() {
            data.as_ref().to_string()
        } else {
            let mut all_tags = self.client.constant_tags.clone();
            match tags {
                Some(v) => {
                    for tag in v {
                        all_tags.push(tag.to_string());
                    }
                }
                None => {
                    // nothing to do
                }
            }
            format!("{}|#{}", data.as_ref(), all_tags.join(","))
        }
    }

    /// Send data along the UDP socket.
    fn send(&self, data: String) {
        let _ = self
            .client
            .socket
            .send_to(data.as_bytes(), self.client.socket_addr);
    }

    /// Get a pipeline struct that allows optimizes the number of UDP
    /// packets used to send multiple metrics
    ///
    /// ```ignore
    /// let mut pipeline = client.pipeline();
    /// pipeline.incr("some.metric", 1);
    /// pipeline.incr("other.metric", 1);
    /// pipeline.send(&mut client);
    /// ```
    pub fn pipeline(&self) -> Pipeline {
        Pipeline::new()
    }

    /// Send a histogram value.
    ///
    /// ```ignore
    /// // pass response size value
    /// client.histogram("response.size", 128.0, tags);
    /// ```
    pub fn histogram(&self, metric: &str, value: f64, tags: Option<Vec<&str>>) {
        let data = self.prepare_with_tags(format!("{}:{}|h", metric, value), tags);
        self.send(data);
    }

    /// Send a event.
    ///
    /// ```ignore
    /// // pass a app start event
    /// client.event("MyApp Start", "MyApp Details", AlertType::Info, &Some(vec!["tag1", "tag2:test"]));
    /// ```
    pub fn event(&self, title: &str, text: &str, alert_type: AlertType, tags: Option<Vec<&str>>) {
        let mut d = vec![];
        d.push(format!("_e{{{},{}}}:{}", title.len(), text.len(), title));
        d.push(text.to_string());
        if alert_type != AlertType::Info {
            d.push(format!("t:{}", alert_type.to_string().to_lowercase()))
        }
        let event_with_tags = self.append_tags(d.join("|"), tags);
        self.send(event_with_tags)
    }

    /// Send a service check.
    ///
    /// ```ignore
    /// // pass a app status
    /// client.service_check("MyApp", ServiceCheckStatus::Ok, &Some(vec!["tag1", "tag2:test"]));
    /// ```
    pub fn service_check(
        &self,
        service_check_name: &str,
        status: ServiceCheckStatus,
        tags: Option<Vec<&str>>,
    ) {
        let mut d = vec![];
        let status_code = (status as u32).to_string();
        d.push("_sc");
        d.push(service_check_name);
        d.push(&status_code);
        let sc_with_tags = self.append_tags(d.join("|"), tags);
        self.send(sc_with_tags)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AlertType {
    Info,
    Error,
    Warning,
    Success,
}

impl fmt::Display for AlertType {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{:?}", self)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ServiceCheckStatus {
    Ok = 0,
    Warning = 1,
    Critical = 2,
    Unknown = 3,
}

pub struct Pipeline {
    stats: VecDeque<String>,
    max_udp_size: usize,
}

impl Default for Pipeline {
    fn default() -> Self {
        Self::new()
    }
}

impl Pipeline {
    pub fn new() -> Pipeline {
        Pipeline {
            stats: VecDeque::new(),
            max_udp_size: 512,
        }
    }

    /// Set max UDP packet size
    ///
    /// ```
    /// use datadog_statsd::client::Pipeline;
    ///
    /// let mut pipe = Pipeline::new();
    /// pipe.set_max_udp_size(128);
    /// ```
    pub fn set_max_udp_size(&mut self, max_udp_size: usize) {
        self.max_udp_size = max_udp_size;
    }

    /// Increment a metric by 1
    ///
    /// ```
    /// use datadog_statsd::client::Pipeline;
    ///
    /// let mut pipe = Pipeline::new();
    /// // Increment a given metric by 1.
    /// pipe.incr("metric.completed");
    /// ```
    ///
    /// This modifies a counter with an effective sampling
    /// rate of 1.0.
    pub fn incr(&mut self, metric: &str) {
        self.count(metric, 1.0);
    }

    /// Decrement a metric by -1
    ///
    /// ```
    /// use datadog_statsd::client::Pipeline;
    ///
    /// let mut pipe = Pipeline::new();
    /// // Decrement a given metric by 1
    /// pipe.decr("metric.completed");
    /// ```
    ///
    /// This modifies a counter with an effective sampling
    /// rate of 1.0.
    pub fn decr(&mut self, metric: &str) {
        self.count(metric, -1.0);
    }

    /// Modify a counter by `value`.
    ///
    /// Will increment or decrement a counter by `value` with
    /// a sampling rate of 1.0.
    ///
    /// ```
    /// use datadog_statsd::client::Pipeline;
    ///
    /// let mut pipe = Pipeline::new();
    /// // Increment by 12
    /// pipe.count("metric.completed", 12.0);
    /// ```
    pub fn count(&mut self, metric: &str, value: f64) {
        let data = format!("{}:{}|c", metric, value);
        self.stats.push_back(data);
    }

    /// Modify a counter by `value` only x% of the time.
    ///
    /// Will increment or decrement a counter by `value` with
    /// a custom sampling rate.
    ///
    /// ```
    /// use datadog_statsd::client::Pipeline;
    ///
    /// let mut pipe = Pipeline::new();
    /// // Increment by 4 50% of the time.
    /// pipe.sampled_count("metric.completed", 4.0, 0.5);
    /// ```
    pub fn sampled_count(&mut self, metric: &str, value: f64, rate: f64) {
        if rand::random::<f64>() >= rate {
            return;
        }
        let data = format!("{}:{}|c|@{}", metric, value, rate);
        self.stats.push_back(data);
    }

    /// Set a gauge value.
    ///
    /// ```
    /// use datadog_statsd::client::Pipeline;
    ///
    /// let mut pipe = Pipeline::new();
    /// // set a gauge to 9001
    /// pipe.gauge("power_level.observed", 9001.0);
    /// ```
    pub fn gauge(&mut self, metric: &str, value: f64) {
        let data = format!("{}:{}|g", metric, value);
        self.stats.push_back(data);
    }

    /// Send a timer value.
    ///
    /// The value is expected to be in ms.
    ///
    /// ```
    /// use datadog_statsd::client::Pipeline;
    ///
    /// let mut pipe = Pipeline::new();
    /// // pass a duration value
    /// pipe.timer("response.duration", 10.123);
    /// ```
    pub fn timer(&mut self, metric: &str, value: f64) {
        let data = format!("{}:{}|ms", metric, value);
        self.stats.push_back(data);
    }

    /// Time a block of code.
    ///
    /// The passed closure will be timed and executed. The block's
    /// duration will be sent as a metric.
    ///
    /// ```
    /// use datadog_statsd::client::Pipeline;
    ///
    /// let mut pipe = Pipeline::new();
    /// // pass a duration value
    /// pipe.time("response.duration", || {
    ///   // Your code here.
    /// });
    /// ```
    pub fn time<F>(&mut self, metric: &str, callable: F)
    where
        F: FnOnce(),
    {
        let start = time::Instant::now();
        callable();
        let used = start.elapsed();
        let data = format!("{}:{}|ms", metric, used.as_millis());
        self.stats.push_back(data);
    }

    /// Send a histogram value.
    ///
    /// ```
    /// use datadog_statsd::client::Pipeline;
    ///
    /// let mut pipe = Pipeline::new();
    /// // pass response size value
    /// pipe.histogram("response.size", 128.0);
    /// ```
    pub fn histogram(&mut self, metric: &str, value: f64) {
        let data = format!("{}:{}|h", metric, value);
        self.stats.push_back(data);
    }

    /// Send data along the UDP socket.
    pub fn send(&mut self, client: &Client) {
        let mut _data = String::new();
        if let Some(data) = self.stats.pop_front() {
            _data += client.prepare(&data).as_ref();
            while !self.stats.is_empty() {
                let stat = client.prepare(self.stats.pop_front().unwrap());
                if data.len() + stat.len() + 1 > self.max_udp_size {
                    client.send(_data.clone());
                    _data.clear();
                    _data += &stat;
                } else {
                    _data += "\n";
                    _data += &stat;
                }
            }
        }
        if !_data.is_empty() {
            client.send(_data);
        }
    }
}

#[cfg(test)]
mod test {
    extern crate rand;
    use self::rand::distributions::{IndependentSample, Range};
    use super::*;
    use std::net::UdpSocket;
    use std::str;
    use std::sync::mpsc::sync_channel;
    use std::thread;

    static PORT: u16 = 8125;

    // Generates random ports.
    // Having random ports helps tests not collide over
    // shared ports.
    fn next_test_ip4() -> String {
        let range = Range::new(0, 1000);
        let mut rng = rand::thread_rng();
        let port = PORT + range.ind_sample(&mut rng);
        format!("127.0.0.1:{}", port)
    }

    // Makes a udpsocket that acts as a statsd server.
    fn make_server(host: &str) -> UdpSocket {
        UdpSocket::bind(host).ok().unwrap()
    }

    // Makes a `Client`.
    fn make_client(host: &str) -> Client {
        let config = ClientConfig::builder(host).build();
        Client::new(&config).unwrap()
    }

    fn server_recv(server: UdpSocket) -> String {
        let (serv_tx, serv_rx) = sync_channel(1);
        let _t = thread::spawn(move || {
            let mut buf = [0; 128];
            let (len, _) = match server.recv_from(&mut buf) {
                Ok(r) => r,
                Err(_) => panic!("No response from test server."),
            };
            drop(server);
            let bytes = Vec::from(&buf[0..len]);
            serv_tx.send(bytes).unwrap();
        });

        let bytes = serv_rx.recv().ok().unwrap();
        str::from_utf8(&bytes).unwrap().to_string()
    }

    #[test]
    fn test_sending_gauge() {
        let host = next_test_ip4();
        let server = make_server(&host);
        let client = make_client(&host);

        client.gauge("metric", 9.1, None);

        let response = server_recv(server);
        assert_eq!("myapp.metric:9.1|g", response);
    }

    #[test]
    fn test_sending_gauge_without_prefix() {
        let host = next_test_ip4();
        let server = make_server(&host);
        let client = make_client(&host);

        client.gauge("metric", 9.1, None);

        let response = server_recv(server);
        assert_eq!("metric:9.1|g", response);
    }

    #[test]
    fn test_sending_incr() {
        let host = next_test_ip4();
        let server = make_server(&host);
        let client = make_client(&host);

        client.incr("metric", None);

        let response = server_recv(server);
        assert_eq!("myapp.metric:1|c", response);
    }

    #[test]
    fn test_sending_decr() {
        let host = next_test_ip4();
        let server = make_server(&host);
        let client = make_client(&host);

        client.decr("metric", None);

        let response = server_recv(server);
        assert_eq!("myapp.metric:-1|c", response);
    }

    #[test]
    fn test_sending_count() {
        let host = next_test_ip4();
        let server = make_server(&host);
        let client = make_client(&host);

        client.count("metric", 12.2, None);

        let response = server_recv(server);
        assert_eq!("myapp.metric:12.2|c", response);
    }

    #[test]
    fn test_sending_timer() {
        let host = next_test_ip4();
        let server = make_server(&host);
        let client = make_client(&host);

        client.timer("metric", 21.39, None);

        let response = server_recv(server);
        assert_eq!("myapp.metric:21.39|ms", response);
    }

    #[test]
    fn test_sending_timed_block() {
        let host = next_test_ip4();
        let server = make_server(&host);
        let client = make_client(&host);
        struct TimeTest {
            num: u8,
        }

        let mut t = TimeTest { num: 10 };
        let output = client.time("time_block", None, || {
            t.num += 2;
            "a string"
        });

        let response = server_recv(server);
        assert_eq!(output, "a string");
        assert_eq!(t.num, 12);
        assert!(response.contains("myapp.time_block"));
        assert!(response.contains("|ms"));
    }

    #[test]
    fn test_sending_histogram() {
        let host = next_test_ip4();
        let server = make_server(&host);
        let client = make_client(&host);

        // without tags
        client.histogram("metric", 9.1, None);
        let mut response = server_recv(server.try_clone().unwrap());
        assert_eq!("myapp.metric:9.1|h", response);
        // with tags
        client.histogram("metric", 9.1, Some(vec!["tag1", "tag2:test"]));
        response = server_recv(server.try_clone().unwrap());
        assert_eq!("myapp.metric:9.1|h|#tag1,tag2:test", response);
    }

    #[test]
    fn test_sending_histogram_with_constant_tags() {
        let host = next_test_ip4();
        let server = make_server(&host);
        let client = make_client(&host);

        // without tags
        client.histogram("metric", 9.1, None);
        let mut response = server_recv(server.try_clone().unwrap());
        assert_eq!("myapp.metric:9.1|h|#tag1common,tag2common:test", response);
        // with tags
        let tags = Some(vec!["tag1", "tag2:test"]);
        client.histogram("metric", 9.1, tags.clone());
        response = server_recv(server.try_clone().unwrap());
        assert_eq!(
            "myapp.metric:9.1|h|#tag1common,tag2common:test,tag1,tag2:test",
            response
        );
        // repeat
        client.histogram("metric", 19.12, tags);
        response = server_recv(server.try_clone().unwrap());
        assert_eq!(
            "myapp.metric:19.12|h|#tag1common,tag2common:test,tag1,tag2:test",
            response
        );
    }

    #[test]
    fn test_sending_event_with_tags() {
        let host = next_test_ip4();
        let server = make_server(&host);
        let client = make_client(&host);

        client.event(
            "Title Test",
            "Text ABC",
            AlertType::Error,
            Some(vec!["tag1", "tag2:test"]),
        );

        let response = server_recv(server);
        assert_eq!(
            "_e{10,8}:Title Test|Text ABC|t:error|#tag1,tag2:test",
            response
        );
    }

    #[test]
    fn test_sending_service_check_with_tags() {
        let host = next_test_ip4();
        let server = make_server(&host);
        let client = make_client(&host);

        client.service_check(
            "Service.check.name",
            ServiceCheckStatus::Critical,
            Some(vec!["tag1", "tag2:test"]),
        );

        let response = server_recv(server);
        assert_eq!("_sc|Service.check.name|2|#tag1,tag2:test", response);
    }

    #[test]
    fn test_pipeline_sending_time_block() {
        let host = next_test_ip4();
        let server = make_server(&host);
        let client = make_client(&host);
        let mut pipeline = client.pipeline();
        pipeline.gauge("metric", 9.1);
        struct TimeTest {
            num: u8,
        }

        let mut t = TimeTest { num: 10 };
        pipeline.time("time_block", || {
            t.num += 2;
        });
        pipeline.send(&client);

        let response = server_recv(server);
        assert_eq!(t.num, 12);
        assert_eq!("myapp.metric:9.1|g\nmyapp.time_block:0|ms", response);
    }

    #[test]
    fn test_pipeline_sending_gauge() {
        let host = next_test_ip4();
        let server = make_server(&host);
        let client = make_client(&host);
        let mut pipeline = client.pipeline();
        pipeline.gauge("metric", 9.1);
        pipeline.send(&client);

        let response = server_recv(server);
        assert_eq!("myapp.metric:9.1|g", response);
    }

    #[test]
    fn test_pipeline_sending_histogram() {
        let host = next_test_ip4();
        let server = make_server(&host);
        let client = make_client(&host);
        let mut pipeline = client.pipeline();
        pipeline.histogram("metric", 9.1);
        pipeline.send(&client);

        let response = server_recv(server);
        assert_eq!("myapp.metric:9.1|h", response);
    }

    #[test]
    fn test_pipeline_sending_multiple_data() {
        let host = next_test_ip4();
        let server = make_server(&host);
        let client = make_client(&host);
        let mut pipeline = client.pipeline();
        pipeline.gauge("metric", 9.1);
        pipeline.count("metric", 12.2);
        pipeline.send(&client);

        let response = server_recv(server);
        assert_eq!("myapp.metric:9.1|g\nmyapp.metric:12.2|c", response);
    }

    #[test]
    fn test_pipeline_set_max_udp_size() {
        let host = next_test_ip4();
        let server = make_server(&host);
        let client = make_client(&host);
        let mut pipeline = client.pipeline();
        pipeline.set_max_udp_size(20);
        pipeline.gauge("metric", 9.1);
        pipeline.count("metric", 12.2);
        pipeline.send(&client);

        let response = server_recv(server);
        assert_eq!("myapp.metric:9.1|g", response);
    }

    #[test]
    fn test_pipeline_send_metric_after_pipeline() {
        let host = next_test_ip4();
        let server = make_server(&host);
        let client = make_client(&host);
        let mut pipeline = client.pipeline();

        pipeline.gauge("load", 9.0);
        pipeline.count("customers", 7.0);
        pipeline.send(&client);

        // Should still be able to send metrics
        // with the client.
        client.count("customers", 6.0, None);

        let response = server_recv(server);
        assert_eq!("myapp.load:9|g\nmyapp.customers:7|c", response);
    }
}
