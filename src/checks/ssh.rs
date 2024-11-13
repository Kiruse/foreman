use std::env;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use async_trait::async_trait;
use regex::Regex;

use crate::{Context, Error, LivenessResult, Result};
use crate::config::SshHealthCheck;

pub async fn check(ctx: &Context, cfg: &SshHealthCheck) -> Result<LivenessResult> {
  let t0 = Instant::now();
  let password = env::var("FOREMAN_SSH_PASSWORD").ok();

  let (user, addr) = if cfg.url.contains('@') {
    let mut split = cfg.url.split('@');
    let user = split.next().map(|s| s.to_string()).expect("Invalid SSH URL format");
    let addr = split.next().expect("Invalid SSH URL format");
    (user, addr)
  } else {
    let user = users::get_current_username()
      .map(|s| s.to_string_lossy().into_owned())
      .ok_or(Error::generic("No user found"))?;
    (user, cfg.url.as_str())
  };

  let (host, port) = if addr.contains(':') {
    let mut split = addr.split(':');
    let host = split.next().expect("Invalid SSH URL format");
    let port = split.next().expect("Invalid SSH URL format").parse().expect("Invalid port");
    (host, port)
  } else {
    (addr, 22)
  };

  let identity = cfg.identity
    .as_ref()
    .map(|p| p.clone())
    .or_else(|| {
      let home = dirs::home_dir().unwrap();
      let home = home.to_str().unwrap();
      let candidates: Vec<_> = vec![
        format!("~/.ssh/id_{}_ed25519", user),
        format!("~/.ssh/id_{}_rsa", user),
        "~/.ssh/id_ed25519".to_string(),
        "~/.ssh/id_rsa".to_string(),
      ];
      candidates.iter()
        .map(|p| Regex::new(r"^~").unwrap().replace(p, home))
        .find(|p| Path::new(p.as_ref()).exists())
        .map(|p| PathBuf::from(p.to_string()))
    })
    .ok_or(Error::ssh(format!("No SSH identity found for {}", cfg.url)))?;

  let keypair = russh::keys::load_secret_key(identity, password.as_deref())
    .map_err(|e| Error::ssh(format!("Failed to load identity: {}", e)))?;

  let client_conf = russh::client::Config {
    inactivity_timeout: Some(ctx.config.timeout.unwrap()),
    ..Default::default()
  };

  let client = Client { host: host.to_string(), port };

  let mut session = russh::client::connect(
    Arc::new(client_conf),
    (host, port),
    client,
  ).await?;

  if !session.authenticate_publickey(user, keypair.into()).await? {
    return Err(Error::ssh(format!("Failed to authenticate to {}", cfg.url)));
  }

  let elapsed = t0.elapsed();
  session.disconnect(russh::Disconnect::ByApplication, "", "English").await?;

  // TODO: check dynamic liveness
  Ok(LivenessResult::Stable(elapsed))
}

struct Client {
  host: String,
  port: u16,
}

#[async_trait]
impl russh::client::Handler for Client {
  type Error = Error;

  async fn check_server_key(
    &mut self,
    server_pubkey: &russh::keys::key::PublicKey,
  ) -> std::result::Result<bool, Self::Error> {
    match russh::keys::known_hosts::check_known_hosts(self.host.as_str(), self.port, server_pubkey) {
      Ok(true) => Ok(true),
      Ok(false) => {
        russh::keys::known_hosts::learn_known_hosts(self.host.as_str(), self.port, server_pubkey)?;
        Ok(true)
      }
      Err(e) => Err(e.into()),
    }
  }
}
