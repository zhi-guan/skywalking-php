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

use super::{Plugin, log_exception};
use crate::{
    component::COMPONENT_PHP_ID,
    context::{RequestContext, SW_HEADER},
    execute::{AfterExecuteHook, BeforeExecuteHook, Noop, get_this_mut, validate_num_args},
};
use anyhow::Context;
use dashmap::DashMap;
use once_cell::sync::Lazy;
use phper::{
    arrays::{IterKey, ZArray},
    functions::call,
    objects::ZObj,
    values::ZVal,
};
use skywalking::{
    proto::v3::SpanLayer,
    trace::span::{HandleSpanObject, Span},
};
use std::{cell::Cell, collections::HashMap};
use tracing::debug;
use url::Url;

const YAR_OPT_HEADER: &str = "YAR_OPT_HEADER";

static CLIENT_INFO_MAP: Lazy<DashMap<u32, YarClientInfo>> = Lazy::new(Default::default);

thread_local! {
    static INTERNAL_SET_OPT: Cell<bool> = const { Cell::new(false) };
}

#[derive(Default, Clone)]
pub struct YarPlugin;

#[derive(Default, Clone)]
struct YarClientInfo {
    uri: String,
    headers: HashMap<String, String>,
}

struct YarCallInfo {
    handle: u32,
    headers: HashMap<String, String>,
    span: Span,
}

#[derive(Clone)]
struct YarPeerInfo {
    uri: String,
    peer: String,
    service: String,
}

impl Plugin for YarPlugin {
    fn class_names(&self) -> Option<&'static [&'static str]> {
        Some(&["Yar_Client"])
    }

    fn function_name_prefix(&self) -> Option<&'static str> {
        None
    }

    fn hook(
        &self, class_name: Option<&str>, function_name: &str,
    ) -> Option<(Box<BeforeExecuteHook>, Box<AfterExecuteHook>)> {
        match (class_name, function_name) {
            (Some("Yar_Client"), "__construct") => Some(self.hook_construct()),
            (Some("Yar_Client"), "setOpt") => Some(self.hook_set_opt()),
            (Some("Yar_Client"), "__call" | "call") => Some(self.hook_call()),
            _ => None,
        }
    }
}

impl YarPlugin {
    fn hook_construct(&self) -> (Box<BeforeExecuteHook>, Box<AfterExecuteHook>) {
        (
            Box::new(|_, execute_data| {
                validate_num_args(execute_data, 1)?;

                let this = get_this_mut(execute_data)?;

                let handle = this.handle();
                let uri = execute_data
                    .get_parameter(0)
                    .expect_z_str()?
                    .to_str()?
                    .to_string();
                let headers = execute_data
                    .get_parameter(1)
                    .as_z_arr()
                    .and_then(extract_headers_from_options)
                    .unwrap_or_default();

                CLIENT_INFO_MAP.insert(handle, YarClientInfo { uri, headers });

                Ok(Box::new(()))
            }),
            Noop::noop(),
        )
    }

    fn hook_set_opt(&self) -> (Box<BeforeExecuteHook>, Box<AfterExecuteHook>) {
        (
            Box::new(|_, execute_data| {
                validate_num_args(execute_data, 2)?;

                if INTERNAL_SET_OPT.with(|flag| flag.get()) {
                    return Ok(Box::new(()));
                }

                let option = execute_data.get_parameter(0).expect_long()?;
                if option != get_yar_opt_header()? {
                    return Ok(Box::new(()));
                }

                let this = get_this_mut(execute_data)?;
                let handle = this.handle();
                let headers = extract_headers(execute_data.get_mut_parameter(1))?;

                let mut info = CLIENT_INFO_MAP.entry(handle).or_default();
                info.headers = headers;

                Ok(Box::new(()))
            }),
            Noop::noop(),
        )
    }

    fn hook_call(&self) -> (Box<BeforeExecuteHook>, Box<AfterExecuteHook>) {
        (
            Box::new(|request_id, execute_data| {
                validate_num_args(execute_data, 1)?;

                let method = execute_data
                    .get_parameter(0)
                    .expect_z_str()?
                    .to_str()?
                    .to_string();
                let this = get_this_mut(execute_data)?;
                let handle = this.handle();
                debug!(request_id, handle, method, "prepare yar client call");
                let info = CLIENT_INFO_MAP
                    .get(&handle)
                    .map(|info| info.value().clone())
                    .context("yar client info not exists")?;

                let peer = parse_peer_info(&info.uri)?;
                debug!(
                    request_id,
                    handle,
                    peer = peer.peer,
                    service = peer.service,
                    "parsed yar peer"
                );

                let mut span = RequestContext::try_with_global_ctx(request_id, |ctx| {
                    Ok(ctx.create_exit_span(&format!("Yar_Client->{}", method), &peer.peer))
                })?;
                debug!(request_id, handle, "created yar exit span");

                let span_object = span.span_object_mut();
                span_object.set_span_layer(SpanLayer::RpcFramework);
                span_object.component_id = COMPONENT_PHP_ID;
                span_object.add_tag("rpc.system", "yar");
                span_object.add_tag("rpc.method", &method);
                span_object.add_tag("rpc.service", &peer.service);
                span_object.add_tag("url", &peer.uri);

                inject_sw_header(request_id, this, &peer.peer, &info.headers)?;
                debug!(request_id, handle, "injected yar sw8 header");

                Ok(Box::new(YarCallInfo {
                    handle,
                    headers: info.headers,
                    span,
                }))
            }),
            Box::new(|_, data, execute_data, return_value| {
                if data.downcast_ref::<()>().is_some() {
                    return Ok(());
                }

                let YarCallInfo {
                    handle,
                    headers,
                    mut span,
                } = *data.downcast::<YarCallInfo>().unwrap();

                debug!(handle, "finish yar client call");
                let this = get_this_mut(execute_data)?;
                restore_headers(this, headers.clone())?;
                debug!(handle, "restored yar headers");

                if let Some(mut info) = CLIENT_INFO_MAP.get_mut(&handle) {
                    info.headers = headers;
                }

                if return_value.as_bool() == Some(false) {
                    debug!(handle, "yar call returned false");
                    span.span_object_mut().is_error = true;
                }

                log_exception(&mut span);
                debug!(handle, "finished yar client span");

                Ok(())
            }),
        )
    }
}

fn get_yar_opt_header() -> crate::Result<i64> {
    Ok(call("constant", [ZVal::from(YAR_OPT_HEADER)])?.expect_long()?)
}

fn extract_headers_from_options(options: &phper::arrays::ZArr) -> Option<HashMap<String, String>> {
    let opt = get_yar_opt_header().ok()? as u64;
    let mut value = options.get(opt)?.clone();
    extract_headers(&mut value).ok()
}

fn extract_headers(value: &mut ZVal) -> crate::Result<HashMap<String, String>> {
    let Some(headers) = value.as_mut_z_arr() else {
        return Ok(HashMap::new());
    };

    let mut result = HashMap::with_capacity(headers.len());
    for (key, value) in headers.iter_mut() {
        match key {
            IterKey::Index(_) => {
                let Some(value) = value.as_z_str() else {
                    continue;
                };
                let value = value.to_str()?;
                let Some((key, value)) = value.split_once(':') else {
                    continue;
                };
                result.insert(key.trim().to_ascii_lowercase(), value.trim().to_string());
            }
            IterKey::ZStr(key) => {
                let key = key.to_str()?.to_ascii_lowercase();
                let value = if let Some(value) = value.as_z_str() {
                    value.to_str()?.to_string()
                } else if let Some(value) = value.as_long() {
                    value.to_string()
                } else if let Some(value) = value.as_bool() {
                    value.to_string()
                } else if let Some(value) = value.as_double() {
                    value.to_string()
                } else {
                    continue;
                };

                result.insert(key, value);
            }
        }
    }

    Ok(result)
}

fn inject_sw_header(
    request_id: Option<i64>, this: &mut ZObj, peer: &str, headers: &HashMap<String, String>,
) -> crate::Result<()> {
    let sw_header = RequestContext::try_get_sw_header(request_id, peer)?;
    let mut headers = headers.clone();
    headers.insert(SW_HEADER.to_string(), sw_header);
    debug!(
        ?request_id,
        peer,
        count = headers.len(),
        "apply yar headers with sw8"
    );
    apply_headers(this, &headers)
}

fn restore_headers(this: &mut ZObj, headers: HashMap<String, String>) -> crate::Result<()> {
    debug!(count = headers.len(), "restore yar headers");
    apply_headers(this, &headers)
}

fn apply_headers(this: &mut ZObj, headers: &HashMap<String, String>) -> crate::Result<()> {
    let mut header_arr = ZArray::new();
    for (key, value) in headers {
        let index = header_arr.len() as u64;
        header_arr.insert(index, format!("{key}: {value}"));
    }

    let yar_opt_header = get_yar_opt_header()?;
    debug!(
        handle = this.handle(),
        count = headers.len(),
        yar_opt_header,
        "update Yar_Client::_options directly"
    );

    let mut options = ZArray::new();
    if let Some(current_options) = this.get_property("_options").as_z_arr() {
        for (key, value) in current_options.iter() {
            match key {
                IterKey::Index(index) if index != yar_opt_header as u64 => {
                    options.insert(index, value.clone());
                }
                IterKey::ZStr(key) => {
                    options.insert(key.to_str()?, value.clone());
                }
                _ => {}
            }
        }
    }
    options.insert(yar_opt_header as u64, ZVal::from(header_arr));
    this.set_property("_options", options);

    Ok(())
}

fn parse_peer_info(uri: &str) -> crate::Result<YarPeerInfo> {
    let url = Url::parse(uri)?;

    let host = url.host_str().unwrap_or("unknown");
    let port = url.port_or_known_default().unwrap_or_default();
    let peer = format!("{}:{}", host, port);
    let service = url
        .path_segments()
        .and_then(|segments| segments.last())
        .filter(|segment| !segment.is_empty())
        .unwrap_or("/")
        .to_string();

    Ok(YarPeerInfo {
        uri: uri.to_string(),
        peer,
        service,
    })
}

#[cfg(test)]
mod tests {
    use super::parse_peer_info;

    #[test]
    fn parse_yar_peer_info() {
        let info = parse_peer_info("http://127.0.0.1:9012/yar.server.php").unwrap();
        assert_eq!(info.peer, "127.0.0.1:9012");
        assert_eq!(info.service, "yar.server.php");
    }

    #[test]
    fn parse_yar_peer_info_without_explicit_port() {
        let info = parse_peer_info("http://example.com/yar").unwrap();
        assert_eq!(info.peer, "example.com:80");
        assert_eq!(info.service, "yar");
    }
}
