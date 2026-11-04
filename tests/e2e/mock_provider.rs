//! Mock provider for e2e tests
//!
//! Returns pre-scripted StreamEvent sequences for deterministic testing.

use anyhow::Result;
use async_stream::stream;
use horon_agent::message::{Message, StreamEvent, ToolDefinition};
use horon_agent::provider::{EventStream, ModelRoute, Provider, RouteSelection};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

pub struct MockProvider {
    /// A stream may itself yield an error, which is how a partial answer
    /// followed by a transport failure is scripted (the rollback path).
    responses: Arc<Mutex<VecDeque<Vec<Result<StreamEvent>>>>>,
    /// Drained before `responses`, so a test can hand the turn loop a
    /// `FailoverRequired` without consuming a scripted stream.
    errors: Arc<Mutex<VecDeque<anyhow::Error>>>,
    routes: Vec<ModelRoute>,
    applied_routes: Arc<Mutex<Vec<RouteSelection>>>,
    calls: Arc<AtomicUsize>,
    models: Vec<&'static str>,
    current_model: Arc<Mutex<String>>,
    /// Captured system prompts from complete() calls (for testing)
    pub captured_system_prompts: Arc<Mutex<Vec<String>>>,
    /// Captured resume session IDs from complete() calls (for testing)
    pub captured_resume_session_ids: Arc<Mutex<Vec<Option<String>>>>,
    /// Captured model names from complete() calls (for testing)
    pub captured_models: Arc<Mutex<Vec<String>>>,
}

impl MockProvider {
    pub fn new() -> Self {
        Self::with_models(Vec::new())
    }

    pub fn with_models(models: Vec<&'static str>) -> Self {
        let current = models
            .first()
            .map(|m| (*m).to_string())
            .unwrap_or_else(|| "mock".to_string());
        Self {
            responses: Arc::new(Mutex::new(VecDeque::new())),
            errors: Arc::new(Mutex::new(VecDeque::new())),
            routes: Vec::new(),
            applied_routes: Arc::new(Mutex::new(Vec::new())),
            calls: Arc::new(AtomicUsize::new(0)),
            models,
            current_model: Arc::new(Mutex::new(current)),
            captured_system_prompts: Arc::new(Mutex::new(Vec::new())),
            captured_resume_session_ids: Arc::new(Mutex::new(Vec::new())),
            captured_models: Arc::new(Mutex::new(Vec::new())),
        }
    }

    pub fn with_routes(routes: Vec<ModelRoute>) -> Self {
        let mut provider = Self::new();
        provider.routes = routes;
        provider
    }

    /// Queue a response (sequence of StreamEvents) to be returned on next complete() call
    pub fn queue_response(&self, events: Vec<StreamEvent>) {
        self.queue_stream(events.into_iter().map(Ok).collect());
    }

    pub fn queue_stream(&self, events: Vec<Result<StreamEvent>>) {
        self.responses.lock().unwrap().push_back(events);
    }

    pub fn queue_error(&self, error: anyhow::Error) {
        self.errors.lock().unwrap().push_back(error);
    }

    pub fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }

    pub fn applied_routes(&self) -> Vec<RouteSelection> {
        self.applied_routes.lock().unwrap().clone()
    }
}

impl Default for MockProvider {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl Provider for MockProvider {
    async fn complete(
        &self,
        _messages: &[Message],
        _tools: &[ToolDefinition],
        system: &str,
        resume_session_id: Option<&str>,
    ) -> Result<EventStream> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        // Capture the system prompt for testing
        self.captured_system_prompts.lock().unwrap().push(system.to_string());
        self.captured_resume_session_ids
            .lock()
            .unwrap()
            .push(resume_session_id.map(|s| s.to_string()));
        self.captured_models.lock().unwrap().push(self.model());

        if let Some(error) = self.errors.lock().unwrap().pop_front() {
            return Err(error);
        }

        let events = self.responses.lock().unwrap().pop_front().unwrap_or_default();

        let stream = stream! {
            for event in events {
                yield event;
            }
        };

        Ok(Box::pin(stream))
    }

    fn name(&self) -> &str {
        "mock"
    }

    fn model(&self) -> String {
        self.current_model.lock().unwrap().clone()
    }

    fn set_model(&self, model: &str) -> Result<()> {
        if !self.models.is_empty() && !self.models.contains(&model) {
            anyhow::bail!("Unknown model: {}", model);
        }
        *self.current_model.lock().unwrap() = model.to_string();
        Ok(())
    }

    fn set_route_selection(&self, selection: &RouteSelection) -> Result<()> {
        self.applied_routes.lock().unwrap().push(selection.clone());
        *self.current_model.lock().unwrap() = selection.model.clone();
        Ok(())
    }

    fn model_routes(&self) -> Vec<ModelRoute> {
        self.routes.clone()
    }

    fn available_models(&self) -> Vec<&'static str> {
        self.models.clone()
    }

    fn fork(&self) -> Arc<dyn Provider> {
        let current = self.current_model.lock().unwrap().clone();
        Arc::new(MockProvider {
            responses: self.responses.clone(),
            errors: self.errors.clone(),
            routes: self.routes.clone(),
            applied_routes: self.applied_routes.clone(),
            calls: self.calls.clone(),
            models: self.models.clone(),
            current_model: Arc::new(Mutex::new(current)),
            captured_system_prompts: self.captured_system_prompts.clone(),
            captured_resume_session_ids: self.captured_resume_session_ids.clone(),
            captured_models: self.captured_models.clone(),
        })
    }
}
