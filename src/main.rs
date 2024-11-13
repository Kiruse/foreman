use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

use clap::Parser;
use log::*;
use simple_logger::SimpleLogger;
use surge_ping::{PingIdentifier, PingSequence};
use thiserror::Error;

mod checks;
mod config;

use config::{Config, HealthCheck, HttpHealthCheck, SshHealthCheck};
use tokio::sync::RwLock;

#[derive(Debug, Error)]
pub enum Error {
  #[error("{0}")]
  Generic(String),

  #[error("I/O error: {0}")]
  Io(#[from] std::io::Error),

  #[error("YAML error: {0}")]
  Yaml(#[from] serde_yaml::Error),

  #[error("Russh error: {0}")]
  Russh(#[from] russh::Error),

  #[error("Russh keys error: {0}")]
  RusshKeys(#[from] russh::keys::Error),

  #[error("SSH error: {0}")]
  Ssh(String),

  #[error("Liveness failure on {0}")]
  LivenessFailure(String),
}

impl Error {
  pub fn generic(msg: impl Into<String>) -> Self {
    Self::Generic(msg.into())
  }

  pub fn ssh(msg: impl Into<String>) -> Self {
    Self::Ssh(msg.into())
  }
}

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum LivenessResult {
  /// The liveness status is unknown, because it hasn't been tried yet.
  Unknown,
  /// The host is known to be unreachable at the moment of the last check.
  Unreachable,
  /// The host is known to be reachable, but had some issues during the last check.
  /// Contains average duration of successful sub-checks & number of failures.
  Unstable(Duration, u32),
  /// The host is known to be reachable and had no issues during the last check.
  /// Contains average duration of successful sub-checks.
  Stable(Duration),
}

impl LivenessResult {
  pub fn is_stable(&self) -> bool {
    matches!(self, LivenessResult::Stable(_))
  }
}

struct Context {
  config: Config,
  next_id: RwLock<u64>,
  states: HashMap<String, RwLock<LivenessResult>>,
}

impl Context {
  pub async fn next_pinger_id(&self) -> PingIdentifier {
    let mut atom = self.next_id.write().await;
    let id = (*atom & 0xFFFF) as u16; // Truncate to u16
    *atom += 1;
    PingIdentifier(id)
  }
}

#[tokio::main]
async fn main() -> Result<()> {
  let cli = Cli::parse();
  let user_config_path = dirs::config_dir().unwrap().join("foreman.yaml");
  let local_config_path = cli.config.unwrap_or(PathBuf::from("foreman.yaml"));

  let config = Config::default()
    .concat(Config::load(user_config_path).await?)
    .concat(Config::load(local_config_path).await?);
  SimpleLogger::new().with_level(config.log_level.unwrap()).init().unwrap();

  let ctx = {
    let health_checks = config.health_checks.clone();
    Context {
      config,
      next_id: RwLock::new(0),
      states: init_states(&health_checks),
    }
  };

  let pings: Vec<_> = ctx.config.health_checks
    .iter()
    .filter_map(|hc| match hc {
      HealthCheck::Ping(host) => Some(host.clone()),
      _ => None,
    })
    .collect();
  let http_checks: Vec<_> = ctx.config.health_checks
    .iter()
    .filter_map(|hc| match hc {
      HealthCheck::Http(hc) => Some(hc.clone()),
      _ => None,
    })
    .collect();
  let ssh_checks: Vec<_> = ctx.config.health_checks
    .iter()
    .filter_map(|hc| match hc {
      HealthCheck::Ssh(hc) => Some(hc.clone()),
      _ => None,
    })
    .collect();

  loop {
    if let Err(e) = tokio::try_join!(
      check_pings(&ctx, &pings),
      check_rest(&ctx, &http_checks),
      check_ssh(&ctx, &ssh_checks),
    ) {
      error!("Error running health checks: {}", e);
    }

    // Sleep for 30 minutes
    tokio::time::sleep(ctx.config.interval.unwrap()).await;
  }
}

#[derive(Debug, Parser)]
#[command(version, about, propagate_version = true)]
struct Cli {
  #[arg(short, long)]
  config: Option<PathBuf>,
}

fn init_states(health_checks: &[HealthCheck]) -> HashMap<String, RwLock<LivenessResult>> {
  health_checks.iter()
    .map(|hc| {
      let host = match hc {
        HealthCheck::Ping(host) => host.clone(),
        HealthCheck::Http(hc) => hc.url.clone(),
        HealthCheck::Ssh(hc) => hc.url.clone(),
      };
      (host, RwLock::new(LivenessResult::Unknown))
    })
    .collect()
}

async fn check_pings(ctx: &Context, hosts: &[String]) -> Result<()> {
  futures::future::try_join_all(
    hosts.iter().map(|host| async move {
      match ping(&ctx, host.clone()).await {
        Ok(result) => update_liveness(ctx, host, result).await,
        Err(e) => {
          error!("Error pinging host {}: {}", host, e);
          notify(
            log::Level::Error,
            "Foreman Error",
            format!("Error pinging host {}: {}", host, e),
            None,
          );
        }
      }
      // always return Ok so the future doesn't fail
      Ok::<(), Error>(())
    })
  ).await?;

  Ok(())
}

async fn check_rest(_ctx: &Context, _checks: &[HttpHealthCheck]) -> Result<()> {
  // TODO: Implement
  Ok(())
}

async fn check_ssh(ctx: &Context, checks: &[SshHealthCheck]) -> Result<()> {
  futures::future::try_join_all(
    checks.iter().map(|hc| async move {
      match crate::checks::ssh::check(ctx, hc).await {
        Ok(result) => update_liveness(ctx, &hc.url, result).await,
        Err(e) => {
          error!("Error SSHing to {}: {}", hc.url, e);
          notify(
            log::Level::Error,
            "Foreman Error",
            format!("Error SSHing to {}: {}", hc.url, e),
            None,
          );
        }
      }
      Ok::<(), Error>(())
    })
  ).await?;

  Ok(())
}

async fn ping(ctx: &Context, host: String) -> Result<LivenessResult> {
  let ips = dns_lookup::lookup_host(host.as_str())?;
  let ip = ips.iter()
    .next()
    .ok_or(Error::generic(format!("No IP addresses found for host: {}", host)))?;

  let client_config = if ip.is_ipv4() {
    surge_ping::Config::default()
  } else {
    surge_ping::Config::builder().kind(surge_ping::ICMP::V6).build()
  };

  let client = surge_ping::Client::new(&client_config)?;

  let pinger_id = ctx.next_pinger_id().await;
  let payload = vec![0; 64];
  let mut pinger = client.pinger(ip.clone(), pinger_id).await;
  pinger.timeout(ctx.config.timeout.unwrap());

  let mut total_duration = Duration::ZERO;
  let mut failures = 0u32;
  let iterations = 5u32;

  for idx in 0..iterations {
    match pinger.ping(PingSequence(idx as u16), &payload).await {
      Ok((_, duration)) => {
        total_duration += duration;
      }
      Err(_) => {
        failures += 1;
      }
    }
  }

  if failures == 0 {
    Ok(LivenessResult::Stable(total_duration / iterations))
  } else if failures == iterations {
    Ok(LivenessResult::Unreachable)
  } else {
    Ok(LivenessResult::Unstable(total_duration / (iterations - failures), failures))
  }
}

async fn update_liveness(ctx: &Context, host: &String, result: LivenessResult) {
  let mut state = ctx.states.get(host).unwrap().write().await;
  let prev = (*state).clone();
  *state = result.clone();

  if prev != result {
    match result {
      LivenessResult::Stable(duration) =>
        info!("{} is up ({}ms)", host, duration.as_millis()),
      LivenessResult::Unstable(duration, failures) =>
        info!("{} is up ({}ms, {} failures)", host, duration.as_millis(), failures),
      LivenessResult::Unreachable => {
        error!("{} is down!", host);
        notify(
          log::Level::Error,
          "Host Unreachable",
          format!("{} is down!", host),
          None,
        );
      }
      LivenessResult::Unknown =>
        error!("{} is unknown! This shouldn't happen!!", host),
    }
  } else if !prev.is_stable() {
    notify(
      log::Level::Info,
      "Host Unstable or Unreachable",
      format!("{} is still unstable or unreachable", host),
      Some(10),
    );
  }
}

fn notify(
  level: log::Level,
  summary: impl AsRef<str>,
  msg: impl AsRef<str>,
  timeout: Option<i32>,
) {
  match notify_rust::Notification::new()
    .summary(summary.as_ref())
    .body(format!("{}: {}", level, msg.as_ref()).as_str())
    .timeout(timeout.unwrap_or(0))
    .show()
  {
    Ok(_) => {}
    Err(e) => {
      error!("Failed to send notification: {}", e);
    }
  }
}
