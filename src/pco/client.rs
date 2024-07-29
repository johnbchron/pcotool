use std::{collections::HashMap, sync::Arc, time::Duration};

use chrono::SecondsFormat;
use color_eyre::{
  eyre::{OptionExt, Result, WrapErr},
  Section, SectionExt,
};
use futures::Stream;
use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, Mutex, Semaphore};
use tokio_stream::wrappers::ReceiverStream;
use tracing::instrument;

use super::PCO_CONCURRENCY;
use crate::{secrets::Secrets, DateTimeUtc};

const URL_PREFIX: &str = "https://api.planningcenteronline.com/calendar/v2";

pub struct PcoClient {
  client:           reqwest::Client,
  secrets:          Secrets,
  semaphore:        Arc<Semaphore>,
  cached_resources: Arc<Mutex<HashMap<String, PcoObject>>>,
}

impl PcoClient {
  pub fn new(secrets: Secrets) -> Self {
    Self {
      client: reqwest::Client::new(),
      secrets,
      semaphore: Arc::new(Semaphore::new(PCO_CONCURRENCY)),
      cached_resources: Arc::new(Mutex::new(HashMap::new())),
    }
  }

  async fn fetch(&self, url: &str, mut retries: u8) -> Result<PcoResponse> {
    let _lock = self
      .semaphore
      .acquire()
      .await
      .wrap_err("failed to acquire semaphore")?;
    loop {
      if retries == 0 {
        tracing::debug!("fetching");
      } else {
        tracing::debug!("fetching - retries = {retries}");
      }

      // make the request
      let resp = self
        .client
        .get(url)
        .basic_auth(
          self.secrets.pco_app_id.clone(),
          Some(self.secrets.pco_secret.clone()),
        )
        .send()
        .await
        .wrap_err("failed to fetch from pco")?;

      // deserialize to PcoResponse
      let text_resp = resp
        .text()
        .await
        .wrap_err("failed to decode pco response as utf-8 plaintext")?;
      let json_resp = serde_json::from_str::<PcoResponse>(&text_resp)
        .wrap_err("failed to parse pco json response")
        .with_section(|| text_resp.header("Original Response:"))?;

      // handle 429 and retries
      if let PcoResponse::Error { ref errors } = json_resp {
        if errors.first().map(|e| e.code.as_str()) == Some("429") {
          if retries < 10 {
            tokio::time::sleep(Duration::from_secs(10)).await;
            retries += 1;
            continue;
          } else {
            tracing::error!("exceeded max retry count (10), aborting");
            return Err(color_eyre::eyre::eyre!(
              "exceeded max request retry count"
            ));
          }
        }
      }

      return Ok(json_resp);
    }
  }

  #[instrument(skip(self))]
  pub async fn fetch_event(&self, id: String) -> Result<PcoObject> {
    let url = format!("{URL_PREFIX}/events/{id}");
    self.fetch(&url, 0).await.and_then(|r| r.into_single())
  }

  #[instrument(skip(self))]
  pub async fn fetch_resource_requests_for_event(
    &self,
    event_id: String,
  ) -> Result<Vec<PcoObject>> {
    let url = format!("{URL_PREFIX}/events/{event_id}/event_resource_requests");
    self.fetch(&url, 0).await.and_then(|r| r.into_data_vec())
  }

  #[instrument(skip(self))]
  pub async fn fetch_resource(&self, resource_id: String) -> Result<PcoObject> {
    let cached = self.cached_resources.lock().await;
    if let Some(resource) = cached.get(&resource_id) {
      return Ok(resource.clone());
    }
    drop(cached);

    let url = format!("{URL_PREFIX}/resources/{resource_id}",);
    let resource = self.fetch(&url, 0).await.and_then(|r| r.into_single())?;
    self
      .cached_resources
      .lock()
      .await
      .insert(resource_id, resource.clone());

    Ok(resource)
  }

  #[instrument(
    skip(self),
    fields(
      start = start.to_rfc3339_opts(SecondsFormat::Secs, false),
      end = end.to_rfc3339_opts(SecondsFormat::Secs, false)
    )
  )]
  async fn fetch_event_instances_in_range(
    &self,
    start: DateTimeUtc,
    end: DateTimeUtc,
    offset: Option<usize>,
  ) -> Result<PcoResponse> {
    let url = format!(
      "{URL_PREFIX}/event_instances?where[starts_at][gte]={}&\
       where[starts_at][lt]={}&offset={}",
      start.to_rfc3339_opts(SecondsFormat::Secs, false),
      end.to_rfc3339_opts(SecondsFormat::Secs, false),
      offset.unwrap_or(0),
    );
    self.fetch(&url, 0).await
  }

  #[instrument(skip(self))]
  pub async fn fetch_all_event_instances_in_range(
    self: &Arc<Self>,
    start: DateTimeUtc,
    end: DateTimeUtc,
  ) -> Result<impl Stream<Item = Result<PcoObject>>> {
    let (tx, rx) = mpsc::channel(100); // Create a channel with a buffer size of 100

    let initial_response = self
      .fetch_event_instances_in_range(start, end, None)
      .await?;

    let total_count = match initial_response {
      PcoResponse::Multiple { meta, .. } => meta
        .get("total_count")
        .ok_or_eyre("missing total count")?
        .as_u64()
        .ok_or_eyre("total count is not a number")?,
      _ => color_eyre::eyre::bail!("unexpected single-item response"),
    };

    let client = Arc::new(self.clone());

    for offset in (0..total_count).step_by(25) {
      let client = client.clone();
      let tx = tx.clone();

      tokio::spawn(async move {
        let response = client
          .fetch_event_instances_in_range(start, end, Some(offset as usize))
          .await
          .and_then(|r| r.into_data_vec());

        match response {
          Ok(data) => {
            for item in data {
              tx.send(Ok(item)).await.unwrap();
            }
          }
          Err(e) => {
            tx.send(Err(e)).await.unwrap();
          }
        }
      });
    }

    // Drop the original sender to close the channel once all tasks are complete
    drop(tx);

    Ok(ReceiverStream::new(rx))
  }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct PcoError {
  code:   String,
  title:  String,
  detail: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(untagged)]
pub enum PcoResponse {
  Single {
    data: PcoObject,
  },
  Multiple {
    data:     Vec<PcoObject>,
    #[serde(default)]
    included: Vec<PcoObject>,
    #[serde(default)]
    links:    HashMap<String, String>,
    #[serde(default)]
    meta:     HashMap<String, serde_json::Value>,
  },
  Error {
    errors: Vec<PcoError>,
  },
}

impl PcoResponse {
  pub fn into_data_vec(self) -> Result<Vec<PcoObject>> {
    match self {
      PcoResponse::Single { data } => Ok(vec![data]),
      PcoResponse::Multiple { data, .. } => Ok(data),
      PcoResponse::Error { errors } => {
        let mut error = color_eyre::eyre::eyre!("asana error");
        for err in errors {
          error = error.section(format!("{:#?}", err));
        }
        Err(error)
      }
    }
  }
  pub fn into_single(self) -> Result<PcoObject> {
    match self {
      PcoResponse::Single { data } => Ok(data),
      PcoResponse::Multiple { .. } => {
        Err(color_eyre::eyre::eyre!("unexpected multi-item response"))
      }
      PcoResponse::Error { errors } => {
        let mut error = color_eyre::eyre::eyre!("asana error");
        for err in errors {
          error = error.section(format!("{:#?}", err));
        }
        Err(error)
      }
    }
  }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct PcoObject {
  #[serde(rename = "type")]
  pub type_:         String,
  pub id:            String,
  #[serde(default)]
  pub attributes:    Option<HashMap<String, serde_json::Value>>,
  #[serde(default)]
  pub relationships: Option<HashMap<String, serde_json::Value>>,
  #[serde(default)]
  pub links:         Option<HashMap<String, Option<String>>>,
}

impl PcoObject {
  pub fn get_attribute(&self, key: &str) -> Option<String> {
    Some(self.attributes.as_ref()?.get(key)?.as_str()?.to_string())
  }
}
