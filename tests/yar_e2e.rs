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

mod common;

use crate::common::{COLLECTOR_HTTP_ADDRESS, HTTP_CLIENT, PROXY_SERVER_1_ADDRESS};
use reqwest::{RequestBuilder, StatusCode, header::CONTENT_TYPE};
use std::{
    future::Future,
    panic::{catch_unwind, resume_unwind},
    time::Duration,
};
use tokio::{
    fs::{self, File},
    runtime::Handle,
    task,
    time::sleep,
};
use tracing::{error, info};

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn yar_e2e() {
    let fixture = common::setup_fpm_only().await;

    sleep(Duration::from_secs(5)).await;

    let result = catch_unwind(|| {
        task::block_in_place(|| {
            Handle::current().block_on(run_e2e());
        });
    });

    common::teardown_fpm_only(fixture).await;

    if let Err(e) = result {
        resume_unwind(e);
    }
}

async fn run_e2e() {
    request_common(
        HTTP_CLIENT.get(format!("http://{}/yar.php", PROXY_SERVER_1_ADDRESS)),
        "ok",
    )
    .await;

    sleep(Duration::from_secs(3)).await;

    request(
        HTTP_CLIENT
            .post(format!("http://{}/dataValidate", COLLECTOR_HTTP_ADDRESS))
            .header(CONTENT_TYPE, "text/yaml")
            .body(
                File::open("./tests/data/expected_context_yar.yaml")
                    .await
                    .unwrap(),
            ),
        "success",
        |content| async move {
            let result_file = "/tmp/skywalking-agent-collector-validate-result-yar.txt";
            if let Err(err) = fs::write(result_file, content).await {
                error!(?err, "write to {} failed", result_file);
            }
        },
    )
    .await;
}

async fn request_common(request_builder: RequestBuilder, actual_content: impl Into<String>) {
    request(request_builder, actual_content, |content| async move {
        info!("response content: {}", content);
    })
    .await
}

async fn request<F>(
    request_builder: RequestBuilder, actual_content: impl Into<String>,
    handler: impl FnOnce(String) -> F,
) where
    F: Future<Output = ()>,
{
    let response = request_builder.send().await.unwrap();
    let status = response.status();
    let content = response.text().await.unwrap();
    handler(content.clone()).await;
    assert_eq!((status, content), (StatusCode::OK, actual_content.into()));
}
