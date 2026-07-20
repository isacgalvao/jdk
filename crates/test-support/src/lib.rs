//! Hermetic test support: a hand-rolled loopback HTTP server (full control
//! over ETag/304, Range/206, redirect chains and failure sequences, one
//! request per connection via `Connection: close` — no extra dev-dependency),
//! fake-JDK zip and catalog fixtures, and on-demand builds of the workspace
//! binaries.

pub use jdk_core::download::sha256_hex;

#[cfg(windows)]
pub mod reg;

use jdk_core::index::{
    IndexEntry, IndexFile, Package, ReleaseStatus, SCHEMA_VERSION, current_platform,
};
use std::collections::{BTreeMap, HashMap};
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex, OnceLock};
use std::thread;
use std::time::Duration;

#[derive(Debug, Clone)]
pub struct Request {
    /// Path including any query string.
    pub path: String,
    /// Header names lowercased.
    pub headers: HashMap<String, String>,
}

impl Request {
    /// Path without the query string — the routing key.
    pub fn route(&self) -> &str {
        self.path.split('?').next().unwrap_or(&self.path)
    }

    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .get(&name.to_ascii_lowercase())
            .map(String::as_str)
    }
}

pub struct Response {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
    /// `(chunk_size, delay)`: body written in paced chunks, simulating a
    /// slow link (timeout-semantics tests).
    pub pace: Option<(usize, Duration)>,
}

impl Response {
    pub fn ok(body: impl Into<Vec<u8>>) -> Self {
        Response {
            status: 200,
            headers: Vec::new(),
            body: body.into(),
            pace: None,
        }
    }

    pub fn empty(status: u16) -> Self {
        Response {
            status,
            headers: Vec::new(),
            body: Vec::new(),
            pace: None,
        }
    }

    pub fn redirect(location: &str) -> Self {
        Response::empty(302).with_header("Location", location)
    }

    pub fn with_header(mut self, name: &str, value: &str) -> Self {
        self.headers.push((name.to_string(), value.to_string()));
        self
    }

    pub fn trickled(mut self, chunk_size: usize, delay: Duration) -> Self {
        self.pace = Some((chunk_size, delay));
        self
    }
}

type Handler = dyn Fn(&Request) -> Response + Send + Sync;

/// Loopback server with per-path handlers; unknown paths get a 404. Every
/// request (path + headers) is recorded for assertions.
pub struct Server {
    url: String,
    routes: Arc<Mutex<HashMap<String, Arc<Handler>>>>,
    requests: Arc<Mutex<Vec<Request>>>,
}

impl Server {
    pub fn start() -> Server {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        let url = format!("http://{}", listener.local_addr().expect("local addr"));
        let routes: Arc<Mutex<HashMap<String, Arc<Handler>>>> = Arc::default();
        let requests: Arc<Mutex<Vec<Request>>> = Arc::default();

        let accept_routes = Arc::clone(&routes);
        let accept_requests = Arc::clone(&requests);
        thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { break };
                let routes = Arc::clone(&accept_routes);
                let requests = Arc::clone(&accept_requests);
                thread::spawn(move || {
                    let _ = serve(stream, &routes, &requests);
                });
            }
        });

        Server {
            url,
            routes,
            requests,
        }
    }

    /// `http://127.0.0.1:<port>`.
    pub fn url(&self) -> &str {
        &self.url
    }

    pub fn route(
        &self,
        path: &str,
        handler: impl Fn(&Request) -> Response + Send + Sync + 'static,
    ) {
        self.routes
            .lock()
            .unwrap()
            .insert(path.to_string(), Arc::new(handler));
    }

    /// How many requests hit `path` (query string ignored).
    pub fn hits(&self, path: &str) -> usize {
        self.requests_to(path).len()
    }

    pub fn requests_to(&self, path: &str) -> Vec<Request> {
        self.requests
            .lock()
            .unwrap()
            .iter()
            .filter(|request| request.route() == path)
            .cloned()
            .collect()
    }
}

fn serve(
    mut stream: TcpStream,
    routes: &Mutex<HashMap<String, Arc<Handler>>>,
    requests: &Mutex<Vec<Request>>,
) -> std::io::Result<()> {
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut request_line = String::new();
    reader.read_line(&mut request_line)?;
    let path = request_line
        .split_whitespace()
        .nth(1)
        .unwrap_or("/")
        .to_string();

    let mut headers = HashMap::new();
    loop {
        let mut line = String::new();
        reader.read_line(&mut line)?;
        let line = line.trim_end();
        if line.is_empty() {
            break;
        }
        if let Some((name, value)) = line.split_once(':') {
            headers.insert(name.trim().to_ascii_lowercase(), value.trim().to_string());
        }
    }

    let request = Request { path, headers };
    let handler = routes.lock().unwrap().get(request.route()).cloned();
    requests.lock().unwrap().push(request.clone());
    let response = match handler {
        Some(handler) => handler(&request),
        None => Response::empty(404),
    };

    let reason = match response.status {
        200 => "OK",
        206 => "Partial Content",
        302 => "Found",
        304 => "Not Modified",
        404 => "Not Found",
        429 => "Too Many Requests",
        500 => "Internal Server Error",
        503 => "Service Unavailable",
        _ => "Response",
    };
    let mut head = format!("HTTP/1.1 {} {reason}\r\n", response.status);
    for (name, value) in &response.headers {
        head.push_str(&format!("{name}: {value}\r\n"));
    }
    head.push_str("Connection: close\r\n");
    let has_body = response.status != 304;
    if has_body {
        head.push_str(&format!("Content-Length: {}\r\n", response.body.len()));
    }
    head.push_str("\r\n");
    stream.write_all(head.as_bytes())?;
    if has_body {
        match response.pace {
            Some((chunk_size, delay)) => {
                for chunk in response.body.chunks(chunk_size.max(1)) {
                    stream.write_all(chunk)?;
                    stream.flush()?;
                    thread::sleep(delay);
                }
            }
            None => stream.write_all(&response.body)?,
        }
    }
    stream.flush()
}

/// A port nothing listens on (bound and immediately released).
pub fn dead_url() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
    let addr = listener.local_addr().expect("local addr");
    drop(listener);
    format!("http://{addr}")
}

/// Zip of a fake JDK: `java.exe`/`javac.exe` are the given binary (the
/// workspace `fake_java` fixture), wrapped one directory level deep so the
/// install has to normalize the layout, plus a `release` file for realism.
pub fn fake_jdk_zip(java_exe: &[u8]) -> Vec<u8> {
    let mut cursor = std::io::Cursor::new(Vec::new());
    let mut writer = zip::ZipWriter::new(&mut cursor);
    let options = zip::write::SimpleFileOptions::default();
    for name in ["bin/java.exe", "bin/javac.exe"] {
        writer
            .start_file(format!("jdk-21.0.5+11/{name}"), options)
            .unwrap();
        writer.write_all(java_exe).unwrap();
    }
    writer.start_file("jdk-21.0.5+11/release", options).unwrap();
    writer.write_all(b"JAVA_VERSION=\"21.0.5\"\n").unwrap();
    writer.finish().unwrap();
    cursor.into_inner()
}

/// A GA/LTS temurin java package for the current platform. Tests tweak the
/// fields they care about afterwards.
pub fn package(version: &str, url: &str, sha256: &str, size: u64) -> Package {
    let (os, arch) = current_platform();
    Package {
        tool: "java".to_string(),
        vendor: "temurin".to_string(),
        version: version.to_string(),
        os: os.to_string(),
        arch: arch.to_string(),
        release_status: ReleaseStatus::Ga,
        lts: true,
        size,
        sha256: sha256.to_string(),
        url: url.to_string(),
    }
}

/// index.json bytes for `files` — for tests that plant a catalog cache by
/// hand instead of serving one.
pub fn index_json(files: Vec<IndexEntry>) -> Vec<u8> {
    let index = IndexFile {
        version: SCHEMA_VERSION,
        updated: "2026-07-17T00:00:00Z".to_string(),
        files,
    };
    serde_json::to_vec(&index).unwrap()
}

/// Publishes `packages` on `server` as a well-formed index for the current
/// platform: one platform file per vendor present, all sha256s consistent.
pub fn serve_catalog(server: &Server, packages: &[Package]) {
    let (os, arch) = current_platform();
    let mut by_vendor: BTreeMap<&str, Vec<&Package>> = BTreeMap::new();
    for package in packages {
        by_vendor.entry(&package.vendor).or_default().push(package);
    }

    let mut files = Vec::new();
    for (vendor, packages) in by_vendor {
        let path = format!("{os}-{arch}/{vendor}.json");
        let body = serde_json::to_vec(&packages).unwrap();
        files.push(IndexEntry {
            path: path.clone(),
            vendor: vendor.to_string(),
            os: os.to_string(),
            arch: arch.to_string(),
            size: body.len() as u64,
            sha256: sha256_hex(&body),
        });
        server.route(&format!("/{path}"), move |_| Response::ok(body.clone()));
    }
    let body = index_json(files);
    server.route("/index.json", move |_| Response::ok(body.clone()));
}

/// Paths of the workspace `jdk-shim` and `fake_java` binaries, built on
/// demand — integration tests of other crates cannot use `CARGO_BIN_EXE_*`
/// (those exist only inside the owning crate's own tests).
pub fn shim_binaries() -> (PathBuf, PathBuf) {
    static BINARIES: OnceLock<(PathBuf, PathBuf)> = OnceLock::new();
    BINARIES
        .get_or_init(|| {
            let mut built = built_executables("jdk-shim");
            (
                built
                    .remove("jdk-shim")
                    .expect("jdk-shim executable in cargo output"),
                built
                    .remove("fake_java")
                    .expect("fake_java executable in cargo output"),
            )
        })
        .clone()
}

/// Path of the workspace `jdk` CLI binary, built on demand (see
/// [`shim_binaries`] for why `CARGO_BIN_EXE_*` does not work here).
pub fn jdk_binary() -> PathBuf {
    static BINARY: OnceLock<PathBuf> = OnceLock::new();
    BINARY
        .get_or_init(|| {
            built_executables("jdk")
                .remove("jdk")
                .expect("jdk executable in cargo output")
        })
        .clone()
}

/// Builds every binary of the sibling crate `crates\<crate_dir>` and returns
/// the executables by target name, parsed from cargo's JSON messages.
fn built_executables(crate_dir: &str) -> HashMap<String, PathBuf> {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("crates dir")
        .join(crate_dir)
        .join("Cargo.toml");
    let output = Command::new(env!("CARGO"))
        .arg("build")
        .arg("--manifest-path")
        .arg(&manifest)
        .arg("--bins")
        .arg("--message-format=json")
        .output()
        .unwrap_or_else(|err| panic!("run cargo build for {crate_dir}: {err}"));
    assert!(
        output.status.success(),
        "building {crate_dir} failed:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let mut executables = HashMap::new();
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        let Ok(message) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        if message["reason"] != "compiler-artifact" {
            continue;
        }
        let Some(executable) = message["executable"].as_str() else {
            continue;
        };
        if let Some(name) = message["target"]["name"].as_str() {
            executables.insert(name.to_string(), PathBuf::from(executable));
        }
    }
    executables
}
