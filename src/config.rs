use std::path::PathBuf;
use std::time::Duration;

use serde::{Deserialize, Deserializer, Serialize};
use tokio::{fs::File, io::{AsyncReadExt, BufReader}};

use crate::Result;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct Config {
  /// Health checks to run.
  pub health_checks: Vec<HealthCheck>,
  /// Log level. Defaults to `warn`.
  #[serde(default, serialize_with = "serialize_log_level", deserialize_with = "deserialize_log_level")]
  pub log_level: Option<log::LevelFilter>,
  /// Interval between health checks. Defaults to 30 minutes.
  #[serde(default, with = "humantime_serde")]
  pub interval: Option<Duration>,
  /// Timeout for each ping. Defaults to 2 seconds.
  #[serde(default, with = "humantime_serde")]
  pub timeout: Option<Duration>,
  /// Maximum age of the server's current timestamp in seconds. Defaults to 10 seconds.
  #[serde(default, with = "humantime_serde")]
  pub timestamp_recency: Option<Duration>,
}

impl Config {
  pub fn new() -> Self {
    Self {
      health_checks: vec![],
      log_level: None,
      interval: None,
      timeout: None,
      timestamp_recency: None,
    }
  }

  pub async fn load(path: PathBuf) -> Result<Self> {
    if !path.exists() {
      return Ok(Self::new());
    }

    let file = File::open(path).await?;
    let mut contents = Vec::new();
    BufReader::new(file).read_to_end(&mut contents).await?;
    let config = serde_yaml::from_slice(&contents)?;
    Ok(config)
  }

  /// Concatenates two configs. The `other` config will take precedence over `self`.
  pub fn concat(&self, other: Self) -> Self {
    Self {
      health_checks: [self.health_checks.clone(), other.health_checks].concat(),
      log_level: other.log_level.or(self.log_level),
      interval: other.interval.or(self.interval),
      timeout: other.timeout.or(self.timeout),
      timestamp_recency: other.timestamp_recency.or(self.timestamp_recency),
    }
  }
}

impl Default for Config {
  fn default() -> Self {
    Self {
      health_checks: vec![],
      log_level: Some(log::LevelFilter::Warn),
      interval: Some(Duration::from_secs(1800)),
      timeout: Some(Duration::from_secs(2)),
      timestamp_recency: Some(Duration::from_secs(10)),
    }
  }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum HealthCheck {
  /// An ICMP ping to the given host to test liveness of a host machine.
  Ping(String),

  /// A simple HTTP GET request to the given URL to test liveness of a web service.
  Http(HttpHealthCheck),

  /// A simple SSH connection. Uses the `ssh` command under the hood, and thus requires it.
  Ssh(SshHealthCheck),
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct HttpHealthCheck {
  /// URL to check.
  pub url: String,
  /// Expected response body. Defaults to `Timestamp`.
  pub expect: Option<ResponseExpectation>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ResponseExpectation {
  /// Response must be the server's current timestamp in seconds.
  /// This timestamp must be
  Timestamp,

  /// Response must match the given string exactly. This is less reliable than `Timestamp`, but
  /// useful when you don't have control over the server's response or it isn't dynamic.
  Body(String),
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct SshHealthCheck {
  /// URL to connect to, including user & port, in the format of `{user}@{host}:{port}`.
  /// `user` defaults to the current user, and `port` defaults to 22.
  pub url: String,
  /// Private key to use for the SSH connection. Defaults to `~/.ssh/id_{user}_ed25519` or
  /// `~/.ssh/id_{user}_rsa`, whichever exists, in this order.
  pub identity: Option<PathBuf>,
}

fn serialize_log_level<S>(level: &Option<log::LevelFilter>, serializer: S) -> std::result::Result<S::Ok, S::Error>
where
  S: serde::Serializer,
{
  serializer.serialize_str(&level.map(|l| l.to_string().to_lowercase()).unwrap_or("warn".to_string()))
}

fn deserialize_log_level<'de, D>(deserializer: D) -> std::result::Result<Option<log::LevelFilter>, D::Error>
where
  D: Deserializer<'de>,
{
  let s = String::deserialize(deserializer)?.to_lowercase();
  match s.as_str() {
    "off" => Ok(None),
    "error" => Ok(Some(log::LevelFilter::Error)),
    "warn" => Ok(Some(log::LevelFilter::Warn)),
    "info" => Ok(Some(log::LevelFilter::Info)),
    "debug" => Ok(Some(log::LevelFilter::Debug)),
    "trace" => Ok(Some(log::LevelFilter::Trace)),
    _ => Err(serde::de::Error::unknown_variant(&s, &["off", "error", "warn", "info", "debug", "trace"])),
  }
}
