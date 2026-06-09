use std::collections::HashMap;
use std::io::{Cursor, Read};
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};
use tiny_http::{Header, Method, Request, Response, Server};

use crate::config::{AppConfig, WebhookConfig};
use crate::error::{CodeSyncError, Result};
use crate::git_backend::Git2Backend;
use crate::sync::sync_once;
use crate::webhook::verify_webhook_secret;

pub fn serve(config: AppConfig) -> Result<()> {
    let webhook_secret = resolve_webhook_secret(&config.webhook)?;
    let webhook_path = config.webhook.path.clone();
    let address = format!("{}:{}", config.listen_host, config.listen_port);
    let server = Server::http(&address).map_err(|err| CodeSyncError::Http(err.to_string()))?;
    let state = Arc::new(ServerState {
        config,
        backend: Mutex::new(Git2Backend::default()),
        webhook_secret,
    });

    tracing::info!(
        address = %server.server_addr(),
        webhook_path = %webhook_path,
        "listening"
    );

    for mut request in server.incoming_requests() {
        let response = process_request(&mut request, &state);
        if let Err(err) = request.respond(response) {
            tracing::warn!(error = %err, "failed to send HTTP response");
        }
    }

    Ok(())
}

struct ServerState {
    config: AppConfig,
    backend: Mutex<Git2Backend>,
    webhook_secret: Option<String>,
}

fn process_request(request: &mut Request, state: &ServerState) -> Response<Cursor<Vec<u8>>> {
    match route_request(request, state) {
        Ok(payload) => json_response(200, payload),
        Err(error) => error_response(error),
    }
}

fn route_request(request: &mut Request, state: &ServerState) -> Result<Value> {
    match (request.method(), request.url()) {
        (&Method::Get, "/healthz") => Ok(json!({"status": "ok"})),
        (&Method::Post, path) if path == state.config.webhook.path => {
            handle_webhook(request, state)
        }
        _ => Err(CodeSyncError::Http("not_found".to_string())),
    }
}

fn handle_webhook(request: &mut Request, state: &ServerState) -> Result<Value> {
    let max_body_bytes = state.config.webhook.max_body_bytes;
    if let Some(body_length) = request.body_length() {
        if body_length > max_body_bytes {
            return Err(CodeSyncError::Http("payload_too_large".to_string()));
        }
    }

    let mut body = Vec::new();
    request
        .as_reader()
        .take(max_body_bytes as u64 + 1)
        .read_to_end(&mut body)
        .map_err(|err| CodeSyncError::Http(err.to_string()))?;

    if body.len() > max_body_bytes {
        return Err(CodeSyncError::Http("payload_too_large".to_string()));
    }

    let headers = headers_to_map(request.headers());
    if !verify_webhook_secret(state.webhook_secret.as_deref(), &headers, &body) {
        return Err(CodeSyncError::Http("unauthorized".to_string()));
    }

    let mut backend = state
        .backend
        .lock()
        .map_err(|_| CodeSyncError::Http("sync mutex poisoned".to_string()))?;
    let result = sync_once(&state.config, &mut *backend, "webhook")?;
    serde_json::to_value(result).map_err(CodeSyncError::from)
}

fn headers_to_map(headers: &[Header]) -> HashMap<String, String> {
    headers
        .iter()
        .map(|header| {
            (
                header.field.as_str().to_string(),
                header.value.as_str().to_string(),
            )
        })
        .collect()
}

fn json_response(status: u16, payload: Value) -> Response<Cursor<Vec<u8>>> {
    let body =
        serde_json::to_vec_pretty(&payload).unwrap_or_else(|_| b"{\"status\":\"error\"}".to_vec());
    let content_type = Header::from_bytes(
        &b"Content-Type"[..],
        &b"application/json; charset=utf-8"[..],
    )
    .expect("static content type header should be valid");

    Response::from_data(body)
        .with_status_code(status)
        .with_header(content_type)
}

fn error_response(error: CodeSyncError) -> Response<Cursor<Vec<u8>>> {
    let status = error.http_status();
    let payload = match &error {
        CodeSyncError::Http(message)
            if matches!(
                message.as_str(),
                "not_found" | "unauthorized" | "payload_too_large"
            ) =>
        {
            json!({"status": message})
        }
        CodeSyncError::Conflict(_) => {
            json!({"status": "conflict", "error": user_error_message(&error)})
        }
        _ => json!({"status": "error", "error": user_error_message(&error)}),
    };

    json_response(status, payload)
}

fn user_error_message(error: &CodeSyncError) -> String {
    match error {
        CodeSyncError::Config(message)
        | CodeSyncError::Unsupported(message)
        | CodeSyncError::Credential(message)
        | CodeSyncError::Conflict(message)
        | CodeSyncError::GitBackend(message)
        | CodeSyncError::Http(message) => message.clone(),
        CodeSyncError::Io { path, source } => format!("io error at {}: {}", path.display(), source),
        CodeSyncError::Json(error) => error.to_string(),
    }
}

fn resolve_webhook_secret(webhook: &WebhookConfig) -> Result<Option<String>> {
    if let Some(secret_env) = webhook.secret_env.as_deref() {
        let secret = std::env::var(secret_env).map_err(|_| {
            CodeSyncError::Config(format!("webhook.secret_env {secret_env:?} is not set"))
        })?;
        if secret.is_empty() {
            return Err(CodeSyncError::Config(format!(
                "webhook.secret_env {secret_env:?} is not set"
            )));
        }
        return Ok(Some(secret));
    }

    Ok(webhook.secret.clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{CredentialConfig, RemoteConfig};
    use crate::git_backend::Git2Backend;
    use serde_json::Value;
    use std::io::Read;
    use std::path::PathBuf;
    use std::sync::{Mutex, OnceLock};
    use tempfile::tempdir;
    use tiny_http::{Method, TestRequest};

    fn env_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    fn test_config(state_dir: PathBuf) -> AppConfig {
        let credential = CredentialConfig {
            username_env: None,
            password_env: None,
            use_http_path: true,
        };
        AppConfig {
            listen_host: "127.0.0.1".to_string(),
            listen_port: 0,
            repo_dir: state_dir.join("repo.git"),
            state_dir: state_dir.clone(),
            branch: "main".to_string(),
            remotes: vec![
                RemoteConfig {
                    name: "repo_a".to_string(),
                    url: "https://example.invalid/a.git".to_string(),
                    credential: credential.clone(),
                    role: None,
                },
                RemoteConfig {
                    name: "repo_b".to_string(),
                    url: "https://example.invalid/b.git".to_string(),
                    credential,
                    role: None,
                },
            ],
            webhook: WebhookConfig {
                path: "/webhook".to_string(),
                secret: None,
                secret_env: None,
                max_body_bytes: 1024,
            },
            git_timeout_seconds: 30,
        }
    }

    fn response_json(response: Response<Cursor<Vec<u8>>>) -> Value {
        let mut body = Vec::new();
        response
            .into_reader()
            .read_to_end(&mut body)
            .expect("response body should be readable");
        serde_json::from_slice(&body).expect("response body should be valid JSON")
    }

    #[test]
    fn resolves_webhook_secret_from_env() {
        let _guard = env_lock()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let secret_env = "CODESYNC_TEST_WEBHOOK_SECRET";
        unsafe {
            std::env::set_var(secret_env, "webhook-secret");
        }
        let webhook = WebhookConfig {
            path: "/webhook".to_string(),
            secret: Some("literal-secret".to_string()),
            secret_env: Some(secret_env.to_string()),
            max_body_bytes: 1024,
        };

        let secret = resolve_webhook_secret(&webhook).expect("secret should resolve");

        assert_eq!(secret.as_deref(), Some("webhook-secret"));
        unsafe {
            std::env::remove_var(secret_env);
        }
    }

    #[test]
    fn resolves_webhook_secret_missing_env_errors() {
        let _guard = env_lock()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let secret_env = "CODESYNC_TEST_WEBHOOK_SECRET_MISSING";
        unsafe {
            std::env::remove_var(secret_env);
        }
        let webhook = WebhookConfig {
            path: "/webhook".to_string(),
            secret: Some("literal-secret".to_string()),
            secret_env: Some(secret_env.to_string()),
            max_body_bytes: 1024,
        };

        let error = resolve_webhook_secret(&webhook).expect_err("missing env should error");

        assert!(matches!(error, CodeSyncError::Config(message) if message.contains("is not set")));
    }

    #[test]
    fn healthz_route_returns_ok_json() {
        let temp = tempdir().unwrap();
        let state = ServerState {
            config: test_config(temp.path().to_path_buf()),
            backend: Mutex::new(Git2Backend::default()),
            webhook_secret: None,
        };
        let mut request: tiny_http::Request = TestRequest::new()
            .with_method(Method::Get)
            .with_path("/healthz")
            .into();

        let response = process_request(&mut request, &state);

        assert_eq!(response.status_code(), 200);
        assert_eq!(response_json(response), json!({"status": "ok"}));
    }

    #[test]
    fn unknown_route_returns_not_found_json() {
        let temp = tempdir().unwrap();
        let state = ServerState {
            config: test_config(temp.path().to_path_buf()),
            backend: Mutex::new(Git2Backend::default()),
            webhook_secret: None,
        };
        let mut request: tiny_http::Request = TestRequest::new()
            .with_method(Method::Get)
            .with_path("/missing")
            .into();

        let response = process_request(&mut request, &state);

        assert_eq!(response.status_code(), 404);
        assert_eq!(response_json(response), json!({"status": "not_found"}));
    }
}
