use std::net::SocketAddr;

use axum::{extract::State, http::StatusCode, routing::post, Json, Router};
use marketplace_model_spend_guard::marketplace_handoff::{
    HandoffRequest, HandoffResult, MarketplaceHandoff,
};
use serde_json::{json, Value};

#[tokio::main]
async fn main() {
    let service = MarketplaceHandoff::from_env().expect("INFRAI_API_KEY must be set");
    let app = Router::new()
        .route("/handoffs", post(create_handoff))
        .with_state(service);
    let address = SocketAddr::from(([127, 0, 0, 1], 3000));
    let listener = tokio::net::TcpListener::bind(address)
        .await
        .expect("bind local service");
    println!("marketplace handoff service listening on http://{address}");
    axum::serve(listener, app).await.expect("serve requests");
}

async fn create_handoff(
    State(service): State<MarketplaceHandoff>,
    Json(request): Json<HandoffRequest>,
) -> Result<Json<HandoffResult>, (StatusCode, Json<Value>)> {
    service.handoff(request).await.map(Json).map_err(|error| {
        let status = error.client_status();
        (status, Json(json!({ "error": error.to_string() })))
    })
}

