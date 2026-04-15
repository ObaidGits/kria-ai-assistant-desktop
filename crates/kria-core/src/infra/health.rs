use dashmap::DashMap;
use chrono::{DateTime, Utc};
use serde::Serialize;

/// Service health status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ServiceStatus {
    Unknown,
    Starting,
    Healthy,
    Degraded,
    Unhealthy,
    Stopped,
}

/// Health entry for a single service.
#[derive(Debug, Clone, Serialize)]
pub struct ServiceHealth {
    pub name: String,
    pub status: ServiceStatus,
    pub last_check: Option<DateTime<Utc>>,
    pub message: Option<String>,
}

/// Global health registry, shared across the application.
#[derive(Debug, Default)]
pub struct HealthRegistry {
    services: DashMap<String, ServiceHealth>,
}

impl HealthRegistry {
    pub fn new() -> Self {
        Self {
            services: DashMap::new(),
        }
    }

    /// Register a new service (initial status: Unknown).
    pub fn register(&self, name: impl Into<String>) {
        let name = name.into();
        self.services.insert(
            name.clone(),
            ServiceHealth {
                name,
                status: ServiceStatus::Unknown,
                last_check: None,
                message: None,
            },
        );
    }

    /// Update a service's status.
    pub fn update(&self, name: &str, status: ServiceStatus, message: Option<String>) {
        if let Some(mut entry) = self.services.get_mut(name) {
            entry.status = status;
            entry.last_check = Some(Utc::now());
            entry.message = message;
        }
    }

    /// Get a single service's health.
    pub fn get(&self, name: &str) -> Option<ServiceHealth> {
        self.services.get(name).map(|e| e.clone())
    }

    /// Get all services' health as a snapshot.
    pub fn status_all(&self) -> Vec<ServiceHealth> {
        self.services.iter().map(|e| e.value().clone()).collect()
    }

    /// True if all registered services are Healthy.
    pub fn all_healthy(&self) -> bool {
        self.services
            .iter()
            .all(|e| e.value().status == ServiceStatus::Healthy)
    }
}
