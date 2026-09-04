use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    routing::{get, post},
    Json, Router,
};
use ed25519_dalek::{Signer, SigningKey};
use hmac::{Hmac, Mac};
use sha2::Sha256;
use std::sync::Arc;

pub struct AppState {
    pub webhook_secret: String,
    pub license_signing_key: SigningKey,
}

pub fn create_router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/health", get(health_check))
        .route("/webhooks/razorpay", post(razorpay_webhook))
        .with_state(state)
}

pub fn generate_signing_key() -> SigningKey {
    let mut bytes = [0u8; 32];
    rand::RngCore::fill_bytes(&mut rand::rngs::OsRng, &mut bytes);
    SigningKey::from_bytes(&bytes)
}

#[tokio::main]
async fn main() {
    let secret = std::env::var("RZP_WEBHOOK_SECRET").unwrap_or_else(|_| "test_webhook_secret".into());
    let state = Arc::new(AppState {
        webhook_secret: secret,
        license_signing_key: generate_signing_key(),
    });

    let app = create_router(state);
    let port = std::env::var("PORT").unwrap_or_else(|_| "8787".into());
    let addr = format!("0.0.0.0:{port}");
    println!("[clockverse-license] server binding on http://{addr}");
    let listener = tokio::net::TcpListener::bind(&addr).await.expect("bind failed");
    axum::serve(listener, app).await.expect("server error");
}

async fn health_check() -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "status": "ok",
        "service": "clockverse-license",
        "version": "0.1.0"
    }))
}

pub async fn razorpay_webhook(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    body: String,
) -> Result<Json<serde_json::Value>, StatusCode> {
    // Server-side truth only — never trust client callbacks (blueprint §7).
    let signature = headers
        .get("x-razorpay-signature")
        .and_then(|v| v.to_str().ok())
        .ok_or(StatusCode::BAD_REQUEST)?;

    let mut mac = Hmac::<Sha256>::new_from_slice(state.webhook_secret.as_bytes())
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    mac.update(body.as_bytes());
    let expected = hex::encode(mac.finalize().into_bytes());

    if !bool::from(subtle::ConstantTimeEq::ct_eq(
        expected.as_bytes(),
        signature.as_bytes(),
    )) {
        return Err(StatusCode::BAD_REQUEST); // reject tampering immediately
    }

    let event: serde_json::Value =
        serde_json::from_str(&body).map_err(|_| StatusCode::BAD_REQUEST)?;

    if event["event"] == "payment.captured" {
        let order_id = event["payload"]["payment"]["entity"]["order_id"]
            .as_str()
            .unwrap_or("unknown");
        // Ed25519-signed, machine-bound license payload
        let payload = format!("clockverse|pro|{order_id}|devices=1|exp=1y");
        let sig = state.license_signing_key.sign(payload.as_bytes());
        let license_key = format!("{payload}|sig={}", hex::encode(sig.to_bytes()));
        return Ok(Json(serde_json::json!({
            "status": "issued",
            "license": license_key
        })));
    }

    Ok(Json(serde_json::json!({ "status": "ignored" })))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use ed25519_dalek::Verifier;
    use tower::ServiceExt;

    #[tokio::test]
    async fn test_valid_webhook_and_ed25519_signature() {
        let secret = "secret123";
        let signing_key = generate_signing_key();
        let verifying_key = signing_key.verifying_key();

        let state = Arc::new(AppState {
            webhook_secret: secret.into(),
            license_signing_key: signing_key,
        });

        let app = create_router(state);

        let body_str = serde_json::json!({
            "event": "payment.captured",
            "payload": {
                "payment": {
                    "entity": {
                        "order_id": "order_HoloCore999"
                    }
                }
            }
        })
        .to_string();

        let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).unwrap();
        mac.update(body_str.as_bytes());
        let sig_hex = hex::encode(mac.finalize().into_bytes());

        let req = Request::builder()
            .method("POST")
            .uri("/webhooks/razorpay")
            .header("content-type", "application/json")
            .header("x-razorpay-signature", sig_hex)
            .body(Body::from(body_str))
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        use axum::body::to_bytes;
        let bytes = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        let json_val: serde_json::Value = serde_json::from_slice(&bytes).unwrap();

        let license = json_val["license"].as_str().unwrap();
        assert!(license.contains("order_HoloCore999"));

        // Verify Ed25519 signature component
        let parts: Vec<&str> = license.split("|sig=").collect();
        assert_eq!(parts.len(), 2);
        let payload = parts[0];
        let sig_bytes = hex::decode(parts[1]).unwrap();
        let signature = ed25519_dalek::Signature::from_slice(&sig_bytes).unwrap();
        assert!(verifying_key.verify(payload.as_bytes(), &signature).is_ok());
    }

    #[tokio::test]
    async fn test_invalid_signature_rejected() {
        let state = Arc::new(AppState {
            webhook_secret: "secret123".into(),
            license_signing_key: generate_signing_key(),
        });

        let app = create_router(state);

        let req = Request::builder()
            .method("POST")
            .uri("/webhooks/razorpay")
            .header("content-type", "application/json")
            .header("x-razorpay-signature", "invalid_tampered_signature")
            .body(Body::from(r#"{"event":"payment.captured"}"#))
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }
}
