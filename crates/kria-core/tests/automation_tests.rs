//! Phase 8: Automation, Health, and Production Hardening tests.
//!
//! Covers AutomationScheduler, MacroRecorder, WorkflowEngine,
//! HealthRegistry, CircuitBreaker, and SupervisedTask.

use kria_core::automation::scheduler::ScheduledTask;
use kria_core::automation::workflows::{Workflow, WorkflowStatus, WorkflowStep};
use kria_core::automation::{AutomationScheduler, MacroRecorder, WorkflowEngine};
use kria_core::infra::circuit_breaker::CircuitBreaker;
use kria_core::infra::health::{HealthRegistry, ServiceStatus};
use serde_json::json;

// ── HealthRegistry ─────────────────────────────────────────────────

mod health {
    use super::*;

    #[test]
    fn register_and_get_service() {
        let registry = HealthRegistry::new();
        registry.register("llm");
        let h = registry.get("llm").unwrap();
        assert_eq!(h.name, "llm");
        assert_eq!(h.status, ServiceStatus::Unknown);
        assert!(h.last_check.is_none());
    }

    #[test]
    fn update_status() {
        let registry = HealthRegistry::new();
        registry.register("sidecar");
        registry.update("sidecar", ServiceStatus::Healthy, Some("all good".into()));
        let h = registry.get("sidecar").unwrap();
        assert_eq!(h.status, ServiceStatus::Healthy);
        assert_eq!(h.message.as_deref(), Some("all good"));
        assert!(h.last_check.is_some());
    }

    #[test]
    fn all_healthy_when_all_services_healthy() {
        let registry = HealthRegistry::new();
        registry.register("a");
        registry.register("b");
        registry.update("a", ServiceStatus::Healthy, None);
        registry.update("b", ServiceStatus::Healthy, None);
        assert!(registry.all_healthy());
    }

    #[test]
    fn not_all_healthy_when_one_degraded() {
        let registry = HealthRegistry::new();
        registry.register("a");
        registry.register("b");
        registry.update("a", ServiceStatus::Healthy, None);
        registry.update("b", ServiceStatus::Degraded, None);
        assert!(!registry.all_healthy());
    }

    #[test]
    fn status_all_returns_all_services() {
        let registry = HealthRegistry::new();
        registry.register("x");
        registry.register("y");
        registry.register("z");
        let all = registry.status_all();
        assert_eq!(all.len(), 3);
    }

    #[test]
    fn get_nonexistent_returns_none() {
        let registry = HealthRegistry::new();
        assert!(registry.get("nope").is_none());
    }

    #[test]
    fn service_status_serializes() {
        let s = serde_json::to_string(&ServiceStatus::Healthy).unwrap();
        assert!(s.contains("healthy"));
        let s = serde_json::to_string(&ServiceStatus::Degraded).unwrap();
        assert!(s.contains("degraded"));
    }
}

// ── AutomationScheduler ────────────────────────────────────────────

mod scheduler {
    use super::*;

    #[test]
    fn add_and_list_tasks() {
        let mut sched = AutomationScheduler::new();
        sched.add_task(ScheduledTask {
            id: "t1".into(),
            name: "test".into(),
            interval_secs: 60,
            prompt: "hi".into(),
            enabled: true,
        });
        let tasks = sched.list_tasks();
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].name, "test");
    }

    #[test]
    fn remove_task() {
        let mut sched = AutomationScheduler::new();
        sched.add_task(ScheduledTask {
            id: "t2".into(),
            name: "temp".into(),
            interval_secs: 300,
            prompt: "check stuff".into(),
            enabled: true,
        });
        assert_eq!(sched.list_tasks().len(), 1);
        sched.remove_task("t2");
        assert_eq!(sched.list_tasks().len(), 0);
    }

    #[test]
    fn remove_nonexistent_is_no_op() {
        let mut sched = AutomationScheduler::new();
        sched.remove_task("doesnt-exist");
        assert_eq!(sched.list_tasks().len(), 0);
    }

    #[tokio::test]
    async fn start_disabled_task_fails() {
        let mut sched = AutomationScheduler::new();
        sched.add_task(ScheduledTask {
            id: "disabled".into(),
            name: "off".into(),
            interval_secs: 60,
            prompt: "nope".into(),
            enabled: false,
        });
        let res = sched.start_task("disabled", |_| {});
        assert!(res.is_err());
    }

    #[test]
    fn start_nonexistent_task_fails() {
        let mut sched = AutomationScheduler::new();
        let res = sched.start_task("nope", |_| {});
        assert!(res.is_err());
    }
}

// ── MacroRecorder ────────────────────────────────────────────────────

mod macros {
    use super::*;

    #[test]
    fn record_and_stop() {
        let mut rec = MacroRecorder::new();
        rec.start_recording("my-macro");
        rec.record_step("file_read", json!({"path": "/tmp/x"}));
        rec.record_step("shell_exec", json!({"command": "ls"}));
        let m = rec.stop_recording("test macro").unwrap();
        assert_eq!(m.name, "my-macro");
        assert_eq!(m.steps.len(), 2);
        assert_eq!(m.description, "test macro");
    }

    #[test]
    fn stop_without_recording_returns_none() {
        let mut rec = MacroRecorder::new();
        assert!(rec.stop_recording("nothing").is_none());
    }

    #[test]
    fn list_and_get_macros() {
        let mut rec = MacroRecorder::new();
        rec.start_recording("m1");
        rec.record_step("tool_a", json!({}));
        rec.stop_recording("first");

        rec.start_recording("m2");
        rec.stop_recording("second");

        assert_eq!(rec.list().len(), 2);
        assert!(rec.get("m1").is_some());
        assert!(rec.get("m2").is_some());
        assert!(rec.get("m3").is_none());
    }

    #[test]
    fn delete_macro() {
        let mut rec = MacroRecorder::new();
        rec.start_recording("del-me");
        rec.stop_recording("temp");

        assert!(rec.delete("del-me"));
        assert!(!rec.delete("del-me")); // already gone
        assert_eq!(rec.list().len(), 0);
    }

    #[test]
    fn save_and_load_macros() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("macros.json");

        let mut rec = MacroRecorder::new();
        rec.start_recording("persistent");
        rec.record_step("read_file", json!({"path": "/etc/hosts"}));
        rec.stop_recording("test save");
        rec.save_to_file(&path).unwrap();

        let mut rec2 = MacroRecorder::new();
        rec2.load_from_file(&path).unwrap();
        assert_eq!(rec2.list().len(), 1);
        assert!(rec2.get("persistent").is_some());
    }

    #[test]
    fn record_step_without_recording_is_no_op() {
        let mut rec = MacroRecorder::new();
        rec.record_step("tool", json!({})); // should not panic
    }
}

// ── WorkflowEngine ──────────────────────────────────────────────────

mod workflows {
    use super::*;

    fn sample_workflow() -> Workflow {
        Workflow {
            id: "wf-1".into(),
            name: "test-workflow".into(),
            description: "a test wf".into(),
            steps: vec![
                WorkflowStep {
                    id: "s1".into(),
                    tool_name: "read_file".into(),
                    args: json!({"path": "/tmp/x"}),
                    condition: None,
                    on_failure: None,
                },
                WorkflowStep {
                    id: "s2".into(),
                    tool_name: "shell_exec".into(),
                    args: json!({"command": "echo hi"}),
                    condition: None,
                    on_failure: None,
                },
            ],
            created_at: "2026-01-01T00:00:00Z".into(),
        }
    }

    #[test]
    fn register_and_list() {
        let mut engine = WorkflowEngine::new();
        engine.register(sample_workflow());
        assert_eq!(engine.list().len(), 1);
        assert!(engine.get("wf-1").is_some());
    }

    #[test]
    fn delete_workflow() {
        let mut engine = WorkflowEngine::new();
        engine.register(sample_workflow());
        assert!(engine.delete("wf-1"));
        assert!(!engine.delete("wf-1"));
        assert_eq!(engine.list().len(), 0);
    }

    #[tokio::test]
    async fn execute_all_steps_succeed() {
        let mut engine = WorkflowEngine::new();
        engine.register(sample_workflow());

        let result = engine
            .execute("wf-1", |_tool, _args| async { Ok("ok".to_string()) })
            .await
            .unwrap();

        assert_eq!(result.status, WorkflowStatus::Completed);
        assert_eq!(result.results.len(), 2);
        assert!(result.results.iter().all(|r| r.success));
    }

    #[tokio::test]
    async fn execute_step_fails_aborts() {
        let mut engine = WorkflowEngine::new();
        let mut wf = sample_workflow();
        wf.steps[0].on_failure = None; // abort on failure
        engine.register(wf);

        let result = engine
            .execute("wf-1", |_tool, _args| async {
                Err(anyhow::anyhow!("boom"))
            })
            .await
            .unwrap();

        match result.status {
            WorkflowStatus::Failed(msg) => assert!(msg.contains("boom")),
            _ => panic!("expected Failed status"),
        }
        // Only first step executed
        assert_eq!(result.results.len(), 1);
    }

    #[tokio::test]
    async fn execute_nonexistent_fails() {
        let engine = WorkflowEngine::new();
        let result = engine
            .execute("nope", |_tool, _args| async { Ok("ok".into()) })
            .await;
        assert!(result.is_err());
    }

    #[test]
    fn save_and_load_workflows() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("workflows.json");

        let mut engine = WorkflowEngine::new();
        engine.register(sample_workflow());
        engine.save_to_file(&path).unwrap();

        let mut engine2 = WorkflowEngine::new();
        engine2.load_from_file(&path).unwrap();
        assert_eq!(engine2.list().len(), 1);
        assert!(engine2.get("wf-1").is_some());
    }
}

// ── CircuitBreaker ──────────────────────────────────────────────────

mod circuit_breaker {
    use super::*;
    use kria_core::infra::circuit_breaker::CircuitBreakerError;
    use std::time::Duration;

    #[tokio::test]
    async fn passes_when_closed() {
        let cb = CircuitBreaker::new("test", 3, Duration::from_secs(5));
        let result = cb
            .call(async { Ok::<_, String>("hello") }, |_: &String| false)
            .await
            .unwrap();
        assert_eq!(result, "hello");
    }

    #[tokio::test]
    async fn opens_after_threshold_failures() {
        let cb = CircuitBreaker::new("test", 2, Duration::from_millis(100));

        // 2 failures → opens
        for _ in 0..2 {
            let _ = cb
                .call(
                    async { Err::<String, _>("fail".to_string()) },
                    |_: &String| false,
                )
                .await;
        }

        // 3rd call should be rejected (circuit open)
        let result = cb
            .call(
                async { Ok::<_, String>("should not run".to_string()) },
                |_: &String| false,
            )
            .await;
        assert!(matches!(result, Err(CircuitBreakerError::Open(_))));
    }

    #[tokio::test]
    async fn half_open_allows_one_call() {
        let cb = CircuitBreaker::new("test", 1, Duration::from_millis(50));

        // 1 failure → opens
        let _ = cb
            .call(
                async { Err::<String, _>("fail".to_string()) },
                |_: &String| false,
            )
            .await;

        // Wait for cooldown
        tokio::time::sleep(Duration::from_millis(80)).await;

        // Should now be half-open, allow one call
        let result = cb
            .call(
                async { Ok::<_, String>("recovered".to_string()) },
                |_: &String| false,
            )
            .await;
        assert_eq!(result.unwrap(), "recovered");
    }

    #[tokio::test]
    async fn reset_clears_state() {
        let cb = CircuitBreaker::new("test", 1, Duration::from_secs(60));

        // Trip the breaker
        let _ = cb
            .call(
                async { Err::<String, _>("fail".to_string()) },
                |_: &String| false,
            )
            .await;

        // Reset
        cb.reset().await;

        // Should work again
        let result = cb
            .call(
                async { Ok::<_, String>("works".to_string()) },
                |_: &String| false,
            )
            .await;
        assert_eq!(result.unwrap(), "works");
    }

    #[tokio::test]
    async fn ignored_errors_dont_count() {
        let cb = CircuitBreaker::new("test", 2, Duration::from_secs(5));

        // "ignored" errors should not trip the breaker
        for _ in 0..5 {
            let _ = cb
                .call(
                    async { Err::<String, _>("ignored".to_string()) },
                    |e: &String| e == "ignored",
                )
                .await;
        }

        // Circuit should still be closed
        let result = cb
            .call(
                async { Ok::<_, String>("still works".to_string()) },
                |_: &String| false,
            )
            .await;
        assert_eq!(result.unwrap(), "still works");
    }
}
