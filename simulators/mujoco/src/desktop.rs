//! Optional egui presentation for the shared simulation coordinator.

use eframe::egui::{self, Color32, Pos2, Sense, Stroke};
use phoxal_mujoco::{ControlledError, StateSnapshot};

use crate::core::{RunBounds, RunOutcome, RunSummary, SimulationCore};

/// Opens the desktop presentation for one finite run bound.
pub fn run(core: SimulationCore, bounds: RunBounds) -> Result<(), String> {
    let resolved = bounds
        .resolve(core.quantum())
        .map_err(|error| error.to_string())?;
    let initial = core.snapshot().map_err(|error| error.to_string())?;
    let app = DesktopApp {
        core,
        bounds: resolved,
        start_boundary: initial.boundary(),
        requested_duration_seconds: match bounds {
            RunBounds::Steps(_) => None,
            RunBounds::Duration(duration) => Some(duration),
        },
        remaining: resolved.steps,
        pending_steps: 0,
        step_count: 1,
        latest: initial,
        camera: Camera::Top,
        playing: false,
        summary: None,
        diagnostic: None,
    };
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([960.0, 720.0])
            .with_min_inner_size([640.0, 480.0]),
        ..Default::default()
    };
    eframe::run_native(
        "Phoxal MuJoCo",
        options,
        Box::new(|_creation_context| Ok(Box::new(app))),
    )
    .map_err(|error| error.to_string())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Camera {
    Top,
    Side,
}

impl Camera {
    fn label(self) -> &'static str {
        match self {
            Self::Top => "Top",
            Self::Side => "Side",
        }
    }
}

struct DesktopApp {
    core: SimulationCore,
    bounds: crate::core::ResolvedBounds,
    start_boundary: u64,
    requested_duration_seconds: Option<f64>,
    remaining: u64,
    pending_steps: u64,
    step_count: u64,
    latest: StateSnapshot,
    camera: Camera,
    playing: bool,
    summary: Option<RunSummary>,
    diagnostic: Option<String>,
}

impl DesktopApp {
    fn advance_one(&mut self) {
        if self.pending_steps == 0 {
            self.playing = false;
            return;
        }
        if self.remaining == 0 {
            self.playing = false;
            if self.summary.is_none() {
                self.finish_success();
            }
            return;
        }
        match self.core.step() {
            Ok(()) => {
                self.remaining -= 1;
                self.pending_steps -= 1;
                self.refresh_snapshot();
                if self.remaining == 0 {
                    self.playing = false;
                    self.finish_success();
                } else if self.pending_steps == 0 {
                    self.playing = false;
                }
            }
            Err(error) => self.finish_failure(error),
        }
    }

    fn advance_many(&mut self) {
        self.pending_steps = self.step_count.min(self.remaining);
        self.playing = self.pending_steps > 0;
        if !self.playing && self.summary.is_none() {
            self.finish_success();
        }
    }

    fn reset(&mut self) {
        self.playing = false;
        self.core.clear_stop();
        match self.core.reset_snapshot() {
            Ok(snapshot) => {
                let boundary = snapshot.boundary();
                self.latest = snapshot;
                self.start_boundary = boundary;
                self.remaining = self.bounds.steps;
                self.pending_steps = 0;
                self.summary = None;
                self.diagnostic = None;
            }
            Err(error) => self.diagnostic = Some(error.to_string()),
        }
    }

    fn stop(&mut self) {
        self.core.request_stop();
        let completed_steps = self.bounds.steps.saturating_sub(self.remaining);
        self.pending_steps = 0;
        self.remaining = 0;
        self.playing = false;
        self.summary = Some(RunSummary {
            schema: "phoxal/simulation-run/v0",
            model_id: self.core.model_id().to_string(),
            execution: self.core.execution().to_string(),
            timeline: self.core.timeline().to_string(),
            quantum_seconds: self.core.quantum().as_seconds(),
            requested_steps: self.bounds.steps,
            requested_duration_seconds: self.requested_duration_seconds,
            start_boundary: self.start_boundary,
            final_boundary: self.core.boundary().index(),
            completed_steps,
            outcome: RunOutcome::Cancelled,
            failure: Some(crate::core::RunFailure {
                kind: "cancelled".to_owned(),
                message: "stop requested at a completed boundary".to_owned(),
                native_boundary: self.core.boundary().index(),
            }),
        });
        self.diagnostic = Some("stopped; reset starts a fresh healthy timeline".to_owned());
    }

    fn refresh_snapshot(&mut self) {
        match self.core.snapshot() {
            Ok(snapshot) => self.latest = snapshot,
            Err(error) => self.diagnostic = Some(error.to_string()),
        }
    }

    fn finish_success(&mut self) {
        self.summary = Some(RunSummary {
            schema: "phoxal/simulation-run/v0",
            model_id: self.core.model_id().to_string(),
            execution: self.core.execution().to_string(),
            timeline: self.core.timeline().to_string(),
            quantum_seconds: self.core.quantum().as_seconds(),
            requested_steps: self.bounds.steps,
            requested_duration_seconds: self.requested_duration_seconds,
            start_boundary: self.start_boundary,
            final_boundary: self.core.boundary().index(),
            completed_steps: self.bounds.steps.saturating_sub(self.remaining),
            outcome: RunOutcome::Success,
            failure: None,
        });
    }

    fn finish_failure(&mut self, error: ControlledError) {
        self.playing = false;
        self.pending_steps = 0;
        self.diagnostic = Some(error.to_string());
        self.summary = Some(RunSummary {
            schema: "phoxal/simulation-run/v0",
            model_id: self.core.model_id().to_string(),
            execution: self.core.execution().to_string(),
            timeline: self.core.timeline().to_string(),
            quantum_seconds: self.core.quantum().as_seconds(),
            requested_steps: self.bounds.steps,
            requested_duration_seconds: self.requested_duration_seconds,
            start_boundary: self.start_boundary,
            final_boundary: self.core.boundary().index(),
            completed_steps: self.bounds.steps.saturating_sub(self.remaining),
            outcome: RunOutcome::Failed,
            failure: Some(crate::core::RunFailure {
                kind: crate::core::controlled_error_kind(&error).to_owned(),
                message: error.to_string(),
                native_boundary: self.core.boundary().index(),
            }),
        });
    }

    fn draw_scene(&self, ui: &mut egui::Ui) {
        let (response, painter) = ui.allocate_painter(ui.available_size(), Sense::hover());
        painter.rect_stroke(
            response.rect,
            0.0,
            Stroke::new(1.0, Color32::DARK_GRAY),
            egui::epaint::StrokeKind::Inside,
        );
        let center = response.rect.center();
        let scale = 80.0_f32;
        for (index, position) in self.latest.body_positions().iter().enumerate() {
            let (horizontal, vertical) = match self.camera {
                Camera::Top => (position[0], position[1]),
                Camera::Side => (position[0], position[2]),
            };
            let point = Pos2::new(
                center.x + horizontal as f32 * scale,
                center.y - vertical as f32 * scale,
            );
            painter.circle_filled(point, 6.0, Color32::from_rgb(40, 130, 220));
            painter.text(
                point + egui::vec2(8.0, -8.0),
                egui::Align2::LEFT_TOP,
                format!("body {index}"),
                egui::FontId::proportional(12.0),
                Color32::WHITE,
            );
        }
    }
}

impl eframe::App for DesktopApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        if self.playing {
            self.advance_one();
            ui.ctx().request_repaint();
        }
        egui::Panel::top("controls").show(ui, |ui| {
            ui.horizontal(|ui| {
                if ui
                    .button(if self.playing { "Pause" } else { "Play" })
                    .clicked()
                {
                    if self.summary.is_some() && self.remaining == 0 {
                        return;
                    }
                    if !self.playing {
                        self.pending_steps = self.remaining;
                    }
                    self.playing = !self.playing;
                    self.diagnostic = None;
                }
                if ui.button("Step").clicked() {
                    self.playing = false;
                    self.pending_steps = 1;
                    self.advance_one();
                }
                ui.add(
                    egui::DragValue::new(&mut self.step_count)
                        .range(1..=1_000_000_u64)
                        .prefix("N = "),
                );
                if ui.button("Step N").clicked() {
                    self.playing = false;
                    self.advance_many();
                }
                if ui.button("Reset").clicked() {
                    self.reset();
                }
                if ui.button("Stop").clicked() {
                    self.stop();
                }
                egui::ComboBox::from_label("Camera")
                    .selected_text(self.camera.label())
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut self.camera, Camera::Top, "Top");
                        ui.selectable_value(&mut self.camera, Camera::Side, "Side");
                    });
            });
        });
        egui::Panel::right("status")
            .default_size(300.0)
            .show(ui, |ui| {
                ui.heading("Simulation");
                ui.label(format!("model {}", self.core.model_id()));
                ui.label(format!("execution {}", self.core.execution()));
                ui.label(format!("timeline {}", self.core.timeline()));
                ui.label(format!("phase {:?}", self.core.phase()));
                ui.label(format!("boundary {}", self.core.boundary().index()));
                ui.label(format!("time {:.6} s", self.latest.time_seconds()));
                ui.label(format!("quantum {:.6} s", self.core.quantum().as_seconds()));
                ui.label(format!("remaining {}", self.remaining));
                if let Some(summary) = &self.summary {
                    ui.separator();
                    ui.label(format!("outcome {:?}", summary.outcome));
                    ui.label(format!("completed {}", summary.completed_steps));
                }
                if let Some(diagnostic) = &self.diagnostic {
                    ui.separator();
                    ui.colored_label(Color32::LIGHT_RED, diagnostic);
                }
            });
        egui::CentralPanel::default().show(ui, |ui| self.draw_scene(ui));
    }
}
