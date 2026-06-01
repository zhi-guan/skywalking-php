// Licensed to the Apache Software Foundation (ASF) under one or more
// contributor license agreements.  See the NOTICE file distributed with
// this work for additional information regarding copyright ownership.
// The ASF licenses this file to You under the Apache License, Version 2.0
// (the "License"); you may not use this file except in compliance with
// the License.  You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

use anyhow::{Context, bail};
use axum::{
    Extension, Router,
    body::Body,
    extract::ConnectInfo,
    http::{Request, StatusCode},
    response::{IntoResponse, Response},
    routing::any,
};
use futures_util::future::join_all;
use libc::{SIGTERM, kill, pid_t};
use once_cell::sync::Lazy;
use std::{
    env,
    fs::{self, File},
    io::{self, Cursor},
    net::SocketAddr,
    process::{ExitStatus, Stdio},
    sync::Arc,
    thread,
    time::Duration,
};
use tokio::{
    net::TcpStream,
    process::{Child, Command},
    task::JoinHandle,
};
use tokio_stream::StreamExt;
use tracing::{Level, error, info, instrument};
use tracing_subscriber::FmtSubscriber;

pub const PROCESS_LOG_LEVEL: &str = "DEBUG";

pub const PROXY_SERVER_1_ADDRESS: &str = "127.0.0.1:9011";
pub const PROXY_SERVER_2_ADDRESS: &str = "127.0.0.1:9012";
pub const FPM_SERVER_1_ADDRESS: &str = "127.0.0.1:9001";
pub const FPM_SERVER_2_ADDRESS: &str = "127.0.0.1:9002";
pub const SWOOLE_SERVER_1_ADDRESS: &str = "127.0.0.1:9501";
pub const SWOOLE_SERVER_2_ADDRESS: &str = "127.0.0.1:9502";
pub const COLLECTOR_GRPC_ADDRESS: &str = "127.0.0.1:19876";
pub const COLLECTOR_HTTP_ADDRESS: &str = "127.0.0.1:12800";

pub const TARGET: &str = if cfg!(debug_assertions) {
    "debug"
} else {
    "release"
};

pub const EXT: &str = if cfg!(target_os = "linux") {
    ".so"
} else if cfg!(target_os = "macos") {
    ".dylib"
} else {
    ""
};

pub static ENABLE_ZEND_OBSERVER: Lazy<String> =
    Lazy::new(|| env::var("ENABLE_ZEND_OBSERVER").unwrap_or_else(|_| "On".to_owned()));

pub static HTTP_CLIENT: Lazy<reqwest::Client> = Lazy::new(reqwest::Client::new);

pub struct Fixture {
    http_server_1_handle: JoinHandle<()>,
    http_server_2_handle: JoinHandle<()>,
    php_fpm_1_child: Child,
    php_fpm_2_child: Child,
    php_swoole_1_child: Child,
    php_swoole_2_child: Child,
}

pub struct FpmOnlyFixture {
    http_server_1_handle: JoinHandle<()>,
    http_server_2_handle: JoinHandle<()>,
    php_fpm_1_child: Child,
    php_fpm_2_child: Child,
}

pub async fn setup() -> Fixture {
    setup_logging();
    info!(
        TARGET = TARGET,
        EXT = EXT,
        ENABLE_ZEND_OBSERVER = &*ENABLE_ZEND_OBSERVER,
        "setup fixture"
    );

    Fixture {
        http_server_1_handle: tokio::spawn(setup_http_proxy_server(
            PROXY_SERVER_1_ADDRESS,
            FPM_SERVER_1_ADDRESS,
        )),
        http_server_2_handle: tokio::spawn(setup_http_proxy_server(
            PROXY_SERVER_2_ADDRESS,
            FPM_SERVER_2_ADDRESS,
        )),
        php_fpm_1_child: setup_php_fpm(1, FPM_SERVER_1_ADDRESS),
        php_fpm_2_child: setup_php_fpm(2, FPM_SERVER_2_ADDRESS),
        php_swoole_1_child: setup_php_swoole(1),
        php_swoole_2_child: setup_php_swoole(2),
    }
}

pub async fn setup_fpm_only() -> FpmOnlyFixture {
    setup_logging();
    info!(
        TARGET = TARGET,
        EXT = EXT,
        ENABLE_ZEND_OBSERVER = &*ENABLE_ZEND_OBSERVER,
        "setup fpm-only fixture"
    );

    FpmOnlyFixture {
        http_server_1_handle: tokio::spawn(setup_http_proxy_server(
            PROXY_SERVER_1_ADDRESS,
            FPM_SERVER_1_ADDRESS,
        )),
        http_server_2_handle: tokio::spawn(setup_http_proxy_server(
            PROXY_SERVER_2_ADDRESS,
            FPM_SERVER_2_ADDRESS,
        )),
        php_fpm_1_child: setup_php_fpm(1, FPM_SERVER_1_ADDRESS),
        php_fpm_2_child: setup_php_fpm(2, FPM_SERVER_2_ADDRESS),
    }
}

pub async fn teardown(fixture: Fixture) {
    fixture.http_server_1_handle.abort();
    fixture.http_server_2_handle.abort();

    let results = join_all([
        kill_command(fixture.php_fpm_1_child),
        kill_command(fixture.php_fpm_2_child),
        kill_command(fixture.php_swoole_1_child),
        kill_command(fixture.php_swoole_2_child),
    ])
    .await;
    for result in results {
        assert!(result.unwrap().success());
    }
}

pub async fn teardown_fpm_only(fixture: FpmOnlyFixture) {
    fixture.http_server_1_handle.abort();
    fixture.http_server_2_handle.abort();

    let results = join_all([
        kill_command(fixture.php_fpm_1_child),
        kill_command(fixture.php_fpm_2_child),
    ])
    .await;
    for result in results {
        assert!(result.unwrap().success());
    }
}

fn setup_logging() {
    let subscriber = FmtSubscriber::builder()
        .with_max_level(Level::INFO)
        .finish();

    tracing::subscriber::set_global_default(subscriber).expect("setting default subscriber failed");
}

#[derive(Debug)]
struct State {
    http_addr: SocketAddr,
    fpm_addr: SocketAddr,
}

#[instrument]
async fn setup_http_proxy_server(http_addr: &str, fpm_addr: &'static str) {
    let http_addr = http_addr.parse().unwrap();
    let fpm_addr = fpm_addr.parse().unwrap();
    let state = Arc::new(State {
        http_addr,
        fpm_addr,
    });
    info!(?state, "start http proxy server");

    let app = Router::new()
        .route("/{*path}", any(http_proxy_fpm_handler))
        .layer(Extension(state.clone()));

    let listener = tokio::net::TcpListener::bind(&state.http_addr)
        .await
        .unwrap();
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .await
    .unwrap();
}

#[instrument(skip_all)]
async fn http_proxy_fpm_handler(
    ConnectInfo(remote_addr): ConnectInfo<SocketAddr>, Extension(state): Extension<Arc<State>>,
    req: Request<Body>,
) -> Response {
    let fut = async move {
        let method = &req.method().to_string();
        let path = &req.uri().path().to_string();
        let query = &req.uri().query().unwrap_or_default().to_string();
        info!(method, uri=?req.uri(), "http proxy to fpm");

        let stream = TcpStream::connect(&state.fpm_addr).await?;
        let client = fastcgi_client::Client::new(stream);

        let mut params = fastcgi_client::Params::default()
            .request_method(method)
            .query_string(query)
            .server_addr(state.http_addr.ip().to_string())
            .server_port(state.http_addr.port())
            .remote_addr(remote_addr.ip().to_string())
            .remote_port(remote_addr.port())
            .server_name("TEST");

        // Only tread with skywalking and custom headers.
        for (key, value) in req.headers() {
            let mut param_key = String::new();

            if key.as_str().starts_with("content-") {
                param_key = key.as_str().replace('-', "_").to_uppercase();
            } else if key.as_str().starts_with("sw")
                || key.as_str().starts_with("x-")
                || key.as_str() == "host"
            {
                param_key = "HTTP_".to_owned() + &key.as_str().replace('-', "_").to_uppercase();
            }

            if !param_key.is_empty() {
                params.insert(
                    param_key.into(),
                    std::str::from_utf8(value.as_bytes())?.to_string().into(),
                );
            }
        }

        let mut buffer = Vec::new();
        let mut body_stream = req.into_body().into_data_stream();
        while let Some(buf) = body_stream.next().await {
            let buf = buf.context("read body failed")?;
            buffer.extend_from_slice(&buf);
        }

        let params = params_set_script(params, path, query);

        info!(?params, "proxy http to php-fpm");

        let fastcgi_client::Response { stdout, stderr, .. } = client
            .execute_once(fastcgi_client::Request::new(params, Cursor::new(buffer)))
            .await
            .context("request fpm failed")?;

        if let Some(stderr) = stderr {
            return Ok((
                StatusCode::INTERNAL_SERVER_ERROR,
                String::from_utf8(stderr).context("decode to UTF-8 string failed")?,
            )
                .into_response());
        }

        if let Some(stdout) = stdout {
            let body = if let Some(index) = stdout.windows(4).position(|w| w == b"\r\n\r\n") {
                stdout[index + 4..].to_vec()
            } else {
                stdout
            };
            return Ok((StatusCode::OK, body).into_response());
        }

        bail!("stdout and stderr are empty");
    };
    match fut.await {
        Ok(x) => x,
        Err(err) => {
            error!(?err, "proxy failed");
            (StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response()
        }
    }
}

fn params_set_script<'a>(
    params: fastcgi_client::Params<'a>, script_name: &'a str, query: &'a str,
) -> fastcgi_client::Params<'a> {
    params
        .script_name(script_name)
        .script_filename(create_script_filename(script_name))
        .request_uri(create_request_uri(script_name, query))
        .document_uri(script_name)
}

fn create_script_filename(script_name: &str) -> String {
    env::current_dir()
        .unwrap()
        .join("tests")
        .join("php")
        .join("fpm")
        .join(script_name.trim_start_matches('/'))
        .to_str()
        .unwrap()
        .to_owned()
}

fn create_request_uri(script_name: &str, query: &str) -> String {
    if query.is_empty() {
        script_name.to_owned()
    } else {
        format!("{}?{}", script_name, query)
    }
}

#[instrument]
fn setup_php_fpm(index: usize, fpm_addr: &str) -> Child {
    let php_fpm = env::var("PHP_FPM_BIN").unwrap_or_else(|_| "php-fpm".to_string());
    let conf = prepare_php_fpm_conf(index);
    let mut args = vec![
        php_fpm,
        "-F".to_string(),
        "-R".to_string(),
        "-y".to_string(),
        conf,
        "-d".to_string(),
        format!("extension={}", agent_extension_path()),
    ];
    append_extra_extensions(&mut args);
    args.extend([
        "-d".to_string(),
        "skywalking_agent.enable=On".to_string(),
        "-d".to_string(),
        format!(
            "skywalking_agent.service_name=skywalking-agent-test-{}",
            index
        ),
        "-d".to_string(),
        format!("skywalking_agent.server_addr={}", COLLECTOR_GRPC_ADDRESS),
        "-d".to_string(),
        format!("skywalking_agent.log_level={}", PROCESS_LOG_LEVEL),
        "-d".to_string(),
        format!(
            "skywalking_agent.log_file=/tmp/fpm-skywalking-agent.{}.log",
            index
        ),
        "-d".to_string(),
        "skywalking_agent.worker_threads=3".to_string(),
        "-d".to_string(),
        format!(
            "skywalking_agent.enable_zend_observer={}",
            *ENABLE_ZEND_OBSERVER
        ),
        "-d".to_string(),
        "skywalking_agent.psr_logging_level=Warning".to_string(),
    ]);
    info!(cmd = args.join(" "), "start command");
    let child = Command::new(&args[0])
        .args(&args[1..])
        .stdin(Stdio::null())
        .stdout(File::create("/tmp/fpm-skywalking-stdout.log").unwrap())
        .stderr(File::create("/tmp/fpm-skywalking-stderr.log").unwrap())
        .spawn()
        .unwrap();
    thread::sleep(Duration::from_secs(3));
    child
}

#[instrument]
fn setup_php_swoole(index: usize) -> Child {
    let php = env::var("PHP_BIN").unwrap_or_else(|_| "php".to_string());
    let mut args = vec![
        php,
        "-d".to_string(),
        format!("extension={}", agent_extension_path()),
    ];
    append_extra_extensions(&mut args);
    args.extend([
        "-d".to_string(),
        "skywalking_agent.enable=On".to_string(),
        "-d".to_string(),
        format!(
            "skywalking_agent.service_name=skywalking-agent-test-{}-swoole",
            index
        ),
        "-d".to_string(),
        format!("skywalking_agent.server_addr={}", COLLECTOR_GRPC_ADDRESS),
        "-d".to_string(),
        format!("skywalking_agent.log_level={}", PROCESS_LOG_LEVEL),
        "-d".to_string(),
        format!(
            "skywalking_agent.log_file=/tmp/swoole-skywalking-agent.{}.log",
            index
        ),
        "-d".to_string(),
        "skywalking.worker_threads=3".to_string(),
        "-d".to_string(),
        format!(
            "skywalking_agent.enable_zend_observer={}",
            *ENABLE_ZEND_OBSERVER
        ),
        format!("tests/php/swoole/main.{}.php", index),
    ]);
    info!(cmd = args.join(" "), "start command");
    let child = Command::new(&args[0])
        .args(&args[1..])
        .stdin(Stdio::null())
        .stdout(File::create("/tmp/swoole-skywalking-stdout.log").unwrap())
        .stderr(File::create("/tmp/swoole-skywalking-stderr.log").unwrap())
        .spawn()
        .unwrap();
    thread::sleep(Duration::from_secs(3));
    child
}

async fn kill_command(mut child: Child) -> io::Result<ExitStatus> {
    if let Some(id) = child.id() {
        unsafe {
            kill(id as pid_t, SIGTERM);
        }
    }
    child.wait().await
}

fn append_extra_extensions(args: &mut Vec<String>) {
    let Ok(extra_extensions) = env::var("PHP_EXTRA_EXTENSIONS") else {
        return;
    };

    for ext in extra_extensions
        .split(',')
        .map(str::trim)
        .filter(|ext| !ext.is_empty())
    {
        args.push("-d".to_string());
        args.push(format!("extension={ext}"));
    }
}

fn agent_extension_path() -> String {
    env::current_dir()
        .unwrap()
        .join("target")
        .join(TARGET)
        .join(format!("libskywalking_agent{}", EXT))
        .to_str()
        .unwrap()
        .to_string()
}

fn prepare_php_fpm_conf(index: usize) -> String {
    let template = env::current_dir()
        .unwrap()
        .join("tests")
        .join("conf")
        .join(format!("php-fpm.{}.conf", index));
    let mut conf = fs::read_to_string(template).unwrap();

    let uid = unsafe { libc::geteuid() };
    let gid = unsafe { libc::getegid() };
    conf = conf.replace("user = 1000", &format!("user = {}", uid));
    conf = conf.replace("group = 1000", &format!("group = {}", gid));

    let path = env::temp_dir().join(format!("skywalking-php-fpm.{}.conf", index));
    fs::write(&path, conf).unwrap();
    path.to_str().unwrap().to_string()
}
