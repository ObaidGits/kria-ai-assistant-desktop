use axum::{extract::Request, http::StatusCode, middleware::Next, response::Response};

/// JWT authentication middleware (optional — for remote server mode).
pub async fn auth_middleware(request: Request, next: Next) -> Result<Response, StatusCode> {
    // If no auth is configured, pass through
    let auth_header = request.headers().get("Authorization");

    match auth_header {
        Some(value) => {
            let token = value
                .to_str()
                .map_err(|_| StatusCode::UNAUTHORIZED)?
                .strip_prefix("Bearer ")
                .ok_or(StatusCode::UNAUTHORIZED)?;

            // Validate JWT token
            if validate_token(token).is_ok() {
                Ok(next.run(request).await)
            } else {
                Err(StatusCode::UNAUTHORIZED)
            }
        }
        None => {
            // Allow unauthenticated access for local-only mode
            Ok(next.run(request).await)
        }
    }
}

fn validate_token(token: &str) -> Result<(), ()> {
    // Placeholder: use jsonwebtoken crate for real validation
    if token.is_empty() {
        Err(())
    } else {
        // TODO: validate with jsonwebtoken::decode
        Ok(())
    }
}
