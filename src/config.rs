/*
 * Copyright 2020 Google LLC
 *
 * Licensed under the Apache License, Version 2.0 (the "License");
 * you may not use this file except in compliance with the License.
 * You may obtain a copy of the License at
 *
 *     http://www.apache.org/licenses/LICENSE-2.0
 *
 * Unless required by applicable law or agreed to in writing, software
 * distributed under the License is distributed on an "AS IS" BASIS,
 * WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
 * See the License for the specific language governing permissions and
 * limitations under the License.
 */

//! Quilkin configuration.

use std::{
    net::SocketAddr,
    path::{Path, PathBuf},
};

use base64_serde::base64_serde_type;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

mod builder;
mod config_type;
mod error;

use crate::endpoint::Endpoint;
use error::TestsuiteDecodeError;

pub(crate) use self::error::ValueInvalidArgs;

pub use self::{builder::Builder, config_type::ConfigType, error::ValidationError};

base64_serde_type!(Base64Standard, base64::STANDARD);

// For some log messages on the hot path (potentially per-packet), we log 1 out
// of every `LOG_SAMPLING_RATE` occurrences to avoid spamming the logs.
pub(crate) const LOG_SAMPLING_RATE: u64 = 1000;

fn find_config_file<P: AsRef<Path>>(
    log: &slog::Logger,
    path: Option<P>,
) -> Result<String, std::io::Error> {
    const ENV_CONFIG_PATH: &str = "QUILKIN_CONFIG";
    const CONFIG_FILE: &str = "quilkin.yaml";

    let config_env = std::env::var(ENV_CONFIG_PATH).ok();

    let config_path = path
        .as_ref()
        .map(AsRef::as_ref)
        .or_else(|| config_env.as_deref().map(AsRef::as_ref))
        .unwrap_or_else(|| CONFIG_FILE.as_ref())
        .canonicalize()?;

    slog::info!(log, "Found configuration file"; "path" => config_path.display());

    std::fs::read_to_string(&config_path)
        .or_else(|error| {
            if cfg!(unix) {
                std::fs::read_to_string("/etc/quilkin/quilkin.yaml")
            } else {
                Err(error)
            }
        })
        .map_err(From::from)
}

/// Config is the configuration of a proxy
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct Config {
    pub version: Version,

    #[serde(default)]
    pub proxy: Proxy,

    #[serde(default)]
    pub admin: Admin,

    #[serde(flatten)]
    pub source: Source,
}

impl Config {
    /// Attempts to locate and parse a `Config` located at either `path`, the
    /// `$QUILKIN_CONFIG` environment variable if set, the current directory,
    /// or the `/etc/quilkin` directory (on unix platforms only). Returns an
    /// error if the found configuration is invalid, or if no configuration
    /// could be found at any location.
    pub fn find(
        log: &slog::Logger,
        path: Option<&str>,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        find_config_file(log, path)
            .map_err(From::from)
            .and_then(|s| serde_yaml::from_str(&s).map_err(From::from))
    }

    /// Attempts to deserialize `input` as a YAML object representing `Self`.
    pub fn from_reader<R: std::io::Read>(input: R) -> Result<Self, serde_yaml::Error> {
        serde_yaml::from_reader(input)
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub enum Version {
    #[serde(rename = "v1alpha1")]
    V1Alpha1,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Proxy {
    #[serde(default = "default_proxy_id")]
    pub id: String,
    #[serde(default = "default_proxy_port")]
    pub port: u16,
}

fn default_proxy_id() -> String {
    Uuid::new_v4().to_hyphenated().to_string()
}

fn default_proxy_port() -> u16 {
    7000
}

impl Default for Proxy {
    fn default() -> Self {
        Proxy {
            id: default_proxy_id(),
            port: default_proxy_port(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Admin {
    pub address: SocketAddr,
}

impl Default for Admin {
    fn default() -> Self {
        Admin {
            address: "[::]:9091".parse().unwrap(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ManagementServer {
    pub address: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub enum Source {
    #[serde(rename = "static")]
    Static {
        #[serde(default)]
        filters: Vec<Filter>,

        endpoints: Vec<Endpoint>,
    },
    #[serde(rename = "dynamic")]
    Dynamic {
        management_servers: Vec<ManagementServer>,
    },
}

impl Source {
    /// Returns a slice list of endpoints if the configuration
    /// is [`Self::Static`].
    pub fn get_static_endpoints(&self) -> Option<&[Endpoint]> {
        match self {
            Source::Static { endpoints, .. } => Some(endpoints),
            _ => None,
        }
    }

    /// Returns a mutable reference to the list of endpoints if the
    /// configuration is [`Self::Static`].
    pub fn get_static_endpoints_mut(&mut self) -> Option<&mut Vec<Endpoint>> {
        match self {
            Source::Static { endpoints, .. } => Some(endpoints),
            _ => None,
        }
    }

    /// Returns the list of filters if the config is a static config and None otherwise.
    /// This is a convenience function and should only be used for doc tests and tests.
    pub fn get_static_filters(&self) -> Option<&[Filter]> {
        match self {
            Source::Static { filters, .. } => Some(filters),
            _ => None,
        }
    }
}

/// Filter is the configuration for a single filter
#[derive(Debug, Deserialize, Serialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct Filter {
    pub name: String,
    pub config: Option<serde_yaml::Value>,
}

/// The configuration of a Quilkin testsuite.
pub struct Testsuite {
    pub config: Config,
    pub options: TestConfig,
}

#[derive(Deserialize, Serialize)]
pub struct TestConfig {
    pub config: Option<PathBuf>,
    pub tests: std::collections::HashMap<String, TestOptions>,
}

impl Testsuite {
    pub fn find<P: AsRef<Path>>(
        log: &slog::Logger,
        path: Option<P>,
    ) -> Result<Self, TestsuiteDecodeError> {
        find_config_file(log, path)
            .map_err(From::from)
            .and_then(|s| Self::from_yaml(&s))
    }

    /// Attempts to deserialize [`Self`] from a YAML document. A valid source is
    /// either a combination of [`Config`] document followed by [`TestConfig`]
    /// document separated by a `---` (YAML document separator), or a
    /// `TestConfig` document containing a `config` key that points to a valid
    /// `Config` file.
    pub fn from_yaml(src: &str) -> Result<Self, TestsuiteDecodeError> {
        Ok(
            if let Ok(options) = serde_yaml::from_str::<TestConfig>(src) {
                let path = options
                    .config
                    .as_deref()
                    .ok_or(TestsuiteDecodeError::MissingConfigInTestOptions)?;
                let config = serde_yaml::from_reader(std::fs::File::open(path)?)?;
                Self { config, options }
            } else {
                let mut de = serde_yaml::Deserializer::from_str(src);
                let config =
                    Config::deserialize(de.next().ok_or(TestsuiteDecodeError::MissingConfig)?)?;
                let options = TestConfig::deserialize(
                    de.next().ok_or(TestsuiteDecodeError::MissingTestOptions)?,
                )?;
                Self { config, options }
            },
        )
    }
}

#[derive(Deserialize, Serialize)]
pub struct TestOptions {
    /// The data to be given to Quilkin.
    pub input: String,
    /// What we expect Quilkin to send to the game server.
    pub output: String,
}

#[cfg(test)]
mod tests {
    use serde_yaml::Value;

    use super::*;

    use crate::endpoint::Metadata;

    fn parse_config(yaml: &str) -> Config {
        Config::from_reader(yaml.as_bytes()).unwrap()
    }

    fn assert_static_endpoints(source: &Source, expected_endpoints: Vec<Endpoint>) {
        match source {
            Source::Static {
                filters: _,
                endpoints,
            } => {
                assert_eq!(&expected_endpoints, endpoints,);
            }
            _ => unreachable!("expected static config source"),
        }
    }

    fn assert_management_servers(source: &Source, expected: Vec<ManagementServer>) {
        match source {
            Source::Dynamic { management_servers } => {
                assert_eq!(&expected, management_servers,);
            }
            _ => unreachable!("expected dynamic config source"),
        }
    }

    #[test]
    fn deserialise_client() {
        let config = Builder::empty()
            .with_port(7000)
            .with_static(
                vec![],
                vec![Endpoint::new("127.0.0.1:25999".parse().unwrap())],
            )
            .build();
        let _ = serde_yaml::to_string(&config).unwrap();
    }

    #[test]
    fn deserialise_server() {
        let config = Builder::empty()
            .with_port(7000)
            .with_static(
                vec![],
                vec![
                    Endpoint::new("127.0.0.1:26000".parse().unwrap()),
                    Endpoint::new("127.0.0.1:26001".parse().unwrap()),
                ],
            )
            .build();
        let _ = serde_yaml::to_string(&config).unwrap();
    }

    #[test]
    fn parse_default_values() {
        let yaml = "
version: v1alpha1
static:
  endpoints:
    - address: 127.0.0.1:25999
  ";
        let config = parse_config(yaml);

        assert_eq!(config.proxy.port, 7000);
        assert_eq!(config.proxy.id.len(), 36);
    }

    #[test]
    fn parse_filter_config() {
        let yaml = "
version: v1alpha1
proxy:
  id: client-proxy
  port: 7000 # the port to receive traffic to locally
static:
  filters: # new filters section
    - name: quilkin.core.v1.rate-limiter
      config:
        map: of arbitrary key value pairs
        could:
          - also
          - be
          - 27
          - true
  endpoints:
    - address: 127.0.0.1:7001
        ";
        let config = parse_config(yaml);

        let filter = config.source.get_static_filters().unwrap().get(0).unwrap();
        assert_eq!("quilkin.core.v1.rate-limiter", filter.name);
        let config = filter.config.as_ref().unwrap();
        let filter_config = config.as_mapping().unwrap();

        let key = Value::from("map");
        assert_eq!(
            "of arbitrary key value pairs",
            filter_config.get(&key).unwrap().as_str().unwrap()
        );

        let key = Value::from("could");
        let could = filter_config.get(&key).unwrap().as_sequence().unwrap();
        assert_eq!("also", could.get(0).unwrap().as_str().unwrap());
        assert_eq!("be", could.get(1).unwrap().as_str().unwrap());
        assert_eq!(27, could.get(2).unwrap().as_i64().unwrap());
        assert!(could.get(3).unwrap().as_bool().unwrap());
    }

    #[test]
    fn parse_proxy() {
        let yaml = "
version: v1alpha1
proxy:
  id: server-proxy
  port: 7000
static:
  endpoints:
    - address: 127.0.0.1:25999
  ";
        let config = parse_config(yaml);

        assert_eq!(config.proxy.port, 7000);
        assert_eq!(config.proxy.id.as_str(), "server-proxy");
    }

    #[test]
    fn parse_client() {
        let yaml = "
version: v1alpha1
static:
  endpoints:
    - address: 127.0.0.1:25999
  ";
        let config = parse_config(yaml);

        assert_static_endpoints(
            &config.source,
            vec![Endpoint::new("127.0.0.1:25999".parse().unwrap())],
        );
    }

    #[test]
    fn parse_server() {
        let yaml = "
---
version: v1alpha1
static:
  endpoints:
    - address: 127.0.0.1:26000
      metadata:
        quilkin.dev:
          tokens:
            - MXg3aWp5Ng== #1x7ijy6
            - OGdqM3YyaQ== #8gj3v2i
    - address: 127.0.0.1:26001
      metadata:
        quilkin.dev:
          tokens:
            - bmt1eTcweA== #nkuy70x";
        let config = parse_config(yaml);
        assert_static_endpoints(
            &config.source,
            vec![
                Endpoint::with_metadata(
                    "127.0.0.1:26000".parse().unwrap(),
                    Metadata {
                        tokens: vec!["1x7ijy6", "8gj3v2i"]
                            .into_iter()
                            .map(From::from)
                            .collect(),
                    },
                ),
                Endpoint::with_metadata(
                    "127.0.0.1:26001".parse().unwrap(),
                    Metadata {
                        tokens: vec!["nkuy70x"].into_iter().map(From::from).collect(),
                    },
                ),
            ],
        );
    }

    #[test]
    fn parse_dynamic_source() {
        let yaml = "
version: v1alpha1
dynamic:
  filters:
    - name: quilkin.core.v1.rate-limiter
      config:
        map: of arbitrary key value pairs
        could:
          - also
          - be
          - 27
          - true
  management_servers:
    - address: 127.0.0.1:25999
    - address: 127.0.0.1:30000
  ";
        let config = parse_config(yaml);

        assert_management_servers(
            &config.source,
            vec![
                ManagementServer {
                    address: "127.0.0.1:25999".into(),
                },
                ManagementServer {
                    address: "127.0.0.1:30000".into(),
                },
            ],
        );
    }

    #[test]
    fn deny_unused_fields() {
        let configs = vec![
            "
version: v1alpha1
foo: bar
static:
  endpoints:
    - address: 127.0.0.1:7001
",
            "
# proxy
version: v1alpha1
proxy:
  foo: bar
  id: client-proxy
  port: 7000
static:
  endpoints:
    - address: 127.0.0.1:7001
",
            "
# admin
version: v1alpha1
admin:
    foo: bar
    address: 127.0.0.1:7001
",
            "
# static.endpoints
version: v1alpha1
static:
  endpoints:
    - address: 127.0.0.1:7001
      connection_ids:
      - Mxg3aWp5Ng==
",
            "
# static.filters
version: v1alpha1
static:
  filters:
    - name: quilkin.core.v1.rate-limiter
      foo: bar
",
            "
# dynamic.management_servers
version: v1alpha1
dynamic:
  management_servers:
    - address: 127.0.0.1:25999
      foo: bar
",
        ];

        for config in configs {
            let result = Config::from_reader(config.as_bytes());
            let error = result.unwrap_err();
            assert!(format!("{:?}", error).contains("unknown field"));
        }
    }
}
