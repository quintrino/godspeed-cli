use crate::task::TaskRequest;
use reqwest::{Method, StatusCode};
use serde::Deserialize;
use serde_json::Value;
use std::collections::HashMap;
use std::env;
use std::time::Instant;

type ApiResult<T> = Result<T, Box<dyn std::error::Error>>;

#[derive(Deserialize, Debug)]
struct ItemsResponse {
    #[serde(default)]
    lists: Vec<Value>,
    #[serde(default)]
    labels: Vec<Value>,
}

/// The API endpoint for `path`. `GODSPEED_API_URL` replaces the base URL so the tests can point
/// the binary at a local mock server; the API token is sent to whatever it names.
fn api_url(path: &str) -> String {
    let base = env::var("GODSPEED_API_URL").unwrap_or_else(|_| "https://api.godspeedapp.com".to_string());
    format!("{}/{path}", base.trim_end_matches('/'))
}

/// What the API answered: the status, the `Retry-After` delay if any, and the body as text.
struct Answer {
    status: StatusCode,
    retry_after: Option<u64>,
    body: String,
}

/// Sends a request, and with `--debug` logs it and the answer. The token is not logged.
fn call(
    method: Method,
    path: &str,
    api_key: &str,
    query: &[(&str, &str)],
    body: Option<&Value>,
) -> ApiResult<Answer> {
    let url = api_url(path);
    let mut request = reqwest::blocking::Client::new()
        .request(method.clone(), &url)
        .header("Authorization", format!("Bearer {api_key}"))
        .header("Accept", "application/json");
    if !query.is_empty() {
        request = request.query(query);
    }
    if let Some(body) = body {
        request = request.json(body);
    }
    debug!("request: {method} {url}{}", if query.is_empty() { String::new() } else { format!(" query {query:?}") });
    debug!("request headers: Authorization: Bearer <token>, Accept: application/json");
    if let Some(body) = body {
        debug!("request body: {}", crate::debug::shorten(&body.to_string()));
    }
    let started = Instant::now();
    let response = match request.send() {
        Ok(response) => response,
        Err(e) => {
            debug!("request failed after {} ms: {e}", started.elapsed().as_millis());
            return Err(Box::new(e));
        }
    };
    let status = response.status();
    let retry_after = response
        .headers()
        .get("retry-after")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.trim().parse().ok());
    let text = response.text()?;
    debug!("response: {status} in {} ms, {} bytes", started.elapsed().as_millis(), text.len());
    debug!("response body: {}", crate::debug::shorten(&text));
    Ok(Answer { status, retry_after, body: text })
}

fn parse_body<T: serde::de::DeserializeOwned>(answer: &Answer) -> ApiResult<T> {
    Ok(serde_json::from_str(&answer.body)?)
}

/// "API error: 404 Not Found", plus the API's own message if its body has one.
fn api_error(answer: &Answer) -> String {
    let detail = serde_json::from_str::<Value>(&answer.body).ok().and_then(|value| {
        ["message", "error"].iter().find_map(|key| value.get(*key).and_then(Value::as_str).map(str::to_string))
    });
    match detail {
        Some(detail) => format!("API error: {} ({detail})", answer.status),
        None => format!("API error: {}", answer.status),
    }
}

fn expect_success(answer: &Answer) -> ApiResult<()> {
    if answer.status.is_success() {
        Ok(())
    } else {
        Err(api_error(answer).into())
    }
}

fn name_to_id(items: &[Value]) -> HashMap<String, String> {
    items
        .iter()
        .filter_map(|item| Some((item.get("name")?.as_str()?.to_lowercase(), item.get("id")?.as_str()?.to_string())))
        .collect()
}

/// All lists as the API describes them, in the order it gives them.
pub fn fetch_list_items(api_key: &str) -> ApiResult<Vec<Value>> {
    let answer = call(Method::GET, "lists", api_key, &[], None)?;
    expect_success(&answer)?;
    Ok(parse_body::<ItemsResponse>(&answer)?.lists)
}

/// All labels as the API describes them, in the order it gives them.
pub fn fetch_label_items(api_key: &str) -> ApiResult<Vec<Value>> {
    let answer = call(Method::GET, "labels", api_key, &[], None)?;
    expect_success(&answer)?;
    Ok(parse_body::<ItemsResponse>(&answer)?.labels)
}

/// All lists, as lowercase name -> id.
pub fn fetch_lists(api_key: &str) -> ApiResult<HashMap<String, String>> {
    Ok(name_to_id(&fetch_list_items(api_key)?))
}

/// All labels, as lowercase name -> id.
pub fn fetch_labels(api_key: &str) -> ApiResult<HashMap<String, String>> {
    Ok(name_to_id(&fetch_label_items(api_key)?))
}

/// One page of `GET /tasks`. The API also returns the lists and labels the tasks use.
#[derive(Deserialize, Debug, Default)]
pub struct TaskPage {
    #[serde(default)]
    pub tasks: Vec<Value>,
    #[serde(default)]
    pub lists: Vec<Value>,
    #[serde(default)]
    pub labels: Vec<Value>,
}

pub enum PageError {
    /// HTTP 429, with the `Retry-After` delay in seconds if the API gave one.
    RateLimited(Option<u64>),
    Failed(Box<dyn std::error::Error>),
}

/// Fetches up to 250 tasks with the given status (`incomplete` or `complete`), newest update
/// first. `updated_before` continues from an earlier page, and `updated_after` limits the tasks
/// to those changed since then.
pub fn fetch_task_page(
    api_key: &str,
    status: &str,
    updated_before: Option<&str>,
    updated_after: Option<&str>,
) -> Result<TaskPage, PageError> {
    let mut query = vec![("status", status)];
    if let Some(before) = updated_before {
        query.push(("updated_before", before));
    }
    if let Some(after) = updated_after {
        query.push(("updated_after", after));
    }
    let answer = call(Method::GET, "tasks", api_key, &query, None).map_err(PageError::Failed)?;
    if answer.status.as_u16() == 429 {
        return Err(PageError::RateLimited(answer.retry_after));
    }
    if !answer.status.is_success() {
        return Err(PageError::Failed(api_error(&answer).into()));
    }
    parse_body(&answer).map_err(PageError::Failed)
}

/// Creates the task and returns what the API answered (`{}` if the answer wasn't JSON).
pub fn send_task(task: &TaskRequest, api_key: &str) -> ApiResult<Value> {
    let body = serde_json::to_value(task)?;
    let answer = call(Method::POST, "tasks", api_key, &[], Some(&body))?;
    expect_success(&answer)?;
    Ok(serde_json::from_str(&answer.body).unwrap_or_else(|_| serde_json::json!({})))
}

/// Why a change to a task didn't happen. `status` is the HTTP status, or none if the API
/// couldn't be reached at all.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApiError {
    pub status: Option<u16>,
    pub message: String,
}

impl ApiError {
    /// Whether trying again later could work: the API couldn't be reached, was busy, or failed.
    /// A refusal such as 404 or 400 will be the same next time.
    pub fn is_retryable(&self) -> bool {
        match self.status {
            None => true,
            Some(status) => status >= 500 || status == 429 || status == 408,
        }
    }
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for ApiError {}

/// Sends a request and turns anything but success into an [`ApiError`], returning the body.
fn call_checked(method: Method, path: &str, api_key: &str, body: Option<&Value>) -> Result<String, ApiError> {
    let answer = call(method, path, api_key, &[], body)
        .map_err(|e| ApiError { status: None, message: e.to_string() })?;
    if answer.status.is_success() {
        Ok(answer.body)
    } else {
        Err(ApiError { status: Some(answer.status.as_u16()), message: api_error(&answer) })
    }
}

/// `PATCH /tasks/:id` with only the fields to change. Returns what the API answered (`{}` if the
/// answer wasn't JSON).
pub fn update_task(task_id: &str, changes: &Value, api_key: &str) -> Result<Value, ApiError> {
    let body = call_checked(Method::PATCH, &format!("tasks/{task_id}"), api_key, Some(changes))?;
    Ok(serde_json::from_str(&body).unwrap_or_else(|_| serde_json::json!({})))
}

/// `DELETE /tasks/:id`.
pub fn delete_task(task_id: &str, api_key: &str) -> Result<(), ApiError> {
    call_checked(Method::DELETE, &format!("tasks/{task_id}"), api_key, None).map(|_| ())
}
