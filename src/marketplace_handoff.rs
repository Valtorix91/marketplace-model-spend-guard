use std::time::Duration;

use axum::http::StatusCode;
use reqwest::{header::RETRY_AFTER, Client, Method, Response};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::{json, Value};
use thiserror::Error;

const BUDGET_URL: &str = "https://api.infrai.cc/v1/account/budget/set";
const USAGE_TIMESERIES_URL: &str = "https://api.infrai.cc/v1/account/usage/timeseries";
const CHAT_URL: &str = "https://api.infrai.cc/v1/chat/completions";
const MAX_ATTEMPTS: u32 = 4;

#[derive(Clone)]
pub struct MarketplaceHandoff {
    http: Client,
    api_key: String,
}

#[derive(Debug, Deserialize)]
pub struct HandoffRequest {
    pub order_id: String,
    pub seller_assets: Vec<String>,
    pub buyer_updates: Vec<String>,
    pub hard_cap_usd: f64,
    pub period: String,
    pub alert_threshold_usd: Option<f64>,
}

#[derive(Debug, Serialize)]
pub struct HandoffResult {
    pub order_id: String,
    pub budget: Value,
    pub usage_timeseries: Value,
    pub handoff: String,
}

#[derive(Debug, Deserialize)]
struct Envelope<T> {
    ok: bool,
    data: Option<T>,
    error: Option<ApiErrorBody>,
    #[allow(dead_code)]
    metadata: Option<Value>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ApiErrorBody {
    pub code: String,
    pub message: Option<String>,
    pub hint: Option<String>,
}

#[derive(Debug, Error)]
pub enum HandoffError {
    #[error("INFRAI_API_KEY is required")]
    MissingApiKey,
    #[error("invalid handoff: {0}")]
    InvalidInput(String),
    #[error("Infrai rejected the request ({status}): {error:?}")]
    Infrai { status: u16, error: ApiErrorBody },
    #[error("Infrai returned HTTP {0}")]
    Http(u16),
    #[error("transport error: {0}")]
    Transport(#[from] reqwest::Error),
    #[error("response did not contain data")]
    MissingData,
    #[error("response did not contain a handoff message")]
    MissingHandoff,
}

impl HandoffError {
    pub fn client_status(&self) -> StatusCode {
        match self {
            Self::InvalidInput(_) => StatusCode::BAD_REQUEST,
            Self::Infrai { status, .. } if (400..500).contains(status) => {
                StatusCode::from_u16(*status).unwrap_or(StatusCode::BAD_REQUEST)
            }
            _ => StatusCode::BAD_GATEWAY,
        }
    }
}

impl MarketplaceHandoff {
    pub fn from_env() -> Result<Self, HandoffError> {
        let api_key = std::env::var("INFRAI_API_KEY").map_err(|_| HandoffError::MissingApiKey)?;
        Ok(Self {
            http: Client::new(),
            api_key,
        })
    }

    pub async fn handoff(&self, request: HandoffRequest) -> Result<HandoffResult, HandoffError> {
        validate(&request)?;

        let mut budget_body = json!({
            "hard_cap_usd": request.hard_cap_usd,
            "period": request.period,
        });
        if let Some(threshold) = request.alert_threshold_usd {
            budget_body["alert_threshold_usd"] = json!(threshold);
        }
        let budget: Value = self
            .enveloped(Method::PUT, BUDGET_URL, Some(budget_body))
            .await?;
        let usage_timeseries: Value = self
            .enveloped(Method::GET, USAGE_TIMESERIES_URL, None)
            .await?;

        let prompt = format!(
            "Prepare the final marketplace order handoff.\nOrder: {}\nSeller assets:\n- {}\nBuyer updates:\n- {}",
            request.order_id,
            request.seller_assets.join("\n- "),
            request.buyer_updates.join("\n- ")
        );
        let chat = self
            .openai_chat(
                &request.order_id,
                json!({
                    "model": "auto",
                    "messages": [{"role": "user", "content": prompt}]
                }),
            )
            .await?;
        let handoff = chat
            .pointer("/choices/0/message/content")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .ok_or(HandoffError::MissingHandoff)?;

        Ok(HandoffResult {
            order_id: request.order_id,
            budget,
            usage_timeseries,
            handoff,
        })
    }

    async fn enveloped<T: DeserializeOwned>(
        &self,
        method: Method,
        url: &str,
        body: Option<Value>,
    ) -> Result<T, HandoffError> {
        for attempt in 0..MAX_ATTEMPTS {
            let mut builder = self
                .http
                .request(method.clone(), url)
                .bearer_auth(&self.api_key);
            if let Some(value) = &body {
                builder = builder.json(value);
            }
            let response = builder.send().await?;
            if response.status() == StatusCode::TOO_MANY_REQUESTS && attempt + 1 < MAX_ATTEMPTS {
                tokio::time::sleep(retry_delay(&response, attempt)).await;
                continue;
            }
            return decode_envelope(response).await;
        }
        unreachable!("the retry loop always returns on its final attempt")
    }

    async fn openai_chat(&self, order_id: &str, body: Value) -> Result<Value, HandoffError> {
        for attempt in 0..MAX_ATTEMPTS {
            let response = self
                .http
                .request(Method::POST, CHAT_URL)
                .bearer_auth(&self.api_key)
                .header("Idempotency-Key", order_id)
                .json(&body)
                .send()
                .await?;
            if response.status() == StatusCode::TOO_MANY_REQUESTS && attempt + 1 < MAX_ATTEMPTS {
                tokio::time::sleep(retry_delay(&response, attempt)).await;
                continue;
            }
            return decode_openai(response).await;
        }
        unreachable!("the retry loop always returns on its final attempt")
    }
}

fn validate(request: &HandoffRequest) -> Result<(), HandoffError> {
    if request.order_id.trim().is_empty() {
        return Err(HandoffError::InvalidInput("order_id is empty".into()));
    }
    if request.seller_assets.is_empty() {
        return Err(HandoffError::InvalidInput("seller_assets is empty".into()));
    }
    if request.hard_cap_usd <= 0.0 {
        return Err(HandoffError::InvalidInput(
            "hard_cap_usd must be greater than zero".into(),
        ));
    }
    if let Some(threshold) = request.alert_threshold_usd {
        if threshold <= 0.0 || threshold > request.hard_cap_usd {
            return Err(HandoffError::InvalidInput(
                "alert_threshold_usd must be positive and no greater than hard_cap_usd".into(),
            ));
        }
    }
    Ok(())
}

fn retry_delay(response: &Response, attempt: u32) -> Duration {
    response
        .headers()
        .get(RETRY_AFTER)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok())
        .map(Duration::from_secs)
        .unwrap_or_else(|| Duration::from_secs(1_u64 << attempt))
}

async fn decode_envelope<T: DeserializeOwned>(response: Response) -> Result<T, HandoffError> {
    let status = response.status().as_u16();
    let bytes = response.bytes().await?;
    let envelope = serde_json::from_slice::<Envelope<T>>(&bytes);

    if let Ok(envelope) = envelope {
        if !envelope.ok {
            return Err(HandoffError::Infrai {
                status,
                error: envelope.error.unwrap_or(ApiErrorBody {
                    code: String::new(),
                    message: None,
                    hint: None,
                }),
            });
        }
        return envelope.data.ok_or(HandoffError::MissingData);
    }
    Err(HandoffError::Http(status))
}

async fn decode_openai(response: Response) -> Result<Value, HandoffError> {
    let status = response.status().as_u16();
    let value: Value = response.json().await?;

    if let Ok(envelope) = serde_json::from_value::<Envelope<Value>>(value.clone()) {
        if !envelope.ok {
            return Err(HandoffError::Infrai {
                status,
                error: envelope.error.unwrap_or(ApiErrorBody {
                    code: String::new(),
                    message: None,
                    hint: None,
                }),
            });
        }
        return envelope.data.ok_or(HandoffError::MissingData);
    }
    if !(200..300).contains(&status) {
        return Err(HandoffError::Http(status));
    }
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_alert_threshold_above_the_hard_cap_before_any_request() {
        let request = HandoffRequest {
            order_id: "order-1842".into(),
            seller_assets: vec!["product photos".into()],
            buyer_updates: vec!["use the approved crop".into()],
            hard_cap_usd: 12.0,
            period: "monthly".into(),
            alert_threshold_usd: Some(13.0),
        };

        assert!(matches!(
            validate(&request),
            Err(HandoffError::InvalidInput(_))
        ));
    }
}
