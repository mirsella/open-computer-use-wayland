use std::{
    collections::BTreeSet,
    future::Future,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use serde_json::json;
use tokio::time::{sleep, timeout};

use crate::{
    errors::{RuntimeError, ToolOutcome},
    input::GeneratedInputAction,
    runtime::{DesktopRuntime, ToolOutput},
    screenshot::{NoScreenshots, ScreenshotMapping, ScreenshotProvider},
    validation::{ApplicationScope, ElementAction, MAX_TEXT_LIMIT, TextLimit, ToolCall},
};

pub const EMPTY_APPS_MESSAGE: &str = "No running applications with accessible windows found.";

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ObjectId {
    pub bus_name: String,
    pub path: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

impl Rect {
    fn is_valid(self) -> bool {
        self.width >= 0 && self.height >= 0
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActionInfo {
    pub name: String,
    pub description: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WindowInfo {
    pub object: ObjectId,
    pub title: String,
    pub states: BTreeSet<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppInfo {
    pub object: ObjectId,
    pub name: String,
    pub pid: u32,
    pub windows: Vec<WindowInfo>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ActionCapabilities {
    Unsupported,
    InspectionFailed,
    Inspected(Vec<ActionInfo>),
}

#[derive(Debug, Clone, PartialEq)]
pub struct InspectedCapabilities {
    pub actions: ActionCapabilities,
    pub component: bool,
    pub editable_text: bool,
    pub value: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub enum NodeCapabilities {
    InspectionFailed,
    Inspected(InspectedCapabilities),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SetValueKind {
    Text,
    Number,
}

impl NodeCapabilities {
    fn actions(&self) -> &[ActionInfo] {
        match self {
            Self::Inspected(InspectedCapabilities {
                actions: ActionCapabilities::Inspected(actions),
                ..
            }) => actions,
            _ => &[],
        }
    }

    fn inspected_actions(&self) -> Result<&[ActionInfo], ()> {
        match self {
            Self::InspectionFailed
            | Self::Inspected(InspectedCapabilities {
                actions: ActionCapabilities::InspectionFailed,
                ..
            }) => Err(()),
            Self::Inspected(InspectedCapabilities {
                actions: ActionCapabilities::Unsupported,
                ..
            }) => Ok(&[]),
            Self::Inspected(InspectedCapabilities {
                actions: ActionCapabilities::Inspected(actions),
                ..
            }) => Ok(actions),
        }
    }

    fn inspection_complete(&self) -> bool {
        self.inspected_actions().is_ok()
    }

    fn supports_focus(&self) -> bool {
        matches!(
            self,
            Self::Inspected(InspectedCapabilities {
                component: true,
                ..
            })
        )
    }

    fn set_value_kind(&self) -> Option<SetValueKind> {
        match self {
            Self::Inspected(InspectedCapabilities {
                editable_text: true,
                ..
            }) => Some(SetValueKind::Text),
            Self::Inspected(InspectedCapabilities { value: true, .. }) => {
                Some(SetValueKind::Number)
            }
            _ => None,
        }
    }

    fn interfaces_inspected(&self) -> bool {
        matches!(self, Self::Inspected(_))
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct NodeInfo {
    pub object: ObjectId,
    pub role: String,
    pub name: String,
    pub value: Option<String>,
    pub text: Option<String>,
    pub selected_text: Option<String>,
    pub states: BTreeSet<String>,
    pub capabilities: NodeCapabilities,
    pub window_frame: Option<Rect>,
    pub children: Vec<ObjectId>,
}

impl NodeInfo {
    pub fn is_defunct(&self) -> bool {
        self.states.contains("defunct") || self.states.contains("stale")
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum SemanticAction {
    InvokeAction(i32),
    GrabFocus,
    ReplaceText(String),
    SetNumericValue(f64),
}

pub trait AccessibilityAdapter: Send + Sync + 'static {
    fn discover(&self) -> impl Future<Output = Result<Vec<AppInfo>, RuntimeError>> + Send + '_;
    fn read_node<'a>(
        &'a self,
        object: &'a ObjectId,
        text_limit: usize,
    ) -> impl Future<Output = Result<NodeInfo, RuntimeError>> + Send + 'a;
    fn act<'a>(
        &'a self,
        object: &'a ObjectId,
        action: SemanticAction,
    ) -> impl Future<Output = Result<(), RuntimeError>> + Send + 'a;
}

#[derive(Debug, Clone, Copy)]
pub struct RuntimeConfig {
    pub default_max_nodes: usize,
    pub default_max_depth: usize,
    pub default_text_limit: usize,
    pub call_timeout: Duration,
    pub portal_timeout: Duration,
    pub snapshot_timeout: Duration,
    pub settle_interval: Duration,
}

impl Default for RuntimeConfig {
    fn default() -> Self {
        Self {
            default_max_nodes: 1_200,
            default_max_depth: 64,
            default_text_limit: 500,
            call_timeout: Duration::from_secs(2),
            portal_timeout: Duration::from_secs(60),
            snapshot_timeout: Duration::from_secs(12),
            settle_interval: Duration::from_millis(150),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ElementSnapshot {
    pub depth: usize,
    pub node: NodeInfo,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SnapshotLimits {
    pub text: usize,
    pub nodes: usize,
    pub depth: usize,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Snapshot {
    pub app_query: String,
    pub app: AppInfo,
    pub window: WindowInfo,
    pub generation: u64,
    pub elements: Vec<ElementSnapshot>,
    pub node_limit_reached: bool,
    pub depth_limit_reached: bool,
    pub limits: SnapshotLimits,
}

#[derive(Debug)]
struct CachedObservation {
    snapshot: Arc<Snapshot>,
    screenshot_mapping: Option<ScreenshotMapping>,
}

#[derive(Debug, Default)]
struct Cache {
    generation: u64,
    current: Option<CachedObservation>,
}

pub struct SemanticRuntime<A, S = NoScreenshots> {
    adapter: A,
    screenshots: S,
    config: RuntimeConfig,
    cache: Mutex<Cache>,
    mutation: Arc<tokio::sync::Mutex<()>>,
    launch_in_progress: Arc<AtomicBool>,
}

impl<A, S> std::fmt::Debug for SemanticRuntime<A, S> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SemanticRuntime")
            .field("config", &self.config)
            .finish_non_exhaustive()
    }
}

impl<A: AccessibilityAdapter> SemanticRuntime<A, NoScreenshots> {
    pub fn new(adapter: A) -> Self {
        Self::with_config(adapter, RuntimeConfig::default())
    }

    pub fn with_config(adapter: A, config: RuntimeConfig) -> Self {
        Self::with_screenshot_provider(adapter, NoScreenshots, config)
    }
}

impl<A: AccessibilityAdapter, S: ScreenshotProvider> SemanticRuntime<A, S> {
    pub fn with_screenshot_provider(adapter: A, screenshots: S, config: RuntimeConfig) -> Self {
        Self {
            adapter,
            screenshots,
            config,
            cache: Mutex::new(Cache::default()),
            mutation: Arc::new(tokio::sync::Mutex::new(())),
            launch_in_progress: Arc::new(AtomicBool::new(false)),
        }
    }

    async fn list_running_apps(&self) -> Result<Vec<AppInfo>, RuntimeError> {
        let mut apps = self.discover().await?;
        apps.retain(|app| !app.windows.is_empty());
        apps.sort_by(|left, right| {
            left.name
                .to_lowercase()
                .cmp(&right.name.to_lowercase())
                .then_with(|| left.name.cmp(&right.name))
                .then_with(|| left.pid.cmp(&right.pid))
        });
        Ok(apps)
    }

    pub async fn list_apps_text(&self) -> Result<String, RuntimeError> {
        let apps = self.list_running_apps().await?;
        Ok(format_running_apps(&apps))
    }

    pub async fn snapshot_text(
        &self,
        app_query: String,
        text_limit: Option<TextLimit>,
        max_nodes: Option<usize>,
        max_depth: Option<usize>,
    ) -> Result<String, RuntimeError> {
        let _mutation = self.mutation.lock().await;
        let snapshot = self
            .requested_snapshot(app_query, text_limit, max_nodes, max_depth)
            .await?;
        Ok(format!(
            "{}\nScreenshot unavailable: capture was not requested by the text-only API.",
            format_snapshot(&snapshot)
        ))
    }

    async fn execute_call(&self, call: ToolCall) -> Result<ToolOutput, RuntimeError> {
        match call {
            ToolCall::ListApplications { .. } => self.execute_call_inner(call).await,
            ToolCall::LaunchApplication { desktop_id } => {
                let _mutation = self.mutation.lock().await;
                self.lock_cache()?.current = None;
                let launched = crate::desktop_launcher::launch(
                    &desktop_id,
                    Arc::clone(&self.launch_in_progress),
                )
                .await?;
                Ok(ToolOutput::text(format!(
                    "Launch requested for {} (desktop_id={}).",
                    launched.name, launched.desktop_id
                ))
                .with_structured_content(json!({
                    "status": "requested",
                    "desktop_id": launched.desktop_id,
                    "name": launched.name
                })))
            }
            call => {
                let _mutation = self.mutation.lock().await;
                if self.launch_in_progress.load(Ordering::Acquire) {
                    return Err(launch_in_progress_error());
                }
                self.execute_call_inner(call).await
            }
        }
    }

    async fn execute_call_inner(&self, call: ToolCall) -> Result<ToolOutput, RuntimeError> {
        match call {
            ToolCall::ListApplications { scope } => self.list_applications(scope).await,
            ToolCall::Observe {
                target,
                text_limit,
                max_tree_nodes,
                max_tree_depth,
            } => {
                let snapshot = self
                    .requested_snapshot(target, text_limit, max_tree_nodes, max_tree_depth)
                    .await?;
                Ok(self.observe(snapshot).await)
            }
            ToolCall::LaunchApplication { .. } => {
                eprintln!("open-computer-use: launch bypassed its mutation fence");
                Err(internal_error("launch mutation invariant failed"))
            }
            ToolCall::ActOnElement {
                state_id,
                element_id,
                action,
            } => {
                let snapshot = self.element_action(&state_id, &element_id, action).await?;
                Ok(self.observe(snapshot).await)
            }
            ToolCall::Pointer { state_id, action } => {
                self.perform_generated(&state_id, GeneratedInputAction::Pointer(action))
                    .await
            }
            ToolCall::Keyboard {
                state_id,
                focus,
                action,
            } => {
                self.perform_generated(&state_id, GeneratedInputAction::Keyboard { focus, action })
                    .await
            }
        }
    }

    async fn list_applications(&self, scope: ApplicationScope) -> Result<ToolOutput, RuntimeError> {
        match scope {
            ApplicationScope::Running => {
                let apps = self.list_running_apps().await?;
                let structured = json!({
                    "scope": "running",
                    "applications": apps.iter().map(running_app_metadata).collect::<Vec<_>>()
                });
                Ok(
                    ToolOutput::text(format_running_apps(&apps))
                        .with_structured_content(structured),
                )
            }
            ApplicationScope::Installed => {
                let apps = crate::desktop_launcher::list_installed_apps().await?;
                let text = if apps.is_empty() {
                    "No installed desktop applications found.".to_owned()
                } else {
                    apps.iter()
                        .map(|app| format!("{} — {}", escape(&app.name), app.desktop_id))
                        .collect::<Vec<_>>()
                        .join("\n")
                };
                let structured = json!({
                    "scope": "installed",
                    "applications": apps.iter().map(|app| json!({
                        "desktop_id": app.desktop_id,
                        "name": app.name,
                        "shown": app.shown
                    })).collect::<Vec<_>>()
                });
                Ok(ToolOutput::text(text).with_structured_content(structured))
            }
        }
    }

    async fn discover(&self) -> Result<Vec<AppInfo>, RuntimeError> {
        timeout(self.config.call_timeout, self.adapter.discover())
            .await
            .map_err(|_| operational_error("AT-SPI application discovery timed out"))?
    }

    async fn read_node(&self, id: &ObjectId, text_limit: usize) -> Result<NodeInfo, RuntimeError> {
        timeout(
            self.config.call_timeout,
            self.adapter.read_node(id, text_limit),
        )
        .await
        .map_err(|_| {
            operational_error(format!(
                "AT-SPI call timed out while reading {}{}",
                id.bus_name, id.path
            ))
        })?
    }

    async fn snapshot_and_cache(
        &self,
        app_query: String,
        limits: SnapshotLimits,
    ) -> Result<Arc<Snapshot>, RuntimeError> {
        let future = self.collect_snapshot(app_query.clone(), None, limits);
        let snapshot = timeout(self.config.snapshot_timeout, future)
            .await
            .map_err(|_| operational_error("AT-SPI snapshot timed out"))??;
        self.commit_snapshot(snapshot)
    }

    fn commit_snapshot(&self, mut snapshot: Snapshot) -> Result<Arc<Snapshot>, RuntimeError> {
        let mut cache = self.lock_cache()?;
        cache.generation = cache
            .generation
            .checked_add(1)
            .ok_or_else(|| operational_error("snapshot generation overflow"))?;
        snapshot.generation = cache.generation;
        let snapshot = Arc::new(snapshot);
        cache.current = Some(CachedObservation {
            snapshot: Arc::clone(&snapshot),
            screenshot_mapping: None,
        });
        Ok(snapshot)
    }

    async fn collect_snapshot(
        &self,
        app_query: String,
        expected_pid: Option<u32>,
        limits: SnapshotLimits,
    ) -> Result<Snapshot, RuntimeError> {
        let apps = self.discover().await?;
        let resolved = resolve_app(&app_query, expected_pid, &apps)?;
        let window = if let Some(window) = resolved.window {
            if !window_is_viable(&window) {
                return Err(operational_error(format!(
                    "matched window {:?} is stale or defunct",
                    window.title
                )));
            }
            window
        } else {
            choose_window(&resolved.app.windows)?.clone()
        };
        let elements = self.traverse(&window, limits).await?;
        Ok(Snapshot {
            app_query,
            app: resolved.app,
            window,
            generation: 0,
            node_limit_reached: elements.node_limit_reached,
            depth_limit_reached: elements.depth_limit_reached,
            elements: elements.elements,
            limits,
        })
    }

    async fn traverse(
        &self,
        window: &WindowInfo,
        limits: SnapshotLimits,
    ) -> Result<Traversal, RuntimeError> {
        let mut stack = vec![(window.object.clone(), 0_usize)];
        let mut elements = Vec::new();
        let mut node_limit_reached = false;
        let mut depth_limit_reached = false;
        while let Some((object, depth)) = stack.pop() {
            if elements.len() >= limits.nodes {
                node_limit_reached = true;
                break;
            }
            let mut node = match self.read_node(&object, limits.text).await {
                Ok(node) => node,
                Err(error) if depth > 0 => {
                    eprintln!(
                        "open-computer-use: skipping stale AT-SPI child: object={}{} error={error}",
                        object.bus_name, object.path
                    );
                    continue;
                }
                Err(error) => return Err(error),
            };
            if node.object != object {
                eprintln!("open-computer-use: AT-SPI adapter returned mismatched object identity");
                return Err(operational_error(
                    "AT-SPI object identity changed while reading",
                ));
            }
            if node.is_defunct() {
                if depth == 0 {
                    return Err(operational_error(format!(
                        "selected window {}{} is defunct or stale",
                        object.bus_name, object.path
                    )));
                }
                eprintln!(
                    "open-computer-use: skipping defunct AT-SPI child: object={}{}",
                    object.bus_name, object.path
                );
                continue;
            }
            node.window_frame = normalize_frame(&node);
            let children = node.children.clone();
            elements.push(ElementSnapshot { depth, node });
            if depth >= limits.depth {
                if !children.is_empty() {
                    depth_limit_reached = true;
                }
                continue;
            }
            for child in children.into_iter().rev() {
                stack.push((child, depth + 1));
            }
        }
        Ok(Traversal {
            elements,
            node_limit_reached,
            depth_limit_reached,
        })
    }

    async fn element_action(
        &self,
        state_id: &str,
        index: &str,
        action: ElementAction,
    ) -> Result<Arc<Snapshot>, RuntimeError> {
        let cached = self.required_cached(state_id)?;
        let old_element = cached_element(&cached, index)?;
        match self
            .read_node(&old_element.node.object, cached.limits.text)
            .await
        {
            Ok(node) if node.is_defunct() => {
                return Err(operational_error("target element is defunct"));
            }
            Ok(_) => {}
            Err(error) => {
                eprintln!(
                    "open-computer-use: cached AT-SPI object path is stale; trying strict relocation: {error}"
                );
            }
        }
        let current = self.fresh_for_action(&cached).await?;
        let target = relocate(old_element, &current.elements)?;
        if target.node.is_defunct() {
            return Err(operational_error("target element is defunct"));
        }
        let semantic = semantic_action(action, target)?;
        self.require_latest_generation(&cached)?;
        self.invalidate_current(&cached)?;
        timeout(
            self.config.call_timeout,
            self.adapter.act(&target.node.object, semantic),
        )
        .await
        .map_err(|_| operational_error("AT-SPI semantic action timed out"))
        .and_then(|result| result)
        .map_err(uncertain_action)?;
        self.settle_and_refresh(cached)
            .await
            .map_err(completed_without_observation)
    }

    async fn fresh_for_action(&self, cached: &Snapshot) -> Result<Snapshot, RuntimeError> {
        let limits = SnapshotLimits {
            text: cached.limits.text,
            nodes: self.config.default_max_nodes.max(cached.elements.len()),
            depth: self.config.default_max_depth,
        };
        let current = self
            .collect_snapshot(cached.app_query.clone(), Some(cached.app.pid), limits)
            .await?;
        if current.app.pid != cached.app.pid {
            return Err(operational_error(format!(
                "stale PID: snapshot had {}, current app has {}",
                cached.app.pid, current.app.pid
            )));
        }
        if current.app.object != cached.app.object {
            return Err(operational_error(
                "application identity changed since the prior state",
            ));
        }
        if current.window.object != cached.window.object {
            return Err(operational_error(
                "window identity changed since the prior state",
            ));
        }
        Ok(current)
    }

    async fn requested_snapshot(
        &self,
        app_query: String,
        text_limit: Option<TextLimit>,
        max_nodes: Option<usize>,
        max_depth: Option<usize>,
    ) -> Result<Arc<Snapshot>, RuntimeError> {
        let text = match text_limit {
            Some(TextLimit::Count(limit)) => limit,
            Some(TextLimit::Max) => MAX_TEXT_LIMIT,
            None => self.config.default_text_limit,
        };
        self.snapshot_and_cache(
            app_query,
            SnapshotLimits {
                text,
                nodes: max_nodes.unwrap_or(self.config.default_max_nodes),
                depth: max_depth.unwrap_or(self.config.default_max_depth),
            },
        )
        .await
    }

    async fn perform_generated(
        &self,
        state_id: &str,
        action: GeneratedInputAction,
    ) -> Result<ToolOutput, RuntimeError> {
        let (cached, mapping) = self.required_screenshot(state_id)?;
        let preparation = timeout(self.config.portal_timeout, self.screenshots.prepare())
            .await
            .map_err(|_| operational_error("portal setup timed out before generated input"))?
            .map_err(|error| operational_error(error.to_string()))?;
        if preparation.consent_interrupted_observation {
            self.consume_screenshot_mapping(&cached)?;
            return Err(operational_error(
                "portal consent invalidated the screenshot; call observe and inspect the new image before retrying generated input",
            ));
        }
        self.fresh_for_action(&cached).await?;
        self.require_latest_generation(&cached)?;
        let preparation = timeout(
            self.config.snapshot_timeout,
            self.screenshots.prepare_input(&cached, &mapping, &action),
        )
        .await;
        let cleanup = self.screenshots.cleanup_input().await;
        let preparation = preparation
            .map_err(|_| operational_error("generated input preparation timed out"))?
            .map_err(operational_error);
        if let Err(error) = preparation {
            if let Err(cleanup) = cleanup {
                eprintln!(
                    "open-computer-use: cleanup also failed after input preparation error: {cleanup}"
                );
                return Err(operational_error(format!(
                    "{error}; generated input cleanup also failed and the input session was invalidated: {cleanup}"
                )));
            }
            return Err(error);
        }
        cleanup.map_err(|error| {
            operational_error(format!(
                "generated input preparation cleanup failed: {error}"
            ))
        })?;
        self.fresh_for_action(&cached).await?;
        self.require_latest_generation(&cached)?;
        self.invalidate_current(&cached)?;
        let result = timeout(
            self.config.snapshot_timeout,
            self.screenshots.perform_input(&cached, &mapping, action),
        )
        .await;
        let cleanup = self.screenshots.cleanup_input().await;
        let result = match result {
            Ok(Ok(())) => Ok(()),
            Ok(Err(error)) => Err(uncertain_action(operational_error(error))),
            Err(_) => Err(uncertain_action(timeout_error(
                "generated input action timed out",
            ))),
        };
        if let Err(error) = result {
            if let Err(cleanup) = cleanup {
                eprintln!(
                    "open-computer-use: cleanup also failed after generated input error: {cleanup}"
                );
                return Err(uncertain_action(operational_error(format!(
                    "{error}; generated input cleanup also failed and the input session was invalidated: {cleanup}"
                ))));
            }
            return Err(error);
        }
        cleanup.map_err(|error| {
            completed_without_observation(operational_error(format!(
                "generated input cleanup failed: {error}"
            )))
        })?;
        let refreshed = self
            .settle_and_refresh(cached)
            .await
            .map_err(completed_without_observation)?;
        Ok(self.observe(refreshed).await)
    }

    async fn settle_and_refresh(&self, old: Arc<Snapshot>) -> Result<Arc<Snapshot>, RuntimeError> {
        sleep(self.config.settle_interval).await;
        let future = self.collect_snapshot(old.app_query.clone(), Some(old.app.pid), old.limits);
        let refreshed = timeout(self.config.snapshot_timeout, future)
            .await
            .map_err(|_| operational_error("AT-SPI snapshot timed out after action"))??;
        if refreshed.app.pid != old.app.pid
            || refreshed.app.object != old.app.object
            || refreshed.window.object != old.window.object
        {
            eprintln!("open-computer-use: app or window changed after action");
            return Err(operational_error(
                "app or window changed while settling after the action",
            ));
        }
        self.commit_snapshot(refreshed)
    }

    fn consume_screenshot_mapping(&self, expected: &Snapshot) -> Result<(), RuntimeError> {
        let mut cache = self.lock_cache()?;
        let current = cache.current.as_mut().ok_or_else(|| {
            state_required_error("state cache lost the observation before generated input")
        })?;
        if current.snapshot.generation != expected.generation
            || current.snapshot.app.pid != expected.app.pid
            || current.snapshot.app.object != expected.app.object
            || current.snapshot.window.object != expected.window.object
        {
            return Err(stale_state_error(format!(
                "stale mutation plan: expected generation {}, latest generation {}",
                expected.generation, current.snapshot.generation
            )));
        }
        if current.screenshot_mapping.take().is_none() {
            return Err(state_required_error(
                "generated input requires a fresh observe screenshot",
            ));
        }
        Ok(())
    }

    async fn observe(&self, mut snapshot: Arc<Snapshot>) -> ToolOutput {
        let preparation = match timeout(self.config.portal_timeout, self.screenshots.prepare())
            .await
        {
            Ok(Ok(preparation)) => preparation,
            Ok(Err(error)) => {
                eprintln!(
                    "open-computer-use: screenshot preparation failed for pid={} window={}{} generation={}: {error}",
                    snapshot.app.pid,
                    snapshot.window.object.bus_name,
                    snapshot.window.object.path,
                    snapshot.generation
                );
                return screenshot_unavailable(&snapshot, &error.to_string());
            }
            Err(_) => {
                eprintln!(
                    "open-computer-use: screenshot preparation timed out for pid={} generation={}",
                    snapshot.app.pid, snapshot.generation
                );
                return screenshot_unavailable(&snapshot, "screenshot preparation timed out");
            }
        };
        if preparation.consent_interrupted_observation {
            snapshot = match self
                .snapshot_and_cache(snapshot.app_query.clone(), snapshot.limits)
                .await
            {
                Ok(refreshed) => refreshed,
                Err(error) => {
                    eprintln!(
                        "open-computer-use: AT-SPI refresh after portal consent failed for pid={}: {error}",
                        snapshot.app.pid
                    );
                    return screenshot_unavailable(
                        &snapshot,
                        &format!("AT-SPI refresh after portal consent failed: {error}"),
                    );
                }
            };
        }
        match timeout(
            self.config.snapshot_timeout,
            self.screenshots.capture(&snapshot),
        )
        .await
        {
            Ok(Ok(observation)) => {
                if observation.mapping.app_pid != snapshot.app.pid
                    || observation.mapping.app_identity != snapshot.app.object
                    || observation.mapping.window_identity != snapshot.window.object
                    || observation.mapping.accessibility_generation != snapshot.generation
                {
                    eprintln!(
                        "open-computer-use: screenshot mapping identity invariant failed: snapshot_pid={} mapping_pid={} snapshot_generation={} mapping_generation={}",
                        snapshot.app.pid,
                        observation.mapping.app_pid,
                        snapshot.generation,
                        observation.mapping.accessibility_generation
                    );
                    return screenshot_unavailable(
                        &snapshot,
                        "screenshot mapping identity changed during capture",
                    );
                }
                if let Err(error) = self.revalidate_screenshot_target(&snapshot).await {
                    eprintln!(
                        "open-computer-use: screenshot target changed after frame acquisition for pid={}: {error}",
                        snapshot.app.pid
                    );
                    return screenshot_unavailable(&snapshot, &error.to_string());
                }
                if let Err(error) = self.cache_screenshot(&snapshot, observation.mapping.clone()) {
                    eprintln!(
                        "open-computer-use: screenshot cache update failed for pid={}: {error}",
                        snapshot.app.pid
                    );
                    return screenshot_unavailable(&snapshot, &error.to_string());
                }
                let (width, height) = observation.mapping.output.size;
                observation_output(
                    &snapshot,
                    true,
                    None,
                    Some((width, height)),
                    Some(observation.png_base64),
                )
            }
            Ok(Err(error)) => {
                eprintln!(
                    "open-computer-use: screenshot unavailable for pid={} window={}{} generation={}: {error}",
                    snapshot.app.pid,
                    snapshot.window.object.bus_name,
                    snapshot.window.object.path,
                    snapshot.generation
                );
                screenshot_unavailable(&snapshot, &error.to_string())
            }
            Err(_) => {
                eprintln!(
                    "open-computer-use: screenshot capture timed out for pid={} generation={}",
                    snapshot.app.pid, snapshot.generation
                );
                screenshot_unavailable(&snapshot, "screenshot capture timed out")
            }
        }
    }

    async fn revalidate_screenshot_target(&self, snapshot: &Snapshot) -> Result<(), RuntimeError> {
        let apps = self.discover().await?;
        let matching_apps = apps
            .iter()
            .filter(|app| app.pid == snapshot.app.pid && app.object == snapshot.app.object)
            .collect::<Vec<_>>();
        let [app] = matching_apps.as_slice() else {
            return Err(operational_error(
                "application PID or identity changed after screenshot frame acquisition",
            ));
        };
        let matching_windows = app
            .windows
            .iter()
            .filter(|window| window.object == snapshot.window.object)
            .collect::<Vec<_>>();
        let [_window] = matching_windows.as_slice() else {
            return Err(operational_error(
                "window identity changed after screenshot frame acquisition",
            ));
        };
        Ok(())
    }

    fn cache_screenshot(
        &self,
        snapshot: &Snapshot,
        mapping: ScreenshotMapping,
    ) -> Result<(), RuntimeError> {
        let mut cache = self.lock_cache()?;
        let cached = cache.current.as_mut().ok_or_else(|| {
            state_required_error("state cache lost the observation during screenshot capture")
        })?;
        if cached.snapshot.generation != snapshot.generation
            || cached.snapshot.app.pid != snapshot.app.pid
            || cached.snapshot.app.object != snapshot.app.object
            || cached.snapshot.window.object != snapshot.window.object
        {
            eprintln!(
                "open-computer-use: refusing stale screenshot cache write: cached_generation={} captured_generation={}",
                cached.snapshot.generation, snapshot.generation
            );
            return Err(stale_state_error(
                "state generation changed during screenshot capture",
            ));
        }
        cached.screenshot_mapping = Some(mapping);
        Ok(())
    }

    pub fn screenshot_mapping(
        &self,
        state_id: &str,
    ) -> Result<Option<ScreenshotMapping>, RuntimeError> {
        let cache = self.lock_cache()?;
        Ok(cache.current.as_ref().and_then(|current| {
            (snapshot_state_id(&current.snapshot) == state_id)
                .then(|| current.screenshot_mapping.clone())
                .flatten()
        }))
    }

    fn required_cached(&self, state_id: &str) -> Result<Arc<Snapshot>, RuntimeError> {
        let cache = self.lock_cache()?;
        let current = cache.current.as_ref().ok_or_else(|| {
            state_required_error("no observation is available; call observe first")
        })?;
        if snapshot_state_id(&current.snapshot) != state_id {
            return Err(stale_state_error(format!(
                "state_id {state_id:?} is stale; current state_id is {:?}",
                snapshot_state_id(&current.snapshot)
            )));
        }
        Ok(Arc::clone(&current.snapshot))
    }

    fn required_screenshot(
        &self,
        state_id: &str,
    ) -> Result<(Arc<Snapshot>, ScreenshotMapping), RuntimeError> {
        let cache = self.lock_cache()?;
        let current = cache.current.as_ref().ok_or_else(|| {
            state_required_error("no observation is available; call observe first")
        })?;
        if snapshot_state_id(&current.snapshot) != state_id {
            return Err(stale_state_error(format!(
                "state_id {state_id:?} is stale; current state_id is {:?}",
                snapshot_state_id(&current.snapshot)
            )));
        }
        let mapping = current.screenshot_mapping.clone().ok_or_else(|| {
            state_required_error(
                "the observation has no usable screenshot; call observe and require screenshot.ready=true",
            )
        })?;
        Ok((Arc::clone(&current.snapshot), mapping))
    }

    fn require_latest_generation(&self, expected: &Snapshot) -> Result<(), RuntimeError> {
        let latest = self.required_cached(&snapshot_state_id(expected))?;
        if latest.generation != expected.generation
            || latest.app.pid != expected.app.pid
            || latest.app.object != expected.app.object
            || latest.window.object != expected.window.object
        {
            return Err(stale_state_error(format!(
                "stale mutation plan: expected generation {}, latest generation {}",
                expected.generation, latest.generation
            )));
        }
        Ok(())
    }

    fn invalidate_current(&self, expected: &Snapshot) -> Result<(), RuntimeError> {
        let mut cache = self.lock_cache()?;
        let current = cache.current.as_ref().ok_or_else(|| {
            state_required_error("state cache lost the observation before action dispatch")
        })?;
        if current.snapshot.generation != expected.generation
            || current.snapshot.app.pid != expected.app.pid
            || current.snapshot.app.object != expected.app.object
            || current.snapshot.window.object != expected.window.object
        {
            return Err(stale_state_error(
                "state changed before action dispatch; call observe again",
            ));
        }
        cache.current = None;
        Ok(())
    }

    fn lock_cache(&self) -> Result<std::sync::MutexGuard<'_, Cache>, RuntimeError> {
        self.cache.lock().map_err(|_| {
            eprintln!("open-computer-use: state cache mutex poisoned");
            operational_error("state cache invariant failed")
        })
    }
}

impl<A: AccessibilityAdapter, S: ScreenshotProvider> DesktopRuntime for SemanticRuntime<A, S> {
    fn execute(
        &self,
        call: ToolCall,
    ) -> impl Future<Output = Result<ToolOutput, RuntimeError>> + Send + '_ {
        self.execute_call(call)
    }

    async fn cleanup(&self) -> Result<(), RuntimeError> {
        let _mutation = self.mutation.lock().await;
        self.screenshots
            .cleanup_input()
            .await
            .map_err(operational_error)?;
        Ok(())
    }

    async fn shutdown(&self) -> Result<(), RuntimeError> {
        let _mutation = self.mutation.lock().await;
        self.screenshots
            .shutdown_input()
            .await
            .map_err(operational_error)
    }
}

#[derive(Debug)]
struct Traversal {
    elements: Vec<ElementSnapshot>,
    node_limit_reached: bool,
    depth_limit_reached: bool,
}

#[derive(Debug)]
struct Resolved {
    app: AppInfo,
    window: Option<WindowInfo>,
}

fn resolve_app(
    query: &str,
    expected_pid: Option<u32>,
    apps: &[AppInfo],
) -> Result<Resolved, RuntimeError> {
    if let Some(pid) = expected_pid {
        let matches: Vec<_> = apps.iter().filter(|app| app.pid == pid).collect();
        if matches.is_empty() {
            return Err(operational_error(format!(
                "stale PID {pid}: the app from the cached state is no longer present"
            )));
        }
        return unique_app(query, "cached expected PID", matches);
    }
    if let Ok(pid) = query.parse::<u32>() {
        let matches: Vec<_> = apps.iter().filter(|app| app.pid == pid).collect();
        return unique_app(query, "exact numeric PID", matches);
    }
    let exact_apps: Vec<_> = apps
        .iter()
        .filter(|app| app.name.eq_ignore_ascii_case(query))
        .collect();
    if !exact_apps.is_empty() {
        return unique_app(query, "exact app name", exact_apps);
    }
    let exact_windows: Vec<_> = apps
        .iter()
        .flat_map(|app| app.windows.iter().map(move |window| (app, window)))
        .filter(|(_, window)| window.title.eq_ignore_ascii_case(query))
        .collect();
    if !exact_windows.is_empty() {
        return unique_window(query, "exact window title", exact_windows);
    }
    let query_lower = query.to_lowercase();
    let mut substring_matches = Vec::new();
    for app in apps {
        if app.name.to_lowercase().contains(&query_lower) {
            substring_matches.push((app, None));
            continue;
        }
        for window in &app.windows {
            if window.title.to_lowercase().contains(&query_lower) {
                substring_matches.push((app, Some(window)));
            }
        }
    }
    match substring_matches.as_slice() {
        [(app, window)] => Ok(Resolved {
            app: (*app).clone(),
            window: window.cloned(),
        }),
        [] => Err(operational_error(format!(
            "app not found for query {query:?}"
        ))),
        matches => Err(ambiguous(query, "substring app/window", matches.len())),
    }
}

fn unique_app(query: &str, tier: &str, matches: Vec<&AppInfo>) -> Result<Resolved, RuntimeError> {
    match matches.as_slice() {
        [app] => Ok(Resolved {
            app: (*app).clone(),
            window: None,
        }),
        [] => Err(operational_error(format!(
            "app not found at {tier} tier for query {query:?}"
        ))),
        matches => Err(ambiguous(query, tier, matches.len())),
    }
}

fn unique_window(
    query: &str,
    tier: &str,
    matches: Vec<(&AppInfo, &WindowInfo)>,
) -> Result<Resolved, RuntimeError> {
    match matches.as_slice() {
        [(app, window)] => Ok(Resolved {
            app: (*app).clone(),
            window: Some((*window).clone()),
        }),
        [] => Err(operational_error(format!(
            "window not found at {tier} tier for query {query:?}"
        ))),
        matches => Err(ambiguous(query, tier, matches.len())),
    }
}

fn ambiguous(query: &str, tier: &str, count: usize) -> RuntimeError {
    operational_error(format!(
        "ambiguous app query {query:?} at {tier} tier: {count} matches"
    ))
}

fn choose_window(windows: &[WindowInfo]) -> Result<&WindowInfo, RuntimeError> {
    windows
        .iter()
        .filter(|window| window_is_viable(window))
        .find(|window| window.states.contains("active"))
        .or_else(|| {
            windows
                .iter()
                .filter(|window| window_is_viable(window))
                .find(|window| window.states.contains("showing"))
        })
        .or_else(|| windows.iter().find(|window| window_is_viable(window)))
        .ok_or_else(|| operational_error("application has no viable top-level window"))
}

fn window_is_viable(window: &WindowInfo) -> bool {
    !window.states.contains("defunct") && !window.states.contains("stale")
}

fn normalize_frame(node: &NodeInfo) -> Option<Rect> {
    node.window_frame.filter(|frame| frame.is_valid())
}

fn relocate<'a>(
    old: &ElementSnapshot,
    current: &'a [ElementSnapshot],
) -> Result<&'a ElementSnapshot, RuntimeError> {
    let mut matches = current.iter().filter(|candidate| {
        candidate.node.object == old.node.object && same_role_name(candidate, old)
    });
    let Some(candidate) = matches.next() else {
        return Err(operational_error(
            "element object identity changed; call observe again",
        ));
    };
    if matches.next().is_some() {
        return Err(operational_error("element object identity is ambiguous"));
    }
    usable(candidate)
}

fn same_role_name(candidate: &ElementSnapshot, old: &ElementSnapshot) -> bool {
    candidate.node.role == old.node.role && candidate.node.name == old.node.name
}

fn usable(element: &ElementSnapshot) -> Result<&ElementSnapshot, RuntimeError> {
    if element.node.is_defunct() {
        return Err(operational_error("relocated element is defunct"));
    }
    Ok(element)
}

fn semantic_action(
    action: ElementAction,
    element: &ElementSnapshot,
) -> Result<SemanticAction, RuntimeError> {
    match action {
        ElementAction::Invoke => {
            let actions = element
                .node
                .capabilities
                .inspected_actions()
                .map_err(|()| {
                    operational_error(
                        "AT-SPI action capability inspection failed for the target element",
                    )
                })?;
            match primary_action_index(actions) {
                Some(index) => i32::try_from(index)
                    .map(SemanticAction::InvokeAction)
                    .map_err(|_| operational_error("AT-SPI action index overflow")),
                None => Err(operational_error(
                    "element exposes no recognized primary AT-SPI action",
                )),
            }
        }
        ElementAction::Named(requested) => {
            let actions = element
                .node
                .capabilities
                .inspected_actions()
                .map_err(|()| {
                    operational_error(
                        "AT-SPI action capability inspection failed for the target element",
                    )
                })?;
            let matches: Vec<_> = actions
                .iter()
                .enumerate()
                .filter(|(_, action)| {
                    action.name.eq_ignore_ascii_case(&requested)
                        || action.description.eq_ignore_ascii_case(&requested)
                })
                .collect();
            match matches.as_slice() {
                [(index, _)] => i32::try_from(*index)
                    .map(SemanticAction::InvokeAction)
                    .map_err(|_| operational_error("AT-SPI action index overflow")),
                [] => Err(operational_error(format!(
                    "named action {requested:?} is not exposed by the element"
                ))),
                _ => Err(operational_error(format!(
                    "named action {requested:?} matches more than one action"
                ))),
            }
        }
        ElementAction::Focus => {
            if !element.node.capabilities.supports_focus() {
                return Err(capability_error(
                    "element does not expose a proven AT-SPI Component focus capability",
                ));
            }
            Ok(SemanticAction::GrabFocus)
        }
        ElementAction::SetValue(value) => {
            if !element.node.capabilities.interfaces_inspected() {
                return Err(operational_error(
                    "AT-SPI interface inspection failed for the target element",
                ));
            }
            match element.node.capabilities.set_value_kind() {
                Some(SetValueKind::Text) => Ok(SemanticAction::ReplaceText(value)),
                Some(SetValueKind::Number) => {
                    let numeric = value.parse::<f64>().map_err(|_| {
                        operational_error("AT-SPI Value requires a finite numeric value")
                    })?;
                    if !numeric.is_finite() {
                        return Err(operational_error(
                            "AT-SPI Value requires a finite numeric value",
                        ));
                    }
                    Ok(SemanticAction::SetNumericValue(numeric))
                }
                None => Err(operational_error(
                    "element supports neither EditableText nor Value",
                )),
            }
        }
    }
}

fn primary_action_index(actions: &[ActionInfo]) -> Option<usize> {
    const PREFERRED: [&str; 8] = [
        "click", "press", "activate", "invoke", "select", "toggle", "open", "default",
    ];
    PREFERRED.iter().find_map(|preferred| {
        actions
            .iter()
            .position(|action| action.name.eq_ignore_ascii_case(preferred))
    })
}

pub fn format_snapshot(snapshot: &Snapshot) -> String {
    let mut output = format!(
        "State ID: {}\nApp: {} (PID: {})\nWindow: {}\nElement frames: atspi_window_coordinates\n",
        snapshot_state_id(snapshot),
        escape(&snapshot.app.name),
        snapshot.app.pid,
        escape(&snapshot.window.title)
    );
    let mut focused = None;
    let mut selected = None;
    for (index, element) in snapshot.elements.iter().enumerate() {
        let indent = "\t".repeat(element.depth + 1);
        output.push_str(&format!(
            "{indent}{}: {} name=\"{}\"",
            index,
            escape(&element.node.role),
            escape(&element.node.name)
        ));
        let value = element.node.value.as_ref().or(element.node.text.as_ref());
        if let Some(value) = value {
            output.push_str(&format!(
                " value=\"{}\"",
                escape(&truncate(value, snapshot.limits.text))
            ));
        }
        let capabilities = text_capabilities(element);
        if !capabilities.is_empty() {
            output.push_str(" capabilities=[");
            output.push_str(&capabilities.join(", "));
            output.push(']');
        }
        let actions = element.node.capabilities.actions();
        if !actions.is_empty() {
            output.push_str(" actions=[");
            output.push_str(
                &actions
                    .iter()
                    .map(|action| {
                        if action.description.is_empty() {
                            escape(&action.name)
                        } else {
                            format!("{} ({})", escape(&action.name), escape(&action.description))
                        }
                    })
                    .collect::<Vec<_>>()
                    .join(", "),
            );
            output.push(']');
        }
        if let Some(frame) = element.node.window_frame {
            output.push_str(&format!(
                " frame_atspi_window=({}, {}, {}, {})",
                frame.x, frame.y, frame.width, frame.height
            ));
        }
        output.push('\n');
        if element.node.states.contains("focused") {
            focused = Some(index);
        }
        if selected.is_none() {
            selected = element
                .node
                .selected_text
                .as_ref()
                .filter(|text| !text.is_empty())
                .map(|text| truncate(text, snapshot.limits.text));
        }
    }
    if let Some(index) = focused {
        output.push_str(&format!("Focused element: {index}\n"));
    }
    if let Some(text) = selected {
        output.push_str(&format!("Selected text: \"{}\"\n", escape(&text)));
    }
    if snapshot.node_limit_reached {
        output.push_str("Warning: accessibility tree node limit reached.\n");
    }
    if snapshot.depth_limit_reached {
        output.push_str("Warning: accessibility tree depth limit reached.\n");
    }
    output
}

fn screenshot_unavailable(snapshot: &Snapshot, reason: &str) -> ToolOutput {
    observation_output(snapshot, false, Some(reason), None, None)
}

fn snapshot_state_id(snapshot: &Snapshot) -> String {
    format!("s-{:016x}", snapshot.generation)
}

fn format_running_apps(apps: &[AppInfo]) -> String {
    if apps.is_empty() {
        return EMPTY_APPS_MESSAGE.to_owned();
    }
    apps.iter()
        .map(|app| {
            let windows = app
                .windows
                .iter()
                .map(|window| {
                    format!(
                        "  Window: {} object={}{} states=[{}]",
                        escape(&window.title),
                        window.object.bus_name,
                        window.object.path,
                        window.states.iter().cloned().collect::<Vec<_>>().join(",")
                    )
                })
                .collect::<Vec<_>>()
                .join("\n");
            format!(
                "App: {} PID={} object={}{}\n{}",
                escape(&app.name),
                app.pid,
                app.object.bus_name,
                app.object.path,
                windows
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn running_app_metadata(app: &AppInfo) -> serde_json::Value {
    json!({
        "name": app.name,
        "pid": app.pid,
        "object": object_metadata(&app.object),
        "windows": app.windows.iter().map(|window| json!({
            "title": window.title,
            "object": object_metadata(&window.object),
            "states": window.states.iter().collect::<Vec<_>>()
        })).collect::<Vec<_>>()
    })
}

fn object_metadata(object: &ObjectId) -> serde_json::Value {
    json!({"bus_name": object.bus_name, "path": object.path})
}

fn text_capabilities(element: &ElementSnapshot) -> Vec<String> {
    let mut capabilities = Vec::new();
    if element
        .node
        .capabilities
        .inspected_actions()
        .is_ok_and(|actions| primary_action_index(actions).is_some())
    {
        capabilities.push("invoke".into());
    }
    if element.node.capabilities.supports_focus() {
        capabilities.push("focus".into());
    }
    match element.node.capabilities.set_value_kind() {
        Some(SetValueKind::Text) => capabilities.push("set_value:text".into()),
        Some(SetValueKind::Number) => capabilities.push("set_value:number".into()),
        None => {}
    }
    capabilities
}

fn element_capabilities(index: usize, element: &ElementSnapshot) -> serde_json::Value {
    let inspection_complete = element.node.capabilities.inspection_complete();
    let set_value = match element.node.capabilities.set_value_kind() {
        Some(SetValueKind::Text) => Some("text"),
        Some(SetValueKind::Number) => Some("number"),
        None => None,
    };
    let actions = element.node.capabilities.actions();
    json!({
        "element_id": index.to_string(),
        "inspection_complete": inspection_complete,
        "invoke": inspection_complete && primary_action_index(actions).is_some(),
        "focus": element.node.capabilities.supports_focus(),
        "named_actions": actions.iter().map(|action| json!({
            "name": action.name,
            "description": action.description
        })).collect::<Vec<_>>(),
        "set_value": set_value
    })
}

fn observation_output(
    snapshot: &Snapshot,
    screenshot_ready: bool,
    screenshot_reason: Option<&str>,
    dimensions: Option<(u32, u32)>,
    png_base64: Option<String>,
) -> ToolOutput {
    let screenshot = match dimensions {
        Some((width, height)) => json!({
            "ready": screenshot_ready,
            "reason": screenshot_reason,
            "width": width,
            "height": height,
            "coordinate_space": "screenshot_png_pixels"
        }),
        None => json!({
            "ready": screenshot_ready,
            "reason": screenshot_reason,
            "width": null,
            "height": null,
            "coordinate_space": "screenshot_png_pixels"
        }),
    };
    let structured = json!({
        "state_id": snapshot_state_id(snapshot),
        "target": {
            "query": snapshot.app_query,
            "app": {
                "name": snapshot.app.name,
                "pid": snapshot.app.pid,
                "object": object_metadata(&snapshot.app.object)
            },
            "window": {
                "title": snapshot.window.title,
                "object": object_metadata(&snapshot.window.object)
            }
        },
        "screenshot": screenshot,
        "coordinate_spaces": {
            "screenshot": "screenshot_png_pixels",
            "element_frames": "atspi_window_coordinates"
        },
        "elements": snapshot.elements.iter().enumerate().map(|(index, element)| {
            element_capabilities(index, element)
        }).collect::<Vec<_>>()
    });
    let mut text = format_snapshot(snapshot);
    if !screenshot_ready {
        text.push_str(&format!(
            "Screenshot unavailable: {}",
            screenshot_reason.unwrap_or("unknown reason")
        ));
    }
    let mut output = ToolOutput::text(text).with_structured_content(structured);
    if let Some(png) = png_base64 {
        output = output.with_png_base64(png);
    }
    output
}

fn escape(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('\r', "\\r")
        .replace('\n', "\\n")
        .replace('"', "\\\"")
}

fn truncate(value: &str, limit: usize) -> String {
    if limit == usize::MAX {
        return value.to_owned();
    }
    let mut chars = value.chars();
    let prefix: String = chars.by_ref().take(limit).collect();
    if chars.next().is_some() {
        format!("{prefix}…")
    } else {
        prefix
    }
}

fn operational_error(message: impl Into<String>) -> RuntimeError {
    RuntimeError::not_started("target_unavailable", message)
}

fn state_required_error(message: impl Into<String>) -> RuntimeError {
    RuntimeError::not_started("state_required", message)
}

fn stale_state_error(message: impl Into<String>) -> RuntimeError {
    RuntimeError::not_started("stale_state", message)
}

fn capability_error(message: impl Into<String>) -> RuntimeError {
    RuntimeError::not_started("capability_unavailable", message)
}

fn timeout_error(message: impl Into<String>) -> RuntimeError {
    RuntimeError::new(
        "backend_timeout",
        message,
        ToolOutcome::NotStarted,
        true,
        "Call observe for current state before retrying.",
    )
}

fn launch_in_progress_error() -> RuntimeError {
    RuntimeError::new(
        "backend_failed",
        "desktop application launch is still in progress",
        ToolOutcome::NotStarted,
        true,
        "Wait for launch completion, then call observe before retrying.",
    )
}

fn internal_error(message: impl Into<String>) -> RuntimeError {
    RuntimeError::new(
        "internal",
        message,
        ToolOutcome::NotStarted,
        false,
        "Restart the server before issuing more computer-use actions.",
    )
}

fn uncertain_action(error: RuntimeError) -> RuntimeError {
    error.with_execution_status(
        ToolOutcome::Unknown,
        false,
        "Call observe and inspect the current state before deciding whether to retry.",
    )
}

fn completed_without_observation(error: RuntimeError) -> RuntimeError {
    error.with_execution_status(
        ToolOutcome::Completed,
        false,
        "The action completed, but refresh failed. Call observe and do not repeat the action blindly.",
    )
}

fn cached_element<'a>(
    snapshot: &'a Snapshot,
    index: &str,
) -> Result<&'a ElementSnapshot, RuntimeError> {
    let parsed = index
        .parse::<usize>()
        .map_err(|_| operational_error(format!("element_id {index:?} is not a snapshot index")))?;
    let element = snapshot.elements.get(parsed).ok_or_else(|| {
        operational_error(format!(
            "element_id {parsed} is not in generation {}",
            snapshot.generation
        ))
    })?;
    Ok(element)
}

#[cfg(test)]
mod tests {
    use std::{collections::HashMap, future, sync::Arc};

    use super::*;
    use crate::{
        screenshot::ScreenshotError,
        validation::{KeyboardAction, MouseButton, PointerAction},
    };

    #[test]
    fn all_resolution_tiers_and_ambiguities_are_strict() {
        let apps = apps();
        assert_eq!(resolve_app("20", None, &apps).unwrap().app.pid, 20);
        assert_eq!(resolve_app("EDITOR", None, &apps).unwrap().app.pid, 10);
        assert_eq!(
            resolve_app("Preferences", None, &apps)
                .unwrap()
                .window
                .unwrap()
                .title,
            "Preferences"
        );
        assert_eq!(resolve_app("term", None, &apps).unwrap().app.pid, 20);
        assert_eq!(
            resolve_app("anything", Some(10), &apps).unwrap().app.pid,
            10
        );
        assert!(
            resolve_app("missing", None, &apps)
                .unwrap_err()
                .message
                .contains("not found")
        );

        let mut ambiguous = apps.clone();
        ambiguous.push(app("editor", 30, "Other"));
        assert!(
            resolve_app("editor", None, &ambiguous)
                .unwrap_err()
                .message
                .contains("ambiguous")
        );
        ambiguous[2].name = "Other".into();
        ambiguous[2].windows[0].title = "Preferences".into();
        assert!(
            resolve_app("Preferences", None, &ambiguous)
                .unwrap_err()
                .message
                .contains("ambiguous")
        );
        assert!(
            resolve_app("e", None, &apps)
                .unwrap_err()
                .message
                .contains("ambiguous")
        );
        assert!(
            resolve_app("x", Some(999), &apps)
                .unwrap_err()
                .message
                .contains("stale PID")
        );
    }

    #[test]
    fn window_choice_prefers_active_then_showing_then_first_viable() {
        let mut windows = vec![
            window("first", &[]),
            window("shown", &["showing"]),
            window("active", &["active"]),
        ];
        assert_eq!(choose_window(&windows).unwrap().title, "active");
        windows[2].states.clear();
        assert_eq!(choose_window(&windows).unwrap().title, "shown");
        windows[1].states.clear();
        assert_eq!(choose_window(&windows).unwrap().title, "first");
        for window in &mut windows {
            window.states.insert("defunct".into());
        }
        assert!(choose_window(&windows).is_err());
    }

    #[test]
    fn formatting_escapes_truncates_and_reports_focus_selection_and_limits() {
        let mut node = node("root", "button", "line\r\n\"name");
        node.text = Some("é🙂tail".into());
        node.selected_text = Some("a\nb".into());
        node.states.insert("focused".into());
        node.window_frame = Some(Rect {
            x: 1,
            y: 2,
            width: 3,
            height: 4,
        });
        node.capabilities = inspected(ActionCapabilities::Inspected(vec![
            ActionInfo {
                name: "click".into(),
                description: String::new(),
            },
            ActionInfo {
                name: "menu".into(),
                description: "Show\nmenu".into(),
            },
        ]));
        let snapshot = Snapshot {
            app_query: "Editor".into(),
            app: app("Editor", 1, "Main"),
            window: window("Main", &["active"]),
            generation: 2,
            elements: vec![ElementSnapshot { depth: 0, node }],
            node_limit_reached: true,
            depth_limit_reached: true,
            limits: SnapshotLimits {
                text: 2,
                nodes: 10,
                depth: 10,
            },
        };
        let text = format_snapshot(&snapshot);
        assert!(text.contains("line\\r\\n\\\"name"));
        assert!(text.contains("value=\"é🙂…\""));
        assert!(text.contains("actions=[click, menu (Show\\nmenu)]"));
        assert!(text.contains("Focused element: 0"));
        assert!(text.contains("Selected text: \"a\\n…\""));
        assert!(text.contains("node limit"));
        assert!(text.contains("depth limit"));
        assert!(!text.contains("Screenshot unavailable"));
    }

    #[test]
    fn click_uses_only_a_recognized_primary_action() {
        let mut target = element(node("button", "button", "Menu"), 1, None);
        target.node.capabilities = inspected(ActionCapabilities::Inspected(vec![
            ActionInfo {
                name: "show menu".into(),
                description: String::new(),
            },
            ActionInfo {
                name: "activate".into(),
                description: String::new(),
            },
        ]));
        assert_eq!(
            semantic_action(ElementAction::Invoke, &target).unwrap(),
            SemanticAction::InvokeAction(1)
        );
        target.node.capabilities = inspected(ActionCapabilities::Inspected(vec![]));
        assert!(semantic_action(ElementAction::Invoke, &target).is_err());
        target.node.capabilities = NodeCapabilities::Inspected(InspectedCapabilities {
            actions: ActionCapabilities::InspectionFailed,
            component: true,
            editable_text: false,
            value: false,
        });
        assert_eq!(
            semantic_action(ElementAction::Focus, &target).unwrap(),
            SemanticAction::GrabFocus
        );
        assert!(semantic_action(ElementAction::Invoke, &target).is_err());
        target.node.capabilities = NodeCapabilities::InspectionFailed;
        assert!(semantic_action(ElementAction::Focus, &target).is_err());
    }

    #[test]
    fn relocation_requires_exact_object_identity() {
        let old = element(
            node("old", "button", "Save"),
            1,
            Some(Rect {
                x: 1,
                y: 1,
                width: 10,
                height: 10,
            }),
        );
        let replaced = element(node("new", "button", "Save"), 1, old.node.window_frame);
        assert!(
            relocate(&old, &[replaced])
                .unwrap_err()
                .message
                .contains("identity changed")
        );
        let mut defunct = old.clone();
        defunct.node.states.insert("defunct".into());
        assert!(
            relocate(&old, &[defunct])
                .unwrap_err()
                .message
                .contains("defunct")
        );
    }

    #[tokio::test]
    async fn traversal_is_depth_first_deterministic_and_bounded() {
        let fake = FakeAdapter::tree();
        {
            let mut state = fake.state.lock().unwrap();
            let label = node("button-label", "label", "Button label");
            state.nodes.insert(label.object.clone(), label);
            state
                .nodes
                .get_mut(&id("button"))
                .unwrap()
                .children
                .push(id("button-label"));
        }
        let runtime = fake_runtime(fake.clone());
        runtime
            .snapshot_text("Editor".into(), None, Some(3), Some(1))
            .await
            .unwrap();
        let snapshot = current_snapshot(&runtime);
        assert_eq!(
            snapshot
                .elements
                .iter()
                .map(|element| element.node.name.as_str())
                .collect::<Vec<_>>(),
            ["Main", "Button", "Editor"]
        );
        assert_eq!(snapshot.elements[1].depth, 1);
        assert_eq!(snapshot.elements[2].depth, 1);
        assert!(snapshot.node_limit_reached);
        assert!(snapshot.depth_limit_reached);
        assert_eq!(snapshot.elements[1].node.window_frame.unwrap().x, 10);
    }

    #[tokio::test]
    async fn adapter_calls_are_timed_out() {
        let fake = FakeAdapter::tree();
        fake.state.lock().unwrap().block_reads = true;
        let mut config = test_config();
        config.call_timeout = Duration::from_millis(5);
        config.snapshot_timeout = Duration::from_millis(20);
        let runtime = SemanticRuntime::with_config(fake, config);
        let error = runtime
            .snapshot_text("Editor".into(), None, None, None)
            .await
            .unwrap_err();
        assert!(error.message.contains("timed out"), "{error}");
    }

    #[tokio::test]
    async fn inconsistent_optional_interface_metadata_does_not_discard_sound_nodes() {
        let fake = FakeAdapter::tree();
        let runtime = fake_runtime(fake);
        let text = runtime
            .snapshot_text("Editor".into(), None, None, None)
            .await
            .unwrap();
        let snapshot = current_snapshot(&runtime);
        let slider = snapshot
            .elements
            .iter()
            .find(|element| element.node.name == "Zoom")
            .unwrap();
        assert_eq!(
            slider.node.capabilities.set_value_kind(),
            Some(SetValueKind::Number)
        );
        assert!(slider.node.value.is_none());
        assert!(text.contains("name=\"Zoom\""));
        assert!(text.contains("Screenshot unavailable"));
    }

    #[tokio::test]
    async fn stale_and_defunct_children_are_skipped_with_stable_included_indexes() {
        let fake = FakeAdapter::tree();
        {
            let mut state = fake.state.lock().unwrap();
            state
                .nodes
                .get_mut(&id("root"))
                .unwrap()
                .children
                .insert(1, id("vanished"));
            let mut defunct = node("defunct", "label", "Gone");
            defunct.states.insert("defunct".into());
            state.nodes.insert(defunct.object.clone(), defunct);
            state
                .nodes
                .get_mut(&id("root"))
                .unwrap()
                .children
                .insert(2, id("defunct"));
        }
        let runtime = fake_runtime(fake);
        runtime
            .snapshot_text("Editor".into(), None, None, None)
            .await
            .unwrap();
        let snapshot = current_snapshot(&runtime);
        assert_eq!(
            snapshot
                .elements
                .iter()
                .enumerate()
                .map(|(index, element)| (index, element.node.name.as_str()))
                .collect::<Vec<_>>(),
            [(0, "Main"), (1, "Button"), (2, "Editor"), (3, "Zoom")]
        );
        assert_eq!(snapshot.elements[2].depth, 1);
        assert!(!snapshot.node_limit_reached);
        assert!(!snapshot.depth_limit_reached);
    }

    #[tokio::test]
    async fn every_semantic_action_uses_the_adapter_and_returns_fresh_state() {
        let fake = FakeAdapter::tree();
        let runtime = fake_runtime(fake.clone());
        runtime
            .snapshot_text("Editor".into(), None, None, None)
            .await
            .unwrap();
        let initial_generation = current_snapshot(&runtime).generation;

        let mut outputs = Vec::new();
        let mut previous_state_id = current_state_id(&runtime);
        for (element_id, action) in [
            ("1", ElementAction::Invoke),
            ("1", ElementAction::Named("SHOW MENU".into())),
            ("2", ElementAction::SetValue("  λ\n".into())),
            ("3", ElementAction::SetValue("42.5".into())),
        ] {
            let output = runtime
                .execute_call(ToolCall::ActOnElement {
                    state_id: previous_state_id.clone(),
                    element_id: element_id.into(),
                    action,
                })
                .await
                .unwrap();
            let next_state_id = output.structured_content.as_ref().unwrap()["state_id"]
                .as_str()
                .unwrap()
                .to_owned();
            assert_ne!(next_state_id, previous_state_id);
            previous_state_id = next_state_id;
            outputs.push(output);
        }
        assert!(
            outputs
                .iter()
                .all(|output| output.text.contains("Screenshot unavailable"))
        );
        assert_eq!(
            fake.state.lock().unwrap().actions,
            [
                (id("button"), SemanticAction::InvokeAction(1)),
                (id("button"), SemanticAction::InvokeAction(2)),
                (id("edit"), SemanticAction::ReplaceText("  λ\n".into())),
                (id("slider"), SemanticAction::SetNumericValue(42.5)),
            ]
        );
        let final_state = current_snapshot(&runtime);
        assert_eq!(final_state.generation, initial_generation + 4);
        assert!(fake.state.lock().unwrap().discoveries >= 9);
    }

    #[tokio::test]
    async fn semantic_failure_invalidates_state_before_dispatch() {
        let fake = FakeAdapter::tree();
        let runtime = fake_runtime(fake.clone());
        runtime
            .snapshot_text("Editor".into(), None, None, None)
            .await
            .unwrap();
        let state_id = current_state_id(&runtime);
        fake.state.lock().unwrap().fail_actions = true;

        let error = runtime
            .execute_call(ToolCall::ActOnElement {
                state_id: state_id.clone(),
                element_id: "1".into(),
                action: ElementAction::Invoke,
            })
            .await
            .unwrap_err();
        assert_eq!(error.outcome, ToolOutcome::Unknown);
        assert_eq!(fake.state.lock().unwrap().actions.len(), 1);

        let retry = runtime
            .execute_call(ToolCall::ActOnElement {
                state_id,
                element_id: "1".into(),
                action: ElementAction::Invoke,
            })
            .await
            .unwrap_err();
        assert_eq!(retry.code, "state_required");
        assert_eq!(fake.state.lock().unwrap().actions.len(), 1);
    }

    #[tokio::test]
    async fn generated_actions_require_prior_state() {
        let fake = FakeAdapter::tree();
        let runtime = fake_runtime(fake.clone());
        let error = runtime
            .execute_call(ToolCall::Keyboard {
                state_id: "s-0000000000000001".into(),
                focus: (1.0, 2.0),
                action: KeyboardAction::Type("x".into()),
            })
            .await
            .unwrap_err();
        assert_eq!(error.code, "state_required");
        assert!(error.message.contains("no observation"));

        for call in [
            ToolCall::Pointer {
                state_id: "s-0000000000000001".into(),
                action: PointerAction::Move { x: 1.0, y: 2.0 },
            },
            ToolCall::Pointer {
                state_id: "s-0000000000000001".into(),
                action: PointerAction::Click {
                    x: 1.0,
                    y: 2.0,
                    button: MouseButton::Left,
                    count: 1,
                },
            },
            ToolCall::Pointer {
                state_id: "s-0000000000000001".into(),
                action: PointerAction::Drag {
                    from: (0.0, 0.0),
                    to: (1.0, 1.0),
                },
            },
            ToolCall::Keyboard {
                state_id: "s-0000000000000001".into(),
                focus: (1.0, 2.0),
                action: KeyboardAction::Press("A".into()),
            },
            ToolCall::Pointer {
                state_id: "s-0000000000000001".into(),
                action: PointerAction::Scroll {
                    x: 1.0,
                    y: 2.0,
                    delta_x: 0,
                    delta_y: 120,
                },
            },
        ] {
            assert_eq!(
                runtime.execute_call(call).await.unwrap_err().code,
                "state_required"
            );
        }
    }

    #[tokio::test]
    async fn launch_in_progress_rejects_calls_without_backend_work() {
        let fake = FakeAdapter::tree();
        let runtime = fake_runtime(fake.clone());
        runtime.launch_in_progress.store(true, Ordering::Release);

        let error = runtime
            .execute_call(ToolCall::Observe {
                target: "Editor".into(),
                text_limit: None,
                max_tree_nodes: None,
                max_tree_depth: None,
            })
            .await
            .unwrap_err();
        assert_eq!(error.code, "backend_failed");
        assert_eq!(error.outcome, ToolOutcome::NotStarted);
        assert_eq!(fake.state.lock().unwrap().discoveries, 0);
        assert!(fake.state.lock().unwrap().actions.is_empty());
    }

    #[tokio::test]
    async fn coordinate_scroll_requires_a_live_screenshot_after_state() {
        let runtime = fake_runtime(FakeAdapter::tree());
        runtime
            .snapshot_text("Editor".into(), None, None, None)
            .await
            .unwrap();
        let error = runtime
            .execute_call(ToolCall::Pointer {
                state_id: current_state_id(&runtime),
                action: PointerAction::Scroll {
                    x: 1.0,
                    y: 2.0,
                    delta_x: 0,
                    delta_y: 120,
                },
            })
            .await
            .unwrap_err();
        assert_eq!(error.code, "state_required");
        assert!(error.message.contains("no usable screenshot"));
    }

    #[tokio::test]
    async fn actions_reject_a_stale_state_id() {
        let runtime = fake_runtime(FakeAdapter::tree());
        runtime
            .execute_call(ToolCall::Observe {
                target: "Editor".into(),
                text_limit: None,
                max_tree_nodes: None,
                max_tree_depth: None,
            })
            .await
            .unwrap();
        let stale_state_id = current_state_id(&runtime);
        runtime
            .execute_call(ToolCall::Observe {
                target: "Editor".into(),
                text_limit: None,
                max_tree_nodes: None,
                max_tree_depth: None,
            })
            .await
            .unwrap();

        let error = runtime
            .execute_call(ToolCall::ActOnElement {
                state_id: stale_state_id,
                element_id: "1".into(),
                action: ElementAction::Invoke,
            })
            .await
            .unwrap_err();
        assert_eq!(error.code, "stale_state");
        assert!(error.message.contains("is stale"));
    }

    #[tokio::test]
    async fn actions_reject_stale_pid_changed_window_and_defunct_target() {
        let fake = FakeAdapter::tree();
        let runtime = fake_runtime(fake.clone());
        runtime
            .snapshot_text("Editor".into(), None, None, None)
            .await
            .unwrap();
        fake.state.lock().unwrap().app.pid = 999;
        let error = click(&runtime).await.unwrap_err();
        assert_eq!(error.code, "target_unavailable");
        assert!(error.message.contains("stale PID"));

        let fake = FakeAdapter::tree();
        let runtime = fake_runtime(fake.clone());
        runtime
            .snapshot_text("Editor".into(), None, None, None)
            .await
            .unwrap();
        {
            let mut state = fake.state.lock().unwrap();
            let mut other = state.nodes[&id("root")].clone();
            other.object = id("other-window");
            state.nodes.insert(other.object.clone(), other);
            state.app.windows[0].object = id("other-window");
        }
        let error = click(&runtime).await.unwrap_err();
        assert_eq!(error.code, "target_unavailable");
        assert!(error.message.contains("window identity changed"));

        let fake = FakeAdapter::tree();
        let runtime = fake_runtime(fake.clone());
        runtime
            .snapshot_text("Editor".into(), None, None, None)
            .await
            .unwrap();
        fake.state
            .lock()
            .unwrap()
            .nodes
            .get_mut(&id("button"))
            .unwrap()
            .states
            .insert("defunct".into());
        let error = click(&runtime).await.unwrap_err();
        assert_eq!(error.code, "target_unavailable");
        assert!(error.message.contains("defunct"));
    }

    #[tokio::test]
    async fn generated_input_failure_invalidates_state_before_dispatch() {
        use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

        use crate::screenshot::{PrepareCapture, ScreenshotObservation};

        struct FakeScreenshots {
            prepared: AtomicBool,
            generated: AtomicUsize,
        }

        impl ScreenshotProvider for FakeScreenshots {
            fn prepare(
                &self,
            ) -> impl Future<Output = Result<PrepareCapture, ScreenshotError>> + Send + '_
            {
                let interrupted = !self.prepared.swap(true, Ordering::AcqRel);
                async move {
                    Ok(PrepareCapture {
                        consent_interrupted_observation: interrupted,
                    })
                }
            }

            async fn capture<'a>(
                &'a self,
                snapshot: &'a Snapshot,
            ) -> Result<ScreenshotObservation, ScreenshotError> {
                Ok(ScreenshotObservation {
                    png_base64: "cG5n".into(),
                    mapping: test_screenshot_mapping(snapshot, "/session/test", Some("mapping")),
                })
            }

            fn perform_input<'a>(
                &'a self,
                _snapshot: &'a Snapshot,
                _mapping: &'a ScreenshotMapping,
                _action: crate::input::GeneratedInputAction,
            ) -> impl Future<Output = Result<(), String>> + Send + 'a {
                self.generated.fetch_add(1, Ordering::AcqRel);
                async { Err("fake does not send generated input".into()) }
            }
        }

        let fake = FakeAdapter::tree();
        let runtime = SemanticRuntime::with_screenshot_provider(
            fake.clone(),
            FakeScreenshots {
                prepared: AtomicBool::new(false),
                generated: AtomicUsize::new(0),
            },
            test_config(),
        );
        let output = runtime
            .execute_call(ToolCall::Observe {
                target: "Editor".into(),
                text_limit: None,
                max_tree_nodes: None,
                max_tree_depth: None,
            })
            .await
            .unwrap();
        assert_eq!(output.png_base64.as_deref(), Some("cG5n"));
        assert!(!output.text.contains("Screenshot unavailable"));
        let state_id = current_state_id(&runtime);
        let metadata = output.structured_content.as_ref().unwrap();
        assert_eq!(metadata["state_id"], state_id);
        assert_eq!(metadata["screenshot"]["ready"], true);
        assert_eq!(metadata["elements"][1]["invoke"], true);
        assert_eq!(metadata["elements"][2]["set_value"], "text");
        assert_eq!(metadata["elements"][3]["set_value"], "number");
        let mapping = runtime.screenshot_mapping(&state_id).unwrap().unwrap();
        assert_eq!(mapping.stream.mapping_id.as_deref(), Some("mapping"));
        assert_eq!(mapping.accessibility_generation, 2);
        assert!(fake.state.lock().unwrap().discoveries >= 2);

        let error = runtime
            .execute_call(ToolCall::Pointer {
                state_id: state_id.clone(),
                action: PointerAction::Move { x: 10.0, y: 10.0 },
            })
            .await
            .unwrap_err();
        assert_eq!(error.code, "target_unavailable");
        assert_eq!(error.outcome, ToolOutcome::Unknown);
        assert!(error.message.contains("fake does not send generated input"));
        assert!(runtime.screenshot_mapping(&state_id).unwrap().is_none());
        assert_eq!(runtime.screenshots.generated.load(Ordering::Acquire), 1);

        let retry = runtime
            .execute_call(ToolCall::Pointer {
                state_id,
                action: PointerAction::Move { x: 10.0, y: 10.0 },
            })
            .await
            .unwrap_err();
        assert_eq!(retry.code, "state_required");
        assert_eq!(runtime.screenshots.generated.load(Ordering::Acquire), 1);
    }

    #[tokio::test]
    async fn generated_mutation_serializes_state_refresh() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        use crate::{
            input::GeneratedInputAction,
            screenshot::{PrepareCapture, ScreenshotObservation},
        };

        struct ConcurrentScreenshots {
            prepares: AtomicUsize,
            entered: tokio::sync::Notify,
            release: tokio::sync::Notify,
            generated: AtomicUsize,
        }

        impl ScreenshotProvider for ConcurrentScreenshots {
            fn prepare(
                &self,
            ) -> impl Future<Output = Result<PrepareCapture, ScreenshotError>> + Send + '_
            {
                let call = self.prepares.fetch_add(1, Ordering::AcqRel);
                async move {
                    if call == 1 {
                        self.entered.notify_one();
                        self.release.notified().await;
                    }
                    Ok(PrepareCapture {
                        consent_interrupted_observation: false,
                    })
                }
            }

            async fn capture<'a>(
                &'a self,
                snapshot: &'a Snapshot,
            ) -> Result<ScreenshotObservation, ScreenshotError> {
                Ok(ScreenshotObservation {
                    png_base64: "cG5n".into(),
                    mapping: test_screenshot_mapping(snapshot, "/session/concurrent", None),
                })
            }

            fn perform_input<'a>(
                &'a self,
                _snapshot: &'a Snapshot,
                _mapping: &'a ScreenshotMapping,
                _action: GeneratedInputAction,
            ) -> impl Future<Output = Result<(), String>> + Send + 'a {
                self.generated.fetch_add(1, Ordering::AcqRel);
                async { Ok(()) }
            }
        }

        let runtime = Arc::new(SemanticRuntime::with_screenshot_provider(
            FakeAdapter::tree(),
            ConcurrentScreenshots {
                prepares: AtomicUsize::new(0),
                entered: tokio::sync::Notify::new(),
                release: tokio::sync::Notify::new(),
                generated: AtomicUsize::new(0),
            },
            test_config(),
        ));
        runtime
            .execute_call(ToolCall::Observe {
                target: "Editor".into(),
                text_limit: None,
                max_tree_nodes: None,
                max_tree_depth: None,
            })
            .await
            .unwrap();
        let initial_state_id = current_state_id(&runtime);
        let mutation_runtime = Arc::clone(&runtime);
        let mutation_state_id = initial_state_id.clone();
        let mutation = tokio::spawn(async move {
            mutation_runtime
                .execute_call(ToolCall::Pointer {
                    state_id: mutation_state_id,
                    action: PointerAction::Click {
                        x: 10.0,
                        y: 10.0,
                        button: MouseButton::Left,
                        count: 1,
                    },
                })
                .await
        });
        runtime.screenshots.entered.notified().await;
        let refresh_runtime = Arc::clone(&runtime);
        let refresh = tokio::spawn(async move {
            refresh_runtime
                .snapshot_text("Editor".into(), None, None, None)
                .await
        });
        tokio::task::yield_now().await;
        assert!(!refresh.is_finished());
        runtime.screenshots.release.notify_one();

        let output = mutation.await.unwrap().unwrap();
        refresh.await.unwrap().unwrap();
        assert_eq!(runtime.screenshots.generated.load(Ordering::Acquire), 1);
        let returned_state_id = output.structured_content.as_ref().unwrap()["state_id"]
            .as_str()
            .unwrap();
        assert_ne!(returned_state_id, initial_state_id);
        assert_ne!(current_state_id(&runtime), initial_state_id);
    }

    #[tokio::test]
    async fn screenshot_revalidation_rejects_window_identity_changes() {
        let fake = FakeAdapter::tree();
        let runtime = SemanticRuntime::with_screenshot_provider(
            fake.clone(),
            MutatingScreenshots {
                state: Arc::clone(&fake.state),
            },
            test_config(),
        );
        let output = runtime
            .execute_call(ToolCall::Observe {
                target: "Editor".into(),
                text_limit: None,
                max_tree_nodes: None,
                max_tree_depth: None,
            })
            .await
            .unwrap();
        assert!(output.png_base64.is_none());
        assert!(
            output.text.contains("Screenshot unavailable:"),
            "{}",
            output.text
        );
        assert!(
            runtime
                .screenshot_mapping(&current_state_id(&runtime))
                .unwrap()
                .is_none()
        );
    }

    async fn click(runtime: &SemanticRuntime<FakeAdapter>) -> Result<ToolOutput, RuntimeError> {
        runtime
            .execute_call(ToolCall::ActOnElement {
                state_id: current_state_id(runtime),
                element_id: "1".into(),
                action: ElementAction::Invoke,
            })
            .await
    }

    fn test_screenshot_mapping(
        snapshot: &Snapshot,
        session: &str,
        mapping_id: Option<&str>,
    ) -> ScreenshotMapping {
        let crop = crate::geometry::PixelRect {
            x: 0,
            y: 0,
            width: 800,
            height: 600,
        };
        ScreenshotMapping {
            app_pid: snapshot.app.pid,
            app_identity: snapshot.app.object.clone(),
            window_identity: snapshot.window.object.clone(),
            accessibility_generation: snapshot.generation,
            portal_session_identity: session.into(),
            portal_session_generation: 1,
            remote_desktop_devices: crate::portal::GrantedDevices::from_mask_for_mapping(3),
            stream: crate::portal::PortalStream {
                stream_index: 0,
                node_id: 1,
                pipewire_serial: Some(2),
                id: Some("stream".into()),
                mapping_id: mapping_id.map(str::to_owned),
                position: Some((0, 0)),
                logical_size: Some((800, 600)),
            },
            source: crate::capture::FrameMetadata {
                generation: snapshot.generation,
                format_generation: 1,
                size: (800, 600),
                crop,
                transform: crate::geometry::Transform::Normal,
            },
            monitor: crate::geometry::MonitorMapping {
                transformed_crop: crop,
                scale_x: 1.0,
                scale_y: 1.0,
            },
            output: crate::encoder::PngMapping {
                size: (800, 600),
                png_to_transformed_x: 1.0,
                png_to_transformed_y: 1.0,
            },
        }
    }

    fn current_snapshot<A, S>(runtime: &SemanticRuntime<A, S>) -> Arc<Snapshot>
    where
        A: AccessibilityAdapter,
        S: ScreenshotProvider,
    {
        Arc::clone(
            &runtime
                .lock_cache()
                .unwrap()
                .current
                .as_ref()
                .unwrap()
                .snapshot,
        )
    }

    fn current_state_id<A, S>(runtime: &SemanticRuntime<A, S>) -> String
    where
        A: AccessibilityAdapter,
        S: ScreenshotProvider,
    {
        snapshot_state_id(&current_snapshot(runtime))
    }

    #[derive(Clone)]
    struct FakeAdapter {
        state: Arc<Mutex<FakeState>>,
    }

    struct FakeState {
        app: AppInfo,
        nodes: HashMap<ObjectId, NodeInfo>,
        actions: Vec<(ObjectId, SemanticAction)>,
        discoveries: usize,
        block_reads: bool,
        fail_actions: bool,
    }

    struct MutatingScreenshots {
        state: Arc<Mutex<FakeState>>,
    }

    impl MutatingScreenshots {
        fn mutate(&self) {
            let mut state = self.state.lock().unwrap();
            state.app.windows[0].object = id("replacement-window");
        }
    }

    impl ScreenshotProvider for MutatingScreenshots {
        async fn prepare(&self) -> Result<crate::screenshot::PrepareCapture, ScreenshotError> {
            Ok(crate::screenshot::PrepareCapture {
                consent_interrupted_observation: false,
            })
        }

        async fn capture<'a>(
            &'a self,
            snapshot: &'a Snapshot,
        ) -> Result<crate::screenshot::ScreenshotObservation, ScreenshotError> {
            self.mutate();
            Ok(crate::screenshot::ScreenshotObservation {
                png_base64: "cG5n".into(),
                mapping: test_screenshot_mapping(snapshot, "/session/revalidation", None),
            })
        }

        async fn perform_input<'a>(
            &'a self,
            _snapshot: &'a Snapshot,
            _mapping: &'a ScreenshotMapping,
            _action: crate::input::GeneratedInputAction,
        ) -> Result<(), String> {
            Ok(())
        }
    }

    impl FakeAdapter {
        fn tree() -> Self {
            let mut root = node("root", "frame", "Main");
            root.window_frame = Some(Rect {
                x: 0,
                y: 0,
                width: 800,
                height: 600,
            });
            root.children = vec![id("button"), id("edit"), id("slider")];

            let mut button = node("button", "button", "Button");
            button.capabilities = inspected(ActionCapabilities::Inspected(vec![
                ActionInfo {
                    name: "default".into(),
                    description: "Default".into(),
                },
                ActionInfo {
                    name: "activate".into(),
                    description: "Activate".into(),
                },
                ActionInfo {
                    name: "menu".into(),
                    description: "Show Menu".into(),
                },
            ]));
            button.window_frame = Some(Rect {
                x: 10,
                y: 20,
                width: 40,
                height: 20,
            });

            let mut edit = node("edit", "text", "Editor");
            edit.capabilities = NodeCapabilities::Inspected(InspectedCapabilities {
                actions: ActionCapabilities::Unsupported,
                component: false,
                editable_text: true,
                value: false,
            });
            edit.states.insert("editable".into());
            edit.states.insert("focused".into());

            let mut slider = node("slider", "slider", "Zoom");
            slider.capabilities = NodeCapabilities::Inspected(InspectedCapabilities {
                actions: ActionCapabilities::Unsupported,
                component: false,
                editable_text: false,
                value: true,
            });

            let nodes = [root, button, edit, slider]
                .into_iter()
                .map(|node| (node.object.clone(), node))
                .collect();
            let mut app = app("Editor", 10, "Main");
            app.windows[0].object = id("root");
            Self {
                state: Arc::new(Mutex::new(FakeState {
                    app,
                    nodes,
                    actions: Vec::new(),
                    discoveries: 0,
                    block_reads: false,
                    fail_actions: false,
                })),
            }
        }
    }

    impl AccessibilityAdapter for FakeAdapter {
        async fn discover(&self) -> Result<Vec<AppInfo>, RuntimeError> {
            let mut state = self.state.lock().unwrap();
            state.discoveries += 1;
            Ok(vec![state.app.clone()])
        }

        async fn read_node<'a>(
            &'a self,
            object: &'a ObjectId,
            _text_limit: usize,
        ) -> Result<NodeInfo, RuntimeError> {
            if self.state.lock().unwrap().block_reads {
                future::pending::<()>().await;
            }
            self.state
                .lock()
                .unwrap()
                .nodes
                .get(object)
                .cloned()
                .ok_or_else(|| operational_error("stale fake object path"))
        }

        async fn act<'a>(
            &'a self,
            object: &'a ObjectId,
            action: SemanticAction,
        ) -> Result<(), RuntimeError> {
            let mut state = self.state.lock().unwrap();
            state.actions.push((object.clone(), action));
            if state.fail_actions {
                return Err(operational_error("fake semantic action failure"));
            }
            Ok(())
        }
    }

    fn fake_runtime(fake: FakeAdapter) -> SemanticRuntime<FakeAdapter> {
        SemanticRuntime::with_config(fake, test_config())
    }

    fn test_config() -> RuntimeConfig {
        RuntimeConfig {
            call_timeout: Duration::from_millis(100),
            snapshot_timeout: Duration::from_millis(500),
            settle_interval: Duration::ZERO,
            ..RuntimeConfig::default()
        }
    }

    fn apps() -> Vec<AppInfo> {
        vec![
            app("Editor", 10, "Preferences"),
            app("Terminal", 20, "Terminal"),
        ]
    }

    fn app(name: &str, pid: u32, title: &str) -> AppInfo {
        AppInfo {
            object: id(&format!("app-{pid}")),
            name: name.into(),
            pid,
            windows: vec![window(title, &["active", "showing"])],
        }
    }

    fn window(title: &str, states: &[&str]) -> WindowInfo {
        WindowInfo {
            object: id(&format!("window-{title}")),
            title: title.into(),
            states: states.iter().map(|state| (*state).into()).collect(),
        }
    }

    fn id(path: &str) -> ObjectId {
        ObjectId {
            bus_name: ":1.2".into(),
            path: format!("/{path}"),
        }
    }

    fn node(path: &str, role: &str, name: &str) -> NodeInfo {
        NodeInfo {
            object: id(path),
            role: role.into(),
            name: name.into(),
            value: None,
            text: None,
            selected_text: None,
            states: BTreeSet::new(),
            capabilities: inspected(ActionCapabilities::Unsupported),
            window_frame: None,
            children: vec![],
        }
    }

    fn inspected(actions: ActionCapabilities) -> NodeCapabilities {
        NodeCapabilities::Inspected(InspectedCapabilities {
            actions,
            component: false,
            editable_text: false,
            value: false,
        })
    }

    fn element(mut node: NodeInfo, depth: usize, frame: Option<Rect>) -> ElementSnapshot {
        node.window_frame = frame;
        ElementSnapshot { depth, node }
    }
}
