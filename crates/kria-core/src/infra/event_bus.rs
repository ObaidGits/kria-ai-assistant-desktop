use tokio::sync::broadcast;

/// Events that flow through the system.
#[derive(Debug, Clone)]
pub enum KriaEvent {
    /// A file was uploaded or received by the system.
    FileUploaded {
        path: String,
        mime_type: String,
        size_bytes: u64,
    },
    /// A user message was received (before processing).
    MessageReceived { session_id: String, content: String },
    /// A tool execution completed.
    ToolCompleted {
        name: String,
        success: bool,
        duration_ms: u64,
    },
    /// The Python sidecar returned a result.
    SidecarResult {
        request_id: String,
        method: String,
        success: bool,
    },
    /// Voice transcription completed.
    VoiceTranscribed { text: String, confidence: f32 },
    /// Hardware tier was (re)detected.
    HardwareChanged { tier: String },
    /// A skill/plugin was installed or removed.
    SkillInstalled { name: String },
    /// Sidecar process is ready.
    SidecarReady,
    /// VRAM pressure detected; includes current free VRAM.
    VramPressure { free_vram_mb: u64 },
    /// LLM server swap initiated (old → new params).
    LlmSwapStarted {
        from_ngl: u32,
        to_ngl: u32,
        emergency: bool,
    },
    /// LLM server swap completed; new server is healthy.
    LlmSwapCompleted {
        new_ngl: u32,
        new_context: u32,
        duration_ms: u64,
    },
    /// Degradation level changed (e.g. "full" → "reduced_context").
    LlmDegradationChanged { level: String },
    /// An in-flight LLM stream was interrupted by a swap.
    LlmStreamInterrupted,
}

/// Central event bus using tokio broadcast channels.
///
/// All subscribers receive every event. Subscribers that fall behind
/// will see a `RecvError::Lagged` and can skip missed events.
#[derive(Clone)]
pub struct EventBus {
    sender: broadcast::Sender<KriaEvent>,
}

impl EventBus {
    /// Create a new EventBus with the given channel capacity.
    pub fn new(capacity: usize) -> Self {
        let (sender, _) = broadcast::channel(capacity);
        Self { sender }
    }

    /// Publish an event to all subscribers.
    pub fn publish(&self, event: KriaEvent) {
        // Ignore send errors — they just mean no active subscribers.
        let _ = self.sender.send(event);
    }

    /// Subscribe to all events. Returns a receiver.
    pub fn subscribe(&self) -> broadcast::Receiver<KriaEvent> {
        self.sender.subscribe()
    }

    /// Get the number of active subscribers.
    pub fn subscriber_count(&self) -> usize {
        self.sender.receiver_count()
    }
}

impl std::fmt::Debug for EventBus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EventBus")
            .field("subscribers", &self.sender.receiver_count())
            .finish()
    }
}
