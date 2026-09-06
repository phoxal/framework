//! Attachment observation and active-controller binding.

use super::*;

pub(super) async fn observe_attachment(
    attachment: tokio::sync::watch::Sender<Option<SimulationAttachmentState>>,
    transitions: Option<tokio::sync::broadcast::Sender<SimulationAttachmentState>>,
    attachments: crate::bus::StreamReceiver<
        crate::supervisor::api::simulation::attachment::SimulationAttachmentStream,
    >,
    time_domains: crate::bus::StreamReceiver<crate::supervisor::api::time_domain::TimeDomainStream>,
    initial_domain: TimeDomain,
    fault: Arc<Mutex<Option<String>>>,
    bus: BusHandle,
) {
    let controller_bus = transitions.is_none().then_some(&bus);
    let result: Result<(), String> = async {
        // A retained stream does not close when its router disappears. Observe the
        // execution-scoped supervisor identity separately, as ordinary sessions do.
        let (lost_tx, mut lost) = tokio::sync::watch::channel(false);
        let identity = bus
            .observe_liveliness_key(
                crate::supervisor::api::connect::PRESENCE_KEY,
                move |status| {
                    if status == crate::bus::LivelinessStatus::Lost {
                        lost_tx.send_replace(true);
                    }
                },
            )
            .await
            .map_err(|error| error.to_string())?;
        if identity.initial() == crate::bus::LivelinessStatus::Lost || *lost.borrow() {
            return Err("the supervisor identity was lost".to_owned());
        }
        let mut transport_check = tokio::time::interval(std::time::Duration::from_millis(250));
        transport_check.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                _ = transport_check.tick() => {
                    // Client reconnection can retain remote tokens after a hard router exit.
                    // Local transport identity must also remain present, independently of physics.
                    if !bus.execution_router_connected().await.map_err(|error| error.to_string())? {
                        return Err("the supervisor identity lost its execution router".to_owned());
                    }
                }
                _ = lost.changed() => {
                    return Err("the supervisor identity was lost".to_owned());
                }
                update = attachments.recv() => {
                    let update = update.map_err(|error| error.to_string())?;
                    let replacement = update.body.attachment;
                    let current = *attachment.borrow();
                    match (replacement, current) {
                        (Some(replacement), Some(installed))
                            if replacement.revision > installed.revision =>
                        {
                            if let Some(bus) = &controller_bus {
                                install_active_controller_binding(bus, Some(replacement));
                            }
                            attachment.send_replace(Some(replacement));
                            if let Some(transitions) = &transitions {
                                let _ = transitions.send(replacement);
                            }
                        }
                        (Some(replacement), None) => {
                            if let Some(bus) = &controller_bus {
                                install_active_controller_binding(bus, Some(replacement));
                            }
                            attachment.send_replace(Some(replacement));
                            if let Some(transitions) = &transitions {
                                let _ = transitions.send(replacement);
                            }
                        }
                        // Absence is only the initial empty authority in Live
                        // v0. Removing remains retained terminal evidence.
                        (None, _) | (Some(_), Some(_)) => {}
                    }
                }
                update = time_domains.recv() => {
                    let update = update.map_err(|error| error.to_string())?.body.domain;
                    if update.revision > initial_domain.revision {
                        return Err(format!(
                            "the execution time domain changed from revision {} to {} during Live attachment",
                            initial_domain.revision,
                            update.revision,
                        ));
                    }
                }
            }
        }
    }
    .await;
    if let Err(detail) = result {
        if let Some(bus) = &controller_bus {
            bus.set_active_simulation_binding(None);
        }
        *fault
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(detail);
    }
}

pub(super) fn install_active_controller_binding(
    bus: &BusHandle,
    attachment: Option<SimulationAttachmentState>,
) {
    let binding = attachment.and_then(|state| {
        (state.phase == SimulationAttachmentPhase::Active)
            .then_some((state.controller, state.revision))
    });
    bus.set_active_simulation_binding(binding);
}
