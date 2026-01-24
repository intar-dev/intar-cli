use crate::widgets::{
    BriefingScreen, CompletedScreen, ConfirmDialog, HelpMode, HelpOverlay, ProbeStatus,
    ScenarioTreeScreen, VmStatus, VmTreeNode, VmTreeProbe,
};
use crate::{ColorLevel, Theme, ThemeMode, ThemeSettings};
use crossterm::{
    event::{
        self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEvent, KeyEventKind,
        KeyModifiers,
    },
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use intar_core::Scenario;
use intar_vm::{
    ActionLineEvent, ActionLineKind, ImageCache, IntarDirs, ScenarioRunner, ScenarioState, VmError,
};
use ratatui::{
    Terminal,
    backend::CrosstermBackend,
    layout::Rect,
    style::{Style, Stylize},
    text::{Line, Span},
    widgets::Block,
};
use std::{
    borrow::Cow,
    io::{self, Stdout},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};
use thiserror::Error;
use tokio::sync::mpsc;
use tracing::warn;

#[derive(Error, Debug)]
pub enum UiError {
    #[error("IO error: {0}")]
    Io(#[from] io::Error),

    #[error("VM error: {0}")]
    Vm(#[from] intar_vm::VmError),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AppPhase {
    Initializing,
    DownloadingImages,
    CreatingVms,
    BootingVms,
    Running,
    Completed,
    ShuttingDown,
}

#[derive(Clone, Debug)]
pub enum ProgressUpdate {
    DownloadStart {
        image: String,
        total: usize,
        index: usize,
    },
    DownloadProgress {
        progress: f64,
    },
    DownloadComplete,
    VmStart {
        name: String,
        step: String,
        total: usize,
        index: usize,
    },
    VmStep {
        step: String,
    },
    VmComplete,
    BootingVms,
    Ready,
    Error(String),
}

#[derive(Clone, Copy, Debug, Default)]
struct StageTimer {
    started_at: Option<Instant>,
    ended_at: Option<Instant>,
}

impl StageTimer {
    fn start_if_needed(&mut self, now: Instant) {
        if self.started_at.is_none() {
            self.started_at = Some(now);
        }
    }

    fn end_if_needed(&mut self, now: Instant) {
        if self.started_at.is_some() && self.ended_at.is_none() {
            self.ended_at = Some(now);
        }
    }

    fn reset_to_running(&mut self, now: Instant) {
        self.started_at = Some(now);
        self.ended_at = None;
    }

    fn elapsed(&self, now: Instant) -> Option<Duration> {
        self.started_at
            .map(|t0| self.ended_at.unwrap_or(now).duration_since(t0))
    }
}

#[derive(Clone, Copy, Debug)]
struct StageTimers {
    init: StageTimer,
    images: StageTimer,
    vms: StageTimer,
    boot: StageTimer,
    run: StageTimer,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MainTab {
    Briefing,
    Logs,
    System,
}

impl MainTab {
    fn next(self) -> Self {
        match self {
            Self::Briefing => Self::Logs,
            Self::Logs => Self::System,
            Self::System => Self::Briefing,
        }
    }

    fn prev(self) -> Self {
        match self {
            Self::Briefing => Self::System,
            Self::Logs => Self::Briefing,
            Self::System => Self::Logs,
        }
    }
}

impl StageTimers {
    fn new(now: Instant) -> Self {
        let mut init = StageTimer::default();
        init.start_if_needed(now);
        Self {
            init,
            images: StageTimer::default(),
            vms: StageTimer::default(),
            boot: StageTimer::default(),
            run: StageTimer::default(),
        }
    }
}

#[derive(Debug, Clone, Copy, Default)]
enum AltScreenMode {
    Enabled,
    #[default]
    Disabled,
}

impl AltScreenMode {
    fn from_env() -> Self {
        Self::Enabled
    }

    fn enabled(self) -> bool {
        matches!(self, Self::Enabled)
    }
}

#[derive(Debug, Default)]
struct UiFlags {
    should_quit: bool,
    show_confirm_reset: bool,
    show_help: bool,
    alt_screen: AltScreenMode,
}

impl UiFlags {
    fn new() -> Self {
        Self {
            alt_screen: AltScreenMode::from_env(),
            ..Self::default()
        }
    }
}

pub struct App {
    pub scenario: Scenario,
    pub runner: Option<ScenarioRunner>,
    pub phase: AppPhase,
    pub theme: Theme,
    pub theme_mode: ThemeMode,
    pub color_level: ColorLevel,
    pub tick: usize,
    pub error_message: Option<String>,
    pub scroll: u16,
    flags: UiFlags,
    shutdown_signal: Arc<AtomicBool>,
    agent_binary_x86_64: Vec<u8>,
    agent_binary_aarch64: Vec<u8>,
    stages: StageTimers,
    action_lines: Vec<ActionLineEvent>,
    actions_since: Instant,
    pub active_tab: MainTab,
    download_image: Option<String>,
    download_total: usize,
    download_index: usize,
    download_progress: f64,
    vm_progress_name: Option<String>,
    vm_progress_total: usize,
    vm_progress_index: usize,
    vm_progress_step: Option<String>,
}

impl App {
    #[must_use]
    pub fn new(
        scenario: Scenario,
        agent_binary_x86_64: Vec<u8>,
        agent_binary_aarch64: Vec<u8>,
    ) -> Self {
        let now = Instant::now();
        let theme_settings = ThemeSettings::resolve();
        Self {
            scenario,
            runner: None,
            phase: AppPhase::Initializing,
            theme_mode: theme_settings.mode,
            color_level: theme_settings.color_level,
            theme: Theme::for_mode(theme_settings.mode, theme_settings.color_level),
            tick: 0,
            error_message: None,
            scroll: 0,
            flags: UiFlags::new(),
            shutdown_signal: Arc::new(AtomicBool::new(false)),
            agent_binary_x86_64,
            agent_binary_aarch64,
            stages: StageTimers::new(now),
            action_lines: Vec::new(),
            actions_since: now,
            active_tab: MainTab::Briefing,
            download_image: None,
            download_total: 0,
            download_index: 0,
            download_progress: 0.0,
            vm_progress_name: None,
            vm_progress_total: 0,
            vm_progress_index: 0,
            vm_progress_step: None,
        }
    }

    /// Run the TUI event loop.
    ///
    /// # Errors
    /// Returns `UiError` when terminal I/O or VM interactions fail.
    pub async fn run(&mut self) -> Result<(), UiError> {
        let mut terminal = setup_terminal(self.flags.alt_screen.enabled())?;
        self.apply_theme(ThemeSettings::resolve());

        Self::spawn_shutdown_listener(self.shutdown_signal.clone());

        let (progress_tx, mut progress_rx) = mpsc::channel::<ProgressUpdate>(100);

        let mut init_handle = Some(tokio::spawn(Self::start_initialization(
            self.scenario.clone(),
            self.agent_binary_x86_64.clone(),
            self.agent_binary_aarch64.clone(),
            progress_tx,
        )));

        let mut init_result: Option<Result<ScenarioRunner, VmError>> = None;

        let tick_rate = Duration::from_millis(100);
        let probe_check_interval = Duration::from_secs(2);
        let mut last_probe_check = Instant::now();

        loop {
            if self.shutdown_signal.load(Ordering::SeqCst) {
                self.initiate_shutdown(&mut terminal).await?;
                break;
            }

            self.poll_initialization(&mut init_handle, &mut init_result)
                .await?;
            self.drain_progress_updates(&mut progress_rx);
            self.drain_action_lines();

            terminal.draw(|f| self.draw(f))?;

            if self.poll_events(&mut terminal, tick_rate).await? {
                break;
            }

            self.tick = self.tick.wrapping_add(1);

            self.maybe_check_probes(&mut last_probe_check, probe_check_interval)
                .await?;

            if self.flags.should_quit {
                break;
            }
        }

        self.finish_initialization(&mut terminal, &mut init_handle, &mut init_result)
            .await?;

        if let Some(Err(e)) = init_result {
            restore_terminal(&mut terminal, self.flags.alt_screen.enabled())?;
            return Err(e.into());
        }

        Ok(())
    }

    fn spawn_shutdown_listener(shutdown_signal: Arc<AtomicBool>) {
        tokio::spawn(async move {
            let _ = tokio::signal::ctrl_c().await;
            shutdown_signal.store(true, Ordering::SeqCst);
        });
    }

    async fn poll_initialization(
        &mut self,
        init_handle: &mut Option<tokio::task::JoinHandle<Result<ScenarioRunner, VmError>>>,
        init_result: &mut Option<Result<ScenarioRunner, VmError>>,
    ) -> Result<(), UiError> {
        if self.runner.is_none()
            && init_handle
                .as_ref()
                .is_some_and(tokio::task::JoinHandle::is_finished)
            && let Some(handle) = init_handle.take()
        {
            match handle.await {
                Ok(Ok(runner)) => {
                    self.runner = Some(runner);
                }
                Ok(Err(e)) => {
                    *init_result = Some(Err(e));
                    self.flags.should_quit = true;
                }
                Err(e) => {
                    *init_result = Some(Err(VmError::Qemu(format!(
                        "Initialization task failed: {e}",
                    ))));
                    self.flags.should_quit = true;
                }
            }
        }

        Ok(())
    }

    fn drain_progress_updates(&mut self, progress_rx: &mut mpsc::Receiver<ProgressUpdate>) {
        while let Ok(update) = progress_rx.try_recv() {
            self.handle_progress_update(update);
        }
    }

    async fn poll_events(
        &mut self,
        terminal: &mut Terminal<CrosstermBackend<Stdout>>,
        tick_rate: Duration,
    ) -> Result<bool, UiError> {
        if event::poll(tick_rate)?
            && let Event::Key(key) = event::read()?
            && key.kind == KeyEventKind::Press
            && self.handle_key_event(key, terminal).await?
        {
            return Ok(true);
        }

        Ok(false)
    }

    async fn maybe_check_probes(
        &mut self,
        last_probe_check: &mut Instant,
        interval: Duration,
    ) -> Result<(), UiError> {
        if matches!(self.phase, AppPhase::Running)
            && last_probe_check.elapsed() >= interval
            && let Some(ref mut runner) = self.runner
        {
            runner.check_probes().await?;
            if runner.state == ScenarioState::Completed {
                let now = Instant::now();
                self.phase = AppPhase::Completed;
                self.scroll = 0;
                self.stages.run.end_if_needed(now);
            }
            *last_probe_check = Instant::now();
        }

        Ok(())
    }

    fn drain_action_lines(&mut self) {
        let Some(runner) = self.runner.as_mut() else {
            return;
        };

        let mut new = runner.drain_action_lines();
        if new.is_empty() {
            return;
        }

        self.action_lines.append(&mut new);
        self.action_lines
            .retain(|ev| ev.received_at >= self.actions_since);
        self.action_lines
            .sort_by(|a, b| a.received_at.cmp(&b.received_at));
    }

    async fn finish_initialization(
        &mut self,
        terminal: &mut Terminal<CrosstermBackend<Stdout>>,
        init_handle: &mut Option<tokio::task::JoinHandle<Result<ScenarioRunner, VmError>>>,
        init_result: &mut Option<Result<ScenarioRunner, VmError>>,
    ) -> Result<(), UiError> {
        let Some(handle) = init_handle.take() else {
            return Ok(());
        };

        // If the user quit before initialization finished, abort to avoid hanging.
        if self.runner.is_none() && init_result.is_none() && !handle.is_finished() {
            handle.abort();
            let _ = handle.await;
            return Ok(());
        }

        match handle.await {
            Ok(Ok(runner)) => {
                if self.runner.is_none() {
                    self.runner = Some(runner);
                }
                Ok(())
            }
            Ok(Err(e)) => {
                restore_terminal(terminal, self.flags.alt_screen.enabled())?;
                Err(e.into())
            }
            Err(e) => {
                restore_terminal(terminal, self.flags.alt_screen.enabled())?;
                Err(VmError::Qemu(format!("Initialization task failed: {e}")).into())
            }
        }
    }

    async fn handle_key_event(
        &mut self,
        key: crossterm::event::KeyEvent,
        terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    ) -> Result<bool, UiError> {
        if self.flags.show_confirm_reset {
            self.handle_confirm_reset(key).await?;
            return Ok(false);
        }

        let is_ctrl = key.modifiers.contains(KeyModifiers::CONTROL);

        if self.is_briefing_phase() {
            if self.handle_overlay_toggles(key) {
                return Ok(false);
            }
            if Self::should_quit(key, is_ctrl) {
                self.initiate_shutdown(terminal).await?;
                return Ok(true);
            }
            return Ok(false);
        }

        if self.handle_overlay_toggles(key) {
            return Ok(false);
        }

        if Self::should_quit(key, is_ctrl) {
            self.initiate_shutdown(terminal).await?;
            return Ok(true);
        }

        if self.should_reset(key, is_ctrl) {
            self.flags.show_confirm_reset = true;
            return Ok(false);
        }

        self.handle_navigation(key);

        Ok(false)
    }

    async fn handle_confirm_reset(&mut self, key: KeyEvent) -> Result<(), UiError> {
        match key.code {
            KeyCode::Char('y' | 'Y') => {
                self.flags.show_confirm_reset = false;
                if let Some(ref mut runner) = self.runner {
                    runner.reset().await?;
                    let now = Instant::now();
                    self.phase = AppPhase::Running;
                    self.error_message = None;
                    self.stages.run.reset_to_running(now);
                    self.scroll = 0;
                    self.action_lines.clear();
                    self.actions_since = now;
                }
            }
            KeyCode::Char('n' | 'N') | KeyCode::Esc => {
                self.flags.show_confirm_reset = false;
            }
            _ => {}
        }
        Ok(())
    }

    fn handle_overlay_toggles(&mut self, key: KeyEvent) -> bool {
        match key.code {
            KeyCode::Char('?') => {
                self.flags.show_help = !self.flags.show_help;
                true
            }
            KeyCode::Char('t') => {
                self.theme_mode = self.theme_mode.toggle();
                self.theme = Theme::for_mode(self.theme_mode, self.color_level);
                true
            }
            _ => false,
        }
    }

    fn should_quit(key: KeyEvent, is_ctrl: bool) -> bool {
        (is_ctrl && matches!(key.code, KeyCode::Char('c' | 'q'))) || key.code == KeyCode::Char('q')
    }

    fn should_reset(&self, key: KeyEvent, is_ctrl: bool) -> bool {
        key.code == KeyCode::Char('r')
            && (is_ctrl || matches!(self.phase, AppPhase::Running | AppPhase::Completed))
            && self.runner.is_some()
    }

    fn handle_navigation(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Tab => {
                if key.modifiers.contains(KeyModifiers::SHIFT) {
                    self.active_tab = self.active_tab.prev();
                } else {
                    self.active_tab = self.active_tab.next();
                }
                self.scroll = 0;
            }
            KeyCode::BackTab => {
                self.active_tab = self.active_tab.prev();
                self.scroll = 0;
            }
            KeyCode::PageUp => {
                if self.active_tab == MainTab::Logs {
                    self.scroll = self.scroll.saturating_add(10);
                }
            }
            KeyCode::PageDown => {
                if self.active_tab == MainTab::Logs {
                    self.scroll = self.scroll.saturating_sub(10);
                }
            }
            KeyCode::Home => {
                if self.active_tab == MainTab::Logs {
                    self.scroll = u16::MAX;
                }
            }
            KeyCode::End => {
                if self.active_tab == MainTab::Logs {
                    self.scroll = 0;
                }
            }
            _ => {}
        }
    }

    async fn start_initialization(
        scenario: Scenario,
        agent_binary_x86_64: Vec<u8>,
        agent_binary_aarch64: Vec<u8>,
        progress_tx: mpsc::Sender<ProgressUpdate>,
    ) -> Result<ScenarioRunner, VmError> {
        let dirs = IntarDirs::new()?;
        dirs.ensure_dirs()?;

        let image_cache = ImageCache::new(dirs.images_dir());
        let arch = detect_arch();

        let mut images_needed: Vec<(String, &intar_core::ImageSource)> = Vec::new();
        for vm_def in &scenario.vms {
            let image_spec = scenario.images.get(&vm_def.image).ok_or_else(|| {
                VmError::Qemu(format!("Image '{}' not defined in scenario", vm_def.image))
            })?;

            let source = image_spec.source_for_arch(&arch).ok_or_else(|| {
                VmError::Qemu(format!(
                    "No image source for architecture '{}' in image '{}'",
                    arch, vm_def.image
                ))
            })?;

            if !images_needed.iter().any(|(name, _)| name == &vm_def.image) {
                images_needed.push((vm_def.image.clone(), source));
            }
        }

        let total_images = images_needed.len();
        for (i, (image_name, source)) in images_needed.iter().enumerate() {
            let _ = progress_tx
                .send(ProgressUpdate::DownloadStart {
                    image: image_name.clone(),
                    total: total_images,
                    index: i,
                })
                .await;

            if !image_cache.is_cached(source) {
                let tx = progress_tx.clone();
                image_cache
                    .ensure_image_with_progress(source, move |progress| {
                        let _ = tx.try_send(ProgressUpdate::DownloadProgress { progress });
                    })
                    .await?;
            }

            let _ = progress_tx.send(ProgressUpdate::DownloadComplete).await;
        }

        let mut runner = ScenarioRunner::new_with_dirs(
            scenario.clone(),
            agent_binary_x86_64,
            agent_binary_aarch64,
            &dirs,
        )?;

        let total_vms = scenario.vms.len();
        for (i, vm_def) in scenario.vms.iter().enumerate() {
            let _ = progress_tx
                .send(ProgressUpdate::VmStart {
                    name: vm_def.name.clone(),
                    step: "Creating overlay disk".to_string(),
                    total: total_vms,
                    index: i,
                })
                .await;

            runner.create_vm(vm_def, &image_cache, &arch)?;

            let _ = progress_tx.send(ProgressUpdate::VmComplete).await;
        }

        let _ = progress_tx.send(ProgressUpdate::BootingVms).await;

        runner.start_vms()?;
        runner.start_action_recording()?;

        runner.wait_for_agents().await?;
        runner.wait_for_boot_probes().await?;

        // Create a snapshot for fast resets.
        runner.save_checkpoint("init").await?;

        runner.state = ScenarioState::Running;

        let _ = progress_tx.send(ProgressUpdate::Ready).await;

        Ok(runner)
    }

    fn handle_progress_update(&mut self, update: ProgressUpdate) {
        let now = Instant::now();

        match update {
            ProgressUpdate::DownloadStart {
                image,
                total,
                index,
            } => {
                self.download_image = Some(image);
                self.download_total = total;
                self.download_index = index;
                self.download_progress = 0.0;
                self.stages.init.end_if_needed(now);
                self.stages.images.start_if_needed(now);
                self.phase = AppPhase::DownloadingImages;
            }
            ProgressUpdate::DownloadProgress { progress } => {
                self.download_progress = progress.clamp(0.0, 1.0);
            }
            ProgressUpdate::DownloadComplete => {
                self.download_progress = 1.0;
            }
            ProgressUpdate::VmStep { step } => {
                self.vm_progress_step = Some(step);
            }
            ProgressUpdate::VmComplete => {}
            ProgressUpdate::VmStart {
                name,
                step,
                total,
                index,
            } => {
                self.vm_progress_name = Some(name);
                self.vm_progress_step = Some(step);
                self.vm_progress_total = total;
                self.vm_progress_index = index;
                self.stages.images.end_if_needed(now);
                self.stages.vms.start_if_needed(now);
                self.phase = AppPhase::CreatingVms;
            }
            ProgressUpdate::BootingVms => {
                self.stages.vms.end_if_needed(now);
                self.stages.boot.start_if_needed(now);
                self.phase = AppPhase::BootingVms;
                self.vm_progress_name = None;
                self.vm_progress_step = None;
            }
            ProgressUpdate::Ready => {
                self.stages.boot.end_if_needed(now);
                self.stages.run.start_if_needed(now);
                self.phase = AppPhase::Running;
                self.scroll = 0;
                self.action_lines.clear();
                self.actions_since = now;
            }
            ProgressUpdate::Error(msg) => {
                self.error_message = Some(msg);
            }
        }
    }

    async fn initiate_shutdown(
        &mut self,
        terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    ) -> Result<(), UiError> {
        self.phase = AppPhase::ShuttingDown;
        terminal.draw(|f| self.draw(f))?;

        if let Some(mut runner) = self.runner.take() {
            let run_dir = runner.work_dir.clone();

            if let Err(e) = runner.stop().await {
                warn!("Failed to stop scenario cleanly: {}", e);
            }

            if let Err(e) = runner.cleanup() {
                warn!(
                    "Failed to delete scenario artifacts at {}: {}",
                    run_dir.display(),
                    e
                );
            }
        }
        restore_terminal(terminal, self.flags.alt_screen.enabled())?;

        Ok(())
    }

    fn draw(&self, f: &mut ratatui::Frame) {
        let area = f.area();
        self.draw_background(f, area);

        match self.phase {
            AppPhase::ShuttingDown => self.draw_shutting_down(f, area),
            AppPhase::Completed => self.draw_completed(f, area),
            AppPhase::Initializing
            | AppPhase::DownloadingImages
            | AppPhase::CreatingVms
            | AppPhase::BootingVms => self.draw_briefing(f, area),
            AppPhase::Running => self.draw_hud(f, area),
        }

        self.draw_overlays(f, area);
    }

    fn draw_background(&self, f: &mut ratatui::Frame, area: Rect) {
        let background = Block::default().style(Style::default().bg(self.theme.bg));
        f.render_widget(background, area);
    }

    fn draw_shutting_down(&self, f: &mut ratatui::Frame, area: Rect) {
        let vm_names = self.runner.as_ref().map_or_else(
            || self.scenario.vms.iter().map(|v| v.name.clone()).collect(),
            |r| r.vms.keys().cloned().collect(),
        );
        let shutdown = crate::widgets::ShutdownScreen {
            vm_names,
            theme: &self.theme,
        };
        f.render_widget(shutdown, area);
    }

    fn draw_hud(&self, f: &mut ratatui::Frame, area: Rect) {
        let now = Instant::now();
        let vms = self.vm_tree_nodes();
        let run_name = self.run_name();
        let boot_elapsed = self.boot_elapsed(now);
        let run_elapsed = self.run_elapsed(now);
        let action_lines = self.action_lines_for_display();

        let screen = ScenarioTreeScreen {
            scenario_name: &self.scenario.name,
            scenario_description: &self.scenario.description,
            run_name,
            phase: self.phase_label(),
            boot_elapsed,
            run_elapsed,
            vms: &vms,
            action_lines: &action_lines,
            scroll: self.scroll,
            theme: &self.theme,
            tick: self.tick,
            active_tab: self.active_tab,
        };
        f.render_widget(screen, area);
    }

    fn draw_briefing(&self, f: &mut ratatui::Frame, area: Rect) {
        let now = Instant::now();
        let vms = self.vm_tree_nodes();
        let run_name = self.run_name();
        let boot_elapsed = self.boot_elapsed(now);
        let run_elapsed = self.run_elapsed(now);

        let screen = BriefingScreen {
            scenario_name: &self.scenario.name,
            scenario_description: &self.scenario.description,
            run_name,
            phase: self.phase_label(),
            boot_elapsed,
            run_elapsed,
            vms: &vms,
            theme: &self.theme,
            tick: self.tick,
        };
        f.render_widget(screen, area);
    }

    fn run_name(&self) -> Option<&str> {
        self.runner
            .as_ref()
            .and_then(|runner| runner.work_dir.file_name())
            .and_then(|name| name.to_str())
    }

    fn draw_completed(&self, f: &mut ratatui::Frame, area: Rect) {
        let now = Instant::now();
        let solve_duration = self.stages.run.elapsed(now).unwrap_or(Duration::ZERO);
        let run_start = self.stages.run.started_at.unwrap_or(now);
        let completed_at = self.stages.run.ended_at.unwrap_or(now);
        let credits_elapsed = now.saturating_duration_since(completed_at);

        let credits = self.action_lines_for_display_with_start(run_start);

        let run_name = self
            .runner
            .as_ref()
            .and_then(|runner| runner.work_dir.file_name())
            .and_then(|name| name.to_str());

        let screen = CompletedScreen {
            scenario_name: &self.scenario.name,
            run_name,
            solve_duration,
            credits,
            credits_elapsed,
            theme: &self.theme,
        };
        f.render_widget(screen, area);
    }

    fn vm_tree_nodes(&self) -> Vec<VmTreeNode<'_>> {
        let runner = self.runner.as_ref();

        self.scenario
            .vms
            .iter()
            .map(|vm_def| {
                let vm_results = runner.and_then(|r| r.probe_results.get(&vm_def.name));
                let vm_state = runner.and_then(|r| r.vms.get(&vm_def.name));
                let status = vm_state.map_or(VmStatus::Unknown, |vm| match vm.state {
                    intar_vm::VmState::Starting => VmStatus::Starting,
                    intar_vm::VmState::Booting => VmStatus::Booting,
                    intar_vm::VmState::CloudInit => VmStatus::CloudInit,
                    intar_vm::VmState::Ready => VmStatus::Ready,
                    intar_vm::VmState::Error => VmStatus::Error,
                });

                let mut boot_passing = 0usize;
                let mut boot_total = 0usize;
                let mut scenario_probes = Vec::new();

                for probe_name in &vm_def.probes {
                    let Some(def) = self.scenario.probes.get(probe_name) else {
                        continue;
                    };

                    match def.phase {
                        intar_core::ProbePhase::Boot => {
                            boot_total += 1;
                            if vm_results
                                .and_then(|m| m.get(probe_name))
                                .is_some_and(|r| r.passed)
                            {
                                boot_passing += 1;
                            }
                        }
                        intar_core::ProbePhase::Scenario => {
                            let status = vm_results.and_then(|m| m.get(probe_name)).map_or(
                                ProbeStatus::Pending,
                                |r| {
                                    if r.passed {
                                        ProbeStatus::Passed
                                    } else {
                                        ProbeStatus::Failed
                                    }
                                },
                            );

                            scenario_probes.push(VmTreeProbe {
                                name: Cow::Borrowed(probe_name.as_str()),
                                status,
                                description: def.description.as_deref().map(Cow::Borrowed),
                            });
                        }
                    }
                }

                VmTreeNode {
                    name: Cow::Borrowed(vm_def.name.as_str()),
                    status,
                    cpu: vm_def.cpu,
                    memory: vm_def.memory,
                    disk: vm_def.disk,
                    ssh_port: vm_state.map(|vm| vm.ssh_port),
                    boot_passing,
                    boot_total,
                    scenario_probes,
                }
            })
            .collect()
    }

    fn phase_label(&self) -> &'static str {
        match self.phase {
            AppPhase::Initializing => "INIT",
            AppPhase::DownloadingImages => "IMAGES",
            AppPhase::CreatingVms => "VMS",
            AppPhase::BootingVms => "BOOT",
            AppPhase::Running => "RUN",
            AppPhase::Completed => "DONE",
            AppPhase::ShuttingDown => "SHUTDOWN",
        }
    }

    fn is_briefing_phase(&self) -> bool {
        matches!(
            self.phase,
            AppPhase::Initializing
                | AppPhase::DownloadingImages
                | AppPhase::CreatingVms
                | AppPhase::BootingVms
        )
    }

    fn boot_elapsed(&self, now: Instant) -> Option<Duration> {
        self.stages.boot.elapsed(now)
    }

    fn run_elapsed(&self, now: Instant) -> Option<Duration> {
        self.stages.run.elapsed(now)
    }

    fn action_lines_for_display(&self) -> Vec<Line<'static>> {
        let run_start = self.stages.run.started_at.unwrap_or(self.actions_since);
        self.action_lines_for_display_with_start(run_start)
    }

    fn action_lines_for_display_with_start(&self, run_start: Instant) -> Vec<Line<'static>> {
        let mut lines = Vec::with_capacity(self.action_lines.len());
        for ev in &self.action_lines {
            let rel = ev.received_at.saturating_duration_since(run_start);
            let ts = format_mm_ss(rel);
            let (prefix, line_style) = match ev.kind {
                ActionLineKind::Input => ("$ ", Style::default().fg(self.theme.primary)),
                ActionLineKind::Output => ("  ", Style::default().fg(self.theme.fg)),
            };

            lines.push(Line::from(vec![
                Span::styled(ts, Style::default().fg(self.theme.secondary)),
                Span::raw("  "),
                Span::styled(ev.vm.clone(), Style::default().fg(self.theme.info).bold()),
                Span::styled(" │ ", Style::default().fg(self.theme.dim)),
                Span::styled(prefix, Style::default().fg(self.theme.dim)),
                Span::styled(ev.line.clone(), line_style),
            ]));
        }
        lines
    }

    fn draw_overlays(&self, f: &mut ratatui::Frame, area: Rect) {
        if self.flags.show_confirm_reset {
            let dialog = ConfirmDialog {
                title: "Restart Scenario",
                message: "Restart scenario from the initial state?\nAll progress will be lost.",
                theme: &self.theme,
            };
            f.render_widget(dialog, area);
            return;
        }

        if self.flags.show_help {
            let mode = if self.is_briefing_phase() {
                HelpMode::Briefing
            } else if matches!(self.phase, AppPhase::Completed) {
                HelpMode::Completed
            } else {
                HelpMode::Running
            };
            let help = HelpOverlay {
                theme: &self.theme,
                mode,
            };
            f.render_widget(help, area);
        }
    }

    fn apply_theme(&mut self, settings: ThemeSettings) {
        self.theme_mode = settings.mode;
        self.color_level = settings.color_level;
        self.theme = Theme::for_mode(settings.mode, settings.color_level);
    }
}

fn setup_terminal(use_alt_screen: bool) -> Result<Terminal<CrosstermBackend<Stdout>>, io::Error> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    if use_alt_screen {
        execute!(stdout, EnterAlternateScreen)?;
    }
    execute!(stdout, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    Terminal::new(backend)
}

fn restore_terminal(
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    use_alt_screen: bool,
) -> Result<(), io::Error> {
    disable_raw_mode()?;
    if use_alt_screen {
        execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    }
    execute!(terminal.backend_mut(), DisableMouseCapture)?;
    terminal.show_cursor()?;
    Ok(())
}

fn detect_arch() -> String {
    #[cfg(target_arch = "x86_64")]
    return "x86_64".to_string();

    #[cfg(target_arch = "aarch64")]
    return "aarch64".to_string();

    #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
    return std::env::consts::ARCH.to_string();
}

fn format_mm_ss(d: Duration) -> String {
    let secs = d.as_secs();
    let mins = secs / 60;
    let secs = secs % 60;
    format!("{mins:02}:{secs:02}")
}
