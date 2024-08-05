use std::{collections::HashMap, fmt::Debug};

use color_eyre::{
  eyre::{Result, WrapErr},
  Section, SectionExt,
};
use phf::phf_map;
use reqwest::Response;
use serde::{Deserialize, Serialize};
use tracing::instrument;

use crate::secrets::Secrets;

const TASK_OPT_FIELDS: &str = "name,html_notes,tags,tags.name";

pub static TRACKED_RESOURCES: phf::Map<&'static str, &'static str> = phf_map! {
  "CONNECT" => "Connect",
  "Kids Auditorium" => "KA",
  "Kids Lobby" => "KA Lobby",
  "Main Auditorium" => "MA",
  "Main Lobby" => "MA Lobby",
  "Room 400" => "400",
  "Room 500" => "500",
  "Second Floor Classroom" => "2nd Floor",

  "ATS Room" => "ATS",
  "Underground Auditorium" => "UA",

  "Digital Recorder" => "Portable",
  "Graphics Computer" => "Portable",
  "Lighting System" => "Portable",
  "Livestream Encoder" => "Portable",
  "Portable Battery" => "Portable",
  "Powerpoint Clicker" => "Portable",
  "Proj. Screen - Large" => "Portable",
  "Proj. Screen - Medium" => "Portable",
  "Projector - Large" => "Portable",
  "Projector - Medium" => "Portable",
  "Projector Accessories" => "Portable",
  "Projector Stand" => "Portable",
  "Sound Box - Large #1" => "Portable",
  "Sound Box - Large #2" => "Portable",
  "Sound Box - Large #3" => "Portable",
  "Sound Box - Small #1" => "Soundbox",
  "Sound Box - Small #2" => "Soundbox",
  "Sound Box - Small #3" => "Soundbox",
  "Sound System - Large" => "Portable",
  "Sound System - Medium" => "Portable",
  "Stage squares" => "Portable",
  "Translation Equip" => "Portable",
  "Video Camera" => "Portable",
};

const SUBTASK_LIST: &[&str] = &[
  "PCO Info",
  "Pre-Production Email",
  "Concept Meeting",
  "Final Meeting",
  "Train/Schedule Tech",
  "Setup Streaming",
  "Equipment List",
  "Equipment Check Out",
  "Equipment Check In",
  "Follow Up",
];

/// The error type returned by the Asana API.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct AsanaError {
  pub message: String,
  pub help:    Option<String>,
  pub phrase:  Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct AsanaNextPage {
  pub offset: String,
  pub path:   String,
  pub uri:    String,
}

/// A generic wrapper type around Asana responses.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(untagged)]
pub enum AsanaResponse<T>
where
  T: Debug,
{
  Success {
    data:      T,
    next_page: Option<AsanaNextPage>,
  },
  Error {
    errors: Vec<AsanaError>,
  },
}

fn error_from_asana_errors(
  errors: Vec<AsanaError>,
) -> color_eyre::eyre::Report {
  let mut error = color_eyre::eyre::eyre!("asana error");
  for err in errors {
    error = error.section(format!("{:#?}", err));
  }
  error
}

impl<T> AsanaResponse<T>
where
  T: Debug + for<'a> serde::Deserialize<'a>,
{
  /// Convert the response into a [`color_eyre`] result.
  pub fn into_result(self) -> Result<T> {
    match self {
      AsanaResponse::Success { data, .. } => Ok(data),
      AsanaResponse::Error { errors } => Err(error_from_asana_errors(errors)),
    }
  }

  /// Decode a response as an Asana response.
  pub async fn decode_response(input: Response) -> Result<Self> {
    let text_resp = input
      .text()
      .await
      .wrap_err("failed to decode response as utf-8 plaintext")?;
    let resp = serde_json::from_str::<AsanaResponse<T>>(&text_resp)
      .wrap_err("failed to parse response")
      .with_section(|| text_resp.header("Original Response:"))?;
    Ok(resp)
  }
}

/// A client for interacting with the Asana API.
pub struct AsanaClient {
  client:    reqwest::Client,
  secrets:   Secrets,
  semaphore: tokio::sync::Semaphore,
  tag_cache: tokio::sync::Mutex<Option<HashMap<String, AsanaTag>>>,
}

impl AsanaClient {
  /// Create a new Asana client.
  pub fn new(secrets: Secrets) -> Self {
    Self {
      client: reqwest::Client::new(),
      secrets,
      semaphore: tokio::sync::Semaphore::new(10),
      tag_cache: tokio::sync::Mutex::new(None),
    }
  }

  /// Fetch tasks in the PCOTool project from the Asana API.
  #[instrument(skip(self))]
  async fn get_tasks(
    &self,
    offset: Option<&str>,
  ) -> Result<AsanaResponse<Vec<AsanaTask>>> {
    let url = format!(
      "https://app.asana.com/api/1.0/projects/{project_gid}/tasks",
      project_gid = self.secrets.asana_project_gid
    );

    let mut req = self
      .client
      .get(&url)
      .bearer_auth(&self.secrets.asana_pat)
      .query(&[("opt_fields", TASK_OPT_FIELDS), ("limit", "25")]);
    if let Some(offset) = offset {
      req = req.query(&[("offset", offset)])
    }

    let permit = self
      .semaphore
      .acquire()
      .await
      .wrap_err("failed to acquire semaphore")?;
    let resp = req.send().await.wrap_err("failed to fetch")?;
    drop(permit);

    AsanaResponse::decode_response(resp).await
  }

  /// Fetch all tasks in the PCOTool project from the Asana API.
  #[instrument(skip(self))]
  pub async fn get_all_tasks(&self) -> Result<Vec<AsanaTask>> {
    let mut resp = self.get_tasks(None).await?;
    let mut results: Vec<AsanaTask> = vec![];

    loop {
      let (mut data, next_page) = match resp {
        AsanaResponse::Success { data, next_page } => (data, next_page),
        AsanaResponse::Error { errors } => {
          return Err(error_from_asana_errors(errors));
        }
      };

      results.append(&mut data);
      let Some(next_page) = next_page else {
        break;
      };

      resp = self.get_tasks(Some(&next_page.offset)).await?;
    }

    Ok(results)
  }

  /// Create an Asana task from a [`PcoInstancedEvent`].
  #[instrument(skip(self, event), fields(event_id = event.event_id, task_gid))]
  pub async fn create_task(
    &self,
    event: crate::CanonTask,
  ) -> Result<Option<AsanaTask>> {
    let url = "https://app.asana.com/api/1.0/tasks";

    let html_notes = format!(
      "<body>\n<code>&gt;&gt;&gt;&gt; {} &lt;&lt;&lt;&lt;</code></body>",
      event.event_id,
    );

    let mut tags = event
      .resource_tags
      .into_iter()
      .map(|t| t.gid)
      .collect::<Vec<_>>();
    let created_tag = self.find_tag_by_name("New").await?;
    if let Some(created_tag) = created_tag {
      tags.push(created_tag.gid);
    } else {
      tracing::warn!("failed to find \"New\" tag");
    }

    let request_payload = serde_json::json!({
      "data": {
        "html_notes": html_notes,
        "name": event.name,
        "projects": [
          self.secrets.asana_project_gid.to_string()
        ],
        "tags": tags,
      },
    });

    let req = self
      .client
      .post(url)
      .query(&[("opt_fields", TASK_OPT_FIELDS)])
      .bearer_auth(&self.secrets.asana_pat)
      .json(&request_payload);

    let permit = self
      .semaphore
      .acquire()
      .await
      .wrap_err("failed to acquire semaphore")?;
    let resp = req.send().await.wrap_err("failed to fetch")?;
    drop(permit);

    let task = AsanaResponse::<AsanaTask>::decode_response(resp)
      .await?
      .into_result()?;
    tracing::Span::current().record("task_gid", &task.gid);
    tracing::info!("created task");

    for subtask_name in SUBTASK_LIST.iter().rev() {
      self
        .create_subtask(&task.gid, subtask_name)
        .await
        .wrap_err("failed to create subtask")?;
    }

    Ok(Some(task))
  }

  #[instrument(skip(self), fields(task_gid))]
  pub async fn create_subtask(
    &self,
    parent_task_gid: &str,
    name: &str,
  ) -> Result<AsanaTask> {
    let url = format!(
      "https://app.asana.com/api/1.0/tasks/{parent_task_gid}/subtasks",
    );

    let request_payload = serde_json::json!({
      "data": {
        "name": name,
      },
    });

    let req = self
      .client
      .post(url)
      .query(&[("opt_fields", TASK_OPT_FIELDS)])
      .bearer_auth(&self.secrets.asana_pat)
      .json(&request_payload);

    let permit = self
      .semaphore
      .acquire()
      .await
      .wrap_err("failed to acquire semaphore")?;
    let resp = req.send().await.wrap_err("failed to fetch")?;
    drop(permit);

    let task = AsanaResponse::<AsanaTask>::decode_response(resp)
      .await?
      .into_result()?;
    tracing::Span::current().record("task_gid", &task.gid);
    tracing::info!("created subtask");

    Ok(task)
  }

  /// Fetch tags from the Asana API.
  #[instrument(skip(self))]
  async fn get_tags(
    &self,
    offset: Option<&str>,
  ) -> Result<AsanaResponse<Vec<AsanaTag>>> {
    let url = "https://app.asana.com/api/1.0/tags";

    let mut req = self
      .client
      .get(url)
      .bearer_auth(&self.secrets.asana_pat)
      .query(&[
        ("limit", "100"),
        ("workspace", &self.secrets.asana_workspace_gid.to_string()),
      ]);
    if let Some(offset) = offset {
      req = req.query(&[("offset", offset)])
    }

    let permit = self
      .semaphore
      .acquire()
      .await
      .wrap_err("failed to acquire semaphore")?;
    let resp = req.send().await.wrap_err("failed to fetch")?;
    drop(permit);

    AsanaResponse::decode_response(resp).await
  }

  #[instrument(skip(self))]
  async fn get_all_tags(&self) -> Result<Vec<AsanaTag>> {
    let mut resp = self.get_tags(None).await?;
    let mut results: Vec<AsanaTag> = vec![];

    loop {
      let (mut data, next_page) = match resp {
        AsanaResponse::Success { data, next_page } => (data, next_page),
        AsanaResponse::Error { errors } => {
          return Err(error_from_asana_errors(errors));
        }
      };

      results.append(&mut data);
      let Some(next_page) = next_page else {
        break;
      };

      resp = self.get_tags(Some(&next_page.offset)).await?;
    }

    let mut tag_cache = self.tag_cache.lock().await;
    match *tag_cache {
      Some(ref mut cache) => {
        for tag in &results {
          cache.insert(tag.gid.clone(), tag.clone());
        }
      }
      None => {
        let mut cache = HashMap::new();
        for tag in &results {
          cache.insert(tag.gid.clone(), tag.clone());
        }
        *tag_cache = Some(cache);
      }
    }

    Ok(results)
  }

  /// Find a tag by name.
  #[instrument(skip(self))]
  pub async fn find_tag_by_name(&self, name: &str) -> Result<Option<AsanaTag>> {
    // look in the cache or fetch all tags and look in the cache
    let tag_cache = self.tag_cache.lock().await;

    let tag_cache = match *tag_cache {
      Some(ref cache) => cache.clone(),
      None => {
        drop(tag_cache);
        let _ = self.get_all_tags().await?;
        self.tag_cache.lock().await.clone().unwrap()
      }
    };

    let mut tags = tag_cache.values().collect::<Vec<&AsanaTag>>();
    tags.sort_by(|a, b| a.gid.cmp(&b.gid));
    let tag = tags.into_iter().find(|tag| tag.name == name).cloned();

    Ok(tag)
  }
}

/// A task from the Asana API.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct AsanaTask {
  pub gid:        String,
  pub name:       String,
  pub html_notes: String,
  pub tags:       Vec<AsanaTag>,
}

/// A tag from the Asana API.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct AsanaTag {
  pub gid:  String,
  pub name: String,
}

impl AsanaTask {
  /// Convert the task into a linked task.
  pub fn into_linked_task(self) -> Option<AsanaLinkedTask> {
    use scraper::{Html, Selector};

    let fragment = Html::parse_fragment(&self.html_notes);
    let selector = Selector::parse("code").unwrap();

    let code_element = fragment
      .select(&selector)
      .next()?
      .text()
      .collect::<String>();

    // extract the event id with the following format:
    // `>>>> [event_id] <<<<`
    let start = code_element.find(">>>>").or_else(|| {
      tracing::warn!(
        "failed to extract event ID from task HTML notes: couldn't find start \
         marker"
      );
      None
    })?;
    let start = start + 4;
    let end = code_element.find("<<<<").or_else(|| {
      tracing::warn!(
        "failed to extract event ID from task HTML notes: couldn't find end \
         marker"
      );
      None
    })?;
    let id = &code_element[start..end].trim();

    if id.is_empty() {
      tracing::warn!(
        "failed to extract event ID from task HTML notes: extracted ID is \
         empty"
      );
      return None;
    }

    Some(AsanaLinkedTask {
      task:     self,
      event_id: id.to_string(),
    })
  }
}

/// A task from the Asana API that has been linked to a PCO event.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct AsanaLinkedTask {
  pub task:     AsanaTask,
  pub event_id: String,
}
