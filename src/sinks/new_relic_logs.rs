use codecs::{CharacterDelimitedEncoderConfig, JsonSerializerConfig};
use http::Uri;
use indexmap::IndexMap;
use snafu::Snafu;
use vector_config::configurable_component;

use crate::{
    codecs::{EncodingConfigWithFraming, Transformer},
    config::{
        AcknowledgementsConfig, GenerateConfig, Input, SinkConfig, SinkContext, SinkDescription,
    },
    sinks::{
        http::{HttpMethod, HttpSinkConfig},
        util::{
            http::RequestConfig, BatchConfig, Compression, SinkBatchSettings, TowerRequestConfig,
        },
    },
};

// New Relic Logs API accepts payloads up to 1MB (10^6 bytes)
const MAX_PAYLOAD_SIZE: usize = 1_000_000_usize;

#[derive(Debug, Snafu)]
enum BuildError {
    #[snafu(display(
        "Missing authentication key, must provide either 'license_key' or 'insert_key'"
    ))]
    MissingAuthParam,
}

/// New Relic region.
#[configurable_component]
#[derive(Clone, Copy, Debug, Derivative, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
#[derivative(Default)]
pub enum NewRelicLogsRegion {
    /// US region.
    #[derivative(Default)]
    Us,

    /// EU region.
    Eu,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct NewRelicLogsDefaultBatchSettings;

impl SinkBatchSettings for NewRelicLogsDefaultBatchSettings {
    const MAX_EVENTS: Option<usize> = None;
    const MAX_BYTES: Option<usize> = Some(1_000_000);
    const TIMEOUT_SECS: f64 = 1.0;
}

/// Configuration for the `new_relic_logs` sink.
#[configurable_component(sink)]
#[derive(Clone, Debug, Derivative)]
#[derivative(Default)]
pub struct NewRelicLogsConfig {
    /// A valid New Relic license key (if applicable).
    pub license_key: Option<String>,

    /// A valid New Relic insert key (if applicable).
    pub insert_key: Option<String>,

    #[configurable(derived)]
    pub region: Option<NewRelicLogsRegion>,

    #[configurable(derived)]
    #[serde(
        default,
        skip_serializing_if = "crate::serde::skip_serializing_if_default"
    )]
    pub encoding: Transformer,

    #[configurable(derived)]
    #[serde(default)]
    pub compression: Compression,

    #[configurable(derived)]
    #[serde(default)]
    pub batch: BatchConfig<NewRelicLogsDefaultBatchSettings>,

    #[configurable(derived)]
    #[serde(default)]
    pub request: TowerRequestConfig,

    #[configurable(derived)]
    #[serde(
        default,
        deserialize_with = "crate::serde::bool_or_struct",
        skip_serializing_if = "crate::serde::skip_serializing_if_default"
    )]
    acknowledgements: AcknowledgementsConfig,
}

inventory::submit! {
    SinkDescription::new::<NewRelicLogsConfig>("new_relic_logs")
}

impl GenerateConfig for NewRelicLogsConfig {
    fn generate_config() -> toml::Value {
        toml::Value::try_from(Self::default()).expect("config must serialize to valid TOML")
    }
}

#[async_trait::async_trait]
#[typetag::serde(name = "new_relic_logs")]
impl SinkConfig for NewRelicLogsConfig {
    async fn build(
        &self,
        cx: SinkContext,
    ) -> crate::Result<(super::VectorSink, super::Healthcheck)> {
        let http_conf = self.create_config()?;
        http_conf.build(cx).await
    }

    fn input(&self) -> Input {
        Input::log()
    }

    fn sink_type(&self) -> &'static str {
        "new_relic_logs"
    }

    fn acknowledgements(&self) -> &AcknowledgementsConfig {
        &self.acknowledgements
    }
}

impl NewRelicLogsConfig {
    fn create_config(&self) -> crate::Result<HttpSinkConfig> {
        let mut headers: IndexMap<String, String> = IndexMap::new();
        if let Some(license_key) = &self.license_key {
            headers.insert("X-License-Key".to_owned(), license_key.clone());
        } else if let Some(insert_key) = &self.insert_key {
            headers.insert("X-Insert-Key".to_owned(), insert_key.clone());
        } else {
            return Err(Box::new(BuildError::MissingAuthParam));
        }

        let uri = match self.region.as_ref().unwrap_or(&NewRelicLogsRegion::Us) {
            NewRelicLogsRegion::Us => Uri::from_static("https://log-api.newrelic.com/log/v1"),
            NewRelicLogsRegion::Eu => Uri::from_static("https://log-api.eu.newrelic.com/log/v1"),
        };

        let batch_settings = self.batch.validate()?.limit_max_bytes(MAX_PAYLOAD_SIZE)?;

        let tower = TowerRequestConfig { ..self.request };

        let request = RequestConfig { tower, headers };

        Ok(HttpSinkConfig {
            uri: uri.into(),
            method: Some(HttpMethod::Post),
            auth: None,
            headers: None,
            compression: self.compression,
            encoding: EncodingConfigWithFraming::new(
                Some(CharacterDelimitedEncoderConfig::new(b',').into()),
                JsonSerializerConfig::new().into(),
                self.encoding.clone(),
            ),
            batch: batch_settings.into(),
            request,
            tls: None,
            acknowledgements: self.acknowledgements,
        })
    }
}

#[cfg(test)]
mod tests {
    use std::io::BufRead;

    use bytes::Buf;
    use codecs::encoding::Serializer;
    use futures::{stream, StreamExt};
    use hyper::Method;
    use serde_json::Value;

    use super::*;
    use crate::{
        codecs::SinkType,
        config::SinkConfig,
        event::{Event, LogEvent},
        sinks::util::{service::RATE_LIMIT_NUM_DEFAULT, test::build_test_server, Concurrency},
        test_util::{
            components::{run_and_assert_sink_compliance, HTTP_SINK_TAGS},
            next_addr,
        },
    };

    #[test]
    fn generate_config() {
        crate::test_util::test_generate_config::<NewRelicLogsConfig>();
    }

    #[test]
    fn new_relic_logs_check_config_no_auth() {
        assert_eq!(
            format!(
                "{}",
                NewRelicLogsConfig::default().create_config().unwrap_err()
            ),
            "Missing authentication key, must provide either 'license_key' or 'insert_key'"
                .to_owned(),
        );
    }

    #[test]
    fn new_relic_logs_check_config_defaults() {
        let nr_config = NewRelicLogsConfig {
            license_key: Some("foo".to_owned()),
            ..Default::default()
        };
        let http_config = nr_config.create_config().unwrap();

        assert_eq!(
            http_config.uri.uri.to_string(),
            "https://log-api.newrelic.com/log/v1".to_string()
        );
        assert_eq!(http_config.method, Some(HttpMethod::Post));
        assert!(matches!(
            http_config
                .encoding
                .build(SinkType::MessageBased)
                .unwrap()
                .1,
            Serializer::Json(_)
        ));
        assert_eq!(http_config.batch.max_bytes, Some(MAX_PAYLOAD_SIZE));
        assert_eq!(http_config.request.tower.concurrency, Concurrency::None);
        assert_eq!(
            http_config.request.tower.rate_limit_num,
            Some(RATE_LIMIT_NUM_DEFAULT)
        );
        assert_eq!(
            http_config.request.headers["X-License-Key"],
            "foo".to_owned()
        );
        assert!(http_config.tls.is_none());
        assert!(http_config.auth.is_none());
    }

    #[test]
    fn new_relic_logs_check_config_custom() {
        let mut batch = BatchConfig::default();
        batch.max_bytes = Some(MAX_PAYLOAD_SIZE);

        let nr_config = NewRelicLogsConfig {
            insert_key: Some("foo".to_owned()),
            region: Some(NewRelicLogsRegion::Eu),
            batch,
            request: TowerRequestConfig {
                concurrency: Concurrency::Fixed(12),
                rate_limit_num: Some(24),
                ..Default::default()
            },
            ..Default::default()
        };

        let http_config = nr_config.create_config().unwrap();

        assert_eq!(
            http_config.uri.uri.to_string(),
            "https://log-api.eu.newrelic.com/log/v1".to_string()
        );
        assert_eq!(http_config.method, Some(HttpMethod::Post));
        assert!(matches!(
            http_config
                .encoding
                .build(SinkType::MessageBased)
                .unwrap()
                .1,
            Serializer::Json(_)
        ));
        assert_eq!(http_config.batch.max_bytes, Some(MAX_PAYLOAD_SIZE));
        assert_eq!(
            http_config.request.tower.concurrency,
            Concurrency::Fixed(12)
        );
        assert_eq!(http_config.request.tower.rate_limit_num, Some(24));
        assert_eq!(
            http_config.request.headers["X-Insert-Key"],
            "foo".to_owned()
        );
        assert!(http_config.tls.is_none());
        assert!(http_config.auth.is_none());
    }

    #[test]
    fn new_relic_logs_check_config_custom_from_toml() {
        let config = r#"
        insert_key = "foo"
        region = "eu"

        [batch]
        max_bytes = 838860

        [request]
        concurrency = 12
        rate_limit_num = 24
    "#;
        let nr_config: NewRelicLogsConfig = toml::from_str(config).unwrap();

        let http_config = nr_config.create_config().unwrap();

        assert_eq!(
            http_config.uri.uri.to_string(),
            "https://log-api.eu.newrelic.com/log/v1".to_string()
        );
        assert_eq!(http_config.method, Some(HttpMethod::Post));
        assert!(matches!(
            http_config
                .encoding
                .build(SinkType::MessageBased)
                .unwrap()
                .1,
            Serializer::Json(_)
        ));
        assert_eq!(http_config.batch.max_bytes, Some(838860));
        assert_eq!(
            http_config.request.tower.concurrency,
            Concurrency::Fixed(12)
        );
        assert_eq!(http_config.request.tower.rate_limit_num, Some(24));
        assert_eq!(
            http_config.request.headers["X-Insert-Key"],
            "foo".to_owned()
        );
        assert!(http_config.tls.is_none());
        assert!(http_config.auth.is_none());
    }

    #[test]
    #[should_panic]
    fn new_relic_logs_check_config_custom_from_toml_batch_max_size_too_high() {
        let config = r#"
        insert_key = "foo"
        region = "eu"

        [batch]
        max_bytes = 8388600

        [request]
        concurrency = 12
        rate_limit_num = 24
    "#;
        let nr_config: NewRelicLogsConfig = toml::from_str(config).unwrap();

        nr_config.create_config().unwrap();
    }

    #[tokio::test]
    async fn new_relic_logs_happy_path() {
        let in_addr = next_addr();

        let nr_config = NewRelicLogsConfig {
            license_key: Some("foo".to_owned()),
            ..Default::default()
        };
        let mut http_config = nr_config.create_config().unwrap();
        http_config.uri = format!("http://{}/fake_nr", in_addr)
            .parse::<http::Uri>()
            .unwrap()
            .into();

        let (sink, _healthcheck) = http_config.build(SinkContext::new_test()).await.unwrap();
        let (rx, trigger, server) = build_test_server(in_addr);
        tokio::spawn(server);

        let input_lines = (0..100).map(|i| format!("msg {}", i)).collect::<Vec<_>>();
        let events = stream::iter(input_lines.clone()).map(|e| Event::Log(LogEvent::from(e)));

        run_and_assert_sink_compliance(sink, events, &HTTP_SINK_TAGS).await;
        drop(trigger);

        let output_lines = rx
            .flat_map(|(parts, body)| {
                assert_eq!(Method::POST, parts.method);
                assert_eq!("/fake_nr", parts.uri.path());
                assert_eq!(
                    parts
                        .headers
                        .get("X-License-Key")
                        .and_then(|v| v.to_str().ok()),
                    Some("foo")
                );
                stream::iter(body.reader().lines())
            })
            .map(Result::unwrap)
            .flat_map(|line| {
                let vals: Vec<Value> = serde_json::from_str(&line).unwrap();
                stream::iter(
                    vals.into_iter()
                        .map(|v| v.get("message").unwrap().as_str().unwrap().to_owned()),
                )
            })
            .collect::<Vec<_>>()
            .await;

        assert_eq!(input_lines, output_lines);
    }
}
